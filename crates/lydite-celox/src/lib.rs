//! Symbolic one-event lifting of compile-only Veryl/Celox SIR.
//!
//! No solver, simulator, sample values or uniqueness queries occur here. Branches
//! are joined with guards, every selected state/input is arbitrary, and sparse
//! NBA regions retain a write mask. Unsupported operations fail closed.
use celox_design::{InitialStateData, RegionedStateAddr, StateAddr, VariableMetadata};
use celox_sir::{BinaryOp, BlockId, RegisterId, SIRInstruction, SIROffset, SIRTerminator, UnaryOp};
use compiled::{Compiled, Unit};
use lydite_ir::{self as ir, Env, Lower, Res, Sort, Term};
use num_bigint::BigUint;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub mod compiled;
mod guard;

type Addr = (u64, u64, u64);
fn num(v: &Value) -> Res<usize> {
    v.as_u64()
        .and_then(|x| usize::try_from(x).ok())
        .ok_or("expected nonnegative integer".into())
}
fn obj(v: &Value) -> Res<&Map<String, Value>> {
    v.as_object().ok_or("expected object".into())
}
fn addr(a: &RegionedStateAddr) -> Addr {
    (a.instance_id.0 as u64, a.var_id.0 as u64, a.region as u64)
}
fn state_addr(a: &StateAddr) -> Addr {
    (a.instance_id.0 as u64, a.var_id.0 as u64, 0)
}
fn base(a: Addr) -> Addr {
    (a.0, a.1, 0)
}
fn width(t: &Term) -> u32 {
    match t.0.sort {
        Sort::Bv(w) => w,
        _ => panic!("internal non-word"),
    }
}
fn constant(t: &Term) -> Option<u64> {
    t.0.op
        .strip_prefix("(_ bv")?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}
fn b(v: bool) -> Term {
    ir::boolv(v)
}
fn bv(w: u32, v: u64) -> Term {
    ir::bv(w, v)
}
fn not(x: Term) -> Term {
    if x == b(true) {
        b(false)
    } else if x == b(false) {
        b(true)
    } else if x.0.op == "not" {
        x.0.args[0].clone()
    } else {
        ir::not(x)
    }
}
fn and(a: Term, c: Term) -> Term {
    if a == b(false) || c == b(false) {
        b(false)
    } else if a == b(true) {
        c
    } else if c == b(true) || a == c {
        a
    } else {
        ir::and(a, c)
    }
}
fn or(a: Term, c: Term) -> Term {
    not(and(not(a), not(c)))
}
fn eq(a: Term, c: Term) -> Term {
    if a == c {
        b(true)
    } else if let (Some(x), Some(y)) = (constant(&a), constant(&c)) {
        b(x == y)
    } else {
        ir::eq(a, c)
    }
}
fn ite(g: Term, a: Term, c: Term) -> Term {
    if g == b(true) || a == c {
        a
    } else if g == b(false) {
        c
    } else if a == b(true) && c == b(false) {
        g
    } else if a == b(false) && c == b(true) {
        not(g)
    } else {
        ir::ite(g, a, c)
    }
}
fn truth(x: Term) -> Term {
    if x.0.op == "ite" {
        return ite(
            x.0.args[0].clone(),
            truth(x.0.args[1].clone()),
            truth(x.0.args[2].clone()),
        );
    }
    let w = width(&x);
    not(eq(x, bv(w, 0)))
}
fn boolword(x: Term, w: u32) -> Term {
    ite(x, bv(w, 1), bv(w, 0))
}
fn extract(x: Term, lo: u32, w: u32) -> Term {
    if lo == 0 && w == width(&x) {
        return x;
    }
    if let Some(n) = constant(&x) {
        return bv(w, n >> lo);
    }
    // Collapse slices of slices; preserve word structure at storage boundaries.
    if x.0.op.starts_with("(_ extract ") {
        let ns =
            x.0.op
                .trim_end_matches(')')
                .split_whitespace()
                .collect::<Vec<_>>();
        if let Ok(inner) = ns[3].parse::<u32>() {
            return extract(x.0.args[0].clone(), inner + lo, w);
        }
    }
    ir::node(
        Sort::Bv(w),
        format!("(_ extract {} {lo})", lo + w - 1),
        vec![x],
    )
}
fn resize(x: Term, w: u32, signed: bool) -> Term {
    let old = width(&x);
    if old == w {
        return x;
    }
    if old > w {
        return extract(x, 0, w);
    }
    if let Some(n) = constant(&x) {
        let value = if signed && old < 64 && n & (1 << (old - 1)) != 0 {
            n | (!0u64 << old)
        } else {
            n
        };
        return bv(w, value);
    }
    ir::node(
        Sort::Bv(w),
        format!(
            "(_ {} {})",
            if signed { "sign_extend" } else { "zero_extend" },
            w - old
        ),
        vec![x],
    )
}
fn bitnot(x: Term) -> Term {
    if let Some(value) = constant(&x) {
        bv(width(&x), !value)
    } else {
        ir::node(x.0.sort.clone(), "bvnot", vec![x])
    }
}
fn op(name: &str, w: u32, a: Term, c: Term) -> Term {
    if let (Some(x), Some(y)) = (constant(&a), constant(&c)) {
        let n = match name {
            "bvadd" => Some(x.wrapping_add(y)),
            "bvsub" => Some(x.wrapping_sub(y)),
            "bvmul" => Some(x.wrapping_mul(y)),
            "bvand" => Some(x & y),
            "bvor" => Some(x | y),
            "bvxor" => Some(x ^ y),
            "bvshl" => Some(if y >= 64 { 0 } else { x << y }),
            "bvlshr" => Some(if y >= 64 { 0 } else { x >> y }),
            _ => None,
        };
        if let Some(n) = n {
            return bv(w, n);
        }
    }
    if name == "bvand" {
        if a == bv(w, 0) || c == bv(w, 0) {
            return bv(w, 0);
        }
        if a == c {
            return a;
        }
    }
    if matches!(name, "bvor" | "bvxor" | "bvadd") {
        if c == bv(w, 0) {
            return a;
        }
        if a == bv(w, 0) {
            return c;
        }
    }
    if matches!(name, "bvshl" | "bvlshr") && c == bv(w, 0) {
        return a;
    }
    ir::node(Sort::Bv(w), name, vec![a, c])
}
fn cat(a: Term, c: Term) -> Res<Term> {
    let w = width(&a) + width(&c);
    if w > 64 {
        return Err("concatenation wider than 64 bits".into());
    }
    if let (Some(x), Some(y)) = (constant(&a), constant(&c)) {
        return Ok(bv(w, (x << width(&c)) | y));
    }
    Ok(ir::node(Sort::Bv(w), "concat", vec![a, c]))
}
fn bytes(v: &BigUint) -> Res<u64> {
    u64::try_from(v).map_err(|_| "constant exceeds 64 bits".into())
}

