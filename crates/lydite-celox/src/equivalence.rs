//! Translation validation of Celox's SIR optimizer.
//!
//! Both programs run from the same arbitrary stable state: every lane of every
//! state object is a fresh symbol, so inputs, registers and internal signals
//! are all unconstrained. Each phase sequence is lifted on both sides and the
//! resulting stable (region 0) state is compared lane by lane. `Equivalent`
//! means the solver proved every lane equal for all such states; anything the
//! lifter cannot model is `Unsupported`, never a pass.
use super::*;
use celox_sir::SirProgram;
use lydite_solver::finite::{Limits, SearchHint, Verdict, solve_with_hint};
use std::collections::HashMap;
use std::hash::BuildHasher;

pub type Program = SirProgram<StateAddr, RegionedStateAddr>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Equivalent,
    /// Some stable lane differs; `lane` names it and `witness` is a validated model.
    Counterexample {
        lane: String,
        witness: BTreeMap<String, String>,
    },
    Unsupported(String),
    Unknown(String),
}

#[derive(Clone, Debug)]
pub struct PhaseResult {
    pub phase: String,
    pub status: Status,
    pub lanes: usize,
    pub solver_queries: usize,
    /// SIR instructions executed by the symbolic run of each side.
    pub instructions: (usize, usize),
}

/// Units run in order from one arbitrary stable state.
struct Phase<'a> {
    name: String,
    pre: Vec<&'a [Unit]>,
    post: Vec<&'a [Unit]>,
}

fn lane_name(a: Addr, element: usize) -> String {
    format!("s.i{}_v{}_e{}", a.0, a.1, element)
}

fn initial<'a>(
    objects: impl IntoIterator<Item = (&'a StateAddr, &'a VariableMetadata)>,
) -> Res<Frame> {
    let mut frame = Frame {
        guard: b(true),
        state: BTreeMap::new(),
        regs: BTreeMap::new(),
    };
    for (address, metadata) in objects {
        let a = state_addr(address);
        let (w, n) = storage_shape(metadata)?;
        let mut storage = Storage::new(w, n, false);
        for (element, cell) in storage.cells.iter_mut().enumerate() {
            cell.value = Some(ir::var(lane_name(a, element), Sort::Bv(w)));
        }
        frame.state.insert(a, storage);
    }
    Ok(frame)
}

struct Run {
    frame: Frame,
    events: Events,
    instructions: usize,
}

/// One proof obligation: under `condition`, `x` and `y` must be equal.
struct Obligation {
    name: String,
    condition: Term,
    x: Term,
    y: Term,
    /// A failed soft obligation weakens the run instead of ending it.
    soft: bool,
}

fn obligation(name: impl Into<String>, condition: Term, x: Term, y: Term) -> Obligation {
    Obligation {
        name: name.into(),
        condition,
        x,
        y,
        soft: false,
    }
}

struct Prover<'a> {
    limits: &'a Limits,
    normalizer: guard::Normalizer,
    queries: usize,
    obligations: usize,
}

impl Prover<'_> {
    /// A satisfiable obligation is a counterexample only when `exact`; under an
    /// inductive hypothesis it may be an unreachable state, so it is unknown.
    /// `Ok(false)` only for a soft obligation that was not proved.
    fn prove(&mut self, o: Obligation, exact: bool) -> Result<bool, Status> {
        self.obligations += 1;
        let (x, y) = (
            self.normalizer.normalize(&o.x),
            self.normalizer.normalize(&o.y),
        );
        if x == y || o.condition == b(false) {
            return Ok(true);
        }
        self.queries += 1;
        let formula = and(o.condition, not(eq(x, y)));
        let outcome = solve_with_hint(
            &formula,
            &Env::new(),
            self.limits.clone(),
            SearchHint::Unsat,
        );
        match outcome.verdict {
            Verdict::Unsat => Ok(true),
            _ if o.soft => Ok(false),
            Verdict::Sat if outcome.original_formula_validated && exact => {
                Err(Status::Counterexample {
                    lane: o.name,
                    witness: outcome
                        .assignments
                        .iter()
                        .map(|(k, v)| (k.clone(), v.json().to_string()))
                        .collect(),
                })
            }
            Verdict::Sat => Err(Status::Unknown(format!(
                "{}: not proved under the loop hypothesis or an approximation",
                o.name
            ))),
            _ => Err(Status::Unknown(format!(
                "{}: {}",
                o.name,
                outcome.diagnostics()["reason"]
            ))),
        }
    }
}

