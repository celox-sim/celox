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

fn run(start: &Frame, units: &[&[Unit]]) -> Res<(Frame, usize)> {
    let mut lifter = Lifter {
        frame: start.clone(),
        stats: Statistics::default(),
    };
    for group in units {
        lifter.phase(group)?;
    }
    Ok((lifter.frame, lifter.stats.instructions))
}

fn compare_frames(pre: &Frame, post: &Frame, limits: &Limits) -> (Status, usize, usize) {
    let mut normalizer = guard::Normalizer::new();
    let (mut lanes, mut queries) = (0, 0);
    for (a, before) in pre.state.iter().filter(|(a, _)| a.2 == 0) {
        let Some(after) = post.state.get(a) else {
            return (
                Status::Unsupported(format!("stable storage {a:?} missing after optimization")),
                lanes,
                queries,
            );
        };
        if before.lane != after.lane || before.cells.len() != after.cells.len() {
            return (
                Status::Unsupported(format!("stable storage {a:?} changed shape")),
                lanes,
                queries,
            );
        }
        for element in 0..before.cells.len() {
            lanes += 1;
            let offset = element * before.lane as usize;
            let read = |s: &Storage| s.read(offset, s.lane);
            let (x, y) = match (read(before), read(after)) {
                (Ok(x), Ok(y)) => (x, y),
                (Err(e), _) | (_, Err(e)) => {
                    return (Status::Unsupported(e), lanes, queries);
                }
            };
            let (x, y) = (normalizer.normalize(&x), normalizer.normalize(&y));
            if x == y {
                continue;
            }
            queries += 1;
            let outcome = solve_with_hint(
                &not(eq(x, y)),
                &Env::new(),
                limits.clone(),
                SearchHint::Unsat,
            );
            match outcome.verdict {
                Verdict::Unsat => {}
                Verdict::Sat if outcome.original_formula_validated => {
                    let witness = outcome
                        .assignments
                        .iter()
                        .map(|(k, v)| (k.clone(), v.json().to_string()))
                        .collect();
                    return (
                        Status::Counterexample {
                            lane: lane_name(*a, element),
                            witness,
                        },
                        lanes,
                        queries,
                    );
                }
                _ => {
                    return (
                        Status::Unknown(format!(
                            "{}: {}",
                            lane_name(*a, element),
                            outcome.diagnostics()["reason"]
                        )),
                        lanes,
                        queries,
                    );
                }
            }
        }
    }
    (Status::Equivalent, lanes, queries)
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
        .map(|p| {
            let side = |name: &str, units: &[&[Unit]]| {
                run(&start, units).map_err(|e| format!("{name}: {e}"))
            };
            let (status, lanes, solver_queries, instructions) =
                match (side("pre", &p.pre), side("post", &p.post)) {
                    (Ok((x, n)), Ok((y, m))) => {
                        let (status, lanes, queries) = compare_frames(&x, &y, limits);
                        (status, lanes, queries, (n, m))
                    }
                    (Err(e), _) | (_, Err(e)) => (Status::Unsupported(e), 0, 0, (0, 0)),
                };
            PhaseResult {
                phase: p.name,
                status,
                lanes,
                solver_queries,
                instructions,
            }
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

    fn compile() -> (Program, Program, Vec<(StateAddr, VariableMetadata)>) {
        let result = Simulator::builder(CODE, "Top")
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
}
