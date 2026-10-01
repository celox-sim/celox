//! Opt-in, bounded decision procedure for the quantifier-free scalar Bool/BV IR.
//!
//! Terms are bit-blasted to definitional CNF and decided by a small CDCL solver.
//! SAT assignments are independently evaluated on the ORIGINAL formula and all
//! requested context terms. Unsupported terms or exhausted budgets yield Unknown.
//! Diagnostics are not proof certificates: UNSAT trusts this Rust implementation.
use hwverify_ir::{Env, Res, Sort, Term};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    time::{Duration, Instant},
};

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
    pub terms: usize,
    pub variables: usize,
    pub clauses: usize,
    pub decisions: u64,
    pub conflicts: u64,
    pub work: u64,
}
#[derive(Debug)]
pub struct Outcome {
    pub verdict: Verdict,
    pub reason: Option<String>,
    pub assignments: BTreeMap<String, Scalar>,
    pub context_values: BTreeMap<String, Scalar>,
    pub original_formula_validated: bool,
    pub stats: Stats,
}
impl Outcome {
    pub fn diagnostics(&self) -> Value {
        json!({"solver_result":self.verdict.as_str(),"reason":self.reason,
            "kind":"bounded bit-blast/CDCL diagnostics; not an independently checkable proof certificate",
            "trusted":"Rust scalar encoding, SAT search, and original-formula evaluation; not a Lean certificate",
            "original_formula_validated":self.original_formula_validated,
            "assignments":self.assignments.iter().map(|(k,v)|(k.clone(),v.json())).collect::<BTreeMap<_,_>>(),
            "context_values":self.context_values.iter().map(|(k,v)|(k.clone(),v.json())).collect::<BTreeMap<_,_>>(),
            "terms":self.stats.terms,"variables":self.stats.variables,"clauses":self.stats.clauses,
            "decisions":self.stats.decisions,"conflicts":self.stats.conflicts,"work":self.stats.work})
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
                Op::Eq => {
                    if let (Op::Variable(x), Op::Variable(y)) =
                        (operation(&term.0.args[0])?, operation(&term.0.args[1])?)
                    {
                        self.join(x, y, b)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}
struct Blast {
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
        let op = operation(t)?;
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

/// Two-watched-literal CDCL. First-UIP learned clauses are resolution consequences
/// of existing clauses; only a conflict at decision level zero establishes UNSAT.
struct Sat {
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
    fn run(&mut self, b: &mut Budget) -> Res<Verdict> {
        for id in 0..self.clauses.len() {
            b.tick(1)?;
            for &p in &self.clauses[id] {
                self.activity[p.unsigned_abs() as usize] += 1.;
            }
            if self.clauses[id].is_empty() {
                return Ok(Verdict::Unsat);
            }
            if self.clauses[id].len() == 1 && !self.enqueue(self.clauses[id][0], Some(id)) {
                return Ok(Verdict::Unsat);
            }
            self.attach(id);
        }
        for v in 1..self.values.len() {
            self.order.insert(v, &self.activity, b)?;
        }
        loop {
            if let Some(conflict) = self.propagate(b)? {
                self.conflicts += 1;
                if self.starts.is_empty() {
                    return Ok(Verdict::Unsat);
                }
                let (learned, level) = self.analyze(conflict, b)?;
                self.backtrack(level, b)?;
                if self.clauses.len() >= b.limits.max_clauses {
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
                        return Ok(Verdict::Sat);
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
}

pub fn solve(formula: &Term, context: &Env, limits: Limits) -> Outcome {
    let mut outcome = Outcome {
        verdict: Verdict::Unknown,
        reason: None,
        assignments: BTreeMap::new(),
        context_values: BTreeMap::new(),
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
        blast.aliases.collect(formula, &mut budget)?;
        let root = blast.term(formula, &mut budget, 0)?;
        for term in context.values() {
            blast.term(term, &mut budget, 0)?;
        }
        blast.clause(vec![root.bits[0]], &mut budget)?;
        Ok(())
    })();
    outcome.stats.terms = blast.memo.len();
    outcome.stats.variables = blast.vars;
    outcome.stats.clauses = blast.clauses.len();
    if let Err(reason) = compile {
        outcome.reason = Some(reason);
        outcome.stats.work = budget.work;
        return outcome;
    }
    let mut sat = Sat::new(blast.vars, std::mem::take(&mut blast.clauses));
    let result = (|| -> Res<Verdict> {
        let verdict = sat.run(&mut budget)?;
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
    outcome.stats.decisions = sat.decisions;
    outcome.stats.conflicts = sat.conflicts;
    outcome.stats.clauses = sat.clauses.len();
    outcome.stats.work = budget.work;
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use hwverify_ir::{and, boolv, bv, eq, ite, node, not, var};
    fn check(t: Term) -> Outcome {
        solve(&t, &Env::new(), Limits::default())
    }
    fn test_budget() -> Budget {
        Budget {
            limits: Limits::default(),
            start: Instant::now(),
            work: 0,
            time_check_in: 0,
        }
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
            eq(memory.clone(), memory),
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
        let ctx = Env::from([("array".into(), var("memory".into(), Sort::Mem(2, 8)))]);
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
