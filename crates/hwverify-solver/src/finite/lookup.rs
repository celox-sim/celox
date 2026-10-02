//! Bounded exact distribution of constant-index word lookups over ITE addresses.
use super::*;
use hwverify_ir::{and, boolv, bv, eq, ite};

const MAX_ROWS: usize = 64;
const MAX_CREATED_NODES: usize = 4096;

/// Run before any rewriting, not merely on the nodes left in the result.
/// All source variables must also keep one consistent sort across the context.
pub(super) fn validate_source(formula: &Term, context: &Env, b: &mut Budget) -> Res<usize> {
    let mut todo = vec![(formula, 0usize)];
    todo.extend(context.values().map(|t| (t, 0)));
    let mut seen = HashSet::new();
    let mut variables = HashMap::new();
    while let Some((t, depth)) = todo.pop() {
        b.tick(1)?;
        if depth > b.limits.max_depth {
            return Err("finite solver term depth budget exhausted".into());
        }
        if !seen.insert(t.clone()) {
            continue;
        }
        if seen.len() > b.limits.max_terms {
            return Err("finite solver term budget exhausted".into());
        }
        if let Op::Variable(name) = operation(t)? {
            if let Some(previous) = variables.insert(name.clone(), t.0.sort.clone()) {
                if previous != t.0.sort {
                    return Err(format!("finite variable {name} has inconsistent sorts"));
                }
            }
        }
        todo.extend(t.0.args.iter().map(|arg| (arg, depth + 1)));
    }
    Ok(seen.len())
}

#[derive(Default)]
pub(super) struct Rewrite {
    pub source_nodes: usize,
    pub created: usize,
    pub rewrites: usize,
    pub word_rewrites: usize,
    pub word_created: usize,
    normalized: HashMap<Term, Term>,
}
impl Rewrite {
    fn room(&self, amount: usize, b: &Budget) -> bool {
        amount <= MAX_CREATED_NODES - self.created
            && self
                .source_nodes
                .saturating_add(self.created)
                .saturating_add(amount)
                <= b.limits.max_terms
    }
    fn charge(&mut self, amount: usize, b: &mut Budget) -> Res<()> {
        b.tick(amount as u64)?;
        self.created += amount;
        self.word_created += amount;
        Ok(())
    }
    pub(super) fn normalize(&mut self, t: &Term, b: &mut Budget, depth: usize) -> Res<Term> {
        b.tick(1)?;
        if depth > b.limits.max_depth {
            return Err("finite normalization depth budget exhausted".into());
        }
        if let Some(v) = self.normalized.get(t) {
            return Ok(v.clone());
        }
        if self.created >= MAX_CREATED_NODES {
            return Ok(t.clone());
        }
        let args =
            t.0.args
                .iter()
                .map(|a| self.normalize(a, b, depth + 1))
                .collect::<Res<Vec<_>>>()?;
        let mut n = if args == t.0.args {
            t.clone()
        } else if self.room(1, b) {
            self.charge(1, b)?;
            hwverify_ir::node(t.0.sort.clone(), t.0.op.clone(), args)
        } else {
            t.clone()
        };
        if matches!(operation(&n)?, Op::Ite) && matches!(n.0.sort, Sort::Bv(_)) {
            if n.0.args[1] == n.0.args[2] {
                n = n.0.args[1].clone();
            } else {
                if let Some(c) = self.compact(&n, b)? {
                    n = c;
                }
                if matches!(operation(&n)?, Op::Ite)
                    && matches!(operation(&n.0.args[1])?, Op::Ite)
                    && n.0.args[2] == n.0.args[1].0.args[2]
                    && self.room(2, b)
                {
                    self.charge(2, b)?;
                    self.word_rewrites += 1;
                    n = ite(
                        and(n.0.args[0].clone(), n.0.args[1].0.args[0].clone()),
                        n.0.args[1].0.args[1].clone(),
                        n.0.args[2].clone(),
                    );
                }
                if let Some(c) = self.read_write(&n, b)? {
                    n = self.normalize(&c, b, depth + 1)?;
                }
            }
        }
        self.normalized.insert(t.clone(), n.clone());
        Ok(n)
    }
    fn compact(&mut self, t: &Term, b: &mut Budget) -> Res<Option<Term>> {
        let mut rows = Vec::new();
        let mut keys = HashSet::new();
        let mut address = None;
        let mut current = t;
        let mut changed = false;
        while matches!(operation(current)?, Op::Ite) {
            b.tick(1)?;
            let condition = &current.0.args[0];
            if !matches!(operation(condition)?, Op::Eq) {
                break;
            }
            let (a, key) = match (
                operation(&condition.0.args[0])?,
                operation(&condition.0.args[1])?,
            ) {
                (_, Op::Word(k)) => (&condition.0.args[0], k),
                (Op::Word(k), _) => (&condition.0.args[1], k),
                _ => break,
            };
            if address.as_ref().is_some_and(|old| old != a) {
                break;
            }
            if rows.len() >= 64 {
                return Ok(None);
            }
            address = Some(a.clone());
            if keys.insert(key) {
                rows.push((key, current.0.args[1].clone()));
            } else {
                changed = true
            }
            current = &current.0.args[2];
        }
        if rows.len() < 2 {
            return Ok(None);
        }
        let address = address.unwrap();
        let Sort::Bv(width) = address.0.sort else {
            return Ok(None);
        };
        if width <= 6
            && rows.len() == (1usize << width) - 1
            && rows.iter().all(|r| r.1 == rows[0].1)
            && self.room(3, b)
        {
            let missing = (0..1u64 << width).find(|k| !keys.contains(k)).unwrap();
            self.charge(3, b)?;
            self.word_rewrites += 1;
            return Ok(Some(ite(
                eq(address, bv(width, missing)),
                current.clone(),
                rows[0].1.clone(),
            )));
        }
        let old = rows.len();
        rows.retain(|row| row.1 != *current);
        changed |= rows.len() != old;
        if !changed || !self.room(3 * rows.len(), b) {
            return Ok(None);
        }
        self.charge(3 * rows.len(), b)?;
        self.word_rewrites += 1;
        let mut result = current.clone();
        for (key, value) in rows.into_iter().rev() {
            result = ite(eq(address.clone(), bv(width, key)), value, result)
        }
        Ok(Some(result))
    }