fn unsupported(e: String) -> Status {
    Status::Unsupported(e)
}

/// Targets of back edges in a depth-first walk from the entry.
fn loop_headers(unit: &Unit) -> BTreeSet<usize> {
    let mut marks = BTreeMap::new();
    let mut headers = BTreeSet::new();
    let mut stack = vec![(unit.entry_block_id.0, 0usize)];
    marks.insert(unit.entry_block_id.0, 1u8);
    while let Some((id, next)) = stack.pop() {
        let successors = unit
            .blocks
            .get(&BlockId(id))
            .map(|block| successors(&block.terminator))
            .unwrap_or_default();
        if let Some(&target) = successors.get(next) {
            stack.push((id, next + 1));
            match marks.get(&target) {
                Some(1) => {
                    headers.insert(target);
                }
                Some(_) => {}
                None => {
                    marks.insert(target, 1);
                    stack.push((target, 0));
                }
            }
        } else {
            marks.insert(id, 2);
        }
    }
    headers
}

/// Storage a unit reads or writes, and the subset it writes.
fn accesses(unit: &Unit) -> (BTreeSet<Addr>, BTreeSet<Addr>) {
    let (mut touched, mut written) = (BTreeSet::new(), BTreeSet::new());
    for inst in unit.blocks.values().flat_map(|b| &b.instructions) {
        match inst {
            SIRInstruction::Load(_, a, ..) => {
                touched.insert(addr(a));
            }
            SIRInstruction::Store(a, ..) => {
                touched.insert(addr(a));
                written.insert(addr(a));
            }
            SIRInstruction::Commit(src, dst, ..) => {
                touched.extend([addr(src), addr(dst)]);
                written.insert(addr(dst));
            }
            _ => {}
        }
    }
    (touched, written)
}

/// Registers assigned (or bound as block parameters) in blocks reachable from
/// `start`; their values at `start` may come from an earlier iteration.
fn assigned_from(unit: &Unit, start: usize) -> BTreeSet<usize> {
    let (mut seen, mut todo, mut result) = (BTreeSet::new(), vec![start], BTreeSet::new());
    while let Some(id) = todo.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(block) = unit.blocks.get(&BlockId(id)) else {
            continue;
        };
        result.extend(block.params.iter().map(|p| p.0));
        for inst in &block.instructions {
            let dst = match inst {
                SIRInstruction::Imm(d, _)
                | SIRInstruction::Binary(d, ..)
                | SIRInstruction::Unary(d, ..)
                | SIRInstruction::Load(d, ..)
                | SIRInstruction::Concat(d, _)
                | SIRInstruction::Slice(d, ..)
                | SIRInstruction::Mux(d, ..) => Some(d),
                SIRInstruction::ExternCall { dst, .. } => dst.as_ref(),
                _ => None,
            };
            result.extend(dst.map(|d| d.0));
        }
        todo.extend(successors(&block.terminator));
    }
    result
}

/// Replace `addrs` in both states by the same fresh symbols.
fn havoc(prefix: &str, addrs: &BTreeSet<Addr>, x: &mut Frame, y: &mut Frame) -> Res<()> {
    havoc_apart(prefix, addrs, &BTreeSet::new(), x, y)
}

