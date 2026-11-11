//! Opt-in, bounded decision procedure for quantifier-free Bool/BV and total-array IR.
//!
//! Terms are bit-blasted to definitional CNF and decided by a small CDCL solver.
//! SAT assignments are independently evaluated on the ORIGINAL formula and all
//! requested context terms. Readonly arrays use exact finite-observation reduction
//! and total sparse-array model replay. Unsupported terms or exhausted budgets yield Unknown.
//! Diagnostics are not proof certificates: UNSAT trusts this Rust implementation.
use crate::{Env, Res, Sort, Term};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    time::{Duration, Instant},
};

mod control;
mod lookup;
mod prepared;
mod readonly;
pub use prepared::{Assumption, Cell, CellStatus, HashCells, PreparedQuery, SampleBatch, Sampler};
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
    pub control_cache_hits: usize,
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
            "control_variables":self.stats.control_variables,"control_cases":self.stats.control_cases,"control_cases_closed":self.stats.control_cases_closed,"control_cache_hits":self.stats.control_cache_hits,"control_case_work":self.stats.control_case_work,"control_preprocess_work":self.stats.control_preprocess_work,"search_hint":self.search_hint.as_str(),"search_strategy":self.search_strategy.as_str(),
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
    if w == 64 { u64::MAX } else { (1u64 << w) - 1 }
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
// Sufficient syntactic entailment from mandatory branch facts. Failure means
// "not established", never falsity. In particular, an asserted disjunction
// does not supply either operand as a fact. This helper creates no terms and
// all traversals share the enclosing query's resource budget.
fn guard_entailed(
    guard: &Term,
    facts: &HashSet<Term>,
    memo: &mut HashMap<Term, bool>,
    b: &mut Budget,
    depth: usize,
) -> Res<bool> {
    b.tick(1)?;
    if depth > b.limits.max_depth {
        return Err("guard entailment depth budget exhausted".into());
    }
    if let Some(value) = memo.get(guard) {
        return Ok(*value);
    }
    if memo.len() >= b.limits.max_terms {
        return Err("guard entailment term budget exhausted".into());
    }
    let known = if facts.contains(guard) {
        true
    } else {
        match operation(guard)? {
            Op::Bool(value) => value,
            Op::And => {
                guard_entailed(&guard.0.args[0], facts, memo, b, depth + 1)?
                    && guard_entailed(&guard.0.args[1], facts, memo, b, depth + 1)?
            }
            Op::Or => {
                guard_entailed(&guard.0.args[0], facts, memo, b, depth + 1)?
                    || guard_entailed(&guard.0.args[1], facts, memo, b, depth + 1)?
            }
            _ => false,
        }
    };
    if memo.len() >= b.limits.max_terms {
        return Err("guard entailment term budget exhausted".into());
    }
    memo.insert(guard.clone(), known);
    Ok(known)
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
            crate::node(t.0.sort.clone(), t.0.op.clone(), args)
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
        let mut entailed = HashMap::new();
        for (name, (guard, rhs)) in &self.aliases.guarded {
            b.tick(1)?;
            let mut established = true;
            for g in guard {
                if !guard_entailed(g, &facts, &mut entailed, b, depth + 1)? {
                    established = false;
                    break;
                }
            }
            if established {
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
        Ok(Some(crate::ite(
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

/// Replay scalar IR terms directly, independently of CNF encoding and SAT search.
/// Unsupported terms, missing values, and exhausted budgets are errors, never evidence.
pub fn evaluate_scalar_terms(
    terms: &Env,
    assignments: &BTreeMap<String, Scalar>,
    limits: Limits,
) -> Res<BTreeMap<String, Scalar>> {
    let mut budget = Budget {
        limits,
        start: Instant::now(),
        work: 0,
        time_check_in: 0,
    };
    let mut memo = HashMap::new();
    let result = terms
        .iter()
        .map(|(name, term)| {
            Ok((
                name.clone(),
                evaluate(term, assignments, &mut memo, &mut budget, 0)?,
            ))
        })
        .collect();
    budget.check_time()?;
    result
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
    fn seed_phases(&mut self, mut state: u64, budget: &mut Budget) -> Res<()> {
        // SplitMix64 is local and deterministic, including seed zero.
        for phase in self.phase.iter_mut().skip(1) {
            budget.tick(1)?;
            state = state.wrapping_add(0x9e3779b97f4a7c15);
            let mut value = state;
            value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
            value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
            *phase = (value ^ (value >> 31)) >> 63 != 0;
        }
        Ok(())
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
        self.run_slice_assuming(b, quantum, &[])
    }
    fn run_slice_assuming(
        &mut self,
        b: &mut Budget,
        quantum: u64,
        assumptions: &[Lit],
    ) -> Res<Search> {
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
                // Reapply assumptions after every backjump. Each has its own
                // level, including already-implied ones, so the level indexes
                // remain stable. An assumption falsified by propagation proves
                // only this query UNSAT, not the permanent clause database.
                if self.starts.len() < assumptions.len() {
                    b.tick(1)?;
                    let p = assumptions[self.starts.len()];
                    if truth(&self.values, p) < 0 {
                        return Ok(Search::Complete(Verdict::Unsat));
                    }
                    self.starts.push(self.trail.len());
                    if truth(&self.values, p) == 0 {
                        self.enqueue(p, None);
                    }
                    continue;
                }
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

/// Validate the entire original formula AND diagnostic context before any
/// compositional projection can discard derived-context expressions.
pub fn validate_original_context(formula: &Term, context: &Env) -> Res<(usize, u64)> {
    validate_original_context_with_limits(formula, context, Limits::default())
}
/// Validate the complete input under an explicit budget; returns term and work counts.
pub fn validate_original_context_with_limits(
    formula: &Term,
    context: &Env,
    limits: Limits,
) -> Res<(usize, u64)> {
    let mut budget = Budget {
        limits,
        start: Instant::now(),
        work: 0,
        time_check_in: 0,
    };
    if formula.0.sort != Sort::Bool {
        return Err("finite formula must have Bool sort".into());
    }
    let mut todo = vec![(formula, 0usize)];
    todo.extend(context.values().map(|value| (value, 0usize)));
    let mut seen = HashSet::new();
    let mut variables = BTreeMap::new();
    while let Some((term, depth)) = todo.pop() {
        budget.tick(1)?;
        if depth > budget.limits.max_depth {
            return Err("original context depth budget exhausted".into());
        }
        if !seen.insert(term.clone()) {
            continue;
        }
        if seen.len() > budget.limits.max_terms {
            return Err("original context term budget exhausted".into());
        }
        readonly::validate(term)?;
        if let Some(name) = term.0.op.strip_prefix('@') {
            if variables
                .insert(name.to_string(), term.0.sort.clone())
                .is_some_and(|sort| sort != term.0.sort)
            {
                return Err("original context variable has inconsistent sorts".into());
            }
        }
        todo.extend(term.0.args.iter().map(|arg| (arg, depth + 1)));
    }
    budget.check_time()?;
    Ok((seen.len(), budget.work))
}

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
/// Find a scalar satisfying assignment using seed-selected initial SAT phases.
///
/// The seed changes search preferences, never the formula. This is a reproducible
/// search heuristic, not uniform model sampling. Reproducibility requires the
/// same solver version, input and sufficient budgets; wall-clock timeouts can
/// interrupt different prefixes. Arrays are unsupported by this entry point and
/// return `Unknown`. Existing proof-oriented entry points are unchanged.
pub fn solve_seeded(formula: &Term, context: &Env, limits: Limits, seed: u64) -> Outcome {
    solve_plain_with_seed(formula, context, limits, SearchHint::Sat, Some(seed))
}

fn solve_plain(formula: &Term, context: &Env, limits: Limits, search_hint: SearchHint) -> Outcome {
    solve_plain_with_seed(formula, context, limits, search_hint, None)
}

fn replay_model(
    formula: &Term,
    context: &Env,
    blast: &Blast,
    sat: &Sat,
    budget: &mut Budget,
) -> Res<(BTreeMap<String, Scalar>, BTreeMap<String, Scalar>)> {
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
    if evaluate(formula, &assignments, &mut memo, budget, 0)? != Scalar::Bool(true) {
        return Err("finite SAT assignment failed original-formula validation".into());
    }
    let mut values = BTreeMap::new();
    for (label, term) in context {
        values.insert(
            label.clone(),
            evaluate(term, &assignments, &mut memo, budget, 0)?,
        );
    }
    budget.check_time()?;
    Ok((assignments, values))
}

fn compile_scalar(
    blast: &mut Blast,
    formula: &Term,
    context: &Env,
    budget: &mut Budget,
    scalar_only: bool,
) -> Res<()> {
    budget.tick(1)?;
    if scalar_only && (formula.contains_memory() || context.values().any(Term::contains_memory)) {
        return Err("scalar search supports scalar formulas and contexts only".into());
    }
    if budget.limits.max_variables < 1 || budget.limits.max_clauses < 1 {
        return Err("finite solver initial CNF budget exhausted".into());
    }
    if formula.0.sort != Sort::Bool {
        return Err("finite formula must have Bool sort".into());
    }
    // Rewriting can bypass source nodes. Validate every original formula
    // and context signature first, including dead branches and Sat hints.
    blast.lookup.source_nodes = lookup::validate_source(formula, context, budget)?;
    blast.aliases.collect(formula, budget)?;
    let root = blast.term(formula, budget, 0)?;
    for term in context.values() {
        blast.term(term, budget, 0)?;
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
        blast.term(term, budget, 0)?;
    }
    blast.clause(vec![root.bits[0]], budget)?;
    Ok(())
}

fn solve_plain_with_seed(
    formula: &Term,
    context: &Env,
    limits: Limits,
    search_hint: SearchHint,
    seed: Option<u64>,
) -> Outcome {
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
    let compile = compile_scalar(&mut blast, formula, context, &mut budget, seed.is_some());
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
    // A seeded query uses one CDCL search so decomposition cannot reset its
    // preferences or bias the result toward the first syntactic alternative.
    let choices = match if seed.is_some() {
        Ok(Vec::new())
    } else {
        split_choices(formula, &blast, &mut budget)
    } {
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
        if let Some(seed) = seed {
            sat.seed_phases(seed, &mut budget)?;
        }
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
        let (assignments, values) = replay_model(formula, context, &blast, &sat, &mut budget)?;
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
mod tests;

#[cfg(test)]
mod guarded_tests;
