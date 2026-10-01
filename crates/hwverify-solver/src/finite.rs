//! Opt-in, bounded decision procedure for the quantifier-free scalar Bool/BV IR.
//!
//! Terms are bit-blasted to definitional CNF and decided by a small CDCL solver.
//! SAT assignments are independently evaluated on the ORIGINAL formula and all
//! requested context terms. Unsupported terms or exhausted budgets yield Unknown.
//! Diagnostics are not proof certificates: UNSAT trusts this Rust implementation.
use hwverify_ir::{Env, Res, Sort, Term};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
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
}
impl Budget {
    fn tick(&mut self, amount: u64) -> Res<()> {
        self.work = self.work.saturating_add(amount);
        if self.work > self.limits.max_work {
            return Err("finite solver work budget exhausted".into());
        }
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
struct Blast {
    memo: HashMap<Term, Bits>,
    gates: HashMap<(u8, Lit, Lit), Lit>,
    inputs: BTreeMap<String, Bits>,
    vars: usize,
    clauses: Vec<Vec<Lit>>,
}
impl Blast {
    fn new() -> Self {
        Self {
            memo: HashMap::new(),
            gates: HashMap::new(),
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
    fn mux(&mut self, g: Lit, x: Lit, y: Lit, b: &mut Budget) -> Res<Lit> {
        if g == TRUE || x == y {
            return Ok(x);
        }
        if g == FALSE {
            return Ok(y);
        }
        let a = self.and(g, x, b)?;
        let c = self.and(-g, y, b)?;
        self.or(a, c, b)
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
                if let Some(v) = self.inputs.get(&n) {
                    if v.sort != t.0.sort {
                        return Err(format!("finite variable {n} has inconsistent sorts"));
                    }
                    v.bits.clone()
                } else {
                    let bits = (0..w).map(|_| self.fresh(b)).collect::<Res<Vec<_>>>()?;
                    self.inputs.insert(
                        n,
                        Bits {
                            sort: t.0.sort.clone(),
                            bits: bits.clone(),
                        },
                    );
                    bits
                }
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
            let pending = std::mem::take(&mut self.watches[watch]);
            for (pos, &id) in pending.iter().enumerate() {
                b.tick(1)?;
                let c = &mut self.clauses[id];
                if c[0] == -p {
                    c.swap(0, 1);
                }
                if c[1] != -p {
                    return Err("finite SAT watch invariant violated".into());
                }
                if truth(&self.values, c[0]) > 0 {
                    self.watches[watch].push(id);
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
                self.watches[watch].push(id);
                if !self.enqueue(other, Some(id)) {
                    self.watches[watch].extend_from_slice(&pending[pos + 1..]);
                    return Ok(Some(id));
                }
            }
        }
        Ok(None)
    }
    fn backtrack(&mut self, level: usize) {
        if self.starts.len() <= level {
            return;
        }
        let keep = self.starts[level];
        for &p in self.trail[keep..].iter().rev() {
            let v = p.unsigned_abs() as usize;
            self.phase[v] = p > 0;
            self.values[v] = 0;
            self.reasons[v] = None;
            self.levels[v] = 0;
        }
        self.trail.truncate(keep);
        self.starts.truncate(level);
        self.head = self.head.min(keep);
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
        loop {
            if let Some(conflict) = self.propagate(b)? {
                self.conflicts += 1;
                if self.starts.is_empty() {
                    return Ok(Verdict::Unsat);
                }
                let (learned, level) = self.analyze(conflict, b)?;
                self.backtrack(level);
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
                let mut choice = None;
                let mut score = -1.;
                for v in 1..self.values.len() {
                    b.tick(1)?;
                    if self.values[v] == 0 && self.activity[v] > score {
                        choice = Some(v);
                        score = self.activity[v];
                    }
                }
                let Some(v) = choice else {
                    return Ok(Verdict::Sat);
                };
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