/// Like `havoc`, but each side gets its own symbols for the addresses in `apart`.
fn havoc_apart(
    prefix: &str,
    addrs: &BTreeSet<Addr>,
    apart: &BTreeSet<Addr>,
    x: &mut Frame,
    y: &mut Frame,
) -> Res<()> {
    for a in addrs {
        if apart.contains(a) {
            let one = BTreeSet::from([*a]);
            havoc(&format!("{prefix}.pre"), &one, x, &mut x.clone())?;
            havoc(&format!("{prefix}.post"), &one, y, &mut y.clone())?;
            continue;
        }
        let shape = x.state.get(a).ok_or("havoc of unknown storage")?;
        let (lane, count) = (shape.lane, shape.cells.len());
        let other = y.state.get(a).ok_or("havoc of unknown storage")?;
        if other.lane != lane || other.cells.len() != count {
            return Err(format!("storage {a:?} differs in shape across the loop"));
        }
        let mut storage = Storage::new(lane, count, a.2 == 2);
        for (e, cell) in storage.cells.iter_mut().enumerate() {
            let name = format!("{prefix}.i{}_v{}_r{}_e{e}", a.0, a.1, a.2);
            cell.value = Some(ir::var(name.clone(), Sort::Bv(lane)));
            if a.2 == 2 {
                cell.mask = ir::var(format!("{name}.mask"), Sort::Bv(lane));
                cell.whole_word = None;
            }
        }
        x.state.insert(*a, storage.clone());
        y.state.insert(*a, storage);
    }
    Ok(())
}

/// Obligations that two frames leaving a region by the same way agree:
/// equal guards, and under the guard equal storage and `values`. With
/// `soft_working`, working-region (1) lanes are soft: a working store the
/// optimizer removed as dead may legitimately leave them different.
fn agree(
    label: &str,
    x: Option<Frame>,
    y: Option<Frame>,
    values: &[(Term, Term)],
    soft_working: bool,
) -> Res<Vec<Obligation>> {
    let none = || Frame {
        guard: b(false),
        state: BTreeMap::new(),
        regs: BTreeMap::new(),
    };
    let (x, y) = (x.unwrap_or_else(none), y.unwrap_or_else(none));
    let mut result = vec![obligation(
        format!("{label}.guard"),
        b(true),
        x.guard.clone(),
        y.guard.clone(),
    )];
    if x.guard == b(false) && y.guard == b(false) {
        return Ok(result);
    }
    let g = x.guard.clone();
    for (i, (p, q)) in values.iter().enumerate() {
        result.push(obligation(
            format!("{label}.arg{i}"),
            g.clone(),
            p.clone(),
            q.clone(),
        ));
    }
    for (a, s) in &x.state {
        let t = y
            .state
            .get(a)
            .ok_or_else(|| format!("storage {a:?} missing on one side"))?;
        if s.lane != t.lane || s.cells.len() != t.cells.len() {
            return Err(format!("storage {a:?} differs in shape"));
        }
        for (e, (c, d)) in s.cells.iter().zip(&t.cells).enumerate() {
            let name = format!("{label}.i{}_v{}_r{}_e{e}", a.0, a.1, a.2);
            if !c.pending_bits.is_empty() || !d.pending_bits.is_empty() {
                return Err(format!("{name}: partially initialized storage"));
            }
            let soft = soft_working && a.2 == 1;
            match (&c.value, &d.value) {
                (Some(p), Some(q)) => result.push(Obligation {
                    soft,
                    ..obligation(&name, g.clone(), p.clone(), q.clone())
                }),
                (None, None) => {}
                // Bound on one side only: unequal by construction.
                _ if soft => result.push(Obligation {
                    soft,
                    ..obligation(&name, b(true), b(true), b(false))
                }),
                _ => return Err(format!("{name}: bound on one side only")),
            }
            if a.2 == 2 {
                result.push(obligation(
                    format!("{name}.mask"),
                    g.clone(),
                    c.mask.clone(),
                    d.mask.clone(),
                ));
            }
        }
    }
    Ok(result)
}