// Preserve whole-word writes as guarded updates. Partial writes invalidate this
// optional tag and continue through the existing bit-mask semantics.
#[derive(Clone, Debug)]
enum WordUpdate {
    Keep,
    Value(Term),
    Choice(Term, std::rc::Rc<WordUpdate>, std::rc::Rc<WordUpdate>),
}
fn word_choice(
    g: Term,
    a: std::rc::Rc<WordUpdate>,
    c: std::rc::Rc<WordUpdate>,
) -> std::rc::Rc<WordUpdate> {
    if g == b(true) {
        a
    } else if g == b(false) || std::rc::Rc::ptr_eq(&a, &c) {
        c
    } else {
        std::rc::Rc::new(WordUpdate::Choice(g, a, c))
    }
}
fn apply_word_update(
    action: &std::rc::Rc<WordUpdate>,
    old: &Term,
    memo: &mut std::collections::HashMap<usize, Term>,
) -> Term {
    let key = std::rc::Rc::as_ptr(action) as usize;
    if let Some(value) = memo.get(&key) {
        return value.clone();
    }
    let value = match action.as_ref() {
        WordUpdate::Keep => old.clone(),
        WordUpdate::Value(value) => value.clone(),
        WordUpdate::Choice(guard, a, c) => {
            let yes = apply_word_update(a, old, memo);
            let no = apply_word_update(c, old, memo);
            // A conditional whole-word write nested under another enable is
            // still one word update: ite(g,ite(h,v,old),old).
            if yes.0.op == "ite" && yes.0.args[2] == no {
                ite(
                    and(guard.clone(), yes.0.args[0].clone()),
                    yes.0.args[1].clone(),
                    no,
                )
            } else {
                ite(guard.clone(), yes, no)
            }
        }
    };
    memo.insert(key, value.clone());
    value
}
#[derive(Clone, Debug)]
struct Cell {
    // Explicit fragments only: an unbound lane has no readable value until complete.
    pending_bits: BTreeMap<u32, Term>,
    whole_word: Option<std::rc::Rc<WordUpdate>>,
    value: Option<Term>,
    mask: Term,
}
#[derive(Clone, Debug)]
struct Storage {
    lane: u32,
    cells: Vec<Cell>,
}
impl Storage {
    fn new(lane: u32, count: usize, sparse: bool) -> Self {
        Self {
            lane,
            cells: (0..count)
                .map(|_| Cell {
                    pending_bits: BTreeMap::new(),
                    whole_word: Some(std::rc::Rc::new(WordUpdate::Keep)),
                    value: if sparse { Some(bv(lane, 0)) } else { None },
                    mask: bv(lane, 0),
                })
                .collect(),
        }
    }
    fn bits(&self) -> usize {
        self.lane as usize * self.cells.len()
    }
    fn read(&self, offset: usize, w: u32) -> Res<Term> {
        let mut pieces = vec![];
        let mut pos = offset;
        let end = offset
            .checked_add(w as usize)
            .ok_or("storage read offset overflow")?;
        while pos < end {
            if pos >= self.bits() {
                pieces.push(bv((end - pos) as u32, 0));
                break;
            }
            let index = pos / self.lane as usize;
            let low = pos % self.lane as usize;
            let n = (end - pos).min(self.lane as usize - low) as u32;
            let v = self.cells[index].value.clone().ok_or_else(|| {
                format!("read of unbound/uninitialized symbolic storage lane {index}")
            })?;
            pieces.push(extract(v, low as u32, n));
            pos += n as usize;
        }
        let mut value = pieces.remove(0);
        for p in pieces {
            value = cat(p, value)?
        }
        Ok(value)
    }
    fn write(&mut self, offset: usize, w: u32, value: Term, guard: Term) -> Res<()> {
        let mut pos = offset;
        let end = offset
            .checked_add(w as usize)
            .ok_or("storage write offset overflow")?
            .min(self.bits());
        while pos < end {
            let index = pos / self.lane as usize;
            let low = pos % self.lane as usize;
            let n = (end - pos).min(self.lane as usize - low) as u32;
            let piece = resize(
                extract(value.clone(), (pos - offset) as u32, n),
                self.lane,
                false,
            );
            let mask = bv(
                self.lane,
                if n == 64 {
                    !0
                } else {
                    ((1u64 << n) - 1) << low
                },
            );
            let moved = op("bvshl", self.lane, piece, bv(self.lane, low as u64));
            let cell = &mut self.cells[index];
            if cell.value.is_none() && !(low == 0 && n == self.lane) {
                if guard != b(true) {
                    return Err("conditional partial write to unbound storage".into());
                }
                // The frontend may split a full initialization into bit ranges.
                // Preserve only explicitly assigned bits; never invent an old value.
                for bit in 0..n {
                    cell.pending_bits.insert(
                        low as u32 + bit,
                        extract(value.clone(), (pos - offset) as u32 + bit, 1),
                    );
                }
                cell.whole_word = None;
                cell.mask = op("bvor", self.lane, cell.mask.clone(), mask);
                if cell.pending_bits.len() == self.lane as usize {
                    let mut joined = cell.pending_bits[&0].clone();
                    for bit in 1..self.lane {
                        joined = cat(cell.pending_bits[&bit].clone(), joined)?;
                    }
                    cell.value = Some(joined);
                    cell.pending_bits.clear();
                }
                pos += n as usize;
                continue;
            }
            let new = if low == 0 && n == self.lane {
                moved.clone()
            } else {
                let old = cell
                    .value
                    .clone()
                    .ok_or("partial write to unbound storage")?;
                let inverse = bitnot(mask.clone());
                op(
                    "bvor",
                    self.lane,
                    op("bvand", self.lane, old, inverse),
                    moved.clone(),
                )
            };
            cell.whole_word = if low == 0 && n == self.lane {
                let value = std::rc::Rc::new(WordUpdate::Value(moved.clone()));
                match &cell.whole_word {
                    Some(previous) => Some(word_choice(guard.clone(), value, previous.clone())),
                    None if guard == b(true) => Some(value),
                    _ => None,
                }
            } else {
                None
            };
            cell.value = Some(if guard == b(true) {
                new
            } else {
                ite(
                    guard.clone(),
                    new,
                    cell.value
                        .clone()
                        .ok_or("conditional write to unbound storage")?,
                )
            });
            cell.pending_bits.clear();
            cell.mask = ite(
                guard.clone(),
                op("bvor", self.lane, cell.mask.clone(), mask),
                cell.mask.clone(),
            );
            pos += n as usize;
        }
        Ok(())
    }
    fn merge(guard: Term, a: &Self, c: &Self) -> Res<Self> {
        if a.lane != c.lane || a.cells.len() != c.cells.len() {
            return Err("storage join shape mismatch".into());
        }
        if a.cells
            .iter()
            .chain(&c.cells)
            .any(|cell| !cell.pending_bits.is_empty())
        {
            return Err("join with incomplete unbound storage initialization".into());
        }
        Ok(Self {
            lane: a.lane,
            cells: a
                .cells
                .iter()
                .zip(&c.cells)
                .map(|(a, c)| Cell {
                    pending_bits: BTreeMap::new(),
                    whole_word: match (&a.whole_word, &c.whole_word) {
                        (Some(a), Some(c)) => {
                            Some(word_choice(guard.clone(), a.clone(), c.clone()))
                        }
                        _ => None,
                    },
                    value: match (&a.value, &c.value) {
                        (Some(a), Some(c)) => Some(ite(guard.clone(), a.clone(), c.clone())),
                        _ => None,
                    },
                    mask: ite(guard.clone(), a.mask.clone(), c.mask.clone()),
                })
                .collect(),
        })
    }
}
#[derive(Clone, Debug)]
struct Frame {
    guard: Term,
    state: BTreeMap<Addr, Storage>,
    regs: BTreeMap<usize, Term>,
}
#[derive(Clone, Debug)]
struct Binding {
    address: Addr,
    element: usize,
    name: String,
    sort: Sort,
    expression: Option<Value>,
}
#[derive(Clone, Debug, Default)]
pub struct Statistics {
    pub guard_nodes: usize,
    pub guard_work: usize,
    pub guard_aborted: bool,
    pub execution_units: usize,
    pub branches: usize,
    pub joins: usize,
    pub dynamic_accesses: usize,
    pub instructions: usize,
}
/// Typed transition before serialization. Outputs are observed before the edge.
#[derive(Clone, Debug)]
pub struct Transition {
    pub next: Env,
    pub outputs: Env,
    pub statistics: Statistics,
}