    /// Exact finite read-over-indexed-write factoring. The complete finite
    /// selector domain must occur exactly once, including the implicit default.
    /// Every selected row has the same value and enable, and writes at its own key.
    pub(super) fn read_write(&mut self, t: &Term, b: &mut Budget) -> Res<Option<Term>> {
        if self.created >= MAX_CREATED_NODES
            || !matches!(operation(t)?, Op::Ite)
            || !matches!(t.0.sort, Sort::Bv(_))
        {
            return Ok(None);
        }
        fn keyed(t: &Term) -> Res<Option<(Term, u64)>> {
            if !matches!(operation(t)?, Op::Eq) {
                return Ok(None);
            }
            match (operation(&t.0.args[0])?, operation(&t.0.args[1])?) {
                (_, Op::Word(k)) if matches!(t.0.args[0].0.sort, Sort::Bv(_)) => {
                    Ok(Some((t.0.args[0].clone(), k)))
                }
                (Op::Word(k), _) if matches!(t.0.args[1].0.sort, Sort::Bv(_)) => {
                    Ok(Some((t.0.args[1].clone(), k)))
                }
                _ => Ok(None),
            }
        }
        let Some((address, _)) = keyed(&t.0.args[0])? else {
            return Ok(None);
        };
        let Sort::Bv(width) = address.0.sort else {
            return Ok(None);
        };
        if !(1..=6).contains(&width) {
            return Ok(None);
        }
        let count = 1usize << width;
        let mut keys = HashSet::new();
        let mut rows = Vec::new();
        let mut current = t;
        for _ in 0..count - 1 {
            b.tick(1)?;
            if !matches!(operation(current)?, Op::Ite) {
                return Ok(None);
            }
            let Some((a, k)) = keyed(&current.0.args[0])? else {
                return Ok(None);
            };
            if a != address || !keys.insert(k) {
                return Ok(None);
            }
            rows.push((k, current.0.args[1].clone()));
            current = &current.0.args[2];
        }
        let missing = (0..count as u64)
            .find(|k| !keys.contains(k))
            .expect("distinct full-domain prefix");
        rows.push((missing, current.clone()));
        let mut parsed = Vec::new();
        for (key, value) in &rows {
            b.tick(1)?;
            if !matches!(operation(value)?, Op::Ite) {
                return Ok(None);
            }
            let mut pending = vec![value.0.args[0].clone()];
            let mut guards = Vec::new();
            while let Some(g) = pending.pop() {
                b.tick(1)?;
                if pending.len() + guards.len() > 16 {
                    return Ok(None);
                }
                if matches!(operation(&g)?, Op::And) {
                    pending.push(g.0.args[1].clone());
                    pending.push(g.0.args[0].clone());
                } else if !matches!(operation(&g)?, Op::Bool(true)) {
                    guards.push(g);
                }
            }
            let candidates = guards
                .iter()
                .enumerate()
                .filter_map(|(i, g)| match keyed(g) {
                    Ok(Some((a, k))) if k == *key && a.0.sort == address.0.sort => Some((i, a)),
                    _ => None,
                })
                .collect::<Vec<_>>();
            if candidates.is_empty() {
                return Ok(None);
            }
            parsed.push((
                guards,
                candidates,
                value.0.args[1].clone(),
                value.0.args[2].clone(),
            ));
        }
        for (first_index, write_address) in &parsed[0].1 {
            b.tick((parsed.len() * 16 * 16) as u64)?;
            let enable = parsed[0]
                .0
                .iter()
                .enumerate()
                .filter(|(i, _)| i != first_index)
                .map(|(_, g)| g.clone())
                .collect::<Vec<_>>();
            let value = &parsed[0].2;
            let all = parsed.iter().all(|(guards, candidates, v, _)| {
                v == value
                    && candidates.iter().any(|(j, a)| {
                        a == write_address
                            && guards
                                .iter()
                                .enumerate()
                                .filter(|(i, _)| i != j)
                                .map(|(_, g)| g)
                                .eq(enable.iter())
                    })
            });
            if !all {
                continue;
            }
            let additional = 3 * (count - 1)
                + enable.len().saturating_sub(1)
                + 3
                + usize::from(enable.is_empty());
            if additional > MAX_CREATED_NODES - self.created
                || self
                    .source_nodes
                    .saturating_add(self.created)
                    .saturating_add(additional)
                    > b.limits.max_terms
            {
                return Ok(None);
            }
            self.charge(additional, b)?;
            let mut base = parsed[count - 1].3.clone();
            for (row, p) in rows[..count - 1].iter().zip(&parsed[..count - 1]).rev() {
                base = ite(eq(address.clone(), bv(width, row.0)), p.3.clone(), base);
            }
            let enable = enable
                .into_iter()
                .reduce(and)
                .unwrap_or_else(|| boolv(true));
            let result = ite(
                and(enable, eq(write_address.clone(), address)),
                value.clone(),
                base,
            );
            self.word_rewrites += 1;
            return Ok(Some(result));
        }
        Ok(None)
    }