fn merged(lifter: &mut Lifter, frames: Option<&Vec<Frame>>) -> Res<Option<Frame>> {
    frames
        .filter(|f| !f.is_empty())
        .map(|f| merge_frames(f.clone(), &mut lifter.stats))
        .transpose()
}

/// Everything leaving a pair of regions must agree: each loop header with its
/// arguments, the returns, and the error condition.
#[allow(clippy::too_many_arguments)]
fn region_obligations(
    label: &str,
    (a, u, ru): (&mut Lifter, &Unit, &Region),
    (c, v, rv): (&mut Lifter, &Unit, &Region),
) -> Res<Vec<Obligation>> {
    let mut result = vec![obligation(
        format!("{label}.error"),
        b(true),
        ru.errors.clone(),
        rv.errors.clone(),
    )];
    let targets = ru
        .exits
        .keys()
        .chain(rv.exits.keys())
        .copied()
        .collect::<BTreeSet<_>>();
    for h in targets {
        let (x, y) = (merged(a, ru.exits.get(&h))?, merged(c, rv.exits.get(&h))?);
        let params = |unit: &Unit, frame: &Option<Frame>| -> Res<Vec<Term>> {
            let Some(frame) = frame else {
                return Ok(vec![]);
            };
            unit.blocks[&BlockId(h)]
                .params
                .iter()
                .map(|p| {
                    frame
                        .regs
                        .get(&p.0)
                        .cloned()
                        .ok_or("unbound loop parameter".into())
                })
                .collect()
        };
        let (p, q) = (params(u, &x)?, params(v, &y)?);
        let values = if p.is_empty() || q.is_empty() {
            vec![]
        } else {
            p.into_iter().zip(q).collect()
        };
        result.extend(agree(&format!("{label}->b{h}"), x, y, &values, false)?);
    }
    let (x, y) = (merged(a, Some(&ru.returns))?, merged(c, Some(&rv.returns))?);
    result.extend(agree(&format!("{label}->return"), x, y, &[], true)?);
    Ok(result)
}

