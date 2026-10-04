//! Symbolic one-event lifting of compile-only Veryl/Celox SIR.
//!
//! No solver, simulator, sample values or uniqueness queries occur here. Branches
//! are joined with guards, every selected state/input is arbitrary, and sparse
//! NBA regions retain a write mask. Unsupported operations fail closed.
use lydite_ir::{self as ir, Env, Lower, Res, Sort, Term};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

mod guard;

type Addr = (u64, u64, u64);
fn num(v: &Value) -> Res<usize> {
    v.as_u64()
        .and_then(|x| usize::try_from(x).ok())
        .ok_or("expected nonnegative integer".into())
}
fn arr(v: &Value) -> Res<&Vec<Value>> {
    v.as_array().ok_or("expected array".into())
}
fn obj(v: &Value) -> Res<&Map<String, Value>> {
    v.as_object().ok_or("expected object".into())
}
fn addr(v: &Value) -> Res<Addr> {
    Ok((
        num(&v["instance_id"])? as u64,
        num(&v["var_id"])? as u64,
        if let Some(region) = v.get("region") {
            num(region)? as u64
        } else {
            0
        },
    ))
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
fn bytes(v: &Value) -> Res<u64> {
    let a = arr(v)?;
    if a.len() > 8 && a[8..].iter().any(|v| v != 0) {
        return Err("constant exceeds 64 bits".into());
    }
    let mut n = 0;
    for (i, v) in a.iter().take(8).enumerate() {
        let x = num(v)?;
        if x > 255 {
            return Err("invalid byte".into());
        }
        n |= (x as u64) << (i * 8)
    }
    Ok(n)
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
fn storage_shape(metadata: &Value) -> Res<(u32, usize)> {
    let total = num(&metadata["width"])?;
    let count = arr(&metadata["array_dims"])?
        .iter()
        .try_fold(1usize, |a, b| {
            a.checked_mul(num(b)?)
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
fn signal(code: &Value, name: &str) -> Res<(Addr, u32, usize)> {
    let found = arr(&code["signals"])?
        .iter()
        .filter(|s| {
            s["instances"] == json!([])
                && s["path"].as_array().is_some_and(|p| {
                    p.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(".")
                        == name
                })
        })
        .collect::<Vec<_>>();
    if found.len() != 1 {
        return Err(format!("missing/ambiguous top-level signal {name}"));
    }
    let s = found[0];
    let (w, n) = storage_shape(&s["metadata"])?;
    Ok((addr(&s["address"])?, w, n))
}
fn bindings(code: &Value, values: &Value) -> Res<Vec<Binding>> {
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
pub fn lift(code: &Value, config: &Value) -> Res<Transition> {
    if code["status"] != "compiled_only_not_verified" {
        return Err("expected compile-only SIR export".into());
    }
    if code["four_state"] != false {
        return Err(
            "symbolic lifting requires an explicit two-state frontend export (four_state=false)"
                .into(),
        );
    }
    for initial in arr(&code["design"]["initial_state"])? {
        let data = obj(&initial["data"])?;
        if data.len() != 1 || !data.contains_key("Writes") {
            return Err("unsupported initial-state format".into());
        }
        for run in arr(&data["Writes"])? {
            if arr(&run["mask_bytes"])?
                .iter()
                .any(|v| v.as_u64() != Some(0))
            {
                return Err("unknown/four-state initialization is unsupported".into());
            }
        }
    }
    if !arr(&code["design"]["cascaded_events"])?.is_empty() {
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
    for o in arr(&code["design"]["state_objects"])? {
        let a = addr(&o["address"])?;
        let m = &o["metadata"];
        let (w, n) = storage_shape(m)?;
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
    l.phase(&code["sir"]["eval_comb"])?;
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
    for pair in arr(&code["design"]["event_aliases"])? {
        if arr(pair)?.len() != 2 {
            return Err("invalid event alias".into());
        }
        if aliases.insert(addr(&pair[0])?, addr(&pair[1])?).is_some() {
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
    let groups = arr(&code["sir"]["eval_apply_ffs"])?;
    let selected = groups
        .iter()
        .filter(|g| addr(&g["event"]).ok() == Some(event))
        .collect::<Vec<_>>();
    if selected.len() != 1 {
        return Err("selected event must have exactly one SIR group".into());
    }
    let units = selected[0];
    l.phase(&units["units"])?;
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
    fn phase(&mut self, units: &Value) -> Res<()> {
        for unit in arr(units)? {
            self.execute(unit)?
        }
        Ok(())
    }
    fn access_guard(offset: &Value, regs: &BTreeMap<usize, Term>, at: usize) -> Res<Term> {
        if let Some(x) = offset.get("Static") {
            return Ok(b(num(x)? == at));
        }
        if let Some(x) = offset.get("PackedElements") {
            return Ok(b(num(&x["bit_offset"])? == at));
        }
        let reg = |v: &Value| -> Res<Term> {
            regs.get(&num(v)?)
                .cloned()
                .ok_or("undefined offset register".into())
        };
        if let Some(x) = offset.get("Dynamic") {
            let x = reg(x)?;
            return Ok(if width(&x) < 64 && (at as u64) >= 1u64 << width(&x) {
                b(false)
            } else {
                eq(x.clone(), bv(width(&x), at as u64))
            });
        }
        if let Some(x) = offset.get("Element") {
            let ew = num(&x["element_width"])?;
            if ew == 0 {
                return Err("zero element width".into());
            }
            let off = num(&x["bit_offset"])?;
            if at < off {
                return Ok(b(false));
            }
            let remainder = at - off;
            let index = reg(&x["index"])?;
            if x["dynamic_bit_offset"].is_null() {
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
            }
            let dynamic = reg(&x["dynamic_bit_offset"])?;
            // Bounds ensure the 64-bit arithmetic cannot wrap into a valid address.
            let ix = resize(index, 64, false);
            let dy = resize(dynamic, 64, false);
            let bounded = and(
                compare("bvule", ix.clone(), bv(64, (remainder / ew) as u64)),
                compare("bvule", dy.clone(), bv(64, remainder as u64)),
            );
            let sum = op("bvadd", 64, op("bvmul", 64, ix, bv(64, ew as u64)), dy);
            return Ok(and(bounded, eq(sum, bv(64, remainder as u64))));
        }
        Err("unsupported SIR storage offset".into())
    }
    fn static_offset(off: &Value) -> Res<Option<usize>> {
        if let Some(x) = off.get("Static") {
            Ok(Some(num(x)?))
        } else if let Some(x) = off.get("PackedElements") {
            Ok(Some(num(&x["bit_offset"])?))
        } else {
            Ok(None)
        }
    }
    fn load(&mut self, a: Addr, off: &Value, w: u32) -> Res<Term> {
        if a.2 == 2 {
            return Err("sparse NBA region is write-only".into());
        }
        self.ensure(a)?;
        let st = &self.frame.state[&a];
        if let Some(offset) = Self::static_offset(off)? {
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
    fn store(&mut self, a: Addr, off: &Value, w: u32, value: Term) -> Res<()> {
        self.ensure(a)?;
        if let Some(offset) = Self::static_offset(off)? {
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
    fn commit(&mut self, src: Addr, dst: Addr, off: &Value, w: usize) -> Res<()> {
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
            if Self::static_offset(off)? != Some(0) || w != source.bits() {
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
        if let Some(offset) = Self::static_offset(off)? {
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
    fn execute(&mut self, unit: &Value) -> Res<()> {
        validate_unit(unit)?;
        self.stats.execution_units += 1;
        self.frame.regs.clear();
        self.frame.guard = b(true);
        let blocks = obj(&unit["blocks"])?;
        let types = obj(&unit["register_map"])?;
        let mut kinds = BTreeMap::new();
        for (k, t) in types {
            let type_object = obj(t)?;
            if type_object.len() != 1 {
                return Err("invalid register type tag".into());
            }
            let (knd, m) = type_object.iter().next().ok_or("empty register type")?;
            if !matches!(knd.as_str(), "Bit" | "Logic") {
                return Err("unknown register type".into());
            }
            let w = num(&m["width"])?;
            if !(1..=64).contains(&w) {
                return Err(format!("SIR register width {w} outside 1..64"));
            }
            kinds.insert(
                k.parse::<usize>().map_err(|_| "invalid register id")?,
                (w as u32, m["signed"].as_bool().unwrap_or(false)),
            );
        }
        for block in blocks.values() {
            for inst in arr(&block["instructions"])? {
                let (op, args) = obj(inst)?.iter().next().ok_or("empty instruction")?;
                let aa = arr(args)?;
                match op.as_str() {
                    "Load" => self.ensure(addr(&aa[1])?)?,
                    "Store" => self.ensure(addr(&aa[0])?)?,
                    "Commit" => {
                        self.ensure(addr(&aa[0])?)?;
                        self.ensure(addr(&aa[1])?)?
                    }
                    _ => {}
                }
            }
        }
        let entry = num(&unit["entry_block_id"])?;
        let mut marks = BTreeMap::new();
        let mut order = vec![];
        fn visit(
            id: usize,
            blocks: &Map<String, Value>,
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
            let block = blocks.get(&id.to_string()).ok_or("missing CFG target")?;
            for target in successors(&block["terminator"])? {
                visit(target, blocks, marks, order)?
            }
            marks.insert(id, 2);
            order.push(id);
            Ok(())
        }
        visit(entry, blocks, &mut marks, &mut order)?;
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
            let block = &blocks[&id.to_string()];
            for instruction in arr(&block["instructions"])? {
                self.stats.instructions += 1;
                let (o, a) = obj(instruction)?.iter().next().ok_or("empty instruction")?;
                let args = arr(a)?;
                let reg = |f: &Frame, v: &Value| -> Res<Term> {
                    f.regs
                        .get(&num(v)?)
                        .cloned()
                        .ok_or_else(|| format!("undefined SIR register {v}"))
                };
                let rwidth = |v: &Value| -> Res<u32> {
                    Ok(kinds.get(&num(v)?).ok_or("untyped register")?.0)
                };
                let result: Option<(usize, Term)> = match o.as_str() {
                    "Imm" => {
                        if bytes(&args[1]["mask"])? != 0 {
                            return Err(
                                "four-state immediate unsupported in symbolic two-state model"
                                    .into(),
                            );
                        }
                        let w = rwidth(&args[0])?;
                        let value = bytes(&args[1]["payload"])?;
                        if w < 64 && value >= 1u64 << w {
                            return Err("SIR immediate exceeds destination width".into());
                        }
                        Some((num(&args[0])?, bv(w, value)))
                    }
                    "Unary" => {
                        let x = reg(&self.frame, &args[2])?;
                        let signed = kinds[&num(&args[2])?].1;
                        Some((
                            num(&args[0])?,
                            unary(
                                args[1].as_str().ok_or("unary name")?,
                                x,
                                rwidth(&args[0])?,
                                signed,
                            )?,
                        ))
                    }
                    "Binary" => {
                        let a = reg(&self.frame, &args[1])?;
                        let c = reg(&self.frame, &args[3])?;
                        Some((
                            num(&args[0])?,
                            binary(
                                args[2].as_str().ok_or("binary name")?,
                                a,
                                c,
                                rwidth(&args[0])?,
                                kinds[&num(&args[1])?].1,
                                kinds[&num(&args[3])?].1,
                            )?,
                        ))
                    }
                    "Load" => {
                        let w = num(&args[3])?;
                        if !(1..=64).contains(&w) {
                            return Err("load width outside 1..64".into());
                        }
                        if rwidth(&args[0])? != w as u32 {
                            return Err("SIR load width mismatch".into());
                        }
                        Some((
                            num(&args[0])?,
                            self.load(addr(&args[1])?, &args[2], w as u32)?,
                        ))
                    }
                    "Store" => {
                        if args.len() != 6
                            || !arr(&args[4])?.is_empty()
                            || !arr(&args[5])?.is_empty()
                        {
                            return Err("triggered/observed SIR store needs event semantics".into());
                        }
                        let value = reg(&self.frame, &args[3])?;
                        let w = num(&args[2])?;
                        if !(1..=64).contains(&w) {
                            return Err("store width outside 1..64".into());
                        }
                        if w as u32 > width(&value) {
                            return Err("SIR store wider than source".into());
                        }
                        self.store(
                            addr(&args[0])?,
                            &args[1],
                            w as u32,
                            resize(value, w as u32, false),
                        )?;
                        None
                    }
                    "Commit" => {
                        if args.len() != 5 || !arr(&args[4])?.is_empty() {
                            return Err("triggered commit needs event semantics".into());
                        }
                        self.commit(addr(&args[0])?, addr(&args[1])?, &args[2], num(&args[3])?)?;
                        None
                    }
                    "Slice" => {
                        let x = reg(&self.frame, &args[1])?;
                        let lo = u32::try_from(num(&args[2])?)
                            .map_err(|_| "SIR slice offset overflow")?;
                        let w = u32::try_from(num(&args[3])?)
                            .map_err(|_| "SIR slice width overflow")?;
                        if w == 0 || w > 64 || lo.checked_add(w).is_none_or(|n| n > width(&x)) {
                            return Err("invalid SIR slice".into());
                        }
                        if rwidth(&args[0])? != w {
                            return Err("SIR slice result width mismatch".into());
                        }
                        Some((num(&args[0])?, extract(x, lo, w)))
                    }
                    "Concat" => {
                        let mut parts = arr(&args[1])?.iter();
                        let mut x = reg(&self.frame, parts.next().ok_or("empty concat")?)?;
                        for part in parts {
                            x = cat(x, reg(&self.frame, part)?)?
                        }
                        if rwidth(&args[0])? != width(&x) {
                            return Err("SIR concat result width mismatch".into());
                        }
                        Some((num(&args[0])?, x))
                    }
                    "Mux" => {
                        let w = rwidth(&args[0])?;
                        if rwidth(&args[2])? != w || rwidth(&args[3])? != w {
                            return Err("SIR mux width mismatch".into());
                        }
                        Some((
                            num(&args[0])?,
                            ite(
                                truth(reg(&self.frame, &args[1])?),
                                resize(reg(&self.frame, &args[2])?, w, false),
                                resize(reg(&self.frame, &args[3])?, w, false),
                            ),
                        ))
                    }
                    _ => return Err(format!("unsupported symbolic SIR instruction {o}")),
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
            let term = &block["terminator"];
            if term == "Return" {
                returns.push(self.frame.clone());
                continue;
            }
            let (kind, data) = obj(term)?.iter().next().ok_or("empty terminator")?;
            let mut edges = vec![];
            match kind.as_str() {
                "Jump" => edges.push((num(&data[0])?, arr(&data[1])?.clone(), b(true))),
                "Branch" => {
                    self.stats.branches += 1;
                    let cond = truth(
                        self.frame
                            .regs
                            .get(&num(&data["cond"])?)
                            .cloned()
                            .ok_or("undefined branch condition")?,
                    );
                    for (key, g) in [("true_block", cond.clone()), ("false_block", not(cond))] {
                        let target = &data[key];
                        edges.push((num(&target[0])?, arr(&target[1])?.clone(), g));
                    }
                }
                "Switch" => {
                    self.stats.branches += 1;
                    let selector = self
                        .frame
                        .regs
                        .get(&num(&data["selector"])?)
                        .cloned()
                        .ok_or("undefined switch selector")?;
                    let mut remaining = b(true);
                    let mut seen = BTreeSet::new();
                    for case in arr(&data["cases"])? {
                        let n = bytes(&case["value"])?;
                        if !seen.insert(n) {
                            return Err("duplicate switch case".into());
                        }
                        if width(&selector) < 64 && n >= 1 << width(&selector) {
                            return Err("switch case exceeds selector width".into());
                        }
                        let cond = eq(selector.clone(), bv(width(&selector), n));
                        edges.push((
                            num(&case["target"])?,
                            vec![],
                            and(remaining.clone(), cond.clone()),
                        ));
                        remaining = and(remaining, not(cond));
                    }
                    edges.push((num(&data["default"])?, vec![], remaining));
                }
                "Error" => return Err(format!("symbolically reachable SIR runtime error {data}")),
                _ => return Err(format!("unsupported terminator {kind}")),
            }
            for (target, arguments, guard) in edges {
                let guard = and(self.frame.guard.clone(), guard);
                if guard == b(false) {
                    continue;
                }
                let mut frame = self.frame.clone();
                frame.guard = guard;
                let params = arr(&blocks[&target.to_string()]["params"])?;
                if params.len() != arguments.len() {
                    return Err("CFG argument arity mismatch".into());
                }
                let values = arguments
                    .iter()
                    .map(|r| {
                        frame
                            .regs
                            .get(&num(r)?)
                            .cloned()
                            .ok_or("undefined CFG argument".into())
                    })
                    .collect::<Res<Vec<_>>>()?;
                for (param, value) in params.iter().zip(values) {
                    let p = num(param)?;
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
fn successors(term: &Value) -> Res<Vec<usize>> {
    if term == "Return" {
        return Ok(vec![]);
    }
    let (k, v) = obj(term)?.iter().next().ok_or("empty terminator")?;
    match k.as_str() {
        "Jump" => Ok(vec![num(&v[0])?]),
        "Branch" => Ok(vec![num(&v["true_block"][0])?, num(&v["false_block"][0])?]),
        "Switch" => {
            let mut out = arr(&v["cases"])?
                .iter()
                .map(|c| num(&c["target"]))
                .collect::<Res<Vec<_>>>()?;
            out.push(num(&v["default"])?);
            Ok(out)
        }
        "Error" => Ok(vec![]),
        _ => Err("unsupported CFG terminator".into()),
    }
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
fn unary(name: &str, x: Term, w: u32, signed: bool) -> Res<Term> {
    if matches!(name, "Minus" | "BitNot" | "ToTwoState") && width(&x) != w {
        return Err("SIR unary operand/result width mismatch".into());
    }
    if matches!(name, "LogicNot" | "And" | "Or" | "Xor") && w != 1 {
        return Err("SIR reduction result must be 1 bit".into());
    }
    Ok(match name {
        "Ident" | "ToTwoState" => resize(x, w, name == "Ident" && signed),
        "BitNot" => {
            let x = resize(x, w, signed);
            if let Some(n) = constant(&x) {
                bv(w, !n)
            } else {
                ir::node(Sort::Bv(w), "bvnot", vec![x])
            }
        }
        "Minus" => op("bvsub", w, bv(w, 0), resize(x, w, true)),
        "LogicNot" => boolword(not(truth(x)), w),
        "And" => {
            let n = width(&x);
            boolword(eq(x, bv(n, if n == 64 { !0 } else { (1 << n) - 1 })), w)
        }
        "Or" => boolword(truth(x), w),
        "Xor" => {
            let mut v = b(false);
            for i in 0..width(&x) {
                v = ir::node(Sort::Bool, "xor", vec![v, truth(extract(x.clone(), i, 1))])
            }
            boolword(v, w)
        }
        _ => return Err(format!("unsupported symbolic unary {name}")),
    })
}
fn binary(name: &str, a: Term, c: Term, w: u32, sa: bool, _sc: bool) -> Res<Term> {
    if matches!(
        name,
        "Eq" | "Ne"
            | "EqCase"
            | "NeCase"
            | "EqWildcard"
            | "NeWildcard"
            | "LtU"
            | "LeU"
            | "GtU"
            | "GeU"
            | "LtS"
            | "LeS"
            | "GtS"
            | "GeS"
    ) {
        if width(&a) != width(&c) || w != 1 {
            return Err("SIR comparison requires equal operands and a 1-bit result".into());
        }
        let n = width(&a).max(width(&c));
        let signed = name.ends_with('S');
        let mut a = resize(a, n, signed);
        let mut c = resize(c, n, signed);
        let p = if name.starts_with("Eq") {
            eq(a, c)
        } else if name.starts_with("Ne") {
            not(eq(a, c))
        } else {
            if name.starts_with("Gt") || name.starts_with("Ge") {
                std::mem::swap(&mut a, &mut c)
            }
            compare(
                match (
                    name.ends_with('S'),
                    name.starts_with("Le") || name.starts_with("Ge"),
                ) {
                    (false, false) => "bvult",
                    (false, true) => "bvule",
                    (true, false) => "bvslt",
                    (true, true) => "bvsle",
                },
                a,
                c,
            )
        };
        return Ok(boolword(p, w));
    }
    if matches!(name, "LogicAnd" | "LogicOr") {
        if w != 1 {
            return Err("SIR logical result must be 1 bit".into());
        }
        return Ok(boolword(
            if name == "LogicAnd" {
                and(truth(a), truth(c))
            } else {
                or(truth(a), truth(c))
            },
            w,
        ));
    }
    if matches!(name, "Shl" | "Shr" | "Sar") {
        let n = if name == "Shl" { w } else { w.max(width(&a)) };
        let operand = resize(a.clone(), n, name == "Sar" || (name == "Shl" && sa));
        let limit = if width(&c) < 64 && (n as u64) >= 1 << width(&c) {
            b(true)
        } else {
            compare("bvult", c.clone(), bv(width(&c), n as u64))
        };
        let amount = resize(c, n, false);
        let shifted = if name == "Shl" {
            op("bvshl", n, operand, amount)
        } else if name == "Shr" {
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
        let fallback = if name == "Sar" {
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
        "Add" => "bvadd",
        "Sub" => "bvsub",
        "Mul" => "bvmul",
        "And" => "bvand",
        "Or" => "bvor",
        "Xor" => "bvxor",
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
fn validate_unit(unit: &Value) -> Res<()> {
    let blocks = obj(&unit["blocks"])?;
    let types = obj(&unit["register_map"])?;
    if blocks.len() > 10000 || types.len() > 100000 {
        return Err("SIR unit exceeds structural budget".into());
    }
    let reg = |v: &Value| -> Res<()> {
        let r = num(v)?;
        if !types.contains_key(&r.to_string()) {
            return Err(format!("untyped SIR register {r}"));
        }
        Ok(())
    };
    let target = |v: &Value| -> Res<()> {
        let id = num(v)?;
        if !blocks.contains_key(&id.to_string()) {
            return Err(format!("missing CFG target {id}"));
        }
        Ok(())
    };
    let edge = |v: &Value| -> Res<()> {
        let a = arr(v)?;
        if a.len() != 2 {
            return Err("invalid CFG edge".into());
        }
        target(&a[0])?;
        for r in arr(&a[1])? {
            reg(r)?
        }
        Ok(())
    };
    let offset = |v: &Value| -> Res<()> {
        let o = obj(v)?;
        if o.len() != 1 {
            return Err("invalid storage offset tag".into());
        }
        let (k, x) = o.iter().next().unwrap();
        match k.as_str() {
            "Static" => {
                num(x)?;
            }
            "PackedElements" => {
                num(&x["bit_offset"])?;
            }
            "Dynamic" => reg(x)?,
            "Element" => {
                reg(&x["index"])?;
                num(&x["element_width"])?;
                num(&x["bit_offset"])?;
                if !x["dynamic_bit_offset"].is_null() {
                    reg(&x["dynamic_bit_offset"])?
                }
            }
            _ => return Err("unknown storage offset".into()),
        }
        Ok(())
    };
    target(&unit["entry_block_id"])?;
    for block in blocks.values() {
        for param in arr(&block["params"])? {
            reg(param)?
        }
        let instructions = arr(&block["instructions"])?;
        if instructions.len() > 100000 {
            return Err("SIR block exceeds instruction budget".into());
        }
        for instruction in instructions {
            let o = obj(instruction)?;
            if o.len() != 1 {
                return Err("invalid instruction tag".into());
            }
            let (k, v) = o.iter().next().unwrap();
            let a = arr(v)?;
            let (n, registers): (usize, &[usize]) = match k.as_str() {
                "Imm" => (2, &[0]),
                "Unary" => (3, &[0, 2]),
                "Binary" => (4, &[0, 1, 3]),
                "Load" => (4, &[0]),
                "Store" => (6, &[3]),
                "Commit" => (5, &[]),
                "Concat" => (2, &[0]),
                "Slice" => (4, &[0, 1]),
                "Mux" => (4, &[0, 1, 2, 3]),
                _ => return Err(format!("unsupported symbolic SIR instruction {k}")),
            };
            if a.len() != n {
                return Err(format!("invalid {k} instruction arity"));
            }
            for index in registers {
                reg(&a[*index])?
            }
            match k.as_str() {
                "Load" => {
                    addr(&a[1])?;
                    offset(&a[2])?;
                }
                "Store" => {
                    addr(&a[0])?;
                    offset(&a[1])?;
                }
                "Commit" => {
                    addr(&a[0])?;
                    addr(&a[1])?;
                    offset(&a[2])?;
                }
                "Concat" => {
                    for r in arr(&a[1])? {
                        reg(r)?
                    }
                }
                _ => {}
            }
        }
        let t = &block["terminator"];
        if t == "Return" {
            continue;
        }
        let o = obj(t)?;
        if o.len() != 1 {
            return Err("invalid terminator tag".into());
        }
        let (k, v) = o.iter().next().unwrap();
        match k.as_str() {
            "Jump" => edge(v)?,
            "Branch" => {
                reg(&v["cond"])?;
                edge(&v["true_block"])?;
                edge(&v["false_block"])?;
            }
            "Switch" => {
                reg(&v["selector"])?;
                target(&v["default"])?;
                for case in arr(&v["cases"])? {
                    target(&case["target"])?;
                    bytes(&case["value"])?;
                }
            }
            "Error" => {}
            _ => return Err("unsupported CFG terminator".into()),
        }
    }
    Ok(())
}