struct Lifter {
    frame: Frame,
    stats: Statistics,
}
fn term_to_word(t: Term, w: u32) -> Res<Term> {
    match t.0.sort {
        Sort::Bool if w == 1 => Ok(boolword(t, 1)),
        Sort::Bv(n) if n == w => Ok(t),
        _ => Err("binding type does not match Veryl storage lane".into()),
    }
}
fn word_to_term(t: Term, s: &Sort) -> Res<Term> {
    match s {
        Sort::Bool if width(&t) == 1 => Ok(truth(t)),
        Sort::Bv(w) if *w == width(&t) => Ok(t),
        _ => Err("output binding type mismatch".into()),
    }
}
// Celox VariableInfo/StateMetadata.width is the TOTAL flattened bit width,
// including unpacked array dimensions. Divide by their product for a lane.
fn storage_shape(metadata: &VariableMetadata) -> Res<(u32, usize)> {
    let total = metadata.width;
    let count = metadata.array_dims.iter().try_fold(1usize, |a, b| {
        a.checked_mul(*b)
            .ok_or_else(|| "storage dimension overflow".to_string())
    })?;
    if total == 0 || total > 65536 || count == 0 || !total.is_multiple_of(count) {
        return Err("invalid flattened storage shape or exceeds 65536-bit budget".into());
    }
    let lane = total / count;
    if !(1..=64).contains(&lane) {
        return Err(format!("symbolic storage lane width {lane} outside 1..64"));
    }
    Ok((lane as u32, count))
}
fn signal(code: &Compiled, name: &str) -> Res<(Addr, u32, usize)> {
    let found = code
        .signals
        .iter()
        .filter(|s| s.instances.is_empty() && s.path.join(".") == name)
        .collect::<Vec<_>>();
    if found.len() != 1 {
        return Err(format!("missing/ambiguous top-level signal {name}"));
    }
    let s = found[0];
    let (w, n) = storage_shape(&s.metadata)?;
    Ok((state_addr(&s.address), w, n))
}
fn bindings(code: &Compiled, values: &Value) -> Res<Vec<Binding>> {
    obj(values)?
        .iter()
        .map(|(key, v)| {
            let name = v["name"].as_str().unwrap_or(key).to_owned();
            if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                return Err("invalid canonical binding name".into());
            }
            let (a, _, n) = signal(code, v["signal"].as_str().unwrap_or(key))?;
            let element = if let Some(element) = v.get("element") {
                num(element)?
            } else {
                0
            };
            if element >= n {
                return Err("binding array element out of range".into());
            }
            Ok(Binding {
                address: a,
                element,
                name,
                sort: ir::ty(&v["type"])?,
                expression: v.get("expr").cloned(),
            })
        })
        .collect()
}
/// Lift a selected named clock/reset event with explicit typed bindings.
///
/// `inputs` and `state` map a source signal (or an entry with `signal`/`element`)
/// to `{name,type,expr?}`. Input-only `expr` is a typed canonical expression
/// over i. bindings; `overrides` replaces canonical input names, useful for reset lifting.
/// `outputs` map observable names to `{signal,type,element?}`.
pub fn lift(code: &Compiled, config: &Value) -> Res<Transition> {
    if code.status != "compiled_only_not_verified" {
        return Err("expected compile-only SIR export".into());
    }
    if code.four_state {
        return Err(
            "symbolic lifting requires an explicit two-state frontend export (four_state=false)"
                .into(),
        );
    }
    for initial in &code.design.initial_state {
        let InitialStateData::Writes(runs) = &initial.data else {
            return Err("unsupported initial-state format".into());
        };
        if runs
            .iter()
            .any(|run| run.mask_bytes.iter().any(|v| *v != 0))
        {
            return Err("unknown/four-state initialization is unsupported".into());
        }
    }
    if !code.design.cascaded_events.is_empty() {
        return Err("cascaded events need an explicit multi-event model".into());
    }
    let inputs = bindings(code, &config["inputs"])?;
    let states = bindings(code, &config["state"])?;
    if states.iter().any(|x| x.expression.is_some()) {
        return Err("state bindings must be direct arbitrary symbols, not expressions".into());
    }
    let mut env = Env::new();
    for (prefix, bs) in [("i", &inputs), ("s", &states)] {
        for x in bs {
            let n = format!("{prefix}.{}", x.name);
            if env.insert(n.clone(), ir::var(n, x.sort.clone())).is_some() {
                return Err("duplicate canonical binding name".into());
            }
        }
    }
    let mut lower = Lower::default();
    if let Some(overrides) = config.get("overrides") {
        let original: Env = env
            .iter()
            .filter(|(k, _)| k.starts_with("i."))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (name, value) in obj(overrides)? {
            let key = format!("i.{name}");
            let old = env.get(&key).ok_or("override has no input binding")?;
            let new = lower.expr(value, &original)?;
            if new.0.sort != old.0.sort {
                return Err("input override sort mismatch".into());
            }
            env.insert(key, new);
        }
    }
    let mut frame = Frame {
        guard: b(true),
        state: BTreeMap::new(),
        regs: BTreeMap::new(),
    };
    for o in &code.design.state_objects {
        let a = state_addr(&o.address);
        let (w, n) = storage_shape(&o.metadata)?;
        if frame.state.insert(a, Storage::new(w, n, false)).is_some() {
            return Err("duplicate storage address".into());
        }
    }
    let mut selected = BTreeSet::new();
    for (prefix, bs) in [("i", &inputs), ("s", &states)] {
        for x in bs {
            if !selected.insert((x.address, x.element)) {
                return Err("same storage lane bound more than once".into());
            }
            let value = simplify(if let Some(e) = &x.expression {
                let input_env: Env = env
                    .iter()
                    .filter(|(k, _)| k.starts_with("i."))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                lower.expr(e, &input_env)?
            } else {
                env[&format!("{prefix}.{}", x.name)].clone()
            });
            let storage = frame
                .state
                .get_mut(&x.address)
                .ok_or("binding storage missing")?;
            storage
                .cells
                .get_mut(x.element)
                .ok_or("signal/storage element shape mismatch")?
                .value = Some(term_to_word(value, storage.lane)?);
        }
    }
    let mut l = Lifter {
        frame,
        stats: Statistics::default(),
    };
    l.phase(&code.sir.eval_comb)?;
    let mut outputs = Env::new();
    for (name, v) in obj(&config["outputs"])? {
        let (a, w, n) = signal(code, v["signal"].as_str().ok_or("output signal")?)?;
        let i = if let Some(element) = v.get("element") {
            num(element)?
        } else {
            0
        };
        if i >= n {
            return Err("output element out of range".into());
        }
        let storage = l.frame.state.get(&a).ok_or("output storage missing")?;
        if storage.lane != w || storage.cells.len() != n {
            return Err("signal/output storage shape mismatch".into());
        }
        let t = storage.read(i * w as usize, w)?;
        outputs.insert(name.clone(), word_to_term(t, &ir::ty(&v["type"])?)?);
    }
    let event = config["event"].as_str().ok_or("event must name a signal")?;
    let (mut event, _, _) = signal(code, event)?;
    let mut aliases = BTreeMap::new();
    for (alias, target) in &code.design.event_aliases {
        if aliases
            .insert(state_addr(alias), state_addr(target))
            .is_some()
        {
            return Err("duplicate event alias".into());
        }
    }
    let mut seen = BTreeSet::new();
    while let Some(target) = aliases.get(&event) {
        if !seen.insert(event) {
            return Err("cyclic event alias".into());
        }
        event = *target;
    }
    let selected = code
        .sir
        .eval_apply_ffs
        .iter()
        .filter(|g| state_addr(&g.event) == event)
        .collect::<Vec<_>>();
    if selected.len() != 1 {
        return Err("selected event must have exactly one SIR group".into());
    }
    l.phase(&selected[0].units)?;
    let mut next = Env::new();
    for x in &states {
        let st = &l.frame.state[&x.address];
        let value = st.read(x.element * st.lane as usize, st.lane)?;
        next.insert(x.name.clone(), word_to_term(value, &x.sort)?);
    }
    let mut normalizer = guard::Normalizer::new();
    for term in next.values_mut().chain(outputs.values_mut()) {
        *term = normalizer.normalize(term);
    }
    l.stats.guard_nodes = normalizer.count();
    l.stats.guard_work = normalizer.work;
    l.stats.guard_aborted = normalizer.aborted;
    Ok(Transition {
        next,
        outputs,
        statistics: l.stats,
    })
}
impl Lifter {
    fn ensure(&mut self, a: Addr) -> Res<()> {
        if a.2 > 2 {
            return Err("unknown storage region".into());
        }
        if !self.frame.state.contains_key(&a) {
            let s = self
                .frame
                .state
                .get(&base(a))
                .ok_or("unknown storage address")?;
            self.frame
                .state
                .insert(a, Storage::new(s.lane, s.cells.len(), a.2 == 2));
        }
        Ok(())
    }
    fn phase(&mut self, units: &[Unit]) -> Res<()> {
        for unit in units {
            self.execute(unit)?
        }
        Ok(())
    }
    fn access_guard(offset: &SIROffset, regs: &BTreeMap<usize, Term>, at: usize) -> Res<Term> {
        let reg = |r: &RegisterId| -> Res<Term> {
            regs.get(&r.0)
                .cloned()
                .ok_or("undefined offset register".into())
        };
        match offset {
            SIROffset::Static(x) | SIROffset::PackedElements { bit_offset: x, .. } => {
                Ok(b(*x == at))
            }
            SIROffset::Dynamic(x) => {
                let x = reg(x)?;
                Ok(if width(&x) < 64 && (at as u64) >= 1u64 << width(&x) {
                    b(false)
                } else {
                    eq(x.clone(), bv(width(&x), at as u64))
                })
            }
            // A run's logical offset is that of its first element.
            SIROffset::ElementRun {
                index,
                element_width,
            } => Self::access_guard(
                &SIROffset::Element {
                    index: *index,
                    element_width: *element_width,
                    bit_offset: 0,
                    dynamic_bit_offset: None,
                },
                regs,
                at,
            ),
            SIROffset::Element {
                index,
                element_width: ew,
                bit_offset: off,
                dynamic_bit_offset,
            } => {
                let (ew, off) = (*ew, *off);
                if ew == 0 {
                    return Err("zero element width".into());
                }
                if at < off {
                    return Ok(b(false));
                }
                let remainder = at - off;
                let index = reg(index)?;
                let Some(dynamic) = dynamic_bit_offset else {
                    if !remainder.is_multiple_of(ew) {
                        return Ok(b(false));
                    }
                    let candidate = remainder / ew;
                    return Ok(
                        if width(&index) < 64 && (candidate as u64) >= 1u64 << width(&index) {
                            b(false)
                        } else {
                            eq(index.clone(), bv(width(&index), candidate as u64))
                        },
                    );
                };
                let dynamic = reg(dynamic)?;
                // Bounds ensure the 64-bit arithmetic cannot wrap into a valid address.
                let ix = resize(index, 64, false);
                let dy = resize(dynamic, 64, false);
                let bounded = and(
                    compare("bvule", ix.clone(), bv(64, (remainder / ew) as u64)),
                    compare("bvule", dy.clone(), bv(64, remainder as u64)),
                );
                let sum = op("bvadd", 64, op("bvmul", 64, ix, bv(64, ew as u64)), dy);
                Ok(and(bounded, eq(sum, bv(64, remainder as u64))))
            }
        }
    }
    fn load(&mut self, a: Addr, off: &SIROffset, w: u32) -> Res<Term> {
        if a.2 == 2 {
            return Err("sparse NBA region is write-only".into());
        }
        self.ensure(a)?;
        let st = &self.frame.state[&a];
        if let Some(offset) = off.constant_bit_offset() {
            return st.read(offset, w);
        }
        self.stats.dynamic_accesses += 1;
        let mut value = bv(w, 0);
        for pos in 0..st.bits() {
            let guard = Self::access_guard(off, &self.frame.regs, pos)?;
            if guard == b(false) {
                continue;
            }
            value = ite(guard, st.read(pos, w)?, value);
        }
        Ok(value)
    }
    fn store(&mut self, a: Addr, off: &SIROffset, w: u32, value: Term) -> Res<()> {
        self.ensure(a)?;
        if let Some(offset) = off.constant_bit_offset() {
            return self
                .frame
                .state
                .get_mut(&a)
                .unwrap()
                .write(offset, w, value, b(true))
                .map_err(|error| format!("{error}: address {a:?}, offset {offset}, width {w}"));
        }
        self.stats.dynamic_accesses += 1;
        let bits = self.frame.state[&a].bits();
        for pos in 0..bits {
            let guard = Self::access_guard(off, &self.frame.regs, pos)?;
            if guard != b(false) {
                self.frame
                    .state
                    .get_mut(&a)
                    .unwrap()
                    .write(pos, w, value.clone(), guard)
                    .map_err(|error| {
                        format!("{error}: address {a:?}, dynamic candidate offset {pos}, width {w}")
                    })?
            }
        }
        Ok(())
    }
    fn commit(&mut self, src: Addr, dst: Addr, off: &SIROffset, w: usize) -> Res<()> {
        if w == 0 || w > 65536 {
            return Err("commit width outside 1..65536".into());
        }
        self.ensure(src)?;
        self.ensure(dst)?;
        let source = self.frame.state[&src].clone();
        if source.lane != self.frame.state[&dst].lane
            || source.cells.len() != self.frame.state[&dst].cells.len()
        {
            return Err("commit shape mismatch".into());
        }
        if src.2 == 2 {
            if off.constant_bit_offset() != Some(0) || w != source.bits() {
                return Err("partial sparse NBA commit is unsupported".into());
            }
            let target = self.frame.state.get_mut(&dst).unwrap();
            for (a, c) in source.cells.iter().zip(&mut target.cells) {
                let old = c.value.clone().ok_or("sparse commit to unbound storage")?;
                let val = a.value.clone().ok_or("sparse source undefined")?;
                let inverse = bitnot(a.mask.clone());
                c.value = Some(if let Some(action) = &a.whole_word {
                    apply_word_update(action, &old, &mut Default::default())
                } else {
                    op(
                        "bvor",
                        source.lane,
                        op("bvand", source.lane, old, inverse),
                        op("bvand", source.lane, val, a.mask.clone()),
                    )
                });
                // The target's mask may include earlier partial updates. Keep
                // its exact bits and conservatively drop the optional tag.
                c.whole_word = None;
                c.mask = op("bvor", source.lane, c.mask.clone(), a.mask.clone());
            }
            self.frame
                .state
                .insert(src, Storage::new(source.lane, source.cells.len(), true));
            return Ok(());
        }
        if let Some(offset) = off.constant_bit_offset() {
            if offset == 0 && w == source.bits() {
                self.frame.state.insert(dst, source);
                return Ok(());
            }
            let end = offset.checked_add(w).ok_or("commit offset overflow")?;
            let mut pos = offset;
            while pos < end {
                let n = (end - pos).min(64) as u32;
                let value = source.read(pos, n)?;
                self.frame
                    .state
                    .get_mut(&dst)
                    .unwrap()
                    .write(pos, n, value, b(true))?;
                pos += n as usize
            }
            return Ok(());
        }
        if w > 64 {
            return Err("dynamic commit wider than 64 bits".into());
        }
        let value = self.load(src, off, w as u32)?;
        self.store(dst, off, w as u32, value)
    }
    fn execute(&mut self, unit: &Unit) -> Res<()> {
        validate_unit(unit)?;
        self.stats.execution_units += 1;
        self.frame.regs.clear();
        self.frame.guard = b(true);
        let mut kinds = BTreeMap::new();
        for (k, t) in &unit.register_map {
            let w = t.width();
            if !(1..=64).contains(&w) {
                return Err(format!("SIR register width {w} outside 1..64"));
            }
            kinds.insert(k.0, (w as u32, t.is_signed()));
        }
        for block in unit.blocks.values() {
            for inst in &block.instructions {
                match inst {
                    SIRInstruction::Load(_, a, _, _) | SIRInstruction::Store(a, ..) => {
                        self.ensure(addr(a))?
                    }
                    SIRInstruction::Commit(src, dst, ..) => {
                        self.ensure(addr(src))?;
                        self.ensure(addr(dst))?
                    }
                    _ => {}
                }
            }
        }
        let get = |id: usize| unit.blocks.get(&BlockId(id)).ok_or("missing CFG target");
        let entry = unit.entry_block_id.0;
        let mut marks = BTreeMap::new();
        let mut order = vec![];
        fn visit(
            id: usize,
            unit: &Unit,
            marks: &mut BTreeMap<usize, u8>,
            order: &mut Vec<usize>,
        ) -> Res<()> {
            match marks.get(&id) {
                Some(1) => {
                    return Err("cyclic SIR CFG requires an explicit loop bound/invariant".into());
                }
                Some(2) => return Ok(()),
                _ => {}
            }
            marks.insert(id, 1);
            let block = unit.blocks.get(&BlockId(id)).ok_or("missing CFG target")?;
            for target in successors(&block.terminator) {
                visit(target, unit, marks, order)?
            }
            marks.insert(id, 2);
            order.push(id);
            Ok(())
        }
        visit(entry, unit, &mut marks, &mut order)?;
        order.reverse();
        let mut incoming: BTreeMap<usize, Vec<Frame>> = BTreeMap::new();
        incoming.insert(entry, vec![self.frame.clone()]);
        let mut returns = vec![];
        for id in order {
            let Some(frames) = incoming.remove(&id) else {
                continue;
            };
            self.frame = merge_frames(frames, &mut self.stats)?;
            if self.frame.guard == b(false) {
                continue;
            }
            let block = get(id)?;
            for instruction in &block.instructions {
                self.stats.instructions += 1;
                let reg = |f: &Frame, r: &RegisterId| -> Res<Term> {
                    f.regs
                        .get(&r.0)
                        .cloned()
                        .ok_or_else(|| format!("undefined SIR register {}", r.0))
                };
                let rwidth = |r: &RegisterId| -> Res<u32> {
                    Ok(kinds.get(&r.0).ok_or("untyped register")?.0)
                };
                let result: Option<(usize, Term)> = match instruction {
                    SIRInstruction::Imm(dst, value) => {
                        if bytes(&value.mask)? != 0 {
                            return Err(
                                "four-state immediate unsupported in symbolic two-state model"
                                    .into(),
                            );
                        }
                        let w = rwidth(dst)?;
                        let value = bytes(&value.payload)?;
                        if w < 64 && value >= 1u64 << w {
                            return Err("SIR immediate exceeds destination width".into());
                        }
                        Some((dst.0, bv(w, value)))
                    }
                    SIRInstruction::Unary(dst, name, src) => {
                        let x = reg(&self.frame, src)?;
                        let signed = kinds[&src.0].1;
                        Some((dst.0, unary(*name, x, rwidth(dst)?, signed)?))
                    }
                    SIRInstruction::Binary(dst, lhs, name, rhs) => {
                        let a = reg(&self.frame, lhs)?;
                        let c = reg(&self.frame, rhs)?;
                        Some((
                            dst.0,
                            binary(*name, a, c, rwidth(dst)?, kinds[&lhs.0].1, kinds[&rhs.0].1)?,
                        ))
                    }
                    SIRInstruction::Load(dst, a, off, w) => {
                        let w = *w;
                        if !(1..=64).contains(&w) {
                            return Err("load width outside 1..64".into());
                        }
                        if rwidth(dst)? != w as u32 {
                            return Err("SIR load width mismatch".into());
                        }
                        Some((dst.0, self.load(addr(a), off, w as u32)?))
                    }
                    SIRInstruction::Store(a, off, w, src, triggers, observers) => {
                        if !triggers.is_empty() || !observers.is_empty() {
                            return Err("triggered/observed SIR store needs event semantics".into());
                        }
                        let value = reg(&self.frame, src)?;
                        let w = *w;
                        if !(1..=64).contains(&w) {
                            return Err("store width outside 1..64".into());
                        }
                        if w as u32 > width(&value) {
                            return Err("SIR store wider than source".into());
                        }
                        self.store(addr(a), off, w as u32, resize(value, w as u32, false))?;
                        None
                    }
                    SIRInstruction::Commit(src, dst, off, w, triggers) => {
                        if !triggers.is_empty() {
                            return Err("triggered commit needs event semantics".into());
                        }
                        self.commit(addr(src), addr(dst), off, *w)?;
                        None
                    }
                    SIRInstruction::Slice(dst, src, lo, w) => {
                        let x = reg(&self.frame, src)?;
                        let lo = u32::try_from(*lo).map_err(|_| "SIR slice offset overflow")?;
                        let w = u32::try_from(*w).map_err(|_| "SIR slice width overflow")?;
                        if w == 0 || w > 64 || lo.checked_add(w).is_none_or(|n| n > width(&x)) {
                            return Err("invalid SIR slice".into());
                        }
                        if rwidth(dst)? != w {
                            return Err("SIR slice result width mismatch".into());
                        }
                        Some((dst.0, extract(x, lo, w)))
                    }
                    SIRInstruction::Concat(dst, parts) => {
                        let mut parts = parts.iter();
                        let mut x = reg(&self.frame, parts.next().ok_or("empty concat")?)?;
                        for part in parts {
                            x = cat(x, reg(&self.frame, part)?)?
                        }
                        if rwidth(dst)? != width(&x) {
                            return Err("SIR concat result width mismatch".into());
                        }
                        Some((dst.0, x))
                    }
                    SIRInstruction::Mux(dst, cond, yes, no) => {
                        let w = rwidth(dst)?;
                        if rwidth(yes)? != w || rwidth(no)? != w {
                            return Err("SIR mux width mismatch".into());
                        }
                        Some((
                            dst.0,
                            ite(
                                truth(reg(&self.frame, cond)?),
                                resize(reg(&self.frame, yes)?, w, false),
                                resize(reg(&self.frame, no)?, w, false),
                            ),
                        ))
                    }
                    _ => return Err(unsupported(instruction)),
                };
                if let Some((r, value)) = result {
                    self.frame.regs.insert(
                        r,
                        resize(
                            value,
                            kinds.get(&r).ok_or("missing register type")?.0,
                            false,
                        ),
                    );
                }
            }
            let mut edges: Vec<(usize, &[RegisterId], Term)> = vec![];
            match &block.terminator {
                SIRTerminator::Return => {
                    returns.push(self.frame.clone());
                    continue;
                }
                SIRTerminator::Jump(target, arguments) => {
                    edges.push((target.0, arguments, b(true)))
                }
                SIRTerminator::Branch {
                    cond,
                    true_block,
                    false_block,
                } => {
                    self.stats.branches += 1;
                    let cond = truth(
                        self.frame
                            .regs
                            .get(&cond.0)
                            .cloned()
                            .ok_or("undefined branch condition")?,
                    );
                    for ((target, arguments), g) in
                        [(true_block, cond.clone()), (false_block, not(cond))]
                    {
                        edges.push((target.0, arguments, g));
                    }
                }
                SIRTerminator::Switch {
                    selector,
                    cases,
                    default,
                } => {
                    self.stats.branches += 1;
                    let selector = self
                        .frame
                        .regs
                        .get(&selector.0)
                        .cloned()
                        .ok_or("undefined switch selector")?;
                    let mut remaining = b(true);
                    let mut seen = BTreeSet::new();
                    for case in cases {
                        let n = bytes(&case.value)?;
                        if !seen.insert(n) {
                            return Err("duplicate switch case".into());
                        }
                        if width(&selector) < 64 && n >= 1 << width(&selector) {
                            return Err("switch case exceeds selector width".into());
                        }
                        let cond = eq(selector.clone(), bv(width(&selector), n));
                        edges.push((case.target.0, &[], and(remaining.clone(), cond.clone())));
                        remaining = and(remaining, not(cond));
                    }
                    edges.push((default.0, &[], remaining));
                }
                SIRTerminator::Error(code) => {
                    return Err(format!("symbolically reachable SIR runtime error {code}"));
                }
            }
            for (target, arguments, guard) in edges {
                let guard = and(self.frame.guard.clone(), guard);
                if guard == b(false) {
                    continue;
                }
                let mut frame = self.frame.clone();
                frame.guard = guard;
                let params = &get(target)?.params;
                if params.len() != arguments.len() {
                    return Err("CFG argument arity mismatch".into());
                }
                let values = arguments
                    .iter()
                    .map(|r| {
                        frame
                            .regs
                            .get(&r.0)
                            .cloned()
                            .ok_or("undefined CFG argument".into())
                    })
                    .collect::<Res<Vec<_>>>()?;
                for (param, value) in params.iter().zip(values) {
                    let p = param.0;
                    if kinds.get(&p).ok_or("missing parameter type")?.0 != width(&value) {
                        return Err("CFG parameter width mismatch".into());
                    }
                    frame.regs.insert(p, value);
                }
                incoming.entry(target).or_default().push(frame);
            }
        }
        if returns.is_empty() {
            return Err("SIR has no returning path".into());
        }
        self.frame = merge_frames(returns, &mut self.stats)?;
        Ok(())
    }
}
fn successors(term: &SIRTerminator) -> Vec<usize> {
    match term {
        SIRTerminator::Jump(target, _) => vec![target.0],
        SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } => vec![true_block.0.0, false_block.0.0],
        SIRTerminator::Switch { cases, default, .. } => cases
            .iter()
            .map(|c| c.target.0)
            .chain([default.0])
            .collect(),
        SIRTerminator::Return | SIRTerminator::Error(_) => vec![],
    }
}
fn unsupported(instruction: &SIRInstruction<RegionedStateAddr>) -> String {
    let name = match instruction {
        SIRInstruction::Imm(..) => "Imm",
        SIRInstruction::Binary(..) => "Binary",
        SIRInstruction::Unary(..) => "Unary",
        SIRInstruction::Load(..) => "Load",
        SIRInstruction::Store(..) => "Store",
        SIRInstruction::Commit(..) => "Commit",
        SIRInstruction::Concat(..) => "Concat",
        SIRInstruction::Slice(..) => "Slice",
        SIRInstruction::Mux(..) => "Mux",
        SIRInstruction::RuntimeEvent { .. } => "RuntimeEvent",
        SIRInstruction::CombCaptureEvent { .. } => "CombCaptureEvent",
        SIRInstruction::CombCaptureEnableIfChanged { .. } => "CombCaptureEnableIfChanged",
    };
    format!("unsupported symbolic SIR instruction {name}")
}
fn merge_frames(mut frames: Vec<Frame>, stats: &mut Statistics) -> Res<Frame> {
    let mut merged = frames.remove(0);
    for frame in frames {
        stats.joins += 1;
        let guard = frame.guard.clone();
        for (a, st) in &frame.state {
            let old = merged.state.get(a).ok_or("storage missing at join")?;
            merged
                .state
                .insert(*a, Storage::merge(guard.clone(), st, old)?);
        }
        merged.regs = merged
            .regs
            .iter()
            .filter_map(|(r, old)| {
                frame
                    .regs
                    .get(r)
                    .map(|new| (*r, ite(guard.clone(), new.clone(), old.clone())))
            })
            .collect();
        merged.guard = or(merged.guard, guard);
    }
    Ok(merged)
}
fn compare(name: &str, a: Term, c: Term) -> Term {
    if let (Some(x), Some(y)) = (constant(&a), constant(&c)) {
        if name == "bvult" {
            return b(x < y);
        }
        if name == "bvule" {
            return b(x <= y);
        }
    }
    ir::node(Sort::Bool, name, vec![a, c])
}
fn unary(name: UnaryOp, x: Term, w: u32, signed: bool) -> Res<Term> {
    use UnaryOp::*;
    if matches!(name, Minus | BitNot | ToTwoState) && width(&x) != w {
        return Err("SIR unary operand/result width mismatch".into());
    }
    if matches!(name, LogicNot | And | Or | Xor) && w != 1 {
        return Err("SIR reduction result must be 1 bit".into());
    }
    Ok(match name {
        Ident | ToTwoState => resize(x, w, name == Ident && signed),
        BitNot => {
            let x = resize(x, w, signed);
            if let Some(n) = constant(&x) {
                bv(w, !n)
            } else {
                ir::node(Sort::Bv(w), "bvnot", vec![x])
            }
        }
        Minus => op("bvsub", w, bv(w, 0), resize(x, w, true)),
        LogicNot => boolword(not(truth(x)), w),
        And => {
            let n = width(&x);
            boolword(eq(x, bv(n, if n == 64 { !0 } else { (1 << n) - 1 })), w)
        }
        Or => boolword(truth(x), w),
        Xor => {
            let mut v = b(false);
            for i in 0..width(&x) {
                v = ir::node(Sort::Bool, "xor", vec![v, truth(extract(x.clone(), i, 1))])
            }
            boolword(v, w)
        }
        PopCount | CountLeadingZeros | CountTrailingZeros => {
            return Err(format!("unsupported symbolic unary {name}"));
        }
    })
}
fn binary(name: BinaryOp, a: Term, c: Term, w: u32, sa: bool, _sc: bool) -> Res<Term> {
    use BinaryOp::*;
    // (relation, signed, swap): `None` is equality; for it `swap` negates.
    let relation = match name {
        Eq | EqCase | EqWildcard => Some((None, false, false)),
        Ne | NeCase | NeWildcard => Some((None, false, true)),
        LtU => Some((Some("bvult"), false, false)),
        LeU => Some((Some("bvule"), false, false)),
        GtU => Some((Some("bvult"), false, true)),
        GeU => Some((Some("bvule"), false, true)),
        LtS => Some((Some("bvslt"), true, false)),
        LeS => Some((Some("bvsle"), true, false)),
        GtS => Some((Some("bvslt"), true, true)),
        GeS => Some((Some("bvsle"), true, true)),
        _ => None,
    };
    if let Some((relation, signed, swap)) = relation {
        if width(&a) != width(&c) || w != 1 {
            return Err("SIR comparison requires equal operands and a 1-bit result".into());
        }
        let n = width(&a).max(width(&c));
        let mut a = resize(a, n, signed);
        let mut c = resize(c, n, signed);
        let p = match relation {
            None if swap => not(eq(a, c)),
            None => eq(a, c),
            Some(relation) => {
                if swap {
                    std::mem::swap(&mut a, &mut c)
                }
                compare(relation, a, c)
            }
        };
        return Ok(boolword(p, w));
    }
    if matches!(name, LogicAnd | LogicOr) {
        if w != 1 {
            return Err("SIR logical result must be 1 bit".into());
        }
        return Ok(boolword(
            if name == LogicAnd {
                and(truth(a), truth(c))
            } else {
                or(truth(a), truth(c))
            },
            w,
        ));
    }
    if matches!(name, Shl | Shr | Sar) {
        let n = if name == Shl { w } else { w.max(width(&a)) };
        let operand = resize(a.clone(), n, name == Sar || (name == Shl && sa));
        let limit = if width(&c) < 64 && (n as u64) >= 1 << width(&c) {
            b(true)
        } else {
            compare("bvult", c.clone(), bv(width(&c), n as u64))
        };
        let amount = resize(c, n, false);
        let shifted = if name == Shl {
            op("bvshl", n, operand, amount)
        } else if name == Shr {
            op("bvlshr", n, operand, amount)
        } else {
            let sign = truth(extract(operand.clone(), n - 1, 1));
            let inv = ir::node(Sort::Bv(n), "bvnot", vec![operand.clone()]);
            let negshift = ir::node(
                Sort::Bv(n),
                "bvnot",
                vec![op("bvlshr", n, inv, amount.clone())],
            );
            ite(sign, negshift, op("bvlshr", n, operand, amount))
        };
        let fallback = if name == Sar {
            let sign = truth(extract(shifted.clone(), n - 1, 1));
            ite(
                sign,
                bv(n, if n == 64 { !0 } else { (1 << n) - 1 }),
                bv(n, 0),
            )
        } else {
            bv(n, 0)
        };
        return Ok(resize(ite(limit, shifted, fallback), w, false));
    }
    let opn = match name {
        Add => "bvadd",
        Sub => "bvsub",
        Mul => "bvmul",
        And => "bvand",
        Or => "bvor",
        Xor => "bvxor",
        _ => return Err(format!("unsupported symbolic binary {name}")),
    };
    Ok(op(opn, w, resize(a, w, sa), resize(c, w, false)))
}
fn simplify(t: Term) -> Term {
    let args = t.0.args.iter().cloned().map(simplify).collect::<Vec<_>>();
    match t.0.op.as_str() {
        "not" => not(args[0].clone()),
        "and" => and(args[0].clone(), args[1].clone()),
        "or" => or(args[0].clone(), args[1].clone()),
        "=" => eq(args[0].clone(), args[1].clone()),
        "ite" => ite(args[0].clone(), args[1].clone(), args[2].clone()),
        _ => {
            if args == t.0.args {
                t
            } else {
                ir::node(t.0.sort.clone(), t.0.op.clone(), args)
            }
        }
    }
}
struct Export {
    seen: std::collections::HashMap<Term, String>,
    wires: Map<String, Value>,
    inline: bool,
}
impl Export {
    fn expression(&mut self, t: &Term) -> Res<Value> {
        if t.0.op == "true" {
            return Ok(json!(true));
        }
        if t.0.op == "false" {
            return Ok(json!(false));
        }
        if let Some(name) = t.0.op.strip_prefix('@') {
            return Ok(json!(name));
        }
        if let Some(value) = constant(t) {
            return Ok(json!(["bv", width(t), value]));
        }
        if !self.inline {
            if let Some(name) = self.seen.get(t) {
                return Ok(json!(format!("w.{name}")));
            }
        }
        let args =
            t.0.args
                .iter()
                .map(|t| self.expression(t))
                .collect::<Res<Vec<_>>>()?;
        let mut value = if t.0.op.starts_with("(_ extract ") {
            let parts =
                t.0.op
                    .trim_end_matches(')')
                    .split_whitespace()
                    .collect::<Vec<_>>();
            vec![
                json!("extract"),
                json!(
                    parts[2]
                        .parse::<u32>()
                        .map_err(|_| "invalid internal extract")?
                ),
                json!(
                    parts[3]
                        .parse::<u32>()
                        .map_err(|_| "invalid internal extract")?
                ),
            ]
        } else if t.0.op.starts_with("(_ zero_extend ") || t.0.op.starts_with("(_ sign_extend ") {
            let parts =
                t.0.op
                    .trim_end_matches(')')
                    .split_whitespace()
                    .collect::<Vec<_>>();
            vec![
                json!(if parts[1] == "zero_extend" {
                    "zext"
                } else {
                    "sext"
                }),
                json!(
                    parts[2]
                        .parse::<u32>()
                        .map_err(|_| "invalid internal extension")?
                ),
            ]
        } else {
            let name = match t.0.op.as_str() {
                "=" => "eq",
                "=>" => "implies",
                "bvnot" => "bnot",
                "bvadd" => "add",
                "bvsub" => "sub",
                "bvmul" => "mul",
                "bvand" => "band",
                "bvor" => "bor",
                "bvxor" => "bxor",
                "bvshl" => "shl",
                "bvlshr" => "lshr",
                "bvult" => "ult",
                "bvule" => "ule",
                "bvslt" => "slt",
                "bvsle" => "sle",
                "not" | "and" | "or" | "xor" | "ite" | "concat" => t.0.op.as_str(),
                _ => return Err(format!("cannot export internal op {}", t.0.op)),
            };
            vec![json!(name)]
        };
        value.extend(args);
        if self.inline {
            return Ok(Value::Array(value));
        }
        let name = format!("sir{}", self.wires.len());
        self.wires.insert(name.clone(), Value::Array(value));
        self.seen.insert(t.clone(), name.clone());
        Ok(json!(format!("w.{name}")))
    }
}
impl Transition {
    /// Export ordinary validated lydite expression JSON with DAG wire sharing.
    /// `inline=true` is useful for reset equations, whose format forbids wires.
    pub fn to_json(&self, inline: bool) -> Res<Value> {
        let mut export = Export {
            seen: Default::default(),
            wires: Map::new(),
            inline,
        };
        let mut next = Map::new();
        let mut outputs = Map::new();
        for (k, v) in &self.next {
            next.insert(k.clone(), export.expression(v)?);
        }
        for (k, v) in &self.outputs {
            outputs.insert(k.clone(), export.expression(v)?);
        }
        Ok(
            json!({"status":"symbolic_one_step_lifted_not_verified","next":next,"outputs":outputs,"wires":export.wires,"statistics":{"guard_nodes":self.statistics.guard_nodes,"guard_work":self.statistics.guard_work,"guard_aborted":self.statistics.guard_aborted,"execution_units":self.statistics.execution_units,"branches":self.statistics.branches,"joins":self.statistics.joins,"dynamic_accesses":self.statistics.dynamic_accesses,"instructions":self.statistics.instructions},"semantics":{"outputs":"pre_edge_after_eval_comb","next":"after_selected_event_eval_apply_ffs","state":"arbitrary_selected_state","domain":"two_state","cyclic_cfg":"unsupported"}}),
        )
    }
}