/// Prove a pair of units containing loops equivalent by induction over loop
/// headers kept by the optimizer, then continue from a shared havocked state.
/// Returns whether the continuation is approximate: working storage that could
/// not be proved equal at the unit's return is havocked separately per side.
fn lockstep(
    (a, u): (&mut Lifter, &Unit),
    (c, v): (&mut Lifter, &Unit),
    prover: &mut Prover,
    fresh: &mut usize,
) -> Result<bool, Status> {
    let mut proved = true;
    let cuts = loop_headers(u)
        .union(&loop_headers(v))
        .copied()
        .collect::<BTreeSet<_>>();
    for h in &cuts {
        match (u.blocks.get(&BlockId(*h)), v.blocks.get(&BlockId(*h))) {
            (Some(x), Some(y)) if x.params.len() == y.params.len() => {}
            _ => {
                return Err(unsupported(format!(
                    "loop header b{h} has no counterpart after optimization"
                )));
            }
        }
    }
    let events = (a.events.clone(), c.events.clone());
    let ku = a.prepare(u).map_err(unsupported)?;
    let kv = c.prepare(v).map_err(unsupported)?;
    let ((tu, wu), (tv, wv)) = (accesses(u), accesses(v));
    for addr in tu.union(&tv) {
        a.ensure(*addr).map_err(unsupported)?;
        c.ensure(*addr).map_err(unsupported)?;
    }
    let (saved_a, saved_c) = (a.frame.clone(), c.frame.clone());
    let start = |frame: &Frame| Frame {
        guard: b(true),
        state: frame.state.clone(),
        regs: BTreeMap::new(),
    };
    let ru = a
        .region(u, &ku, u.entry_block_id.0, start(&saved_a), &cuts)
        .map_err(unsupported)?;
    let rv = c
        .region(v, &kv, v.entry_block_id.0, start(&saved_c), &cuts)
        .map_err(unsupported)?;
    for o in region_obligations("entry", (a, u, &ru), (c, v, &rv)).map_err(unsupported)? {
        proved &= prover.prove(o, true)?;
    }
    for h in &cuts {
        let (Some(x), Some(y)) = (
            merged(a, ru.exits.get(h)).map_err(unsupported)?,
            merged(c, rv.exits.get(h)).map_err(unsupported)?,
        ) else {
            return Err(unsupported(format!(
                "loop header b{h} is not entered directly from the unit entry"
            )));
        };
        *fresh += 1;
        let prefix = format!("loop{fresh}");
        let (mut x, mut y) = (x, y);
        let addrs = x.state.keys().copied().collect::<BTreeSet<_>>();
        havoc(&prefix, &addrs, &mut x, &mut y).map_err(unsupported)?;
        // Only values fixed before the loop carry over from the entry run.
        x.regs.retain(|r, _| !assigned_from(u, *h).contains(r));
        y.regs.retain(|r, _| !assigned_from(v, *h).contains(r));
        let (pu, pv) = (
            &u.blocks[&BlockId(*h)].params,
            &v.blocks[&BlockId(*h)].params,
        );
        for (i, (p, q)) in pu.iter().zip(pv).enumerate() {
            let (wp, wq) = (ku[&p.0].0, kv[&q.0].0);
            if wp != wq {
                return Err(unsupported(format!(
                    "loop header b{h} parameter widths differ"
                )));
            }
            let symbol = ir::var(format!("{prefix}.p{i}"), Sort::Bv(wp));
            x.regs.insert(p.0, symbol.clone());
            y.regs.insert(q.0, symbol);
        }
        x.guard = b(true);
        y.guard = b(true);
        let su = a.region(u, &ku, *h, x, &cuts).map_err(unsupported)?;
        let sv = c.region(v, &kv, *h, y, &cuts).map_err(unsupported)?;
        let label = format!("b{h}");
        for o in region_obligations(&label, (a, u, &su), (c, v, &sv)).map_err(unsupported)? {
            proved &= prover.prove(o, false)?;
        }
    }
    if (a.events.as_ref().map(|e| (&e.triggers, &e.sites)))
        != (events.0.as_ref().map(|e| (&e.triggers, &e.sites)))
        || (c.events.as_ref().map(|e| (&e.triggers, &e.sites)))
            != (events.1.as_ref().map(|e| (&e.triggers, &e.sites)))
    {
        return Err(unsupported("trigger or capture inside a loop unit".into()));
    }
    *fresh += 1;
    let prefix = format!("after{fresh}");
    let (mut x, mut y) = (saved_a, saved_c);
    let written = wu.union(&wv).copied().collect::<BTreeSet<_>>();
    let apart = if proved {
        BTreeSet::new()
    } else {
        written.iter().filter(|a| a.2 == 1).copied().collect()
    };
    havoc_apart(&prefix, &written, &apart, &mut x, &mut y).map_err(unsupported)?;
    a.frame = x;
    c.frame = y;
    // Equal loop units fail together; the condition itself is unbounded.
    let error = ir::var(format!("{prefix}.error"), Sort::Bool);
    for side in [a, c] {
        if let Some(events) = &mut side.events {
            events.errors = or(events.errors.clone(), error.clone());
        }
    }
    Ok(!proved)
}

