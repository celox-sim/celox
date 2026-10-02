//! Opt-in, bounded decision procedure for quantifier-free Bool/BV and total-array IR.
//!
//! Terms are bit-blasted to definitional CNF and decided by a small CDCL solver.
//! SAT assignments are independently evaluated on the ORIGINAL formula and all
//! requested context terms. Readonly arrays use exact finite-observation reduction
//! and total sparse-array model replay. Unsupported terms or exhausted budgets yield Unknown.
//! Diagnostics are not proof certificates: UNSAT trusts this Rust implementation.
use hwverify_ir::{Env, Res, Sort, Term};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    time::{Duration, Instant},
};

mod control;
mod lookup;
mod readonly;
pub use readonly::ArrayModel;

type Lit = i32;
const TRUE: Lit = 1;
const FALSE: Lit = -1;

#[derive(Clone, Debug)]
pub struct Limits {
    pub timeout_ms: u64,
    pub max_terms: usize,
    pub max_variables: usize,
    pub max_clauses: usize,
    pub max_work: u64,
    pub max_depth: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            timeout_ms: 10_000,
            max_terms: 100_000,
            max_variables: 200_000,
            max_clauses: 1_000_000,
            max_work: 100_000_000,
            max_depth: 512,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Sat,
    Unsat,
    Unknown,
}
impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sat => "sat",
            Self::Unsat => "unsat",
            Self::Unknown => "unknown",
        }
    }
}
/// Caller-declared expected result, used ONLY to choose search order.
/// It never adds an assumption or changes what a completed SAT/UNSAT means.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchHint {
    Sat,
    #[default]
    Unsat,
}
impl SearchHint {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sat => "sat",
            Self::Unsat => "unsat",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchStrategy {
    NotStarted,
    ControlCofactors,
    SingleSearch,
    ProofDecomposition,
    CounterexampleProbeFair,
}
impl SearchStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotStarted => "not_started",
            Self::ControlCofactors => "control_cofactors",
            Self::SingleSearch => "single_search",
            Self::ProofDecomposition => "proof_decomposition",
            Self::CounterexampleProbeFair => "counterexample_probe_fair",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scalar {
    Bool(bool),
    Bv { width: u32, value: u64 },
}
impl Scalar {
    pub fn smt(&self) -> String {
        match self {
            Self::Bool(b) => b.to_string(),
            Self::Bv { width, value } => format!("(_ bv{value} {width})"),
        }
    }
    pub fn json(&self) -> Value {
        match self {
            Self::Bool(b) => json!({"sort":"Bool","value":b}),
            Self::Bv { width, value } => json!({"sort":"Bv","width":width,"value":value}),
        }
    }
    fn sort(&self) -> Sort {
        match self {
            Self::Bool(_) => Sort::Bool,
            Self::Bv { width, .. } => Sort::Bv(*width),
        }
    }
    fn boolean(&self) -> bool {
        match self {
            Self::Bool(b) => *b,
            _ => unreachable!("checked type"),
        }
    }
    fn word(&self) -> u64 {
        match self {
            Self::Bv { value, .. } => *value,
            _ => unreachable!("checked type"),
        }
    }
}
#[derive(Default, Clone, Debug)]
pub struct Stats {
    pub control_variables: Vec<String>,
    pub control_cases: usize,
    pub control_cases_closed: usize,
    pub control_case_work: Vec<u64>,
    pub control_preprocess_work: u64,
    pub asserted_definitions: usize,
    pub guarded_equalities: usize,
    pub guarded_rewrites: usize,
    pub guarded_expansion_nodes: usize,
    pub lookup_rewrites: usize,
    pub word_rewrites: usize,
    pub word_expansion_nodes: usize,
    pub lookup_expansion_nodes: usize,
    pub base_cnf_reused: bool,
    pub terms: usize,
    pub variables: usize,
    pub clauses: usize,
    pub base_clauses: usize,
    pub peak_live_clauses: usize,
    pub split_alternatives: usize,
    pub split_completed: usize,
    pub split_unsat: usize,
    pub probe_result: Option<Verdict>,
    pub probe_work: u64,
    pub search_slices: u64,
    pub search_yields: u64,
    pub decisions: u64,
    pub conflicts: u64,
    pub work: u64,
}
#[derive(Debug)]
pub struct Outcome {
    pub search_hint: SearchHint,
    pub search_strategy: SearchStrategy,
    pub verdict: Verdict,
    pub reason: Option<String>,
    pub assignments: BTreeMap<String, Scalar>,
    pub context_values: BTreeMap<String, Scalar>,
    pub array_assignments: BTreeMap<String, ArrayModel>,
    pub array_context_values: BTreeMap<String, ArrayModel>,
    pub original_formula_validated: bool,
    pub stats: Stats,
}
impl Outcome {
    pub fn diagnostics(&self) -> Value {
        let mut result = json!({"solver_result":self.verdict.as_str(),"reason":self.reason,
            "control_variables":self.stats.control_variables,"control_cases":self.stats.control_cases,"control_cases_closed":self.stats.control_cases_closed,"control_case_work":self.stats.control_case_work,"control_preprocess_work":self.stats.control_preprocess_work,"search_hint":self.search_hint.as_str(),"search_strategy":self.search_strategy.as_str(),
            "kind":"bounded bit-blast/CDCL diagnostics; not an independently checkable proof certificate",
            "trusted":"Rust scalar encoding, SAT search, and original-formula evaluation; not a Lean certificate",
            "original_formula_validated":self.original_formula_validated,
            "assignments":self.assignments.iter().map(|(k,v)|(k.clone(),v.json())).collect::<BTreeMap<_,_>>(),
            "context_values":self.context_values.iter().map(|(k,v)|(k.clone(),v.json())).collect::<BTreeMap<_,_>>(),
            "terms":self.stats.terms,"variables":self.stats.variables,"clauses":self.stats.clauses,
            "word_rewrites":self.stats.word_rewrites,"word_expansion_nodes":self.stats.word_expansion_nodes,
            "lookup_rewrites":self.stats.lookup_rewrites,"lookup_expansion_nodes":self.stats.lookup_expansion_nodes,
            "guarded_equalities":self.stats.guarded_equalities,"guarded_rewrites":self.stats.guarded_rewrites,"guarded_expansion_nodes":self.stats.guarded_expansion_nodes,
            "asserted_definitions":self.stats.asserted_definitions,"base_cnf_reused":self.stats.base_cnf_reused,"base_clauses":self.stats.base_clauses,"peak_live_clauses":self.stats.peak_live_clauses,
            "split_alternatives":self.stats.split_alternatives,"split_completed":self.stats.split_completed,
            "split_unsat":self.stats.split_unsat,
            "probe_result":self.stats.probe_result.map(Verdict::as_str),"probe_work":self.stats.probe_work,
            "search_slices":self.stats.search_slices,"search_yields":self.stats.search_yields,
            "clause_accounting":"aggregate allocated CNF/branch/learned clauses within the whole-query limit; sequential proof splits retain one immutable base and one reusable working CNF",
            "decisions":self.stats.decisions,"conflicts":self.stats.conflicts,"work":self.stats.work});
        result["readonly_arrays"] = json!({            "readonly_array_rule":"Complete finite read/store-index congruence, exact read-over-write, and extensional disequality witnesses; consistent observations extend to total arrays with common zero default. Rust reduction and original-array replay are trusted",            "array_assignments":self.array_assignments.iter().map(|(k,v)|(k.clone(),v.json())).collect::<BTreeMap<_,_>>(),            "array_context_values":self.array_context_values.iter().map(|(k,v)|(k.clone(),v.json())).collect::<BTreeMap<_,_>>()});
        result
    }
}
struct Budget {
    limits: Limits,
    start: Instant,
    work: u64,
    time_check_in: u16,
}
impl Budget {
    fn tick(&mut self, amount: u64) -> Res<()> {
        self.work = self.work.saturating_add(amount);
        if self.work > self.limits.max_work {
            return Err("finite solver work budget exhausted".into());
        }
        // Work limits are exact. Amortize the monotonic-clock call over at
        // most 256 bounded work checkpoints, and always check before returning
        // a verdict. This avoids a clock syscall/vDSO call per literal/heap step.
        if self.time_check_in == 0 {
            self.check_time()?;
            self.time_check_in = 255;
        } else {
            self.time_check_in -= 1;
        }
        Ok(())
    }
    fn check_time(&self) -> Res<()> {
        if self.start.elapsed() >= Duration::from_millis(self.limits.timeout_ms) {
            return Err("finite solver time budget exhausted".into());
        }
        Ok(())
    }
}
fn mask(w: u32) -> u64 {
    if w == 64 {
        u64::MAX
    } else {
        (1u64 << w) - 1
    }
}
fn valid_sort(s: &Sort) -> bool {
    matches!(s, Sort::Bool | Sort::Bv(1..=64))
}
#[derive(Clone, Debug)]
enum Op {
    Variable(String),
    Bool(bool),
    Word(u64),
    Not,
    And,
    Or,
    Xor,
    Implies,
    Eq,
    Ite,
    BNot,
    BAnd,
    BOr,
    BXor,
    Add,
    Sub,
    Mul,
    Shl,
    LShr,
    Ult,
    Ule,
    Slt,
    Sle,
    Concat,
    Extract(u32, u32),
    Zext,
    Sext,
}
/// Check the entire node signature before any encoder or evaluator indexes it.
fn operation(t: &Term) -> Res<Op> {
    if !valid_sort(&t.0.sort) {
        return Err(format!("unsupported finite sort {}", t.0.sort.smt()));
    }
    let a = &t.0.args;
    let name = t.0.op.as_str();
    let bad = || format!("unsupported or ill-typed finite operation {name}");
    if let Some(n) = name.strip_prefix('@') {
        if !a.is_empty() || n.is_empty() {
            return Err(bad());
        }
        return Ok(Op::Variable(n.into()));
    }
    if a.is_empty() {
        if t.0.sort == Sort::Bool {
            return match name {
                "true" => Ok(Op::Bool(true)),
                "false" => Ok(Op::Bool(false)),
                _ => Err(bad()),
            };
        }
        if let Some(text) = name.strip_prefix("(_ bv").and_then(|s| s.strip_suffix(')')) {
            let fields = text.split_whitespace().collect::<Vec<_>>();
            if fields.len() == 2 {
                if let (Ok(v), Ok(w)) = (fields[0].parse::<u64>(), fields[1].parse::<u32>()) {
                    if t.0.sort == Sort::Bv(w) {
                        return Ok(Op::Word(v & mask(w)));
                    }
                }
            }
        }
        return Err(bad());
    }
    let bools = || a.iter().all(|x| x.0.sort == Sort::Bool) && t.0.sort == Sort::Bool;
    let words =
        || a.iter().all(|x| x.0.sort == a[0].0.sort) && matches!(a[0].0.sort, Sort::Bv(1..=64));
    let op = match name {
        "not" if a.len() == 1 && bools() => Op::Not,
        "and" if a.len() == 2 && bools() => Op::And,
        "or" if a.len() == 2 && bools() => Op::Or,
        "xor" if a.len() == 2 && bools() => Op::Xor,
        "=>" if a.len() == 2 && bools() => Op::Implies,
        "=" if a.len() == 2 && a[0].0.sort == a[1].0.sort && t.0.sort == Sort::Bool => Op::Eq,
        "ite"
            if a.len() == 3
                && a[0].0.sort == Sort::Bool
                && a[1].0.sort == a[2].0.sort
                && t.0.sort == a[1].0.sort =>
        {
            Op::Ite
        }
        "bvnot" if a.len() == 1 && words() && t.0.sort == a[0].0.sort => Op::BNot,
        "bvand" | "bvor" | "bvxor" | "bvadd" | "bvsub" | "bvmul" | "bvshl" | "bvlshr"
            if a.len() == 2 && words() && t.0.sort == a[0].0.sort =>
        {
            match name {
                "bvand" => Op::BAnd,
                "bvor" => Op::BOr,
                "bvxor" => Op::BXor,
                "bvadd" => Op::Add,
                "bvsub" => Op::Sub,
                "bvmul" => Op::Mul,
                "bvshl" => Op::Shl,
                _ => Op::LShr,
            }
        }
        "bvult" | "bvule" | "bvslt" | "bvsle"
            if a.len() == 2 && words() && t.0.sort == Sort::Bool =>
        {
            match name {
                "bvult" => Op::Ult,
                "bvule" => Op::Ule,
                "bvslt" => Op::Slt,
                _ => Op::Sle,
            }
        }
        "concat" if a.len() == 2 => match (&a[0].0.sort, &a[1].0.sort, &t.0.sort) {
            (Sort::Bv(x), Sort::Bv(y), Sort::Bv(z))
                if *x > 0 && *y > 0 && x.checked_add(*y) == Some(*z) =>
            {
                Op::Concat
            }
            _ => return Err(bad()),
        },
        _ => {
            let f = name
                .strip_prefix("(_ ")
                .and_then(|s| s.strip_suffix(')'))
                .map(|s| s.split_whitespace().collect::<Vec<_>>())
                .ok_or_else(bad)?;
            if a.len() != 1 {
                return Err(bad());
            }
            let (Sort::Bv(w), Sort::Bv(out)) = (&a[0].0.sort, &t.0.sort) else {
                return Err(bad());
            };
            match f.as_slice() {
                ["extract", hi, lo] => {
                    let hi = hi.parse::<u32>().map_err(|_| bad())?;
                    let lo = lo.parse::<u32>().map_err(|_| bad())?;
                    if lo > hi || hi >= *w || hi - lo + 1 != *out {
                        return Err(bad());
                    }
                    Op::Extract(hi, lo)
                }
                [kind, n] if *kind == "zero_extend" || *kind == "sign_extend" => {
                    let n = n.parse::<u32>().map_err(|_| bad())?;
                    if w.checked_add(n) != Some(*out) || *w == 0 {
                        return Err(bad());
                    }
                    if *kind == "zero_extend" {
                        Op::Zext
                    } else {
                        Op::Sext
                    }
                }
                _ => return Err(bad()),
            }
        }
    };
    Ok(op)
}
#[derive(Clone)]
struct Bits {
    sort: Sort,
    bits: Vec<Lit>,
}
/// Variables equated by mandatory, positive top-level conjuncts may share bits.
/// No equality below an implication, disjunction, negation or context term is used.
/// Union-by-size bounds lookup depth; every traversal is charged to the budget.
#[derive(Default)]
struct Aliases {
    definitions: HashMap<String, Term>,
    guarded: HashMap<String, (Vec<Term>, Term)>,
    ids: HashMap<String, usize>,
    names: Vec<String>,
    parents: Vec<usize>,
    sizes: Vec<usize>,
}
impl Aliases {
    fn intern(&mut self, name: String) -> usize {
        if let Some(&id) = self.ids.get(&name) {
            return id;
        }
        let id = self.parents.len();
        self.ids.insert(name.clone(), id);
        self.names.push(name);
        self.parents.push(id);
        self.sizes.push(1);
        id
    }
    fn root(&self, mut id: usize, b: &mut Budget) -> Res<usize> {
        loop {
            b.tick(1)?;
            if self.parents[id] == id {
                return Ok(id);
            }
            id = self.parents[id];
        }
    }
    fn join(&mut self, x: String, y: String, b: &mut Budget) -> Res<()> {
        let x = self.intern(x);
        let y = self.intern(y);
        let mut x = self.root(x, b)?;
        let mut y = self.root(y, b)?;
        if x != y {
            if self.sizes[x] < self.sizes[y] {
                std::mem::swap(&mut x, &mut y);
            }
            self.parents[y] = x;
            self.sizes[x] += self.sizes[y];
        }
        Ok(())
    }
    fn representative(&self, name: &str, b: &mut Budget) -> Res<String> {
        match self.ids.get(name) {
            Some(&id) => Ok(self.names[self.root(id, b)?].clone()),
            None => Ok(name.into()),
        }
    }
    fn collect(&mut self, formula: &Term, b: &mut Budget) -> Res<()> {
        let mut candidates = Vec::new();
        let mut pending = vec![(formula, 0usize)];
        let mut seen = HashSet::new();
        while let Some((term, depth)) = pending.pop() {
            b.tick(1)?;
            if depth > b.limits.max_depth {
                return Err("finite solver term depth budget exhausted".into());
            }
            if !seen.insert(term.clone()) {
                continue;
            }
            if seen.len() > b.limits.max_terms {
                return Err("finite solver term budget exhausted".into());
            }
            match operation(term)? {
                Op::And => {
                    pending.push((&term.0.args[1], depth + 1));
                    pending.push((&term.0.args[0], depth + 1));
                }
                Op::Eq => match (operation(&term.0.args[0])?, operation(&term.0.args[1])?) {
                    (Op::Variable(x), Op::Variable(y)) => self.join(x, y, b)?,
                    (Op::Variable(x), _) => candidates.push((x, term.0.args[1].clone())),
                    (_, Op::Variable(y)) => candidates.push((y, term.0.args[0].clone())),
                    _ => {}
                },
                _ => {}
            }
        }
        // Optimize only asserted equalities; disjunctions/implications never
        // introduce definitions. Union-find is complete before cycle checks.
        for (name, rhs) in candidates {
            if self.definitions.len() >= 64 {
                break;
            }
            let name = self.representative(&name, b)?;
            if self.definitions.contains_key(&name) {
                continue;
            }
            let mut todo = vec![(rhs.clone(), 0usize)];
            let mut visited = HashSet::new();
            let mut occurs = false;
            while let Some((term, depth)) = todo.pop() {
                b.tick(1)?;
                if depth > b.limits.max_depth {
                    return Err("finite definition depth budget exhausted".into());
                }
                if !visited.insert(term.clone()) {
                    continue;
                }
                if visited.len() > b.limits.max_terms {
                    return Err("finite definition term budget exhausted".into());
                }
                if let Op::Variable(variable) = operation(&term)? {
                    let representative = self.representative(&variable, b)?;
                    if representative == name {
                        occurs = true;
                        break;
                    }
                    if let Some(definition) = self.definitions.get(&representative) {
                        todo.push((definition.clone(), depth + 1));
                    }
                }
                for argument in &term.0.args {
                    todo.push((argument.clone(), depth + 1));
                }
            }
            if !occurs {
                self.definitions.insert(name, rhs);
            }
        }
        // Mandatory implication paths yield conditional facts, never aliases.
        // Keep guards as bounded vectors instead of constructing conjunctions.
        let mut pending = vec![(formula.clone(), Vec::<Term>::new(), 0usize)];
        let mut seen = HashSet::new();
        let mut candidates = Vec::new();
        while let Some((term, guard, depth)) = pending.pop() {
            b.tick(1)?;
            if depth > b.limits.max_depth {
                return Err("guarded definition depth exhausted".into());
            }
            if !seen.insert((term.clone(), guard.clone())) {
                continue;
            }
            if seen.len() > b.limits.max_terms {
                return Err("guarded definition terms exhausted".into());
            }
            match operation(&term)? {
                Op::And => {
                    pending.push((term.0.args[1].clone(), guard.clone(), depth + 1));
                    pending.push((term.0.args[0].clone(), guard, depth + 1));
                }
                Op::Implies => {
                    if guard.len() < 16 {
                        let mut guards = guard;
                        guards.push(term.0.args[0].clone());
                        pending.push((term.0.args[1].clone(), guards, depth + 1));
                    }
                }
                Op::Eq if !guard.is_empty() && matches!(term.0.args[0].0.sort, Sort::Bv(_)) => {
                    match (operation(&term.0.args[0])?, operation(&term.0.args[1])?) {
                        (Op::Variable(x), _) => candidates.push((x, guard, term.0.args[1].clone())),
                        (_, Op::Variable(y)) => candidates.push((y, guard, term.0.args[0].clone())),
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        for (name, guard, rhs) in candidates {
            if self.guarded.len() >= 64 {
                break;
            }
            let name = self.representative(&name, b)?;
            if self.definitions.contains_key(&name) || self.guarded.contains_key(&name) {
                continue;
            }
            let mut todo = vec![(rhs.clone(), 0usize)];
            todo.extend(guard.iter().cloned().map(|g| (g, 0)));
            let mut visited = HashSet::new();
            let mut occurs = false;
            while let Some((term, depth)) = todo.pop() {
                b.tick(1)?;
                if depth > b.limits.max_depth {
                    return Err("guarded definition depth exhausted".into());
                }
                if !visited.insert(term.clone()) {
                    continue;
                }
                if visited.len() > b.limits.max_terms {
                    return Err("guarded definition terms exhausted".into());
                }
                if let Op::Variable(variable) = operation(&term)? {
                    let representative = self.representative(&variable, b)?;
                    if representative == name {
                        occurs = true;
                        break;
                    }
                    if let Some(definition) = self.definitions.get(&representative) {
                        todo.push((definition.clone(), depth + 1));
                    }
                    if let Some((g, r)) = self.guarded.get(&representative) {
                        todo.extend(g.iter().cloned().map(|g| (g, depth + 1)));
                        todo.push((r.clone(), depth + 1));
                    }
                }
                for argument in &term.0.args {
                    todo.push((argument.clone(), depth + 1));
                }
            }
            if !occurs {
                self.guarded.insert(name, (guard, rhs));
            }
        }
        Ok(())
    }
}
struct Blast {
    lookup: lookup::Rewrite,
    guarded_created: usize,
    guarded_rewrites: usize,
    aliases: Aliases,
    memo: HashMap<Term, Bits>,
    gates: HashMap<(u8, Lit, Lit), Lit>,
    muxes: HashMap<(Lit, Lit, Lit), Lit>,
    inputs: BTreeMap<String, Bits>,
    vars: usize,
    clauses: Vec<Vec<Lit>>,
}
impl Blast {
    fn new() -> Self {
        Self {
            lookup: lookup::Rewrite::default(),
            guarded_created: 0,
            guarded_rewrites: 0,
            aliases: Aliases::default(),
            memo: HashMap::new(),
            gates: HashMap::new(),
            muxes: HashMap::new(),
            inputs: BTreeMap::new(),
            vars: 1,
            clauses: vec![vec![TRUE]],
        }
    }
    fn fresh(&mut self, b: &mut Budget) -> Res<Lit> {
        b.tick(1)?;
        if self.vars >= b.limits.max_variables || self.vars >= i32::MAX as usize {
            return Err("finite solver variable budget exhausted".into());
        }
        self.vars += 1;
        Ok(self.vars as Lit)
    }
    fn clause(&mut self, c: Vec<Lit>, b: &mut Budget) -> Res<()> {
        b.tick(c.len() as u64)?;
        if self.clauses.len() >= b.limits.max_clauses {
            return Err("finite solver clause budget exhausted".into());
        }
        self.clauses.push(c);
        Ok(())
    }
    fn and(&mut self, mut x: Lit, mut y: Lit, b: &mut Budget) -> Res<Lit> {
        if x == FALSE || y == FALSE || x == -y {
            return Ok(FALSE);
        }
        if x == TRUE {
            return Ok(y);
        }
        if y == TRUE || x == y {
            return Ok(x);
        }
        if x > y {
            std::mem::swap(&mut x, &mut y);
        }
        if let Some(z) = self.gates.get(&(0, x, y)) {
            return Ok(*z);
        }
        let z = self.fresh(b)?;
        self.clause(vec![-z, x], b)?;
        self.clause(vec![-z, y], b)?;
        self.clause(vec![z, -x, -y], b)?;
        self.gates.insert((0, x, y), z);
        Ok(z)
    }
    fn or(&mut self, x: Lit, y: Lit, b: &mut Budget) -> Res<Lit> {
        Ok(-self.and(-x, -y, b)?)
    }
    fn xor(&mut self, mut x: Lit, mut y: Lit, b: &mut Budget) -> Res<Lit> {
        if x == y {
            return Ok(FALSE);
        }
        if x == -y {
            return Ok(TRUE);
        }
        if x == FALSE {
            return Ok(y);
        }
        if y == FALSE {
            return Ok(x);
        }
        if x == TRUE {
            return Ok(-y);
        }
        if y == TRUE {
            return Ok(-x);
        }
        // Canonical positive inputs also share complements of XOR gates.
        let negate = (x < 0) ^ (y < 0);
        x = x.abs();
        y = y.abs();
        if x > y {
            std::mem::swap(&mut x, &mut y);
        }
        let z = if let Some(z) = self.gates.get(&(1, x, y)) {
            *z
        } else {
            let z = self.fresh(b)?;
            self.clause(vec![-x, -y, -z], b)?;
            self.clause(vec![x, y, -z], b)?;
            self.clause(vec![x, -y, z], b)?;
            self.clause(vec![-x, y, z], b)?;
            self.gates.insert((1, x, y), z);
            z
        };
        Ok(if negate { -z } else { z })
    }
    fn mux(&mut self, mut g: Lit, mut x: Lit, mut y: Lit, b: &mut Budget) -> Res<Lit> {
        if g == TRUE || x == y {
            return Ok(x);
        }
        if g == FALSE {
            return Ok(y);
        }
        if x == TRUE {
            return self.or(g, y, b);
        }
        if x == FALSE {
            return self.and(-g, y, b);
        }
        if y == TRUE {
            return self.or(-g, x, b);
        }
        if y == FALSE {
            return self.and(g, x, b);
        }
        if x == -y {
            return self.xor(g, y, b);
        }
        if g < 0 {
            g = -g;
            std::mem::swap(&mut x, &mut y);
        }
        let negate = x < 0;
        if negate {
            x = -x;
            y = -y;
        }
        let z = if let Some(z) = self.muxes.get(&(g, x, y)) {
            *z
        } else {
            let z = self.fresh(b)?;
            self.clause(vec![-g, -x, z], b)?;
            self.clause(vec![-g, x, -z], b)?;
            self.clause(vec![g, -y, z], b)?;
            self.clause(vec![g, y, -z], b)?;
            self.muxes.insert((g, x, y), z);
            z
        };
        Ok(if negate { -z } else { z })
    }
    fn add(&mut self, x: &[Lit], y: &[Lit], mut carry: Lit, b: &mut Budget) -> Res<Vec<Lit>> {
        let mut out = Vec::new();
        for (i, (&a, &c)) in x.iter().zip(y).enumerate() {
            let pair = self.xor(a, c, b)?;
            out.push(self.xor(pair, carry, b)?);
            if i + 1 < x.len() {
                let ac = self.and(a, c, b)?;
                let pc = self.and(pair, carry, b)?;
                carry = self.or(ac, pc, b)?;
            }
        }
        Ok(out)
    }
    fn lt(&mut self, x: &[Lit], y: &[Lit], b: &mut Budget) -> Res<Lit> {
        let mut less = FALSE;
        for (&x, &y) in x.iter().zip(y) {
            let equal = -self.xor(x, y, b)?;
            let lower = self.and(equal, less, b)?;
            let here = self.and(-x, y, b)?;
            less = self.or(here, lower, b)?;
        }
        Ok(less)
    }
    fn substitute_guarded(
        &mut self,
        t: &Term,
        replacements: &HashMap<String, Term>,
        memo: &mut HashMap<Term, Term>,
        b: &mut Budget,
        depth: usize,
    ) -> Res<Term> {
        b.tick(1)?;
        if depth > b.limits.max_depth {
            return Err("guarded rewrite depth exhausted".into());
        }
        if let Some(value) = memo.get(t) {
            return Ok(value.clone());
        }
        if self.guarded_created >= 4096 {
            return Ok(t.clone());
        }
        if memo.len() >= b.limits.max_terms {
            return Err("guarded rewrite term limit exhausted".into());
        }
        if let Op::Variable(name) = operation(t)? {
            let representative = self.aliases.representative(&name, b)?;
            if let Some(value) = replacements.get(&representative) {
                return Ok(value.clone());
            }
            return Ok(t.clone());
        }
        let args =
            t.0.args
                .iter()
                .map(|a| self.substitute_guarded(a, replacements, memo, b, depth + 1))
                .collect::<Res<Vec<_>>>()?;
        let value = if args == t.0.args || !self.guarded_room(b) {
            t.clone()
        } else {
            self.charge_guarded(b)?;
            hwverify_ir::node(t.0.sort.clone(), t.0.op.clone(), args)
        };
        memo.insert(t.clone(), value.clone());
        Ok(value)
    }
    fn guarded_room(&self, b: &Budget) -> bool {
        self.guarded_created < 4096
            && self
                .lookup
                .source_nodes
                .saturating_add(self.lookup.created)
                .saturating_add(1)
                <= b.limits.max_terms
    }
    fn charge_guarded(&mut self, b: &mut Budget) -> Res<()> {
        b.tick(1)?;
        self.guarded_created += 1;
        // This shared source/expansion envelope also constrains lookup rewrites.
        self.lookup.source_nodes += 1;
        Ok(())
    }
    fn guarded_ite(&mut self, t: &Term, b: &mut Budget, depth: usize) -> Res<Option<Term>> {
        if self.aliases.guarded.is_empty() || !self.guarded_room(b) {
            return Ok(None);
        }
        let mut facts = HashSet::new();
        let mut pending = vec![t.0.args[0].clone()];
        while let Some(g) = pending.pop() {
            b.tick(1)?;
            if !facts.insert(g.clone()) {
                continue;
            }
            if facts.len() > 64 {
                return Ok(None);
            }
            if matches!(operation(&g)?, Op::And) {
                pending.extend(g.0.args.iter().cloned());
            }
        }
        let mut replacements = HashMap::new();
        for (name, (guard, rhs)) in &self.aliases.guarded {
            b.tick(1)?;
            if guard.iter().all(|g| facts.contains(g)) {
                replacements.insert(name.clone(), rhs.clone());
            }
        }
        if replacements.is_empty() {
            return Ok(None);
        }
        let yes = self.substitute_guarded(
            &t.0.args[1],
            &replacements,
            &mut HashMap::new(),
            b,
            depth + 1,
        )?;
        if yes == t.0.args[1] || !self.guarded_room(b) {
            return Ok(None);
        }
        self.charge_guarded(b)?;
        self.guarded_rewrites += 1;
        Ok(Some(hwverify_ir::ite(
            t.0.args[0].clone(),
            yes,
            t.0.args[2].clone(),
        )))
    }
    fn term(&mut self, t: &Term, b: &mut Budget, depth: usize) -> Res<Bits> {
        b.tick(1)?;
        if depth > b.limits.max_depth {
            return Err("finite solver term depth budget exhausted".into());
        }
        if let Some(v) = self.memo.get(t) {
            return Ok(v.clone());
        }
        if self.memo.len() >= b.limits.max_terms {
            return Err("finite solver term budget exhausted".into());
        }
        let normalized = if matches!(t.0.sort, Sort::Bv(_)) {
            self.lookup.normalize(t, b, depth)?
        } else {
            t.clone()
        };
        if normalized != *t {
            let value = self.term(&normalized, b, depth + 1)?;
            self.memo.insert(t.clone(), value.clone());
            return Ok(value);
        }
        let op = operation(t)?;
        if matches!(op, Op::Ite) {
            if let Some(rewritten) = self.guarded_ite(t, b, depth)? {
                let value = self.term(&rewritten, b, depth + 1)?;
                self.memo.insert(t.clone(), value.clone());
                return Ok(value);
            }
        }
        if matches!(op, Op::Ite) && matches!(t.0.sort, Sort::Bv(_)) {
            if let Some(rewritten) = self.lookup.distribute(t, b)? {
                let value = self.term(&rewritten, b, depth + 1)?;
                self.memo.insert(t.clone(), value.clone());
                return Ok(value);
            }
        }
        let args =
            t.0.args
                .iter()
                .map(|t| self.term(t, b, depth + 1))
                .collect::<Res<Vec<_>>>()?;
        let w = match t.0.sort {
            Sort::Bool => 1,
            Sort::Bv(w) => w as usize,
            _ => unreachable!(),
        };
        let bits = match op {
            Op::Variable(n) => {
                let representative = self.aliases.representative(&n, b)?;
                let value = if let Some(v) = self.inputs.get(&representative) {
                    if v.sort != t.0.sort {
                        return Err(format!("finite variable {n} has inconsistent sorts"));
                    }
                    v.clone()
                } else if let Some(definition) =
                    self.aliases.definitions.get(&representative).cloned()
                {
                    let value = self.term(&definition, b, depth + 1)?;
                    if value.sort != t.0.sort {
                        return Err(format!("finite definition {n} has inconsistent sorts"));
                    }
                    self.inputs.insert(representative, value.clone());
                    value
                } else {
                    let value = Bits {
                        sort: t.0.sort.clone(),
                        bits: (0..w).map(|_| self.fresh(b)).collect::<Res<Vec<_>>>()?,
                    };
                    self.inputs.insert(representative, value.clone());
                    value
                };
                if let Some(previous) = self.inputs.insert(n.clone(), value.clone()) {
                    if previous.sort != value.sort {
                        return Err(format!("finite variable {n} has inconsistent sorts"));
                    }
                }
                value.bits
            }
            Op::Bool(v) => vec![if v { TRUE } else { FALSE }],
            Op::Word(v) => (0..w)
                .map(|i| if v & (1u64 << i) != 0 { TRUE } else { FALSE })
                .collect(),
            Op::Not | Op::BNot => args[0].bits.iter().map(|x| -x).collect(),
            Op::And | Op::BAnd | Op::Or | Op::BOr | Op::Xor | Op::BXor | Op::Implies => args[0]
                .bits
                .iter()
                .zip(&args[1].bits)
                .map(|(&x, &y)| match op {
                    Op::And | Op::BAnd => self.and(x, y, b),
                    Op::Or | Op::BOr => self.or(x, y, b),
                    Op::Implies => self.or(-x, y, b),
                    _ => self.xor(x, y, b),
                })
                .collect::<Res<Vec<_>>>()?,
            Op::Eq => {
                let mut equal = TRUE;
                for (&x, &y) in args[0].bits.iter().zip(&args[1].bits) {
                    let same = -self.xor(x, y, b)?;
                    equal = self.and(equal, same, b)?;
                }
                vec![equal]
            }
            Op::Ite => args[1]
                .bits
                .iter()
                .zip(&args[2].bits)
                .map(|(&x, &y)| self.mux(args[0].bits[0], x, y, b))
                .collect::<Res<Vec<_>>>()?,
            Op::Add => self.add(&args[0].bits, &args[1].bits, FALSE, b)?,
            Op::Sub => self.add(
                &args[0].bits,
                &args[1].bits.iter().map(|x| -x).collect::<Vec<_>>(),
                TRUE,
                b,
            )?,
            Op::Mul => {
                let mut sum = vec![FALSE; w];
                for (i, &y) in args[1].bits.iter().enumerate() {
                    let mut row = vec![FALSE; w];
                    for (j, bit) in row.iter_mut().enumerate().skip(i) {
                        *bit = self.and(args[0].bits[j - i], y, b)?;
                    }
                    sum = self.add(&sum, &row, FALSE, b)?;
                }
                sum
            }
            Op::Shl | Op::LShr => {
                let mut value = args[0].bits.clone();
                for (i, &shift) in args[1].bits.iter().enumerate() {
                    let amount = 1usize.checked_shl(i as u32).unwrap_or(usize::MAX);
                    let mut next = vec![FALSE; w];
                    for j in 0..w {
                        let from = if matches!(op, Op::Shl) {
                            j.checked_sub(amount)
                        } else {
                            j.checked_add(amount).filter(|k| *k < w)
                        };
                        next[j] =
                            self.mux(shift, from.map(|k| value[k]).unwrap_or(FALSE), value[j], b)?;
                    }
                    value = next;
                }
                value
            }
            Op::Ult | Op::Ule | Op::Slt | Op::Sle => {
                let mut x = args[0].bits.clone();
                let mut y = args[1].bits.clone();
                if matches!(op, Op::Slt | Op::Sle) {
                    *x.last_mut().unwrap() *= -1;
                    *y.last_mut().unwrap() *= -1;
                }
                vec![if matches!(op, Op::Ule | Op::Sle) {
                    -self.lt(&y, &x, b)?
                } else {
                    self.lt(&x, &y, b)?
                }]
            }
            Op::Concat => args[1].bits.iter().chain(&args[0].bits).copied().collect(),
            Op::Extract(hi, lo) => args[0].bits[lo as usize..=hi as usize].to_vec(),
            Op::Zext | Op::Sext => {
                let mut x = args[0].bits.clone();
                let high = if matches!(op, Op::Sext) {
                    *x.last().unwrap()
                } else {
                    FALSE
                };
                x.resize(w, high);
                x
            }
        };
        if bits.len() != w {
            return Err("finite encoder width mismatch".into());
        }
        if self.memo.len() >= b.limits.max_terms {
            return Err("finite solver term budget exhausted".into());
        }
        let value = Bits {
            sort: t.0.sort.clone(),
            bits,
        };
        self.memo.insert(t.clone(), value.clone());
        Ok(value)
    }
}

/// Independent word-level interpretation: does not inspect CNF or gate values.
fn evaluate(
    t: &Term,
    inputs: &BTreeMap<String, Scalar>,
    memo: &mut HashMap<Term, Scalar>,
    b: &mut Budget,
    depth: usize,
) -> Res<Scalar> {
    b.tick(1)?;
    if depth > b.limits.max_depth {
        return Err("finite witness depth budget exhausted".into());
    }
    if let Some(v) = memo.get(t) {
        return Ok(v.clone());
    }
    let op = operation(t)?;
    let a =
        t.0.args
            .iter()
            .map(|t| evaluate(t, inputs, memo, b, depth + 1))
            .collect::<Res<Vec<_>>>()?;
    let word = |value: u64| match t.0.sort {
        Sort::Bv(width) => Scalar::Bv {
            width,
            value: value & mask(width),
        },
        _ => unreachable!(),
    };
    let value = match op {
        Op::Variable(n) => inputs
            .get(&n)
            .cloned()
            .ok_or(format!("finite witness missing {n}"))?,
        Op::Bool(v) => Scalar::Bool(v),
        Op::Word(v) => word(v),
        Op::Not => Scalar::Bool(!a[0].boolean()),
        Op::And => Scalar::Bool(a[0].boolean() && a[1].boolean()),
        Op::Or => Scalar::Bool(a[0].boolean() || a[1].boolean()),
        Op::Xor => Scalar::Bool(a[0].boolean() ^ a[1].boolean()),
        Op::Implies => Scalar::Bool(!a[0].boolean() || a[1].boolean()),
        Op::Eq => Scalar::Bool(a[0] == a[1]),
        Op::Ite => a[if a[0].boolean() { 1 } else { 2 }].clone(),
        Op::BNot => word(!a[0].word()),
        Op::BAnd => word(a[0].word() & a[1].word()),
        Op::BOr => word(a[0].word() | a[1].word()),
        Op::BXor => word(a[0].word() ^ a[1].word()),
        Op::Add => word(a[0].word().wrapping_add(a[1].word())),
        Op::Sub => word(a[0].word().wrapping_sub(a[1].word())),
        Op::Mul => word(a[0].word().wrapping_mul(a[1].word())),
        Op::Shl => word(if a[1].word() >= 64 {
            0
        } else {
            a[0].word() << a[1].word()
        }),
        Op::LShr => word(if a[1].word() >= 64 {
            0
        } else {
            a[0].word() >> a[1].word()
        }),
        Op::Ult => Scalar::Bool(a[0].word() < a[1].word()),
        Op::Ule => Scalar::Bool(a[0].word() <= a[1].word()),
        Op::Slt | Op::Sle => {
            let Sort::Bv(w) = a[0].sort() else {
                unreachable!()
            };
            let signed = |x: u64| {
                if w == 64 {
                    x as i64
                } else {
                    ((x << (64 - w)) as i64) >> (64 - w)
                }
            };
            let (x, y) = (signed(a[0].word()), signed(a[1].word()));
            Scalar::Bool(if matches!(op, Op::Slt) { x < y } else { x <= y })
        }
        Op::Concat => {
            let Sort::Bv(w) = a[1].sort() else {
                unreachable!()
            };
            word((a[0].word() << w) | a[1].word())
        }
        Op::Extract(_, lo) => word(a[0].word() >> lo),
        Op::Zext => word(a[0].word()),
        Op::Sext => {
            let Sort::Bv(w) = a[0].sort() else {
                unreachable!()
            };
            let x = a[0].word();
            word(if x & (1u64 << (w - 1)) != 0 {
                x | !mask(w)
            } else {
                x
            })
        }
    };
    if value.sort() != t.0.sort {
        return Err("finite witness type mismatch".into());
    }
    memo.insert(t.clone(), value.clone());
    Ok(value)
}

/// Indexed activity heap. Equal scores prefer the lowest variable number, just
/// like the original linear scan. Assigned entries may remain until popped;
/// backtracking reinserts every newly unassigned variable if it was removed.
struct VarOrder {
    heap: Vec<usize>,
    positions: Vec<usize>,
}
impl VarOrder {
    const ABSENT: usize = usize::MAX;

    fn new(vars: usize) -> Self {
        Self {
            heap: Vec::with_capacity(vars),
            positions: vec![Self::ABSENT; vars + 1],
        }
    }
    fn higher(a: usize, c: usize, activity: &[f64]) -> bool {
        activity[a] > activity[c] || (activity[a] == activity[c] && a < c)
    }
    fn swap(&mut self, a: usize, c: usize) {
        self.heap.swap(a, c);
        self.positions[self.heap[a]] = a;
        self.positions[self.heap[c]] = c;
    }
    fn promote(&mut self, v: usize, activity: &[f64], b: &mut Budget) -> Res<()> {
        let mut pos = self.positions[v];
        if pos == Self::ABSENT {
            return Ok(());
        }
        while pos > 0 {
            b.tick(1)?;
            let parent = (pos - 1) / 2;
            if !Self::higher(v, self.heap[parent], activity) {
                break;
            }
            self.swap(pos, parent);
            pos = parent;
        }
        Ok(())
    }
    fn insert(&mut self, v: usize, activity: &[f64], b: &mut Budget) -> Res<()> {
        b.tick(1)?;
        if self.positions[v] == Self::ABSENT {
            self.positions[v] = self.heap.len();
            self.heap.push(v);
            self.promote(v, activity, b)?;
        }
        Ok(())
    }
    fn pop(&mut self, activity: &[f64], b: &mut Budget) -> Res<Option<usize>> {
        b.tick(1)?;
        let Some(&v) = self.heap.first() else {
            return Ok(None);
        };
        let last = self.heap.pop().unwrap();
        self.positions[v] = Self::ABSENT;
        if !self.heap.is_empty() {
            self.heap[0] = last;
            self.positions[last] = 0;
            let mut pos = 0;
            while 2 * pos + 1 < self.heap.len() {
                b.tick(1)?;
                let left = 2 * pos + 1;
                let right = left + 1;
                let child = if right < self.heap.len()
                    && Self::higher(self.heap[right], self.heap[left], activity)
                {
                    right
                } else {
                    left
                };
                if !Self::higher(self.heap[child], last, activity) {
                    break;
                }
                self.swap(pos, child);
                pos = child;
            }
        }
        Ok(Some(v))
    }
    fn rebuild(&mut self, activity: &[f64], b: &mut Budget) -> Res<()> {
        // Rescaling can round two nearby scores to a tie. Restore the exact
        // activity/variable-id order even in that case.
        let previous = std::mem::take(&mut self.heap);
        self.positions.fill(Self::ABSENT);
        for v in previous {
            self.insert(v, activity, b)?;
        }
        Ok(())
    }
}

/// A cooperative scheduling yield is distinct from a resource error/UNKNOWN.
#[derive(Debug, PartialEq, Eq)]
enum Search {
    Complete(Verdict),
    Pending,
}

/// Two-watched-literal CDCL. First-UIP learned clauses are resolution consequences
/// of existing clauses; only a conflict at decision level zero establishes UNSAT.
struct Sat {
    initialized: bool,
    previous_clauses: usize,
    clauses: Vec<Vec<Lit>>,
    watches: Vec<Vec<usize>>,
    values: Vec<i8>,
    levels: Vec<usize>,
    reasons: Vec<Option<usize>>,
    trail: Vec<Lit>,
    starts: Vec<usize>,
    head: usize,
    activity: Vec<f64>,
    order: VarOrder,
    increment: f64,
    phase: Vec<bool>,
    decisions: u64,
    conflicts: u64,
}
fn index(lit: Lit) -> usize {
    2 * lit.unsigned_abs() as usize + usize::from(lit < 0)
}
fn truth(values: &[i8], lit: Lit) -> i8 {
    values[lit.unsigned_abs() as usize] * if lit > 0 { 1 } else { -1 }
}
impl Sat {
    fn new(vars: usize, clauses: Vec<Vec<Lit>>) -> Self {
        Self {
            initialized: false,
            previous_clauses: 0,
            clauses,
            watches: vec![vec![]; 2 * (vars + 1)],
            values: vec![0; vars + 1],
            levels: vec![0; vars + 1],
            reasons: vec![None; vars + 1],
            trail: vec![],
            starts: vec![],
            head: 0,
            activity: vec![0.; vars + 1],
            order: VarOrder::new(vars),
            increment: 1.,
            phase: vec![false; vars + 1],
            decisions: 0,
            conflicts: 0,
        }
    }
    fn enqueue(&mut self, p: Lit, reason: Option<usize>) -> bool {
        let v = p.unsigned_abs() as usize;
        if self.values[v] != 0 {
            return truth(&self.values, p) > 0;
        }
        self.values[v] = if p > 0 { 1 } else { -1 };
        self.levels[v] = self.starts.len();
        self.reasons[v] = reason;
        self.trail.push(p);
        true
    }
    fn attach(&mut self, id: usize) {
        if self.clauses[id].len() >= 2 {
            self.watches[index(self.clauses[id][0])].push(id);
            self.watches[index(self.clauses[id][1])].push(id);
        }
    }
    fn propagate(&mut self, b: &mut Budget) -> Res<Option<usize>> {
        while self.head < self.trail.len() {
            let p = self.trail[self.head];
            self.head += 1;
            let watch = index(-p);
            // Compact retained watches in the existing allocation. Replacement
            // watches move to other lists; no new watch can target the currently
            // false literal because replacement literals must be non-false.
            let mut pending = std::mem::take(&mut self.watches[watch]);
            let mut retained = 0;
            for pos in 0..pending.len() {
                b.tick(1)?;
                let id = pending[pos];
                let c = &mut self.clauses[id];
                if c[0] == -p {
                    c.swap(0, 1);
                }
                if c[1] != -p {
                    return Err("finite SAT watch invariant violated".into());
                }
                if truth(&self.values, c[0]) > 0 {
                    pending[retained] = id;
                    retained += 1;
                    continue;
                }
                let mut replacement = None;
                for (k, &lit) in c.iter().enumerate().skip(2) {
                    b.tick(1)?;
                    if truth(&self.values, lit) >= 0 {
                        replacement = Some(k);
                        break;
                    }
                }
                if let Some(k) = replacement {
                    c.swap(1, k);
                    self.watches[index(c[1])].push(id);
                    continue;
                }
                let other = c[0];
                pending[retained] = id;
                retained += 1;
                if !self.enqueue(other, Some(id)) {
                    let unprocessed = pending.len() - pos - 1;
                    pending.copy_within(pos + 1.., retained);
                    pending.truncate(retained + unprocessed);
                    self.watches[watch] = pending;
                    return Ok(Some(id));
                }
            }
            pending.truncate(retained);
            self.watches[watch] = pending;
        }
        Ok(None)
    }
    fn backtrack(&mut self, level: usize, b: &mut Budget) -> Res<()> {
        if self.starts.len() <= level {
            return Ok(());
        }
        let keep = self.starts[level];
        for &p in self.trail[keep..].iter().rev() {
            let v = p.unsigned_abs() as usize;
            self.phase[v] = p > 0;
            self.values[v] = 0;
            self.reasons[v] = None;
            self.levels[v] = 0;
            self.order.insert(v, &self.activity, b)?;
        }
        self.trail.truncate(keep);
        self.starts.truncate(level);
        self.head = self.head.min(keep);
        Ok(())
    }
    fn analyze(&mut self, conflict: usize, b: &mut Budget) -> Res<(Vec<Lit>, usize)> {
        let mut learned = vec![0];
        let mut seen = vec![false; self.values.len()];
        let mut unresolved = 0usize;
        let mut cursor = self.trail.len();
        let mut id = conflict;
        let mut resolved = None;
        loop {
            for &q in &self.clauses[id] {
                b.tick(1)?;
                let v = q.unsigned_abs() as usize;
                if Some(v) == resolved || seen[v] || self.levels[v] == 0 {
                    continue;
                }
                seen[v] = true;
                self.activity[v] += self.increment;
                self.order.promote(v, &self.activity, b)?;
                if self.levels[v] == self.starts.len() {
                    unresolved += 1;
                } else {
                    learned.push(q);
                }
            }
            let p = loop {
                if cursor == 0 {
                    return Err("finite SAT conflict analysis invariant violated".into());
                }
                cursor -= 1;
                let p = self.trail[cursor];
                if seen[p.unsigned_abs() as usize] {
                    break p;
                }
            };
            let v = p.unsigned_abs() as usize;
            seen[v] = false;
            unresolved = unresolved
                .checked_sub(1)
                .ok_or("finite SAT conflict path invariant violated")?;
            if unresolved == 0 {
                learned[0] = -p;
                break;
            }
            id = self.reasons[v].ok_or("finite SAT missing implication reason")?;
            resolved = Some(v);
        }
        let backtrack = learned
            .iter()
            .skip(1)
            .map(|q| self.levels[q.unsigned_abs() as usize])
            .max()
            .unwrap_or(0);
        // Keeping the next-highest-level literal watched avoids delayed propagation.
        if learned.len() > 2 {
            let k = (1..learned.len())
                .max_by_key(|&i| self.levels[learned[i].unsigned_abs() as usize])
                .unwrap();
            learned.swap(1, k);
        }
        self.increment /= 0.95;
        if self.increment > 1e100 {
            for x in &mut self.activity {
                *x *= 1e-100;
            }
            self.increment *= 1e-100;
            self.order.rebuild(&self.activity, b)?;
        }
        Ok((learned, backtrack))
    }
    fn run_slice(&mut self, b: &mut Budget, quantum: u64) -> Res<Search> {
        let stop_work = b.work.saturating_add(quantum.max(1));
        if !self.initialized {
            for id in 0..self.clauses.len() {
                b.tick(1)?;
                for &p in &self.clauses[id] {
                    self.activity[p.unsigned_abs() as usize] += 1.;
                }
                if self.clauses[id].is_empty() {
                    return Ok(Search::Complete(Verdict::Unsat));
                }
                if self.clauses[id].len() == 1 && !self.enqueue(self.clauses[id][0], Some(id)) {
                    return Ok(Search::Complete(Verdict::Unsat));
                }
                self.attach(id);
            }
            for v in 1..self.values.len() {
                self.order.insert(v, &self.activity, b)?;
            }
            self.initialized = true;
        }
        loop {
            // Yield only here: propagation, conflict analysis, backtracking and
            // clause insertion have all finished. A Budget error is terminal;
            // it may interrupt a mutation and is NEVER used as a resumable yield.
            if b.work >= stop_work {
                return Ok(Search::Pending);
            }
            if let Some(conflict) = self.propagate(b)? {
                self.conflicts += 1;
                if self.starts.is_empty() {
                    return Ok(Search::Complete(Verdict::Unsat));
                }
                let (learned, level) = self.analyze(conflict, b)?;
                self.backtrack(level, b)?;
                if self.previous_clauses.saturating_add(self.clauses.len()) >= b.limits.max_clauses
                {
                    return Err("finite SAT learned-clause budget exhausted".into());
                }
                let p = learned[0];
                let id = self.clauses.len();
                self.clauses.push(learned);
                self.attach(id);
                if !self.enqueue(p, Some(id)) {
                    return Err("finite SAT asserting clause invariant violated".into());
                }
            } else {
                let v = loop {
                    let Some(v) = self.order.pop(&self.activity, b)? else {
                        #[cfg(test)]
                        assert!(self.values.iter().skip(1).all(|&x| x != 0));
                        return Ok(Search::Complete(Verdict::Sat));
                    };
                    if self.values[v] == 0 {
                        break v;
                    }
                };
                // Every SAT unit test also checks equivalence with the original
                // linear selector, including lazy removals and backtracking.
                #[cfg(test)]
                {
                    let expected = (1..self.values.len())
                        .filter(|&x| self.values[x] == 0)
                        .reduce(|a, c| {
                            if VarOrder::higher(a, c, &self.activity) {
                                a
                            } else {
                                c
                            }
                        });
                    assert_eq!(Some(v), expected);
                }
                self.decisions += 1;
                self.starts.push(self.trail.len());
                self.enqueue(if self.phase[v] { v as Lit } else { -(v as Lit) }, None);
            }
        }
    }
    #[cfg(test)]
    fn run(&mut self, b: &mut Budget) -> Res<Verdict> {
        match self.run_slice(b, u64::MAX)? {
            Search::Complete(verdict) => Ok(verdict),
            Search::Pending => Err("finite SAT unresolved search".into()),
        }
    }
}

/// Pick a disjunctive factor whose alternatives cover the asserted formula.
/// Every branch keeps the complete original CNF and adds one alternative as a
/// unit assumption. Branches share a single work/time budget.
fn split_choices(formula: &Term, blast: &Blast, b: &mut Budget) -> Res<Vec<Lit>> {
    let mut conjuncts = vec![formula];
    let mut visited_conjuncts = HashSet::new();
    let mut best = Vec::new();
    while let Some(term) = conjuncts.pop() {
        b.tick(1)?;
        if !visited_conjuncts.insert(term.clone()) {
            continue;
        }
        if term.0.op == "and" {
            conjuncts.extend(&term.0.args);
            continue;
        }
        let mut alternatives = vec![(term, false)];
        let mut visited_alternatives = HashSet::new();
        let mut choices = Vec::new();
        let mut seen = HashSet::new();
        while let Some((part, negative)) = alternatives.pop() {
            b.tick(1)?;
            if !visited_alternatives.insert((part.clone(), negative)) {
                continue;
            }
            if part.0.op == "not" {
                alternatives.push((&part.0.args[0], !negative));
            } else if (!negative && part.0.op == "or") || (negative && part.0.op == "and") {
                alternatives.push((&part.0.args[1], negative));
                alternatives.push((&part.0.args[0], negative));
            } else {
                let lit = blast.memo[part].bits[0] * if negative { -1 } else { 1 };
                if lit != FALSE && seen.insert(lit) {
                    choices.push(lit);
                }
            }
        }
        if !choices.contains(&TRUE) && choices.len() > best.len() {
            best = choices;
        }
    }
    if best.len() < 2 {
        best.clear();
    }
    Ok(best)
}

/// Compatibility entry point: use proof-oriented search, as in 0.11.2.
/// SAT is still returned and validated normally, even though the hint is UNSAT.
pub fn solve(formula: &Term, context: &Env, limits: Limits) -> Outcome {
    solve_with_hint(formula, context, limits, SearchHint::Unsat)
}

/// Decide the unchanged formula under a caller-selected search-order hint.
/// Both hints share the same support checks, limits, and SAT witness validation.
pub fn solve_with_hint(
    formula: &Term,
    context: &Env,
    limits: Limits,
    search_hint: SearchHint,
) -> Outcome {
    if formula.contains_memory() || context.values().any(Term::contains_memory) {
        return readonly::solve(formula, context, limits, search_hint);
    }
    solve_scalar(formula, context, limits, search_hint)
}
fn solve_scalar(formula: &Term, context: &Env, limits: Limits, search_hint: SearchHint) -> Outcome {
    if search_hint == SearchHint::Unsat {
        control::route(formula, context, limits, search_hint, false)
    } else {
        solve_plain(formula, context, limits, search_hint)
    }
}
fn solve_plain(formula: &Term, context: &Env, limits: Limits, search_hint: SearchHint) -> Outcome {
    let mut outcome = Outcome {
        search_hint,
        search_strategy: SearchStrategy::NotStarted,
        verdict: Verdict::Unknown,
        reason: None,
        assignments: BTreeMap::new(),
        context_values: BTreeMap::new(),
        array_assignments: BTreeMap::new(),
        array_context_values: BTreeMap::new(),
        original_formula_validated: false,
        stats: Stats::default(),
    };
    let mut budget = Budget {
        limits,
        start: Instant::now(),
        work: 0,
        time_check_in: 0,
    };
    let mut blast = Blast::new();
    let compile = (|| -> Res<()> {
        budget.tick(1)?;
        if budget.limits.max_variables < 1 || budget.limits.max_clauses < 1 {
            return Err("finite solver initial CNF budget exhausted".into());
        }
        if formula.0.sort != Sort::Bool {
            return Err("finite formula must have Bool sort".into());
        }
        // Rewriting can bypass source nodes. Validate every original formula
        // and context signature first, including dead branches and Sat hints.
        blast.lookup.source_nodes = lookup::validate_source(formula, context, &mut budget)?;
        blast.aliases.collect(formula, &mut budget)?;
        let root = blast.term(formula, &mut budget, 0)?;
        for term in context.values() {
            blast.term(term, &mut budget, 0)?;
        }
        // Exact normalization may erase the last occurrence of a variable.
        // Materialize missing ORIGINAL inputs after normal blasting, preserving
        // the existing search order while keeping original SAT replay complete.
        let mut source = vec![formula];
        source.extend(context.values());
        let mut seen = HashSet::new();
        let mut variables = BTreeMap::new();
        while let Some(term) = source.pop() {
            budget.tick(1)?;
            if !seen.insert(term.clone()) {
                continue;
            }
            if let Some(name) = term.0.op.strip_prefix('@') {
                if !blast.inputs.contains_key(name) {
                    variables.insert(name.to_string(), term.clone());
                }
            }
            source.extend(&term.0.args);
        }
        for term in variables.values() {
            blast.term(term, &mut budget, 0)?;
        }
        blast.clause(vec![root.bits[0]], &mut budget)?;
        Ok(())
    })();
    outcome.stats.asserted_definitions = blast.aliases.definitions.len();
    outcome.stats.guarded_equalities = blast.aliases.guarded.len();
    outcome.stats.guarded_rewrites = blast.guarded_rewrites;
    outcome.stats.guarded_expansion_nodes = blast.guarded_created;
    outcome.stats.lookup_rewrites = blast.lookup.rewrites;
    outcome.stats.word_rewrites = blast.lookup.word_rewrites;
    outcome.stats.word_expansion_nodes = blast.lookup.word_created;
    outcome.stats.lookup_expansion_nodes = blast.lookup.created - blast.lookup.word_created;
    outcome.stats.terms = blast.memo.len();
    outcome.stats.variables = blast.vars;
    outcome.stats.clauses = blast.clauses.len();
    outcome.stats.base_clauses = blast.clauses.len();
    outcome.stats.peak_live_clauses = blast.clauses.len();
    if let Err(reason) = compile {
        outcome.reason = Some(reason);
        outcome.stats.work = budget.work;
        return outcome;
    }
    let choices = match split_choices(formula, &blast, &mut budget) {
        Ok(choices) => choices,
        Err(reason) => {
            outcome.reason = Some(reason);
            outcome.stats.work = budget.work;
            return outcome;
        }
    };
    let mut original_clauses = std::mem::take(&mut blast.clauses);
    let mut base_clauses = original_clauses.len();
    let mut sat = Sat::new(blast.vars, Vec::new());
    let mut cumulative_clauses = base_clauses;
    outcome.stats.split_alternatives = choices.len();
    outcome.search_strategy = if choices.is_empty() {
        SearchStrategy::SingleSearch
    } else if search_hint == SearchHint::Unsat {
        SearchStrategy::ProofDecomposition
    } else {
        SearchStrategy::CounterexampleProbeFair
    };
    let result = (|| -> Res<Verdict> {
        let mut probe_verdict = None;
        if !choices.is_empty() && search_hint == SearchHint::Sat {
            // Probe the original query for a quick witness, scaled to its CNF
            // size and capped at a tenth of the DEFAULT whole-query work limit.
            // This fixed scheduling quantum does not change with user limits;
            // reducing a limit must not change the search prefix. Every operation
            // still charges the actual global Budget. Like all slices, it may
            // finish its current atomic search step beyond the local quantum.
            let probe_work = (base_clauses as u64).saturating_mul(512).min(10_000_000);
            let before_work = budget.work;
            sat.clauses = std::mem::take(&mut original_clauses);
            outcome.stats.search_slices += 1;
            let result = sat.run_slice(&mut budget, probe_work);
            outcome.stats.probe_work = budget.work - before_work;
            cumulative_clauses = sat.clauses.len();
            outcome.stats.peak_live_clauses = cumulative_clauses;
            outcome.stats.decisions = sat.decisions;
            outcome.stats.conflicts = sat.conflicts;
            match result? {
                Search::Complete(verdict) => {
                    outcome.stats.probe_result = Some(verdict);
                    probe_verdict = Some(verdict);
                }
                Search::Pending => {
                    outcome.stats.search_yields += 1;
                    outcome.stats.probe_result = Some(Verdict::Unknown);
                    // The probe had NO branch assumption, so all of its learned
                    // clauses follow from the original CNF. Reuse these clauses
                    // in every branch. Branch-local learning is never shared.
                    original_clauses = std::mem::take(&mut sat.clauses);
                    base_clauses = original_clauses.len();
                    sat = Sat::new(blast.vars, Vec::new());
                }
            }
        }
        let verdict = if let Some(verdict) = probe_verdict {
            verdict
        } else if choices.is_empty() {
            // Preserve the original no-copy, single-search path.
            sat.clauses = original_clauses;
            outcome.stats.search_slices += 1;
            let result = sat.run_slice(&mut budget, u64::MAX);
            cumulative_clauses = sat.clauses.len();
            outcome.stats.peak_live_clauses = cumulative_clauses;
            outcome.stats.decisions = sat.decisions;
            outcome.stats.conflicts = sat.conflicts;
            match result? {
                Search::Complete(verdict) => verdict,
                Search::Pending => return Err("finite solver unresolved search".into()),
            }
        } else if search_hint == SearchHint::Unsat {
            // Sequential proof splits keep one immutable base and reuse one
            // working allocation. Restore literal order exactly, so this does
            // not silently alter the search heuristic. Drop the previous unit
            // and ALL branch-local learning before resetting search state.
            outcome.stats.base_cnf_reused = true;
            let mut verdict = Verdict::Unsat;
            for &choice in &choices {
                let first = sat.clauses.is_empty();
                let additional = if first { base_clauses + 1 } else { 1 };
                let next_count = cumulative_clauses
                    .checked_add(additional)
                    .ok_or("finite solver split clause count overflow")?;
                if next_count > budget.limits.max_clauses {
                    return Err("finite solver aggregate split clause budget exhausted".into());
                }
                for clause in &original_clauses {
                    budget.tick(clause.len() as u64)?;
                }
                budget.tick(1)?;
                let mut clauses = if first {
                    original_clauses.clone()
                } else {
                    let mut clauses = std::mem::take(&mut sat.clauses);
                    clauses.truncate(base_clauses);
                    for (working, original) in clauses.iter_mut().zip(&original_clauses) {
                        working.copy_from_slice(original);
                    }
                    clauses
                };
                debug_assert_eq!(clauses.len(), base_clauses);
                clauses.push(vec![choice]);
                sat = Sat::new(blast.vars, clauses);
                // Retain cumulative charges for earlier units/learned clauses,
                // including discarded ones. Each of the two base allocations
                // is charged once; restoring existing buffers allocates none.
                sat.previous_clauses = next_count - sat.clauses.len();
                outcome.stats.search_slices += 1;
                let result = sat.run_slice(&mut budget, u64::MAX);
                cumulative_clauses = sat.previous_clauses + sat.clauses.len();
                outcome.stats.peak_live_clauses = outcome
                    .stats
                    .peak_live_clauses
                    .max(base_clauses + sat.clauses.len());
                outcome.stats.decisions += sat.decisions;
                outcome.stats.conflicts += sat.conflicts;
                match result? {
                    Search::Complete(Verdict::Sat) => {
                        outcome.stats.split_completed += 1;
                        verdict = Verdict::Sat;
                        break;
                    }
                    Search::Complete(Verdict::Unsat) => {
                        outcome.stats.split_completed += 1;
                        outcome.stats.split_unsat += 1;
                    }
                    _ => return Err("finite solver unresolved split branch".into()),
                }
            }
            verdict
        } else {
            // Start every alternative with a small quantum, then double it on
            // each round. A late SAT alternative need not wait for preceding
            // hard UNSAT proofs. Every suspended branch keeps its exact CDCL
            // state; no learning, phase saving or search work is discarded.
            let mut branches: Vec<Option<Sat>> = (0..choices.len()).map(|_| None).collect();
            let mut completed = vec![false; choices.len()];
            let mut quantum = 50_000u64;
            let mut live_clauses = base_clauses;
            'rounds: loop {
                for (i, &choice) in choices.iter().enumerate() {
                    if completed[i] {
                        continue;
                    }
                    budget.tick(1)?;
                    if branches[i].is_none() {
                        // Count the retained base, each materialized copy and
                        // unit, and every learned clause, including discarded
                        // completed branches. Never reset the query allowance.
                        let additional = base_clauses
                            .checked_add(1)
                            .ok_or("finite solver split clause count overflow")?;
                        let next_count = cumulative_clauses
                            .checked_add(additional)
                            .ok_or("finite solver split clause count overflow")?;
                        if next_count > budget.limits.max_clauses {
                            return Err(
                                "finite solver aggregate split clause budget exhausted".into()
                            );
                        }
                        for clause in &original_clauses {
                            budget.tick(clause.len() as u64)?;
                        }
                        budget.tick(1)?;
                        let mut clauses = original_clauses.clone();
                        clauses.push(vec![choice]);
                        cumulative_clauses = next_count;
                        live_clauses += additional;
                        outcome.stats.peak_live_clauses =
                            outcome.stats.peak_live_clauses.max(live_clauses);
                        branches[i] = Some(Sat::new(blast.vars, clauses));
                    }
                    let branch = branches[i].as_mut().unwrap();
                    let before_clauses = branch.clauses.len();
                    let before_decisions = branch.decisions;
                    let before_conflicts = branch.conflicts;
                    // All other current and discarded branch clauses remain
                    // charged while this branch learns additional clauses.
                    branch.previous_clauses = cumulative_clauses - before_clauses;
                    outcome.stats.search_slices += 1;
                    let result = branch.run_slice(&mut budget, quantum);
                    let learned = branch.clauses.len() - before_clauses;
                    cumulative_clauses += learned;
                    live_clauses += learned;
                    outcome.stats.peak_live_clauses =
                        outcome.stats.peak_live_clauses.max(live_clauses);
                    outcome.stats.decisions += branch.decisions - before_decisions;
                    outcome.stats.conflicts += branch.conflicts - before_conflicts;
                    match result? {
                        Search::Complete(Verdict::Sat) => {
                            outcome.stats.split_completed += 1;
                            sat = branches[i].take().unwrap();
                            break 'rounds Verdict::Sat;
                        }
                        Search::Complete(Verdict::Unsat) => {
                            outcome.stats.split_completed += 1;
                            outcome.stats.split_unsat += 1;
                            completed[i] = true;
                            live_clauses -= branches[i].take().unwrap().clauses.len();
                        }
                        Search::Pending => outcome.stats.search_yields += 1,
                        Search::Complete(Verdict::Unknown) => {
                            return Err("finite solver unresolved split branch".into());
                        }
                    }
                }
                if outcome.stats.split_unsat == choices.len() {
                    break Verdict::Unsat;
                }
                quantum = quantum.saturating_mul(2);
            }
        };
        budget.check_time()?;
        if verdict != Verdict::Sat {
            return Ok(verdict);
        }
        // Validate every original CNF clause as well as the source-level witness.
        for clause in &sat.clauses {
            budget.tick(clause.len() as u64)?;
            if !clause.iter().any(|&p| truth(&sat.values, p) > 0) {
                return Err("finite SAT assignment failed CNF validation".into());
            }
        }
        let mut assignments = BTreeMap::new();
        for (n, bits) in &blast.inputs {
            let value = match bits.sort {
                Sort::Bool => Scalar::Bool(truth(&sat.values, bits.bits[0]) > 0),
                Sort::Bv(width) => Scalar::Bv {
                    width,
                    value: bits.bits.iter().enumerate().fold(0u64, |v, (i, &p)| {
                        v | ((truth(&sat.values, p) > 0) as u64) << i
                    }),
                },
                _ => unreachable!(),
            };
            assignments.insert(n.clone(), value);
        }
        let mut memo = HashMap::new();
        if evaluate(formula, &assignments, &mut memo, &mut budget, 0)? != Scalar::Bool(true) {
            return Err("finite SAT assignment failed original-formula validation".into());
        }
        let mut values = BTreeMap::new();
        for (label, term) in context {
            values.insert(
                label.clone(),
                evaluate(term, &assignments, &mut memo, &mut budget, 0)?,
            );
        }
        budget.check_time()?;
        outcome.assignments = assignments;
        outcome.context_values = values;
        outcome.original_formula_validated = true;
        Ok(Verdict::Sat)
    })();
    match result {
        Ok(verdict) => outcome.verdict = verdict,
        Err(reason) => outcome.reason = Some(reason),
    }
    outcome.stats.clauses = cumulative_clauses;
    outcome.stats.work = budget.work;
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use hwverify_ir::{and, boolv, bv, eq, ite, node, not, var};
    fn check(t: Term) -> Outcome {
        let proof = solve(&t, &Env::new(), Limits::default());
        let witness = solve_with_hint(&t, &Env::new(), Limits::default(), SearchHint::Sat);
        assert_eq!(proof.verdict, witness.verdict);
        assert_eq!(
            proof.original_formula_validated,
            witness.original_formula_validated
        );
        proof
    }
    fn test_budget() -> Budget {
        Budget {
            limits: Limits::default(),
            start: Instant::now(),
            work: 0,
            time_check_in: 0,
        }
    }
    fn pigeonhole(pigeons: usize, holes: usize) -> Term {
        let vars: Vec<Vec<Term>> = (0..pigeons)
            .map(|p| {
                (0..holes)
                    .map(|h| var(format!("p{p}h{h}"), Sort::Bool))
                    .collect()
            })
            .collect();
        let mut constraints = Vec::new();
        for row in &vars {
            constraints.push(
                row.iter()
                    .cloned()
                    .reduce(|a, b| node(Sort::Bool, "or", vec![a, b]))
                    .unwrap(),
            );
        }
        for (h, _) in vars[0].iter().enumerate() {
            for (p, row) in vars.iter().enumerate() {
                for other in vars.iter().skip(p + 1) {
                    constraints.push(not(and(row[h].clone(), other[h].clone())));
                }
            }
        }
        constraints.into_iter().reduce(and).unwrap()
    }
    #[test]
    fn resumed_cdcl_is_identical_to_uninterrupted_search() {
        let formula = pigeonhole(4, 3);
        let mut blast = Blast::new();
        let mut compile_budget = test_budget();
        let root = blast.term(&formula, &mut compile_budget, 0).unwrap();
        blast
            .clause(vec![root.bits[0]], &mut compile_budget)
            .unwrap();
        let mut original = Sat::new(blast.vars, blast.clauses.clone());
        let mut whole = test_budget();
        assert_eq!(original.run(&mut whole).unwrap(), Verdict::Unsat);
        assert!(original.conflicts > 1);
        assert!(original.clauses.len() > blast.clauses.len());
        for quantum in [1, 2, 7, 31, 127] {
            let mut resumed = Sat::new(blast.vars, blast.clauses.clone());
            let mut sliced = test_budget();
            let mut yields = 0;
            loop {
                match resumed.run_slice(&mut sliced, quantum).unwrap() {
                    Search::Complete(verdict) => {
                        assert_eq!(verdict, Verdict::Unsat);
                        break;
                    }
                    Search::Pending => yields += 1,
                }
            }
            assert!(yields > 0);
            assert_eq!(sliced.work, whole.work);
            assert_eq!(resumed.clauses, original.clauses);
            assert_eq!(resumed.watches, original.watches);
            assert_eq!(resumed.values, original.values);
            assert_eq!(resumed.trail, original.trail);
            assert_eq!(resumed.starts, original.starts);
            assert_eq!(resumed.head, original.head);
            assert_eq!(resumed.levels, original.levels);
            assert_eq!(resumed.reasons, original.reasons);
            assert_eq!(resumed.phase, original.phase);
            assert_eq!(resumed.activity, original.activity);
            assert_eq!(resumed.increment, original.increment);
            assert_eq!(resumed.order.heap, original.order.heap);
            assert_eq!(resumed.order.positions, original.order.positions);
            assert_eq!(resumed.decisions, original.decisions);
            assert_eq!(resumed.conflicts, original.conflicts);
        }
    }
    #[test]
    fn unassumed_probe_learning_preserves_every_later_assumption() {
        let clauses = vec![vec![1, 2], vec![1, -2], vec![-1, 3]];
        let mut probe = Sat::new(3, clauses.clone());
        let mut budget = test_budget();
        while probe.conflicts == 0 {
            assert_eq!(probe.run_slice(&mut budget, 1).unwrap(), Search::Pending);
        }
        assert!(probe.clauses.len() > clauses.len());
        for assumption in [-3, -2, -1, 1, 2, 3] {
            let mut original = clauses.clone();
            original.push(vec![assumption]);
            let mut enriched = probe.clauses.clone();
            enriched.push(vec![assumption]);
            let expected = Sat::new(3, original).run(&mut test_budget()).unwrap();
            let actual = Sat::new(3, enriched).run(&mut test_budget()).unwrap();
            assert_eq!(actual, expected);
        }
    }
    fn split_fixture(late_sat: bool) -> (Term, Env) {
        let a = var("a".into(), Sort::Bool);
        let b = var("b".into(), Sort::Bool);
        let c = var("c".into(), Sort::Bool);
        let d = var("d".into(), Sort::Bool);
        let alternatives = node(Sort::Bool, "or", vec![and(a.clone(), b), and(c.clone(), d)]);
        let constraints = if late_sat {
            not(a)
        } else {
            and(not(a), not(c))
        };
        (
            and(alternatives, constraints),
            Env::from([("extra".into(), var("extra".into(), Sort::Bv(64)))]),
        )
    }
    #[test]
    fn sequential_base_reuse_discards_assumption_learning_and_keeps_global_limits() {
        let a = var("switch".into(), Sort::Bool);
        let formula = node(
            Sort::Bool,
            "or",
            vec![and(a.clone(), pigeonhole(6, 5)), not(a)],
        );
        let full = solve_with_hint(&formula, &Env::new(), Limits::default(), SearchHint::Unsat);
        assert_eq!(full.verdict, Verdict::Sat);
        assert!(full.original_formula_validated);
        assert!(full.stats.base_cnf_reused);
        assert_eq!(full.stats.split_completed, 2);
        assert_eq!(full.stats.split_unsat, 1);
        assert!(full.stats.conflicts > 1);
        assert!(full.stats.clauses > 2 * full.stats.base_clauses + 2);
        for limits in [
            Limits {
                max_clauses: full.stats.clauses - 1,
                ..Limits::default()
            },
            Limits {
                max_work: full.stats.work - 1,
                ..Limits::default()
            },
        ] {
            assert_eq!(
                solve_with_hint(&formula, &Env::new(), limits, SearchHint::Unsat).verdict,
                Verdict::Unknown
            );
        }
        assert_eq!(
            solve_with_hint(
                &formula,
                &Env::new(),
                Limits {
                    max_clauses: full.stats.clauses,
                    max_work: full.stats.work,
                    ..Limits::default()
                },
                SearchHint::Unsat
            )
            .verdict,
            Verdict::Sat
        );
    }
    #[test]
    fn split_reuse_agrees_with_independent_exhaustive_boolean_truth_sets() {
        let mut seed = 0x879f421abu64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let vars = (0..6)
            .map(|i| {
                let truth = (0..64)
                    .filter(|assignment| assignment & (1 << i) != 0)
                    .fold(0u64, |set, assignment| set | (1u64 << assignment));
                (var(format!("v{i}"), Sort::Bool), truth)
            })
            .collect::<Vec<_>>();
        let literal = |n: u64| {
            let (term, truth) = vars[(n % 6) as usize].clone();
            if n & 8 != 0 {
                (not(term), !truth)
            } else {
                (term, truth)
            }
        };
        let mut reused = 0;
        for _ in 0..500 {
            let mut arms = Vec::new();
            let mut truth = 0u64;
            for _ in 0..4 {
                let (a, ta) = literal(next());
                let (c, tc) = literal(next());
                arms.push(and(a, c));
                truth |= ta & tc;
            }
            let mut formula = arms
                .into_iter()
                .reduce(|a, c| node(Sort::Bool, "or", vec![a, c]))
                .unwrap();
            for _ in 0..4 {
                let (a, ta) = literal(next());
                let (c, tc) = literal(next());
                formula = and(formula, node(Sort::Bool, "or", vec![a, c]));
                truth &= ta | tc;
            }
            for hint in [SearchHint::Unsat, SearchHint::Sat] {
                let result = solve_with_hint(&formula, &Env::new(), Limits::default(), hint);
                assert_eq!(
                    result.verdict,
                    if truth == 0 {
                        Verdict::Unsat
                    } else {
                        Verdict::Sat
                    }
                );
                reused += usize::from(result.stats.base_cnf_reused);
                if result.verdict == Verdict::Sat {
                    assert!(result.original_formula_validated);
                    let mut assignment = 0;
                    for i in 0..6 {
                        if result.assignments.get(&format!("v{i}")) == Some(&Scalar::Bool(true)) {
                            assignment |= 1 << i;
                        }
                    }
                    assert_ne!(truth & (1u64 << assignment), 0);
                }
            }
        }
        assert!(reused > 100);
    }
    #[test]
    fn asserted_expression_definitions_are_acyclic_and_restore_original_words() {
        let x = var("x".into(), Sort::Bv(3));
        let y = var("y".into(), Sort::Bv(3));
        let plus = node(Sort::Bv(3), "bvadd", vec![y.clone(), bv(3, 1)]);
        let formula = and(eq(x.clone(), plus.clone()), eq(y.clone(), bv(3, 7)));
        let result = solve(&formula, &Env::new(), Limits::default());
        assert_eq!(result.verdict, Verdict::Sat);
        assert!(result.original_formula_validated);
        assert_eq!(result.assignments["x"], Scalar::Bv { width: 3, value: 0 });
        assert!(result.stats.asserted_definitions > 0);
        let cycle = and(eq(x.clone(), plus), eq(y.clone(), x.clone()));
        assert_eq!(
            solve(&cycle, &Env::new(), Limits::default()).verdict,
            Verdict::Unsat
        );
        let neg = |v| node(Sort::Bv(3), "bvnot", vec![v]);
        let mutual = and(eq(x.clone(), neg(y.clone())), eq(y.clone(), neg(x.clone())));
        let result = solve(&mutual, &Env::new(), Limits::default());
        assert_eq!(result.verdict, Verdict::Sat);
        assert!(result.original_formula_validated);
        let conditional = and(
            node(
                Sort::Bool,
                "or",
                vec![eq(x.clone(), bv(3, 0)), eq(x.clone(), bv(3, 1))],
            ),
            eq(x, bv(3, 1)),
        );
        assert_eq!(
            solve(&conditional, &Env::new(), Limits::default()).verdict,
            Verdict::Sat
        );
    }
    #[test]
    fn expected_result_is_only_a_hint_for_both_actual_verdicts() {
        for actual_sat in [false, true] {
            let (formula, context) = split_fixture(actual_sat);
            for hint in [SearchHint::Sat, SearchHint::Unsat] {
                let result = solve_with_hint(&formula, &context, Limits::default(), hint);
                assert_eq!(
                    result.verdict,
                    if actual_sat {
                        Verdict::Sat
                    } else {
                        Verdict::Unsat
                    }
                );
                assert_eq!(result.search_hint, hint);
                assert_eq!(result.original_formula_validated, actual_sat);
                assert_eq!(result.context_values.contains_key("extra"), actual_sat);
                assert_eq!(result.assignments.is_empty(), !actual_sat);
                if hint == SearchHint::Unsat {
                    assert_eq!(result.search_strategy, SearchStrategy::ProofDecomposition);
                    assert_eq!(result.stats.probe_result, None);
                    assert_eq!(result.stats.probe_work, 0);
                    assert_eq!(result.stats.search_yields, 0);
                    assert_eq!(result.stats.split_completed, 2);
                    assert_eq!(result.stats.split_unsat, if actual_sat { 1 } else { 2 });
                    let default = solve(&formula, &context, Limits::default());
                    assert_eq!(default.stats.work, result.stats.work);
                    assert_eq!(default.stats.clauses, result.stats.clauses);
                } else {
                    assert_eq!(
                        result.search_strategy,
                        SearchStrategy::CounterexampleProbeFair
                    );
                    assert!(result.stats.probe_work > 0);
                }
                let bounded = solve_with_hint(
                    &formula,
                    &context,
                    Limits {
                        max_work: result.stats.work - 1,
                        ..Limits::default()
                    },
                    hint,
                );
                assert_eq!(bounded.verdict, Verdict::Unknown);
                assert!(!bounded.original_formula_validated);
                assert!(bounded.assignments.is_empty());
                assert!(bounded.context_values.is_empty());
            }
        }
    }
    #[test]
    fn both_hints_preserve_unsupported_and_deadline_unknown() {
        let (formula, _) = split_fixture(true);
        let unsupported =
            Env::from([("array".into(), node(Sort::Mem(2, 2), "unsupported", vec![]))]);
        for hint in [SearchHint::Sat, SearchHint::Unsat] {
            let result = solve_with_hint(&formula, &unsupported, Limits::default(), hint);
            assert_eq!(result.verdict, Verdict::Unknown);
            assert_eq!(result.search_strategy, SearchStrategy::NotStarted);
            assert!(!result.original_formula_validated);
            let result = solve_with_hint(
                &formula,
                &Env::new(),
                Limits {
                    timeout_ms: 0,
                    ..Limits::default()
                },
                hint,
            );
            assert_eq!(result.verdict, Verdict::Unknown);
            assert_eq!(result.search_hint, hint);
            assert!(result.assignments.is_empty());
        }
    }
    #[test]
    fn original_probe_decides_only_original_query_and_preserves_context() {
        for late_sat in [false, true] {
            let (formula, context) = split_fixture(late_sat);
            let result = solve_with_hint(&formula, &context, Limits::default(), SearchHint::Sat);
            assert_eq!(result.stats.split_alternatives, 2);
            assert_eq!(result.stats.split_completed, 0);
            assert_eq!(result.stats.split_unsat, 0);
            assert_eq!(result.stats.probe_result, Some(result.verdict));
            assert_eq!(
                result.verdict,
                if late_sat {
                    Verdict::Sat
                } else {
                    Verdict::Unsat
                }
            );
            assert!(result.stats.peak_live_clauses <= result.stats.clauses);
            if late_sat {
                assert!(result.original_formula_validated);
                assert_eq!(result.assignments.len(), 5);
                assert!(result.context_values.contains_key("extra"));
            }
        }
    }
    #[test]
    fn split_budgets_are_whole_query_not_per_branch() {
        let choices = node(
            Sort::Bool,
            "or",
            vec![var("a".into(), Sort::Bool), var("b".into(), Sort::Bool)],
        );
        let formula = and(choices, pigeonhole(8, 7));
        let context = Env::new();
        let full = solve_with_hint(&formula, &context, Limits::default(), SearchHint::Sat);
        assert_eq!(full.verdict, Verdict::Unsat);
        assert!(full.stats.search_yields > 0);
        assert_eq!(full.stats.split_completed, full.stats.split_alternatives);
        assert!(full.stats.clauses > 2 * full.stats.base_clauses);
        for limits in [
            Limits {
                max_clauses: 2 * full.stats.base_clauses + 1,
                ..Limits::default()
            },
            Limits {
                max_clauses: full.stats.clauses - 1,
                ..Limits::default()
            },
            Limits {
                max_work: full.stats.work - 1,
                ..Limits::default()
            },
        ] {
            let result = solve_with_hint(&formula, &context, limits, SearchHint::Sat);
            assert_eq!(result.verdict, Verdict::Unknown);
            assert!(result.reason.is_some());
            assert!(result.stats.split_completed < result.stats.split_alternatives);
            assert!(!result.original_formula_validated);
        }
        let exact = solve_with_hint(
            &formula,
            &context,
            Limits {
                max_clauses: full.stats.clauses,
                max_work: full.stats.work,
                ..Limits::default()
            },
            SearchHint::Sat,
        );
        assert_eq!(exact.verdict, Verdict::Unsat);
    }
    #[test]
    fn split_polarity_duplicates_and_opaque_operators_are_exact() {
        let a = var("a".into(), Sort::Bool);
        let b = var("b".into(), Sort::Bool);
        let c = var("c".into(), Sort::Bool);
        let left = and(a.clone(), b.clone());
        let alternatives = node(
            Sort::Bool,
            "or",
            vec![
                left.clone(),
                node(
                    Sort::Bool,
                    "or",
                    vec![boolv(false), node(Sort::Bool, "or", vec![left, c.clone()])],
                ),
            ],
        );
        let mut budget = test_budget();
        let mut blast = Blast::new();
        blast.term(&alternatives, &mut budget, 0).unwrap();
        assert_eq!(
            split_choices(&alternatives, &blast, &mut budget)
                .unwrap()
                .len(),
            2
        );
        let negative_and = not(and(not(a.clone()), and(not(b.clone()), not(c.clone()))));
        blast.term(&negative_and, &mut budget, 0).unwrap();
        assert_eq!(
            split_choices(&negative_and, &blast, &mut budget)
                .unwrap()
                .len(),
            3
        );
        for opaque in [
            not(alternatives.clone()),
            ite(a.clone(), alternatives.clone(), boolv(true)),
            node(Sort::Bool, "=>", vec![a, alternatives]),
        ] {
            blast.term(&opaque, &mut budget, 0).unwrap();
            assert!(split_choices(&opaque, &blast, &mut budget)
                .unwrap()
                .is_empty());
        }
        let (formula, _) = split_fixture(true);
        let malformed = Env::from([("dead".into(), node(Sort::Bool, "unsupported", vec![]))]);
        assert_eq!(
            solve(&formula, &malformed, Limits::default()).verdict,
            Verdict::Unknown
        );
    }
    #[test]
    fn canonical_mux_definitions_are_exact_and_shared() {
        let mut b = test_budget();
        let mut blast = Blast::new();
        let g = blast.fresh(&mut b).unwrap();
        let x = blast.fresh(&mut b).unwrap();
        let y = blast.fresh(&mut b).unwrap();
        let before = (blast.vars, blast.clauses.len());
        let z = blast.mux(g, x, y, &mut b).unwrap();
        assert_eq!(
            (blast.vars, blast.clauses.len()),
            (before.0 + 1, before.1 + 4)
        );
        assert_eq!(blast.mux(-g, y, x, &mut b).unwrap(), z);
        assert_eq!(blast.mux(g, -x, -y, &mut b).unwrap(), -z);
        assert_eq!(blast.mux(-g, -y, -x, &mut b).unwrap(), -z);
        assert_eq!(blast.vars, before.0 + 1);
        for assignment in 0..16 {
            let values = [
                1,
                1,
                if assignment & 1 == 0 { -1 } else { 1 },
                if assignment & 2 == 0 { -1 } else { 1 },
                if assignment & 4 == 0 { -1 } else { 1 },
                if assignment & 8 == 0 { -1 } else { 1 },
            ];
            let satisfies = blast
                .clauses
                .iter()
                .all(|clause| clause.iter().any(|&lit| truth(&values, lit) > 0));
            let expected =
                truth(&values, z) == truth(&values, if truth(&values, g) > 0 { x } else { y });
            assert_eq!(satisfies, expected);
        }
    }
    #[test]
    fn mandatory_aliases_share_bits_and_replay_every_original_name() {
        for sort in [Sort::Bool, Sort::Bv(1), Sort::Bv(32), Sort::Bv(64)] {
            let x = var("x".into(), sort.clone());
            let y = var("y".into(), sort.clone());
            let z = var("z".into(), sort.clone());
            let formula = and(
                eq(x.clone(), y.clone()),
                and(eq(z.clone(), y.clone()), eq(y.clone(), x.clone())),
            );
            let mut b = test_budget();
            let mut blast = Blast::new();
            blast.aliases.collect(&formula, &mut b).unwrap();
            blast.term(&formula, &mut b, 0).unwrap();
            assert_eq!(blast.inputs["x"].bits, blast.inputs["y"].bits);
            assert_eq!(blast.inputs["y"].bits, blast.inputs["z"].bits);
            let context = Env::from([
                ("original x".into(), x),
                ("original y".into(), y),
                ("original z".into(), z),
            ]);
            let result = solve(&formula, &context, Limits::default());
            assert_eq!(result.verdict, Verdict::Sat);
            assert!(result.original_formula_validated);
            assert_eq!(result.assignments.len(), 3);
            assert_eq!(result.assignments["x"], result.assignments["z"]);
            assert_eq!(
                result.context_values["original x"],
                result.context_values["original y"]
            );
        }
    }
    #[test]
    fn conditional_and_context_equalities_never_become_assumptions() {
        let x = var("x".into(), Sort::Bv(8));
        let y = var("y".into(), Sort::Bv(8));
        let equal = eq(x.clone(), y.clone());
        let different = not(equal.clone());
        for condition in [
            not(equal.clone()),
            node(Sort::Bool, "or", vec![boolv(true), equal.clone()]),
            node(Sort::Bool, "=>", vec![boolv(false), equal.clone()]),
            ite(boolv(false), equal.clone(), boolv(true)),
        ] {
            let formula = and(condition, different.clone());
            let result = solve(
                &formula,
                &Env::from([("equal".into(), equal.clone())]),
                Limits::default(),
            );
            assert_eq!(result.verdict, Verdict::Sat);
            assert_eq!(result.context_values["equal"], Scalar::Bool(false));
            assert_ne!(result.assignments["x"], result.assignments["y"]);
        }
        let mismatched = and(equal, eq(var("x".into(), Sort::Bool), boolv(true)));
        assert_eq!(check(mismatched).verdict, Verdict::Unknown);
    }
    #[test]
    fn indexed_order_matches_linear_scan_after_updates_and_reinsertion() {
        let mut b = test_budget();
        let mut order = VarOrder::new(128);
        let mut scores = vec![0.; 129];
        let mut present = [false; 129];
        // Equal (including zero) activity must keep smallest-id-first order.
        for v in (1..129).rev() {
            order.insert(v, &scores, &mut b).unwrap();
            present[v] = true;
        }
        for (v, present) in present.iter_mut().enumerate().skip(1) {
            assert_eq!(order.pop(&scores, &mut b).unwrap(), Some(v));
            *present = false;
        }
        assert_eq!(order.pop(&scores, &mut b).unwrap(), None);
        let mut state = 0x91aa_u64;
        for step in 0..20_000 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let v = (state >> 32) as usize % 128 + 1;
            match state % 5 {
                0 | 1 => {
                    // Include duplicate insertions and reinsertion after pop.
                    order.insert(v, &scores, &mut b).unwrap();
                    present[v] = true;
                }
                2 => {
                    scores[v] += (step % 7) as f64;
                    order.promote(v, &scores, &mut b).unwrap();
                }
                3 => {
                    let expected = (1..129).filter(|&v| present[v]).reduce(|a, c| {
                        if VarOrder::higher(a, c, &scores) {
                            a
                        } else {
                            c
                        }
                    });
                    assert_eq!(order.pop(&scores, &mut b).unwrap(), expected);
                    if let Some(v) = expected {
                        present[v] = false;
                    }
                }
                _ => {
                    // Deliberately create rounded ties, as rescaling can do.
                    for score in &mut scores {
                        *score = (*score * 0.5).floor();
                    }
                    order.rebuild(&scores, &mut b).unwrap();
                }
            }
            for (pos, &v) in order.heap.iter().enumerate() {
                assert_eq!(order.positions[v], pos);
                assert!(present[v]);
                if pos > 0 {
                    assert!(!VarOrder::higher(v, order.heap[(pos - 1) / 2], &scores));
                }
            }
            assert_eq!(order.heap.len(), present.iter().filter(|&&x| x).count());
        }
    }
    #[test]
    fn exact_work_limits_and_deadline_checks_survive_amortization() {
        let mut b = test_budget();
        b.limits.max_work = 2;
        b.tick(2).unwrap();
        assert!(b.tick(1).unwrap_err().contains("work budget"));
        let mut b = test_budget();
        b.limits.timeout_ms = 1;
        b.start = Instant::now() - Duration::from_millis(100);
        b.time_check_in = 255;
        // Clock polling may be delayed, but the mandatory verdict-boundary
        // check must reject a completion that crossed the deadline.
        b.tick(1).unwrap();
        assert!(b.check_time().unwrap_err().contains("time budget"));
        for _ in 0..254 {
            b.tick(1).unwrap();
        }
        assert!(b.tick(1).unwrap_err().contains("time budget"));
    }
    #[test]
    fn sat_model_is_complete_and_replays_original_and_context() {
        let x = var("x".into(), Sort::Bv(8));
        let g = var("g".into(), Sort::Bool);
        let sum = node(Sort::Bv(8), "bvadd", vec![x.clone(), bv(8, 1)]);
        let formula = and(g.clone(), eq(sum.clone(), bv(8, 0)));
        let ctx = Env::from([
            ("sum".into(), sum),
            ("chosen".into(), ite(g, bv(8, 3), x.clone())),
            ("extra".into(), var("extra".into(), Sort::Bool)),
        ]);
        let result = solve(&formula, &ctx, Limits::default());
        assert_eq!(result.verdict, Verdict::Sat);
        assert!(result.original_formula_validated);
        assert_eq!(
            result.assignments["x"],
            Scalar::Bv {
                width: 8,
                value: 255
            }
        );
        assert!(result.assignments.contains_key("extra"));
        assert_eq!(
            result.context_values["sum"],
            Scalar::Bv { width: 8, value: 0 }
        );
        assert_eq!(
            result.context_values["chosen"],
            Scalar::Bv { width: 8, value: 3 }
        );
    }
    #[test]
    fn contradictions_require_unsat_and_all_limits_are_unknown() {
        let x = var("x".into(), Sort::Bv(4));
        assert_eq!(
            check(and(eq(x.clone(), bv(4, 2)), eq(x, bv(4, 3)))).verdict,
            Verdict::Unsat
        );
        let formula = not(var("flag".into(), Sort::Bool));
        let limits = [
            Limits {
                timeout_ms: 0,
                ..Default::default()
            },
            Limits {
                max_work: 0,
                ..Default::default()
            },
            Limits {
                max_terms: 1,
                ..Default::default()
            },
            Limits {
                max_variables: 1,
                ..Default::default()
            },
            Limits {
                max_clauses: 1,
                ..Default::default()
            },
            Limits {
                max_depth: 0,
                ..Default::default()
            },
        ];
        for limit in limits {
            let result = solve(&formula, &Env::new(), limit);
            assert_eq!(result.verdict, Verdict::Unknown);
            assert!(result.reason.is_some());
            assert!(!result.original_formula_validated);
        }
    }
    #[test]
    fn unsupported_and_malformed_terms_never_prove_anything() {
        let memory = var("mem".into(), Sort::Mem(2, 8));
        let tests = [
            eq(node(Sort::Mem(2, 8), "store", vec![memory.clone()]), memory),
            node(Sort::Bool, "unknown", vec![]),
            node(Sort::Bool, "=", vec![]),
            node(Sort::Bv(4), "bvadd", vec![bv(4, 1)]),
            eq(var("x".into(), Sort::Bv(0)), var("x".into(), Sort::Bv(0))),
            and(
                var("same".into(), Sort::Bool),
                eq(var("same".into(), Sort::Bv(1)), bv(1, 0)),
            ),
            node(
                Sort::Bool,
                "and",
                vec![boolv(false), node(Sort::Bool, "unknown", vec![])],
            ),
        ];
        for formula in tests {
            assert_eq!(check(formula).verdict, Verdict::Unknown);
        }
        let ctx = Env::from([("array".into(), node(Sort::Mem(2, 8), "unsupported", vec![]))]);
        assert_eq!(
            solve(&boolv(true), &ctx, Limits::default()).verdict,
            Verdict::Unknown
        );
    }
    #[test]
    fn arithmetic_signedness_shifts_and_slices_are_exact() {
        for w in 1..=4 {
            for x in 0..1u64 << w {
                for y in 0..1u64 << w {
                    let v = var("x".into(), Sort::Bv(w));
                    let u = var("y".into(), Sort::Bv(w));
                    let signed = |n: u64| ((n << (64 - w)) as i64) >> (64 - w);
                    let values = [
                        ("bvadd", x.wrapping_add(y)),
                        ("bvsub", x.wrapping_sub(y)),
                        ("bvmul", x * y),
                        ("bvand", x & y),
                        ("bvor", x | y),
                        ("bvxor", x ^ y),
                        ("bvshl", x << y),
                        ("bvlshr", x >> y),
                    ];
                    for (op, value) in values {
                        let expression = node(Sort::Bv(w), op, vec![v.clone(), u.clone()]);
                        let formula = and(
                            and(eq(v.clone(), bv(w, x)), eq(u.clone(), bv(w, y))),
                            not(eq(expression, bv(w, value))),
                        );
                        assert_eq!(
                            check(formula).verdict,
                            Verdict::Unsat,
                            "{op} width{w} {x} {y}"
                        );
                    }
                    for (op, value) in [
                        ("bvult", x < y),
                        ("bvule", x <= y),
                        ("bvslt", signed(x) < signed(y)),
                        ("bvsle", signed(x) <= signed(y)),
                    ] {
                        let expression = node(Sort::Bool, op, vec![v.clone(), u.clone()]);
                        let formula = and(
                            and(eq(v.clone(), bv(w, x)), eq(u.clone(), bv(w, y))),
                            not(eq(expression, boolv(value))),
                        );
                        assert_eq!(
                            check(formula).verdict,
                            Verdict::Unsat,
                            "{op} width{w} {x} {y}"
                        );
                    }
                }
            }
        }
        let x = var("wide".into(), Sort::Bv(64));
        let wrap = node(Sort::Bv(64), "bvadd", vec![x.clone(), bv(64, 1)]);
        assert_eq!(
            check(and(eq(x, bv(64, u64::MAX)), not(eq(wrap, bv(64, 0))))).verdict,
            Verdict::Unsat
        );
    }
    #[test]
    fn cdcl_agrees_with_exhaustive_small_cnf() {
        let mut state = 13u64;
        let mut next = || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            state
        };
        for vars in 1..=7 {
            for _case in 0..1000 {
                let count = (next() % 25) as usize;
                let mut clauses = Vec::new();
                for _ in 0..count {
                    let len = (next() % 4) as usize;
                    let mut c = Vec::new();
                    for _ in 0..len {
                        let v = (next() % vars as u64 + 1) as Lit;
                        let p = if next() & 0x800000 != 0 { v } else { -v };
                        if !c.contains(&p) {
                            c.push(p);
                        }
                    }
                    if !c.iter().any(|p| c.contains(&-p)) {
                        clauses.push(c);
                    }
                }
                let exists = (0..1u64 << vars).any(|bits| {
                    clauses.iter().all(|c| {
                        c.iter()
                            .any(|&p| ((bits >> (p.unsigned_abs() - 1)) & 1 != 0) == (p > 0))
                    })
                });
                let mut sat = Sat::new(vars, clauses.clone());
                let mut budget = Budget {
                    limits: Limits::default(),
                    start: Instant::now(),
                    work: 0,
                    time_check_in: 0,
                };
                let verdict = sat.run(&mut budget).unwrap();
                assert_eq!(
                    verdict,
                    if exists { Verdict::Sat } else { Verdict::Unsat },
                    "{clauses:?}"
                );
                if exists {
                    assert!(clauses
                        .iter()
                        .all(|c| c.iter().any(|&p| truth(&sat.values, p) > 0)));
                }
            }
        }
    }
}

#[cfg(test)]
mod guarded_tests;