#[cfg(test)]
mod tests;

// Check structural boundaries before any indexed instruction access. The
// compile-only exporter is trusted for language lowering, not for memory safety.
fn validate_unit(unit: &Unit) -> Res<()> {
    let (blocks, types) = (&unit.blocks, &unit.register_map);
    if blocks.len() > 10000 || types.len() > 100000 {
        return Err("SIR unit exceeds structural budget".into());
    }
    let reg = |r: &RegisterId| -> Res<()> {
        if !types.contains_key(r) {
            return Err(format!("untyped SIR register {}", r.0));
        }
        Ok(())
    };
    let target = |id: &BlockId| -> Res<()> {
        if !blocks.contains_key(id) {
            return Err(format!("missing CFG target {}", id.0));
        }
        Ok(())
    };
    let edge = |id: &BlockId, arguments: &[RegisterId]| -> Res<()> {
        target(id)?;
        arguments.iter().try_for_each(reg)
    };
    let offset = |o: &SIROffset| o.dynamic_registers().iter().flatten().try_for_each(reg);
    target(&unit.entry_block_id)?;
    for block in blocks.values() {
        block.params.iter().try_for_each(reg)?;
        if block.instructions.len() > 100000 {
            return Err("SIR block exceeds instruction budget".into());
        }
        for instruction in &block.instructions {
            match instruction {
                SIRInstruction::Imm(d, _) => reg(d)?,
                SIRInstruction::Unary(d, _, s) | SIRInstruction::Slice(d, s, ..) => {
                    reg(d)?;
                    reg(s)?
                }
                SIRInstruction::Binary(d, a, _, c) => [d, a, c].into_iter().try_for_each(reg)?,
                SIRInstruction::Load(d, _, o, _) | SIRInstruction::Store(_, o, _, d, ..) => {
                    reg(d)?;
                    offset(o)?
                }
                SIRInstruction::Commit(_, _, o, ..) => offset(o)?,
                SIRInstruction::Concat(d, parts) => {
                    reg(d)?;
                    parts.iter().try_for_each(reg)?
                }
                SIRInstruction::Mux(d, c, a, e) => [d, c, a, e].into_iter().try_for_each(reg)?,
                _ => return Err(unsupported(instruction)),
            }
        }
        match &block.terminator {
            SIRTerminator::Jump(id, arguments) => edge(id, arguments)?,
            SIRTerminator::Branch {
                cond,
                true_block,
                false_block,
            } => {
                reg(cond)?;
                edge(&true_block.0, &true_block.1)?;
                edge(&false_block.0, &false_block.1)?;
            }
            SIRTerminator::Switch {
                selector,
                cases,
                default,
            } => {
                reg(selector)?;
                target(default)?;
                for case in cases {
                    target(&case.target)?;
                    bytes(&case.value)?;
                }
            }
            SIRTerminator::Return | SIRTerminator::Error(_) => {}
        }
    }
    Ok(())
}