/// Runs both sides; the flag reports an approximate (havocked apart) state.
fn run_pair(
    start: &Frame,
    pre: &[&[Unit]],
    post: &[&[Unit]],
    prover: &mut Prover,
) -> Result<(Run, Run, bool), Status> {
    let mut approximate = false;
    let new = || Lifter {
        frame: start.clone(),
        stats: Statistics::default(),
        events: Some(Events::default()),
    };
    let (mut a, mut c) = (new(), new());
    let mut fresh = 0;
    for (gu, gv) in pre.iter().zip(post) {
        if gu
            .iter()
            .chain(gv.iter())
            .all(|u| loop_headers(u).is_empty())
        {
            a.phase(gu).map_err(|e| unsupported(format!("pre: {e}")))?;
            c.phase(gv).map_err(|e| unsupported(format!("post: {e}")))?;
            continue;
        }
        if gu.len() != gv.len() {
            return Err(unsupported(
                "units of a group with loops do not correspond one to one".into(),
            ));
        }
        a.snapshot_group();
        c.snapshot_group();
        for (u, v) in gu.iter().zip(gv.iter()) {
            if loop_headers(u).is_empty() && loop_headers(v).is_empty() {
                a.execute(u).map_err(|e| unsupported(format!("pre: {e}")))?;
                c.execute(v)
                    .map_err(|e| unsupported(format!("post: {e}")))?;
            } else {
                approximate |= lockstep((&mut a, u), (&mut c, v), prover, &mut fresh)?;
            }
        }
    }
    let finish = |l: Lifter| Run {
        frame: l.frame,
        events: l.events.unwrap_or_default(),
        instructions: l.stats.instructions,
    };
    Ok((finish(a), finish(c), approximate))
}

/// Every observable that must agree: the error condition, and when no error
/// occurred, stable lanes, trigger bits and capture enables.
fn observables(pre: &Run, post: &Run) -> Res<Vec<Obligation>> {
    let ok = not(pre.events.errors.clone());
    let mut result = vec![obligation(
        "error",
        b(true),
        pre.events.errors.clone(),
        post.events.errors.clone(),
    )];
    for (a, before) in pre.frame.state.iter().filter(|(a, _)| a.2 == 0) {
        let after = post
            .frame
            .state
            .get(a)
            .ok_or_else(|| format!("stable storage {a:?} missing after optimization"))?;
        if before.lane != after.lane || before.cells.len() != after.cells.len() {
            return Err(format!("stable storage {a:?} changed shape"));
        }
        for element in 0..before.cells.len() {
            let offset = element * before.lane as usize;
            result.push(obligation(
                lane_name(*a, element),
                ok.clone(),
                before.read(offset, before.lane)?,
                after.read(offset, after.lane)?,
            ));
        }
    }
    let mut flags = |name: &str, x: BTreeMap<String, Term>, y: BTreeMap<String, Term>| {
        for k in x.keys().chain(y.keys()).collect::<BTreeSet<_>>() {
            let get = |m: &BTreeMap<String, Term>| m.get(k).cloned().unwrap_or(b(false));
            result.push(obligation(
                format!("{name}[{k}]"),
                ok.clone(),
                get(&x),
                get(&y),
            ));
        }
    };
    let named =
        |m: &BTreeMap<usize, Term>| m.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
    let sites =
        |m: &BTreeMap<u32, Term>| m.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
    flags(
        "trigger",
        named(&pre.events.triggers),
        named(&post.events.triggers),
    );
    flags("site", sites(&pre.events.sites), sites(&post.events.sites));
    Ok(result)
}

fn compare(pre: &[&[Unit]], post: &[&[Unit]], start: &Frame, limits: &Limits) -> PhaseResult {
    let mut prover = Prover {
        limits,
        normalizer: guard::Normalizer::new(),
        queries: 0,
        obligations: 0,
    };
    let mut instructions = (0, 0);
    let status = (|| -> Result<(), Status> {
        let (x, y, approximate) = run_pair(start, pre, post, &mut prover)?;
        instructions = (x.instructions, y.instructions);
        for o in observables(&x, &y).map_err(unsupported)? {
            prover.prove(o, !approximate)?;
        }
        Ok(())
    })()
    .err()
    .unwrap_or(Status::Equivalent);
    PhaseResult {
        phase: String::new(),
        status,
        lanes: prover.obligations,
        solver_queries: prover.queries,
        instructions,
    }
}

fn group<'a, S: BuildHasher>(
    map: &'a HashMap<StateAddr, Vec<Unit>, S>,
    e: &StateAddr,
) -> &'a [Unit] {
    map.get(e).map_or(&[], Vec::as_slice)
}