    /// L(ite(g,a,b)) = ite(g,L(a),L(b)), where L is an ordered chain of
    /// comparisons against constants. Preserve all values, duplicate cases,
    /// and the default verbatim; they may themselves depend on the address.
    /// No totality, uniqueness, or table-name assumption is needed.
    pub(super) fn distribute(&mut self, t: &Term, b: &mut Budget) -> Res<Option<Term>> {
        if self.created >= MAX_CREATED_NODES {
            return Ok(None);
        }
        if !matches!(operation(t)?, Op::Ite) || !matches!(t.0.sort, Sort::Bv(_)) {
            return Ok(None);
        }
        let mut rows = Vec::new();
        let mut current = t;
        let mut address: Option<Term> = None;
        while matches!(operation(current)?, Op::Ite) {
            b.tick(1)?;
            let condition = &current.0.args[0];
            if !matches!(operation(condition)?, Op::Eq) {
                break;
            }
            let (a, constant) = if matches!(operation(&condition.0.args[1])?, Op::Word(_)) {
                (&condition.0.args[0], &condition.0.args[1])
            } else if matches!(operation(&condition.0.args[0])?, Op::Word(_)) {
                (&condition.0.args[1], &condition.0.args[0])
            } else {
                break;
            };
            if !matches!(operation(a)?, Op::Ite) || !matches!(a.0.sort, Sort::Bv(_)) {
                break;
            }
            if address.as_ref().is_some_and(|previous| previous != a) {
                break;
            }
            if rows.len() == MAX_ROWS {
                return Ok(None);
            }
            address = Some(a.clone());
            rows.push((constant.clone(), current.0.args[1].clone()));
            current = &current.0.args[2];
        }
        if rows.len() < 2 {
            return Ok(None);
        }
        // Two copies, each with one equality and one ITE per row, plus the
        // outer ITE. Both the fixed expansion allowance and max_terms include
        // ALL created nodes, even duplicates later merged by structural memo.
        let additional = 4 * rows.len() + 1;
        if additional > MAX_CREATED_NODES - self.created
            || self
                .source_nodes
                .saturating_add(self.created)
                .saturating_add(additional)
                > b.limits.max_terms
        {
            return Ok(None);
        }
        b.tick(additional as u64)?;
        let address = address.expect("at least two rows");
        let make = |value: &Term| {
            let mut result = current.clone();
            for (constant, branch) in rows.iter().rev() {
                result = ite(eq(value.clone(), constant.clone()), branch.clone(), result);
            }
            result
        };
        let result = ite(
            address.0.args[0].clone(),
            make(&address.0.args[1]),
            make(&address.0.args[2]),
        );
        self.created += additional;
        self.rewrites += 1;
        Ok(Some(result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hwverify_ir::{and, boolv, bv, node, not, var};

    fn budget() -> Budget {
        Budget {
            limits: Limits::default(),
            start: Instant::now(),
            work: 0,
            time_check_in: 0,
        }
    }
    fn lookup(address: &Term, rows: &[(u64, Term)], default: Term, reverse: bool) -> Term {
        let Sort::Bv(width) = address.0.sort else {
            panic!("word address")
        };
        rows.iter().rev().fold(default, |tail, (key, value)| {
            let constant = bv(width, *key);
            let condition = if reverse {
                eq(constant, address.clone())
            } else {
                eq(address.clone(), constant)
            };
            ite(condition, value.clone(), tail)
        })
    }
    fn fixture() -> (Term, Term, Term, Term) {
        let g = var("g".into(), Sort::Bool);
        let a = var("a".into(), Sort::Bv(2));
        let b = var("b".into(), Sort::Bv(2));
        let address = ite(g, a.clone(), b.clone());
        let rows = vec![(0, bv(2, 1)), (1, bv(2, 2))];
        (lookup(&address, &rows, bv(2, 3), false), address, a, b)
    }
    fn source_checked(t: &Term, context: &Env, b: &mut Budget) -> Rewrite {
        Rewrite {
            source_nodes: validate_source(t, context, b).unwrap(),
            ..Rewrite::default()
        }
    }

    #[test]
    fn duplicate_keys_missing_rows_and_address_dependent_values_are_exact() {
        let (_, address, _, _) = fixture();
        let g = var("g".into(), Sort::Bool);
        let value = ite(g.clone(), address.clone(), bv(2, 2));
        // First key 0 wins; keys 2/3 use the address-dependent default.
        let rows = vec![(0, value), (1, bv(2, 0)), (0, bv(2, 3))];
        for reverse in [false, true] {
            let original = lookup(&address, &rows, address.clone(), reverse);
            let mut budget = budget();
            let mut state = source_checked(&original, &Env::new(), &mut budget);
            let rewritten = state.distribute(&original, &mut budget).unwrap().unwrap();
            assert_eq!(state.created, 13);
            for g in [false, true] {
                for a in 0..4 {
                    for b in 0..4 {
                        let values = BTreeMap::from([
                            ("g".into(), Scalar::Bool(g)),
                            ("a".into(), Scalar::Bv { width: 2, value: a }),
                            ("b".into(), Scalar::Bv { width: 2, value: b }),
                        ]);
                        let chosen = if g { a } else { b };
                        let expected = if chosen == 0 {
                            if g {
                                chosen
                            } else {
                                2
                            }
                        } else if chosen == 1 {
                            0
                        } else {
                            chosen
                        };
                        for term in [&original, &rewritten] {
                            assert_eq!(
                                evaluate(term, &values, &mut HashMap::new(), &mut budget, 0)
                                    .unwrap(),
                                Scalar::Bv {
                                    width: 2,
                                    value: expected
                                }
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn non_tables_different_addresses_and_single_rows_are_left_unchanged() {
        let (table, address, a, b) = fixture();
        let mut budget = budget();
        let mut state = Rewrite::default();
        let other = ite(var("h".into(), Sort::Bool), a.clone(), b.clone());
        let mixed = ite(
            eq(address.clone(), bv(2, 0)),
            bv(2, 1),
            ite(eq(other, bv(2, 1)), bv(2, 2), bv(2, 3)),
        );
        for t in [
            a.clone(),
            ite(var("g".into(), Sort::Bool), a, b),
            lookup(&address, &[(0, bv(2, 1))], bv(2, 0), false),
            mixed,
        ] {
            assert!(state.distribute(&t, &mut budget).unwrap().is_none());
        }
        assert_eq!(state.created, 0);
        assert!(state.distribute(&table, &mut budget).unwrap().is_some());
    }

    #[test]
    fn valid_prefix_keeps_an_unrelated_conditional_default() {
        let (_, address, a, b) = fixture();
        let default = ite(var("h".into(), Sort::Bool), a, b);
        let table = lookup(
            &address,
            &[(0, bv(2, 1)), (1, bv(2, 2))],
            default.clone(),
            false,
        );
        let mut budget = budget();
        let rewritten = Rewrite::default()
            .distribute(&table, &mut budget)
            .unwrap()
            .unwrap();
        for branch in &rewritten.0.args[1..] {
            assert_eq!(branch.0.args[2].0.args[2], default);
        }
        assert_eq!(
            super::super::solve(&not(eq(table, rewritten)), &Env::new(), Limits::default()).verdict,
            Verdict::Unsat
        );
    }

    #[test]
    fn row_and_cumulative_expansion_caps_fall_back_to_original_encoding() {
        let g = var("g".into(), Sort::Bool);
        let address = ite(
            g,
            var("a".into(), Sort::Bv(8)),
            var("b".into(), Sort::Bv(8)),
        );
        for n in [1, 2, 64, 65] {
            let rows = (0..n)
                .map(|i| (i as u64, bv(8, i as u64)))
                .collect::<Vec<_>>();
            let table = lookup(&address, &rows, bv(8, 255), false);
            let mut budget = budget();
            let mut state = source_checked(&table, &Env::new(), &mut budget);
            assert_eq!(
                state.distribute(&table, &mut budget).unwrap().is_some(),
                matches!(n, 2 | 64)
            );
            assert!(state.created <= MAX_CREATED_NODES);
        }
        let (table, _, _, _) = fixture();
        let mut budget = budget();
        let mut state = source_checked(&table, &Env::new(), &mut budget);
        while state.distribute(&table, &mut budget).unwrap().is_some() {}
        assert!(state.created <= MAX_CREATED_NODES);
        assert!(MAX_CREATED_NODES - state.created < 9);
        let mut term_limited = source_checked(&table, &Env::new(), &mut budget);
        budget.limits.max_terms = term_limited.source_nodes + 8;
        assert!(term_limited
            .distribute(&table, &mut budget)
            .unwrap()
            .is_none());
        assert_eq!(term_limited.created, 0);
        let before = budget.work;
        budget.limits.max_terms = 100_000;
        budget.limits.max_work = before + 2; // scan fits; construction does not
        assert!(term_limited.distribute(&table, &mut budget).is_err());
        assert_eq!(term_limited.created, 0);
    }

    #[test]
    fn nested_shared_addresses_prove_and_sat_models_replay_original_context() {
        let (_, address, a, _) = fixture();
        let nested = ite(var("h".into(), Sort::Bool), address.clone(), a);
        let rows = vec![
            (0, var("v0".into(), Sort::Bv(2))),
            (1, var("v1".into(), Sort::Bv(2))),
        ];
        let table = lookup(&nested, &rows, bv(2, 3), false);
        let expected = ite(
            nested.0.args[0].clone(),
            lookup(&nested.0.args[1], &rows, bv(2, 3), false),
            lookup(&nested.0.args[2], &rows, bv(2, 3), false),
        );
        let proof = super::super::solve(
            &not(eq(table.clone(), expected)),
            &Env::new(),
            Limits::default(),
        );
        assert_eq!(proof.verdict, Verdict::Unsat);
        assert!(proof.stats.lookup_rewrites >= 2);
        let formula = eq(table.clone(), bv(2, 2));
        let context = Env::from([("table".into(), table), ("address".into(), nested)]);
        for hint in [SearchHint::Sat, SearchHint::Unsat] {
            let result = super::super::solve_with_hint(&formula, &context, Limits::default(), hint);
            assert_eq!(result.verdict, Verdict::Sat);
            assert!(result.original_formula_validated);
            assert_eq!(
                result.context_values["table"],
                Scalar::Bv { width: 2, value: 2 }
            );
            for name in ["g", "h", "a", "b", "v0", "v1"] {
                assert!(result.assignments.contains_key(name));
            }
            let limited = super::super::solve_with_hint(
                &formula,
                &context,
                Limits {
                    max_work: result.stats.work - 1,
                    ..Limits::default()
                },
                hint,
            );
            assert_eq!(limited.verdict, Verdict::Unknown);
            assert!(!limited.original_formula_validated);
            let exact = super::super::solve_with_hint(
                &formula,
                &context,
                Limits {
                    max_work: result.stats.work,
                    ..Limits::default()
                },
                hint,
            );
            assert_eq!(exact.verdict, Verdict::Sat);
        }
    }

    #[test]
    fn entire_original_and_context_are_validated_before_rewrite_for_both_hints() {
        let (_, address, _, _) = fixture();
        let unsupported = node(Sort::Bv(2), "unsupported", vec![]);
        let malformed_address = node(Sort::Bv(2), "ite", vec![bv(2, 0), bv(2, 0), bv(2, 1)]);
        let inconsistent = var("a".into(), Sort::Bv(3));
        let valid = lookup(&address, &[(0, bv(2, 0)), (1, bv(2, 1))], bv(2, 2), false);
        let cases = [
            (
                eq(
                    lookup(
                        &address,
                        &[(0, bv(2, 0)), (0, unsupported.clone())],
                        bv(2, 1),
                        false,
                    ),
                    bv(2, 0),
                ),
                Env::new(),
            ),
            (
                eq(
                    lookup(
                        &malformed_address,
                        &[(0, bv(2, 0)), (1, bv(2, 1))],
                        bv(2, 2),
                        false,
                    ),
                    bv(2, 0),
                ),
                Env::new(),
            ),
            (
                and(boolv(false), eq(valid.clone(), bv(2, 0))),
                Env::from([("dead".into(), unsupported)]),
            ),
            (
                eq(valid, bv(2, 0)),
                Env::from([("mismatch".into(), inconsistent)]),
            ),
        ];
        for hint in [SearchHint::Sat, SearchHint::Unsat] {
            for (formula, context) in &cases {
                let result =
                    super::super::solve_with_hint(formula, context, Limits::default(), hint);
                assert_eq!(result.verdict, Verdict::Unknown);
                assert_eq!(result.stats.lookup_rewrites, 0);
                assert!(!result.original_formula_validated);
            }
        }
    }
}

#[cfg(test)]
mod word_normalization_tests {
    use super::*;
    use hwverify_ir::{node, not, var};
    fn b() -> Budget {
        Budget {
            limits: Limits::default(),
            start: Instant::now(),
            work: 0,
            time_check_in: 0,
        }
    }
    fn w(n: &str, width: u32) -> Term {
        var(n.into(), Sort::Bv(width))
    }
    fn selector(a: &Term, rows: &[(u64, Term)], default: Term) -> Term {
        rows.iter().rev().fold(default, |tail, (k, v)| {
            ite(
                eq(
                    a.clone(),
                    bv(
                        match a.0.sort {
                            Sort::Bv(w) => w,
                            _ => panic!(),
                        },
                        *k,
                    ),
                ),
                v.clone(),
                tail,
            )
        })
    }
    // Tiny independent semantics for this normalization fragment, without using
    // the solver operation decoder, bit-blaster, or model evaluator.
    fn eval(t: &Term, a: &BTreeMap<String, u64>) -> u64 {
        if let Some(n) = t.0.op.strip_prefix('@') {
            return a[n];
        }
        if t.0.op.starts_with("(_ bv") {
            return t.0.op[5..].split(' ').next().unwrap().parse().unwrap();
        }
        match t.0.op.as_str() {
            "true" => 1,
            "false" => 0,
            "=" => u64::from(eval(&t.0.args[0], a) == eval(&t.0.args[1], a)),
            "and" => u64::from(eval(&t.0.args[0], a) != 0 && eval(&t.0.args[1], a) != 0),
            "not" => u64::from(eval(&t.0.args[0], a) == 0),
            "ite" => eval(&t.0.args[if eval(&t.0.args[0], a) != 0 { 1 } else { 2 }], a),
            x => panic!("unsupported independent operation {x}"),
        }
    }
    fn normalized(t: &Term) -> (Term, Rewrite) {
        let mut b = b();
        let mut r = Rewrite {
            source_nodes: validate_source(t, &Env::new(), &mut b).unwrap(),
            ..Rewrite::default()
        };
        let n = r.normalize(t, &mut b, 0).unwrap();
        (n, r)
    }
    fn fixture(width: u32, priority: bool) -> Term {
        let count = 1 << width;
        let rd = w("rd", width);
        let rs = w("rs", width);
        let v = w("v", width);
        let en = var("en".into(), Sort::Bool);
        let values = (0..count)
            .map(|k| {
                let old = w(&format!("r{k}"), width);
                if priority {
                    let rows = (0..count - 1)
                        .map(|j| (j, if j == k { v.clone() } else { old.clone() }))
                        .collect::<Vec<_>>();
                    ite(
                        en.clone(),
                        selector(
                            &rd,
                            &rows,
                            if k == count - 1 {
                                v.clone()
                            } else {
                                old.clone()
                            },
                        ),
                        old,
                    )
                } else {
                    ite(
                        and(en.clone(), eq(rd.clone(), bv(width, k))),
                        v.clone(),
                        old,
                    )
                }
            })
            .collect::<Vec<_>>();
        selector(
            &rs,
            &(0..count - 1)
                .map(|i| (i, values[i as usize].clone()))
                .collect::<Vec<_>>(),
            values[count as usize - 1].clone(),
        )
    }
    #[test]
    fn read_write_and_priority_lowering_exhaustive_small_domains() {
        for width in 1..=2 {
            let count = 1u64 << width;
            for priority in [false, true] {
                let t = fixture(width, priority);
                let (n, state) = normalized(&t);
                assert!(state.word_rewrites > 0);
                assert!(state.created <= 4096);
                for cells in 0..1u64 << (count as u32 * width) {
                    let mut a = BTreeMap::new();
                    for i in 0..count {
                        a.insert(format!("r{i}"), (cells >> (i as u32 * width)) & (count - 1));
                    }
                    for rd in 0..count {
                        for rs in 0..count {
                            for v in 0..count {
                                for en in 0..2 {
                                    a.extend([
                                        ("rd".into(), rd),
                                        ("rs".into(), rs),
                                        ("v".into(), v),
                                        ("en".into(), en),
                                    ]);
                                    assert_eq!(eval(&t, &a), eval(&n, &a));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn compaction_preserves_duplicates_missing_rows_and_address_dependent_values() {
        let a = w("a", 2);
        let x = w("x", 2);
        let g = var("g".into(), Sort::Bool);
        let pool = [
            a.clone(),
            x.clone(),
            bv(2, 0),
            ite(g.clone(), a.clone(), x.clone()),
        ];
        let mut seed = 91823u64;
        for _ in 0..300 {
            let mut next = || {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                seed
            };
            let count = 2 + next() % 7;
            let rows = (0..count)
                .map(|_| (next() % 4, pool[(next() % 4) as usize].clone()))
                .collect::<Vec<_>>();
            let default = pool[(next() % 4) as usize].clone();
            let t = selector(&a, &rows, default);
            let (n, _) = normalized(&t);
            for av in 0..4 {
                for xv in 0..4 {
                    for gv in 0..2 {
                        let values =
                            BTreeMap::from([("a".into(), av), ("x".into(), xv), ("g".into(), gv)]);
                        assert_eq!(eval(&t, &values), eval(&n, &values));
                    }
                }
            }
        }
    }
    #[test]
    fn incomplete_duplicate_and_mismatched_writes_do_not_factor() {
        let t = fixture(2, false);
        let mut bads = vec![t.0.args[2].clone()];
        let mut args = t.0.args.clone();
        args[0] = eq(w("rs", 2), bv(2, 1));
        bads.push(node(t.0.sort.clone(), "ite", args));
        let mut args = t.0.args.clone();
        args[1] = ite(
            and(var("other".into(), Sort::Bool), eq(w("rd", 2), bv(2, 0))),
            w("v", 2),
            w("r0", 2),
        );
        bads.push(node(t.0.sort.clone(), "ite", args));
        for t in bads {
            let mut b = b();
            assert!(Rewrite::default().read_write(&t, &mut b).unwrap().is_none())
        }
    }
    #[test]
    fn removed_selector_variables_remain_available_for_original_sat_replay() {
        let a = w("a", 2);
        let t = selector(&a, &[(0, bv(2, 3)), (1, bv(2, 3)), (2, bv(2, 3))], bv(2, 3));
        for hint in [SearchHint::Sat, SearchHint::Unsat] {
            for context in [Env::new(), Env::from([("table".into(), t.clone())])] {
                let q = eq(t.clone(), bv(2, 3));
                let result = super::super::solve_with_hint(&q, &context, Limits::default(), hint);
                assert_eq!(result.verdict, Verdict::Sat, "{}", result.diagnostics());
                assert!(result.original_formula_validated);
                assert!(result.assignments.contains_key("a"));
            }
        }
    }
    #[test]
    fn normalization_caps_and_api_work_boundary_remain_fail_closed() {
        let t = fixture(2, true);
        let mut budget = b();
        let source = validate_source(&t, &Env::new(), &mut budget).unwrap();
        for allowance in [0, 1, 2, 3, 4, 16, 64] {
            let mut budget = b();
            budget.limits.max_terms = source + allowance;
            let mut r = Rewrite {
                source_nodes: source,
                ..Rewrite::default()
            };
            let _ = r.normalize(&t, &mut budget, 0).unwrap();
            assert!(r.created <= allowance);
        }
        let mut state = Rewrite {
            source_nodes: source,
            created: 4096,
            ..Rewrite::default()
        };
        assert_eq!(state.normalize(&t, &mut b(), 0).unwrap(), t);
        let (n, _) = normalized(&t);
        let q = not(eq(t, n));
        let full = super::super::solve(&q, &Env::new(), Limits::default());
        assert_eq!(full.verdict, Verdict::Unsat);
        for delta in [0, 1] {
            let result = super::super::solve(
                &q,
                &Env::new(),
                Limits {
                    max_work: full.stats.work - delta,
                    ..Limits::default()
                },
            );
            assert_eq!(
                result.verdict,
                if delta == 0 {
                    Verdict::Unsat
                } else {
                    Verdict::Unknown
                }
            );
            assert!(!result.original_formula_validated);
        }
    }
}
