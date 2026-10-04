//! Bounded, UNSAT-only structural proof kernel. No observations, SAT claims,
//! interval inference, or untrusted rewrite instructions. Failure to close is
//! inconclusive and the original query goes to Z3.
use lydite_ir::*;
use std::collections::{BTreeMap, HashMap, HashSet};

pub fn literal(t: &Term) -> Option<u64> {
    if !matches!(t.0.sort, Sort::Bv(_)) || !t.0.args.is_empty() {
        return None;
    }
    t.0.op
        .strip_prefix("(_ bv")?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}
fn boolean(t: &Term) -> Option<bool> {
    if t.0.sort != Sort::Bool {
        return None;
    }
    match t.0.op.as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}
fn signed(v: u64, w: u32) -> i64 {
    if w == 64 {
        v as i64
    } else {
        ((v << (64 - w)) as i64) >> (64 - w)
    }
}
fn flatten(t: &Term, op: &str, out: &mut Vec<Term>) {
    if t.0.op == op {
        for a in &t.0.args {
            flatten(a, op, out);
        }
    } else {
        out.push(t.clone());
    }
}
fn contains(t: &Term, needle: &Term) -> bool {
    t == needle || t.0.args.iter().any(|a| contains(a, needle))
}
#[derive(Default)]
struct Context {
    subst: HashMap<Term, Term>,
    facts: HashMap<Term, bool>,
    rules: BTreeMap<String, u64>,
    contradiction: bool,
}
impl Context {
    fn count(&mut self, rule: &str) {
        *self.rules.entry(rule.into()).or_default() += 1;
    }
    fn norm(&mut self, t: &Term, memo: &mut HashMap<Term, Term>, use_root_fact: bool) -> Term {
        if use_root_fact {
            if let Some(v) = self.facts.get(t).copied() {
                return boolv(v);
            }
            if let Some(v) = memo.get(t) {
                return v.clone();
            }
        }
        if let Some(v) = self.subst.get(t).cloned() {
            // Bindings are acyclic: RHS is normalized, and occurs checked before insertion.
            let r = self.norm(&v, memo, true);
            if use_root_fact {
                memo.insert(t.clone(), r.clone());
            }
            return r;
        }
        // An asserted negation must not use its own negated-atom fact to
        // hide the contradiction we are trying to discover.
        let child_facts = use_root_fact || t.0.op != "not";
        let args =
            t.0.args
                .iter()
                .map(|a| self.norm(a, memo, child_facts))
                .collect();
        let rebuilt = node(t.0.sort.clone(), t.0.op.clone(), args);
        let mut r = self.local(rebuilt);
        if use_root_fact {
            if let Some(v) = self.facts.get(&r).copied() {
                r = boolv(v);
            }
            memo.insert(t.clone(), r.clone());
        }
        r
    }
    fn fixed(&mut self, t: &Term, use_root_fact: bool) -> Term {
        let mut r = t.clone();
        for _ in 0..8 {
            let n = self.norm(&r, &mut HashMap::new(), use_root_fact);
            if n == r {
                break;
            }
            r = n;
        }
        r
    }
    fn local(&mut self, t: Term) -> Term {
        let a = &t.0.args;
        let op = t.0.op.as_str();
        let mut result = None;
        if op == "not" {
            result = boolean(&a[0]).map(|x| boolv(!x));
            if a[0].0.op == "not" {
                result = Some(a[0].0.args[0].clone());
            } else if a[0].0.op == "=>" {
                // Exact polarity rule. Unlike a positive implication, its
                // negation asserts the guard and negates the consequent.
                result = Some(and(a[0].0.args[0].clone(), not(a[0].0.args[1].clone())));
            }
        } else if op == "ite" {
            result = boolean(&a[0]).map(|g| a[if g { 1 } else { 2 }].clone());
            if a[1] == a[2] {
                result = Some(a[1].clone());
            }
            if boolean(&a[1]) == Some(true) && boolean(&a[2]) == Some(false) {
                result = Some(a[0].clone());
            }
            if boolean(&a[1]) == Some(false) && boolean(&a[2]) == Some(true) {
                result = Some(not(a[0].clone()));
            }
        } else if op == "and" || op == "or" {
            let identity = op == "and";
            let mut items = vec![];
            flatten(&t, op, &mut items);
            if items.iter().any(|x| boolean(x) == Some(!identity)) {
                result = Some(boolv(!identity));
            } else {
                items.retain(|x| boolean(x) != Some(identity));
                items.sort();
                items.dedup();
                let set: HashSet<_> = items.iter().cloned().collect();
                if items
                    .iter()
                    .any(|x| x.0.op == "not" && set.contains(&x.0.args[0]))
                {
                    result = Some(boolv(!identity));
                } else {
                    result = Some(
                        items
                            .into_iter()
                            .reduce(|x, y| node(Sort::Bool, op, vec![x, y]))
                            .unwrap_or(boolv(identity)),
                    );
                }
            }
        } else if op == "=>" {
            result = match (boolean(&a[0]), boolean(&a[1])) {
                (Some(false), _) | (_, Some(true)) => Some(boolv(true)),
                (Some(true), _) => Some(a[1].clone()),
                (_, Some(false)) => Some(not(a[0].clone())),
                _ => None,
            };
            if a[0] == a[1] {
                result = Some(boolv(true));
            }
        } else if op == "xor" {
            result = match (boolean(&a[0]), boolean(&a[1])) {
                (Some(false), _) => Some(a[1].clone()),
                (_, Some(false)) => Some(a[0].clone()),
                (Some(true), _) => Some(not(a[1].clone())),
                (_, Some(true)) => Some(not(a[0].clone())),
                _ => None,
            };
            if a[0] == a[1] {
                result = Some(boolv(false));
            }
        } else if op == "=" {
            if a[0] == a[1] {
                result = Some(boolv(true));
            } else if let (Some(x), Some(y)) = (literal(&a[0]), literal(&a[1])) {
                result = Some(boolv(x == y));
            } else if let (Some(x), Some(y)) = (boolean(&a[0]), boolean(&a[1])) {
                result = Some(boolv(x == y));
            } else if a[0].0.sort == Sort::Bool {
                if let Some(v) = boolean(&a[0]) {
                    result = Some(if v { a[1].clone() } else { not(a[1].clone()) });
                } else if let Some(v) = boolean(&a[1]) {
                    result = Some(if v { a[0].clone() } else { not(a[0].clone()) });
                }
            }
            if result.is_none() && a[1] < a[0] {
                result = Some(eq(a[1].clone(), a[0].clone()));
            }
        } else if ["bvult", "bvule", "bvslt", "bvsle"].contains(&op) {
            if a[0] == a[1] {
                result = Some(boolv(op.ends_with("le")));
            } else if let (Some(x), Some(y), Sort::Bv(w)) =
                (literal(&a[0]), literal(&a[1]), &a[0].0.sort)
            {
                result = Some(boolv(match op {
                    "bvult" => x < y,
                    "bvule" => x <= y,
                    "bvslt" => signed(x, *w) < signed(y, *w),
                    _ => signed(x, *w) <= signed(y, *w),
                }));
            }
        } else if op == "select" {
            if a[0].0.op == "store" || a[0].0.op.starts_with("(as const ") {
                // Existing alias-safe memory laws, with bounded expansion.
                result = Some(memory_read(a[0].clone(), a[1].clone(), 32, &mut self.rules));
            } else if a[0].0.op == "ite" {
                let m = &a[0].0.args;
                result = Some(ite(
                    m[0].clone(),
                    node(t.0.sort.clone(), "select", vec![m[1].clone(), a[1].clone()]),
                    node(t.0.sort.clone(), "select", vec![m[2].clone(), a[1].clone()]),
                ));
            }
        } else if op == "store" {
            result = Some(memory_write(
                a[0].clone(),
                a[1].clone(),
                a[2].clone(),
                &mut self.rules,
            ));
        } else if let Sort::Bv(w) = t.0.sort {
            if op == "bvnot" {
                result = literal(&a[0]).map(|x| bv(w, !x));
            } else if op == "concat" {
                if let (Some(x), Some(y), Sort::Bv(yw)) =
                    (literal(&a[0]), literal(&a[1]), &a[1].0.sort)
                {
                    result = Some(bv(w, (x << yw) | y));
                }
            } else if let Some(params) = op.strip_prefix("(_ extract ") {
                let nums = params
                    .trim_end_matches(')')
                    .split_whitespace()
                    .filter_map(|x| x.parse::<u32>().ok())
                    .collect::<Vec<_>>();
                if nums.len() == 2 {
                    result = literal(&a[0]).map(|x| bv(w, x >> nums[1]));
                }
            } else if op.starts_with("(_ zero_extend ") {
                result = literal(&a[0]).map(|x| bv(w, x));
            } else if op.starts_with("(_ sign_extend ") {
                if let (Some(x), Sort::Bv(old)) = (literal(&a[0]), &a[0].0.sort) {
                    result = Some(bv(w, signed(x, *old) as u64));
                }
            } else if a.len() == 2 {
                if let (Some(x), Some(y)) = (literal(&a[0]), literal(&a[1])) {
                    result = match op {
                        "bvadd" => Some(bv(w, x.wrapping_add(y))),
                        "bvsub" => Some(bv(w, x.wrapping_sub(y))),
                        "bvmul" => Some(bv(w, x.wrapping_mul(y))),
                        "bvand" => Some(bv(w, x & y)),
                        "bvor" => Some(bv(w, x | y)),
                        "bvxor" => Some(bv(w, x ^ y)),
                        "bvshl" => Some(bv(w, if y >= w as u64 { 0 } else { x << y })),
                        "bvlshr" => Some(bv(w, if y >= w as u64 { 0 } else { x >> y })),
                        _ => None,
                    };
                }
                if result.is_none() {
                    match op {
                        "bvadd" | "bvmul" => {
                            let mut terms = vec![];
                            flatten(&t, op, &mut terms);
                            let mut c = if op == "bvadd" { 0u64 } else { 1u64 };
                            terms.retain(|x| {
                                if let Some(v) = literal(x) {
                                    c = if op == "bvadd" {
                                        c.wrapping_add(v)
                                    } else {
                                        c.wrapping_mul(v)
                                    };
                                    false
                                } else {
                                    true
                                }
                            });
                            c = literal(&bv(w, c)).unwrap();
                            if op == "bvmul" && c == 0 {
                                result = Some(bv(w, 0));
                            } else {
                                if c != if op == "bvadd" { 0 } else { 1 } || terms.is_empty() {
                                    terms.push(bv(w, c));
                                }
                                terms.sort();
                                result = terms
                                    .into_iter()
                                    .reduce(|x, y| node(Sort::Bv(w), op, vec![x, y]));
                            }
                        }
                        "bvsub" if a[0] == a[1] => result = Some(bv(w, 0)),
                        "bvsub" | "bvshl" | "bvlshr" if literal(&a[1]) == Some(0) => {
                            result = Some(a[0].clone())
                        }
                        "bvxor" if a[0] == a[1] => result = Some(bv(w, 0)),
                        "bvand" | "bvor" if a[0] == a[1] => result = Some(a[0].clone()),
                        "bvand" if literal(&a[0]) == Some(0) || literal(&a[1]) == Some(0) => {
                            result = Some(bv(w, 0))
                        }
                        "bvor" | "bvxor" if literal(&a[0]) == Some(0) => {
                            result = Some(a[1].clone())
                        }
                        "bvor" | "bvxor" if literal(&a[1]) == Some(0) => {
                            result = Some(a[0].clone())
                        }
                        _ => {}
                    }
                }
            }
        }
        if let Some(r) = result {
            if r != t {
                self.count(op);
            }
            r
        } else {
            t
        }
    }
    fn fact(&mut self, t: Term, value: bool) {
        if let Some(v) = boolean(&t) {
            if v != value {
                self.contradiction = true;
            }
            return;
        }
        if t.0.op == "not" {
            self.fact(t.0.args[0].clone(), !value);
            return;
        }
        if self.facts.get(&t).is_some_and(|v| *v != value) {
            self.contradiction = true;
        }
        self.facts.insert(t, value);
    }
    fn bind(&mut self, lhs: &Term, rhs: &Term) {
        if !lhs.0.op.starts_with('@') || lhs.0.sort != rhs.0.sort || self.subst.contains_key(lhs) {
            return;
        }
        let rhs = self.norm(rhs, &mut HashMap::new(), true);
        if !contains(&rhs, lhs) {
            self.subst.insert(lhs.clone(), rhs);
            self.count("assumed_equality_substitution");
        }
    }
    fn assumptions(&mut self, assumptions: &[Term]) -> usize {
        let mut rounds = 0;
        for _ in 0..8 {
            rounds += 1;
            let before = (self.facts.len(), self.subst.len());
            let mut terms = vec![];
            for t in assumptions {
                let t = self.fixed(t, false);
                flatten(&t, "and", &mut terms);
            }
            terms.sort();
            terms.dedup();
            for t in terms {
                if boolean(&t) == Some(false) {
                    self.contradiction = true;
                }
                if t.0.op == "=" {
                    // Orient variable-variable equality deterministically; only one direction.
                    let a = &t.0.args;
                    if a[0].0.op.starts_with('@') {
                        self.bind(&a[0], &a[1]);
                    } else if a[1].0.op.starts_with('@') {
                        self.bind(&a[1], &a[0]);
                    }
                } else if t.0.op.starts_with('@') && t.0.sort == Sort::Bool {
                    self.bind(&t, &boolv(true));
                } else if t.0.op == "not"
                    && t.0.args[0].0.op.starts_with('@')
                    && t.0.args[0].0.sort == Sort::Bool
                {
                    self.bind(&t.0.args[0], &boolv(false));
                }
                self.fact(t, true);
            }
            if self.contradiction || before == (self.facts.len(), self.subst.len()) {
                break;
            }
        }
        rounds
    }
}
/// Equivalent to t under these explicit assumptions; never infer them from samples.
pub fn simplify(t: &Term, assumptions: &[Term]) -> Term {
    let mut c = Context::default();
    c.assumptions(assumptions);
    if c.contradiction && t.0.sort == Sort::Bool {
        return boolv(false);
    }
    let mut r = t.clone();
    for _ in 0..4 {
        let n = c.norm(&r, &mut HashMap::new(), true);
        if n == r {
            break;
        }
        r = n;
    }
    r
}
/// Weighted unique DAG nodes. Data-dependent selects/ites have extra cost.
pub fn complexity(t: &Term) -> u64 {
    fn go(t: &Term, seen: &mut HashSet<Term>) -> u64 {
        if !seen.insert(t.clone()) {
            return 0;
        }
        let weight = match t.0.op.as_str() {
            "select" | "store" => 8,
            "ite" => 5,
            "bvmul" => 4,
            _ => 1,
        };
        weight + t.0.args.iter().map(|a| go(a, seen)).sum::<u64>()
    }
    go(t, &mut HashSet::new())
}
pub struct ProofAttempt {
    pub closed: bool,
    pub residual: Term,
    pub rules: BTreeMap<String, u64>,
    pub rounds: usize,
}
pub fn refute(t: &Term) -> ProofAttempt {
    let mut c = Context::default();
    let mut assumptions = vec![];
    let mut r = t.clone();
    for _ in 0..4 {
        let n = c.norm(&r, &mut HashMap::new(), true);
        if n == r {
            break;
        }
        r = n;
    }
    flatten(&r, "and", &mut assumptions);
    let rounds = c.assumptions(&assumptions);
    // Evaluate each asserted clause without using its own root truth as a shortcut.
    for a in &assumptions {
        let n = c.fixed(a, false);
        if boolean(&n) == Some(false) {
            c.contradiction = true;
        }
    }
    if c.contradiction {
        r = boolv(false);
    } else {
        // This residual is diagnostic only. It is not supplied to Z3: removed
        // assumptions need not preserve satisfiability without their context.
        let residuals = assumptions
            .iter()
            .map(|a| c.fixed(a, false))
            .collect::<Vec<_>>();
        r = residuals.into_iter().fold(boolv(true), and);
        r = c.local(r);
    }
    ProofAttempt {
        closed: boolean(&r) == Some(false),
        residual: r,
        rules: c.rules,
        rounds,
    }
}