fn phases<'a>(pre: &'a Program, post: &'a Program) -> Vec<Phase<'a>> {
    let mut events = BTreeSet::new();
    for program in [pre, post] {
        for map in [
            &program.eval_apply_ffs,
            &program.eval_comb_apply_ffs,
            &program.eval_only_ffs,
            &program.apply_ffs,
        ] {
            events.extend(map.keys().map(|e| (state_addr(e), *e)));
        }
    }
    let mut result = vec![Phase {
        name: "eval_comb".into(),
        pre: vec![&pre.eval_comb],
        post: vec![&post.eval_comb],
    }];
    for (key, e) in events {
        let tag = format!("i{}_v{}", key.0, key.1);
        let present = |maps: &[&HashMap<StateAddr, Vec<Unit>, _>]| {
            maps.iter().any(|m| !group(m, &e).is_empty())
        };
        if present(&[&pre.eval_apply_ffs, &post.eval_apply_ffs]) {
            result.push(Phase {
                name: format!("eval_comb;eval_apply_ffs[{tag}]"),
                pre: vec![&pre.eval_comb, group(&pre.eval_apply_ffs, &e)],
                post: vec![&post.eval_comb, group(&post.eval_apply_ffs, &e)],
            });
        }
        if present(&[&pre.eval_comb_apply_ffs, &post.eval_comb_apply_ffs]) {
            result.push(Phase {
                name: format!("eval_comb_apply_ffs[{tag}]"),
                pre: vec![group(&pre.eval_comb_apply_ffs, &e)],
                post: vec![group(&post.eval_comb_apply_ffs, &e)],
            });
        }
        if present(&[
            &pre.eval_only_ffs,
            &post.eval_only_ffs,
            &pre.apply_ffs,
            &post.apply_ffs,
        ]) {
            result.push(Phase {
                name: format!("eval_comb;eval_only_ffs;apply_ffs[{tag}]"),
                pre: vec![
                    &pre.eval_comb,
                    group(&pre.eval_only_ffs, &e),
                    group(&pre.apply_ffs, &e),
                ],
                post: vec![
                    &post.eval_comb,
                    group(&post.eval_only_ffs, &e),
                    group(&post.apply_ffs, &e),
                ],
            });
        }
    }
    result
}

/// Collections this check does not model; non-empty ones are reported, not passed.
pub fn unchecked(program: &Program) -> Vec<&'static str> {
    let mut result = vec![];
    if !program.processes.is_empty() {
        result.push("processes");
    }
    if program.parallel.is_some() {
        result.push("parallel");
    }
    result
}

