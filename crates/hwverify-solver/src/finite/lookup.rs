//! Bounded exact distribution of constant-index word lookups over ITE addresses.
use super::*;
use hwverify_ir::{eq, ite};

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
}
impl Rewrite {
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