#[cfg(test)]
mod implication_tests {
    use super::*;
    fn evaluate(t: &Term, a: bool, b: bool, c: bool) -> bool {
        match t.0.op.as_str() {
            "@a" => a,
            "@b" => b,
            "@c" => c,
            "true" => true,
            "false" => false,
            "not" => !evaluate(&t.0.args[0], a, b, c),
            "and" => evaluate(&t.0.args[0], a, b, c) && evaluate(&t.0.args[1], a, b, c),
            "or" => evaluate(&t.0.args[0], a, b, c) || evaluate(&t.0.args[1], a, b, c),
            "=>" => !evaluate(&t.0.args[0], a, b, c) || evaluate(&t.0.args[1], a, b, c),
            _ => panic!("independent Boolean oracle"),
        }
    }
    #[test]
    fn negated_implication_truth_table_and_nested_guards() {
        let (a, b, c) = (
            var("a".into(), Sort::Bool),
            var("b".into(), Sort::Bool),
            var("c".into(), Sort::Bool),
        );
        for term in [
            not(node(Sort::Bool, "=>", vec![a.clone(), b.clone()])),
            not(node(
                Sort::Bool,
                "=>",
                vec![
                    a.clone(),
                    node(Sort::Bool, "=>", vec![b.clone(), c.clone()]),
                ],
            )),
        ] {
            let simplified = simplify(&term, &[]);
            for bits in 0..8 {
                assert_eq!(
                    evaluate(&term, bits & 1 != 0, bits & 2 != 0, bits & 4 != 0),
                    evaluate(&simplified, bits & 1 != 0, bits & 2 != 0, bits & 4 != 0)
                );
            }
        }
        let neg = not(node(Sort::Bool, "=>", vec![a.clone(), b.clone()]));
        assert!(refute(&and(neg.clone(), not(a.clone()))).closed);
        assert!(refute(&and(neg.clone(), b.clone())).closed);
        assert!(!refute(&neg).closed);
        assert!(!refute(&and(node(Sort::Bool, "=>", vec![a.clone(), b]), not(a))).closed);
    }
}