pub fn check<'a>(
    objects: impl IntoIterator<Item = (&'a StateAddr, &'a VariableMetadata)>,
    pre: &Program,
    post: &Program,
    limits: &Limits,
) -> Vec<PhaseResult> {
    let start = match initial(objects) {
        Ok(frame) => frame,
        Err(e) => {
            return vec![PhaseResult {
                phase: "state".into(),
                status: Status::Unsupported(e),
                lanes: 0,
                solver_queries: 0,
                instructions: (0, 0),
            }];
        }
    };
    phases(pre, post)
        .into_iter()
        .map(|p| PhaseResult {
            phase: p.name.clone(),
            ..compare(&p.pre, &p.post, &start, limits)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use celox::{Simulator, TraceOptions};

    const CODE: &str = r#"
module Top (
    clk: input clock,
    rst: input reset,
    a: input logic<8>,
    b: input logic<8>,
    y: output logic<8>,
    q: output logic<8>,
) {
    var r: logic<8>;
    assign y = a + b;
    assign q = r;
    always_ff (clk, rst) {
        if_reset {
            r = 0;
        } else {
            r = r + a;
        }
    }
}
"#;

    const LOOP: &str = r#"
module Top (
    count: input logic<8>,
    a: input logic<8>,
    sum: output logic<8>,
) {
    always_comb {
        sum = 0;
        for i in 0..count {
            sum += i + a;
        }
    }
}
"#;

    fn compile() -> (Program, Program, Vec<(StateAddr, VariableMetadata)>) {
        compile_code(CODE)
    }

    fn compile_code(code: &str) -> (Program, Program, Vec<(StateAddr, VariableMetadata)>) {
        let result = Simulator::builder(code, "Top")
            .trace(TraceOptions {
                pre_optimized_sir: true,
                post_optimized_sir: true,
                ..Default::default()
            })
            .build_with_trace();
        result.res.as_ref().map_err(|e| e.to_string()).unwrap();
        let pre = result.trace.pre_optimized_sir.unwrap();
        let post = result.trace.post_optimized_sir.unwrap();
        let objects = pre
            .runtime
            .design
            .semantic()
            .state_objects
            .iter()
            .map(|(a, m)| (*a, m.clone()))
            .collect();
        (pre.sir, post.sir, objects)
    }

    fn statuses(results: &[PhaseResult]) -> Vec<(&str, &Status)> {
        results
            .iter()
            .map(|r| (r.phase.as_str(), &r.status))
            .collect()
    }

    #[test]
    fn optimized_counter_is_equivalent() {
        let (pre, post, objects) = compile();
        let results = check(
            objects.iter().map(|(a, m)| (a, m)),
            &pre,
            &post,
            &Limits::default(),
        );
        assert!(results.len() > 1, "{:?}", statuses(&results));
        for r in &results {
            assert_eq!(r.status, Status::Equivalent, "{}: {:?}", r.phase, r.status);
        }
    }

    #[test]
    fn a_miscompiled_add_is_a_counterexample() {
        let (pre, mut post, objects) = compile();
        let mutated = post
            .eval_comb
            .iter_mut()
            .chain(post.eval_apply_ffs.values_mut().flatten())
            .chain(post.eval_comb_apply_ffs.values_mut().flatten())
            .flat_map(|unit| unit.blocks.values_mut())
            .flat_map(|block| block.instructions.iter_mut())
            .find_map(|inst| match inst {
                SIRInstruction::Binary(_, _, op @ BinaryOp::Add, _) => {
                    *op = BinaryOp::Sub;
                    Some(())
                }
                _ => None,
            });
        assert!(mutated.is_some(), "optimized SIR has no Add to corrupt");
        let results = check(
            objects.iter().map(|(a, m)| (a, m)),
            &pre,
            &post,
            &Limits::default(),
        );
        assert!(
            results
                .iter()
                .any(|r| matches!(r.status, Status::Counterexample { .. })),
            "{:?}",
            statuses(&results)
        );
    }

    #[test]
    fn a_runtime_bounded_loop_is_proved_by_induction() {
        let (pre, post, objects) = compile_code(LOOP);
        assert!(!loop_headers(&pre.eval_comb[0]).is_empty());
        let results = check(
            objects.iter().map(|(a, m)| (a, m)),
            &pre,
            &post,
            &Limits::default(),
        );
        for r in &results {
            assert_eq!(r.status, Status::Equivalent, "{}: {:?}", r.phase, r.status);
        }
    }

    #[test]
    fn a_miscompiled_loop_body_is_not_proved() {
        let (pre, mut post, objects) = compile_code(LOOP);
        let unit = &mut post.eval_comb[0];
        let headers = loop_headers(unit);
        let body = assigned_from(unit, *headers.first().unwrap());
        let mutated = unit
            .blocks
            .values_mut()
            .flat_map(|block| block.instructions.iter_mut())
            .find_map(|inst| match inst {
                SIRInstruction::Binary(d, _, op @ BinaryOp::Add, _) if body.contains(&d.0) => {
                    *op = BinaryOp::Sub;
                    Some(())
                }
                _ => None,
            });
        assert!(mutated.is_some(), "no Add inside the loop");
        let results = check(
            objects.iter().map(|(a, m)| (a, m)),
            &pre,
            &post,
            &Limits::default(),
        );
        assert!(
            results.iter().any(|r| r.status != Status::Equivalent),
            "{:?}",
            statuses(&results)
        );
    }
}
