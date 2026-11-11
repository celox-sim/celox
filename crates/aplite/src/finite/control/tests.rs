use super::*;
use crate::{and, eq, ite, var};
fn or(x: Term, y: Term) -> Term {
    node(Sort::Bool, "or", vec![x, y])
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
fn asserted_polarity_and_units_match_exhaustive_boolean_oracle() {
    let vars = (0..4)
        .map(|i| var(format!("p{i}"), Sort::Bool))
        .collect::<Vec<_>>();
    let imp = |x, y| node(Sort::Bool, "=>", vec![x, y]);
    let forms = vec![
        not(imp(vars[0].clone(), imp(vars[1].clone(), vars[2].clone()))),
        and(vars[0].clone(), not(imp(vars[0].clone(), vars[1].clone()))),
        and(
            vars[0].clone(),
            not(imp(not(vars[0].clone()), vars[1].clone())),
        ),
        not(or(vars[0].clone(), not(vars[1].clone()))),
        or(vars[0].clone(), not(imp(vars[1].clone(), vars[2].clone()))),
        imp(vars[0].clone(), and(vars[1].clone(), vars[2].clone())),
        and(
            vars[0].clone(),
            imp(vars[0].clone(), and(vars[1].clone(), not(vars[3].clone()))),
        ),
    ];
    for formula in forms {
        let normalized = asserted_polarity(&formula, false, &mut test_budget(), 0).unwrap();
        let (folded, units) = forced_units(&formula, &mut test_budget()).unwrap();
        for mask in 0..16 {
            let values = (0..4)
                .map(|i| (format!("p{i}"), Scalar::Bool(mask & (1 << i) != 0)))
                .collect::<BTreeMap<_, _>>();
            let ev = |t: &Term| {
                evaluate(t, &values, &mut HashMap::new(), &mut test_budget(), 0).unwrap()
            };
            assert_eq!(ev(&formula), ev(&normalized));
            let satisfies_units = units
                .iter()
                .all(|(name, value)| values[name] == Scalar::Bool(*value));
            assert_eq!(
                ev(&formula),
                Scalar::Bool(satisfies_units && ev(&folded) == Scalar::Bool(true))
            );
        }
    }
}
#[test]
fn asserted_polarity_shared_dag_and_limits_are_bounded() {
    let p = var("shared".into(), Sort::Bool);
    let mut dag = not(node(Sort::Bool, "=>", vec![p.clone(), p]));
    for _ in 0..24 {
        dag = and(dag.clone(), dag);
    }
    let mut budget = test_budget();
    let normalized = asserted_polarity(&dag, false, &mut budget, 0).unwrap();
    assert!(
        budget.work < 100,
        "shared DAG must not expand exponentially"
    );
    let mut seen = HashSet::new();
    let mut todo = vec![normalized];
    while let Some(t) = todo.pop() {
        if seen.insert(t.clone()) {
            todo.extend(t.0.args.iter().cloned());
        }
    }
    assert!(seen.len() < 32);
    for limits in [
        Limits {
            max_terms: 10,
            ..Limits::default()
        },
        Limits {
            max_work: 10,
            ..Limits::default()
        },
        Limits {
            max_depth: 10,
            ..Limits::default()
        },
    ] {
        let mut budget = Budget {
            limits,
            ..test_budget()
        };
        assert!(asserted_polarity(&dag, false, &mut budget, 0).is_err());
    }
}
#[test]
fn forced_guard_route_keeps_original_replay_context_and_both_hints() {
    let (base, mut context) = many_controls(9, false);
    let p = var("mandatory".into(), Sort::Bool);
    let q = var("conclusion".into(), Sort::Bool);
    let tail = not(node(Sort::Bool, "=>", vec![p.clone(), q.clone()]));
    context.insert("mandatory".into(), p.clone());
    context.insert("conclusion".into(), q.clone());
    for conflict in [false, true] {
        let formula = and(
            base.clone(),
            if conflict {
                and(not(p.clone()), tail.clone())
            } else {
                tail.clone()
            },
        );
        for hint in [SearchHint::Sat, SearchHint::Unsat] {
            let out = super::super::solve_with_hint(&formula, &context, Limits::default(), hint);
            assert_eq!(
                out.verdict,
                if conflict {
                    Verdict::Unsat
                } else {
                    Verdict::Sat
                }
            );
            if !conflict {
                assert!(out.original_formula_validated);
                assert_eq!(out.context_values["mandatory"], Scalar::Bool(true));
                assert_eq!(out.context_values["conclusion"], Scalar::Bool(false));
            }
        }
    }
}
fn cache_fixture(varying: bool) -> (Term, Env) {
    let x = var("cache_word".into(), Sort::Bv(8));
    let c = var("cache_control".into(), Sort::Bool);
    let mut formula = boolv(true);
    for i in 0..48 {
        formula = and(
            formula,
            eq(var(format!("cache_padding{i}"), Sort::Bv(8)), bv(8, i)),
        );
    }
    let left = node(Sort::Bv(8), "bvadd", vec![x.clone(), bv(8, 1)]);
    let value = if varying {
        ite(c.clone(), bv(8, 2), bv(8, 1))
    } else {
        ite(c.clone(), bv(8, 1), bv(8, 1))
    };
    let right = node(Sort::Bv(8), "bvadd", vec![value, x]);
    formula = and(formula, not(eq(left, right)));
    (formula, Env::from([("selected case".into(), c)]))
}
#[test]
fn exact_unsat_cofactor_reuse_and_distinct_case_sat_replay() {
    let (formula, context) = cache_fixture(false);
    let out =
        super::super::solve_with_hint(&formula, &context, Limits::default(), SearchHint::Unsat);
    assert_eq!(out.verdict, Verdict::Unsat);
    assert_eq!(out.stats.control_cases, 2);
    assert_eq!(out.stats.control_cases_closed, 2);
    assert_eq!(out.stats.control_cache_hits, 1);
    assert_eq!(out.diagnostics()["control_cache_hits"], 1);
    for work in [out.stats.work - 1, out.stats.work] {
        let limited = super::super::solve_with_hint(
            &formula,
            &context,
            Limits {
                max_work: work,
                ..Limits::default()
            },
            SearchHint::Unsat,
        );
        assert_eq!(
            limited.verdict,
            if work < out.stats.work {
                Verdict::Unknown
            } else {
                Verdict::Unsat
            }
        );
    }
    // An intervening trivially false cofactor must not evict the proof.
    let skipped = and(not(var("aaa_skip".into(), Sort::Bool)), formula.clone());
    let reused =
        super::super::solve_with_hint(&skipped, &context, Limits::default(), SearchHint::Unsat);
    assert_eq!(reused.verdict, Verdict::Unsat);
    assert_eq!(reused.stats.control_cases_closed, 4);
    assert_eq!(reused.stats.control_cache_hits, 1);
    // Different source/context and later calls never inherit cached UNSAT.
    let (formula, context) = cache_fixture(true);
    for hint in [SearchHint::Unsat, SearchHint::Sat] {
        let out = super::super::solve_with_hint(&formula, &context, Limits::default(), hint);
        assert_eq!(out.verdict, Verdict::Sat);
        assert!(out.original_formula_validated);
        assert_eq!(out.context_values["selected case"], Scalar::Bool(true));
        assert_eq!(out.stats.control_cache_hits, 0);
    }
    let (formula, mut context) = cache_fixture(false);
    context.insert("invalid".into(), node(Sort::Bool, "unsupported", vec![]));
    let out =
        super::super::solve_with_hint(&formula, &context, Limits::default(), SearchHint::Unsat);
    assert_eq!(out.verdict, Verdict::Unknown);
    assert_eq!(out.stats.control_cache_hits, 0);
}
#[test]
fn cofactor_comparison_is_structural_bounded_and_shared_dag_safe() {
    let mut a = var("atom".into(), Sort::Bool);
    let mut b = var("atom".into(), Sort::Bool);
    for _ in 0..24 {
        a = and(a.clone(), a);
        b = and(b.clone(), b);
    }
    let mut budget = test_budget();
    assert!(same_formula(&a, &b, &mut budget).unwrap());
    assert!(budget.work < 100);
    assert!(!same_formula(&a, &not(b.clone()), &mut test_budget()).unwrap());
    assert!(
        !same_formula(
            &var("atom".into(), Sort::Bool),
            &var("atom".into(), Sort::Bv(1)),
            &mut test_budget()
        )
        .unwrap()
    );
    for limits in [
        Limits {
            max_terms: 10,
            ..Limits::default()
        },
        Limits {
            max_depth: 10,
            ..Limits::default()
        },
        Limits {
            max_work: budget.work - 1,
            ..Limits::default()
        },
    ] {
        assert!(
            same_formula(
                &a,
                &b,
                &mut Budget {
                    limits,
                    ..test_budget()
                }
            )
            .is_err()
        );
    }
}
fn fixture(last_only: bool) -> (Term, Env) {
    let controls = (0..3)
        .map(|i| var(format!("c{i}"), Sort::Bool))
        .collect::<Vec<_>>();
    let mut relation = boolv(true);
    for i in 0..48 {
        relation = and(relation, eq(var(format!("x{i}"), Sort::Bv(8)), bv(8, i)));
    }
    for (i, c) in controls.iter().enumerate() {
        relation = and(
            relation,
            node(
                Sort::Bool,
                "=>",
                vec![
                    c.clone(),
                    eq(var(format!("x{i}"), Sort::Bv(8)), bv(8, i as u64)),
                ],
            ),
        );
    }
    let formula = if last_only {
        let all = controls.iter().cloned().fold(boolv(true), and);
        and(
            relation,
            and(
                all,
                eq(
                    ite(
                        controls[0].clone(),
                        bv(8, 7),
                        var("dead_word".into(), Sort::Bv(8)),
                    ),
                    bv(8, 7),
                ),
            ),
        )
    } else {
        and(relation.clone(), not(relation))
    };
    (formula, Env::from([("flag".into(), controls[0].clone())]))
}
#[test]
fn all_control_cases_close_or_original_sat_model_is_reconstructed() {
    for sat in [false, true] {
        let (formula, context) = fixture(sat);
        let result =
            super::super::solve_with_hint(&formula, &context, Limits::default(), SearchHint::Unsat);
        assert_eq!(result.search_strategy, SearchStrategy::ControlCofactors);
        assert_eq!(result.stats.control_cases, 8);
        assert_eq!(result.stats.control_cases_closed, if sat { 7 } else { 8 });
        assert_eq!(
            result.verdict,
            if sat { Verdict::Sat } else { Verdict::Unsat }
        );
        assert_eq!(result.original_formula_validated, sat);
        assert!(result.stats.control_preprocess_work > 0);
        if sat {
            for i in 0..3 {
                assert_eq!(result.assignments[&format!("c{i}")], Scalar::Bool(true));
            }
            assert_eq!(result.context_values["flag"], Scalar::Bool(true));
            assert!(result.assignments.contains_key("dead_word"));
        }
        let limited = super::super::solve_with_hint(
            &formula,
            &context,
            Limits {
                max_work: result.stats.work - 1,
                ..Limits::default()
            },
            SearchHint::Unsat,
        );
        assert_eq!(limited.verdict, Verdict::Unknown);
        assert!(!limited.original_formula_validated);
        assert_eq!(
            super::super::solve_with_hint(
                &formula,
                &context,
                Limits {
                    max_work: result.stats.work,
                    ..Limits::default()
                },
                SearchHint::Unsat
            )
            .verdict,
            result.verdict
        );
    }
}
#[test]
fn one_and_two_control_word_muxes_replay_and_share_limits() {
    for count in 1..=2 {
        for sat in [false, true] {
            let controls = (0..count)
                .map(|i| var(format!("select{i}"), Sort::Bool))
                .collect::<Vec<_>>();
            let mut formula = boolv(true);
            for i in 0..48 {
                formula = and(formula, eq(var(format!("word{i}"), Sort::Bv(8)), bv(8, i)));
            }
            let mut mux = bv(8, 7);
            for control in &controls {
                mux = ite(control.clone(), mux, bv(8, 3));
            }
            formula = and(formula, eq(mux.clone(), bv(8, if sat { 7 } else { 9 })));
            let context = Env::from([("mux".into(), mux)]);
            let out = super::super::solve_with_hint(
                &formula,
                &context,
                Limits::default(),
                SearchHint::Unsat,
            );
            assert_eq!(out.search_strategy, SearchStrategy::ControlCofactors);
            assert_eq!(out.stats.control_cases, 1 << count);
            assert_eq!(
                out.stats.control_cases_closed,
                (1 << count) - usize::from(sat)
            );
            assert_eq!(out.verdict, if sat { Verdict::Sat } else { Verdict::Unsat });
            assert_eq!(out.original_formula_validated, sat);
            if sat {
                assert_eq!(out.context_values["mux"], Scalar::Bv { width: 8, value: 7 });
                for i in 0..count {
                    assert_eq!(out.assignments[&format!("select{i}")], Scalar::Bool(true));
                }
            }
            let limited = super::super::solve_with_hint(
                &formula,
                &context,
                Limits {
                    max_work: out.stats.work - 1,
                    ..Limits::default()
                },
                SearchHint::Unsat,
            );
            assert_eq!(limited.verdict, Verdict::Unknown);
            assert!(!limited.original_formula_validated);
            assert!(limited.assignments.is_empty());
        }
    }
}
#[test]
fn mux_routing_handles_many_controls_and_validates_dead_context() {
    for count in [0, 1, 7] {
        let mut formula = boolv(true);
        for i in 0..48 {
            formula = and(formula, eq(var(format!("word{i}"), Sort::Bv(8)), bv(8, i)));
        }
        let mut mux = var("value".into(), Sort::Bv(8));
        for i in 0..count {
            mux = ite(var(format!("c{i}"), Sort::Bool), mux, bv(8, i));
        }
        formula = and(formula, eq(mux, bv(8, 42)));
        let result = super::super::solve_with_hint(
            &formula,
            &Env::new(),
            Limits::default(),
            SearchHint::Unsat,
        );
        assert_eq!(result.verdict, Verdict::Sat);
        assert!(result.original_formula_validated);
        assert_eq!(
            result.search_strategy == SearchStrategy::ControlCofactors,
            count > 0
        );
        let context = Env::from([(
            "dead".into(),
            ite(
                boolv(false),
                node(Sort::Bv(8), "unsupported", vec![]),
                bv(8, 0),
            ),
        )]);
        let result =
            super::super::solve_with_hint(&formula, &context, Limits::default(), SearchHint::Unsat);
        assert_eq!(result.verdict, Verdict::Unknown);
        assert!(!result.original_formula_validated);
    }
}
fn many_controls(count: usize, unsat: bool) -> (Term, Env) {
    let controls = (0..count)
        .map(|i| var(format!("control{i:02}"), Sort::Bool))
        .collect::<Vec<_>>();
    let mut formula = boolv(true);
    for i in 0..48 {
        formula = and(formula, eq(var(format!("word{i}"), Sort::Bv(8)), bv(8, i)));
    }
    for control in &controls[..count - 1] {
        formula = and(
            formula,
            eq(ite(control.clone(), bv(8, 7), bv(8, 3)), bv(8, 7)),
        );
    }
    formula = and(formula, controls[count - 1].clone());
    if unsat {
        // This variable is outside the lexical three-control selection.
        formula = and(formula, not(controls[count - 1].clone()));
    }
    // Both names must survive in the original-model reconstruction even
    // though exact folding removes their last occurrences.
    formula = and(
        formula,
        eq(
            ite(
                boolv(false),
                var("dead_formula".into(), Sort::Bv(8)),
                bv(8, 0),
            ),
            bv(8, 0),
        ),
    );
    let context = Env::from([
        ("unselected".into(), controls[count - 1].clone()),
        (
            "dead_context".into(),
            ite(
                controls[0].clone(),
                bv(8, 9),
                var("context_only".into(), Sort::Bv(8)),
            ),
        ),
    ]);
    (formula, context)
}

#[test]
fn selected_subset_covers_all_cases_and_keeps_other_controls_symbolic() {
    for count in 7..=15 {
        for unsat in [false, true] {
            let (formula, context) = many_controls(count, unsat);
            let out = super::super::solve_with_hint(
                &formula,
                &context,
                Limits::default(),
                SearchHint::Unsat,
            );
            assert_eq!(out.search_strategy, SearchStrategy::ControlCofactors);
            if unsat {
                assert!(out.stats.control_variables.is_empty());
                assert_eq!(out.stats.control_cases, 1);
                assert_eq!(out.stats.control_cases_closed, 1);
            } else {
                assert_eq!(
                    out.stats.control_variables,
                    ["control00", "control01", "control02"]
                );
                assert_eq!(out.stats.control_cases, 8);
                assert_eq!(out.stats.control_cases_closed, 7);
            }
            assert_eq!(
                out.verdict,
                if unsat { Verdict::Unsat } else { Verdict::Sat }
            );
            assert_eq!(out.original_formula_validated, !unsat);
            if !unsat {
                assert_eq!(out.context_values["unselected"], Scalar::Bool(true));
                assert_eq!(
                    out.context_values["dead_context"],
                    Scalar::Bv { width: 8, value: 9 }
                );
                assert!(out.assignments.contains_key("dead_formula"));
                assert!(out.assignments.contains_key("context_only"));
                for i in 0..count {
                    assert_eq!(
                        out.assignments[&format!("control{i:02}")],
                        Scalar::Bool(true)
                    );
                }
            }
            for work in [out.stats.work - 1, out.stats.work] {
                let limited = super::super::solve_with_hint(
                    &formula,
                    &context,
                    Limits {
                        max_work: work,
                        ..Limits::default()
                    },
                    SearchHint::Unsat,
                );
                assert_eq!(
                    limited.verdict,
                    if work < out.stats.work {
                        Verdict::Unknown
                    } else {
                        out.verdict
                    }
                );
                if work < out.stats.work {
                    assert!(limited.assignments.is_empty());
                    assert!(!limited.original_formula_validated);
                }
            }
        }
    }
}

#[test]
fn subset_probes_validate_unselected_sorts_and_dead_original_nodes() {
    let (formula, context) = many_controls(15, false);
    let mut bad_sort = context.clone();
    bad_sort.insert("bad_sort".into(), var("control14".into(), Sort::Bv(8)));
    let mut bad_context = context.clone();
    bad_context.insert(
        "bad_context".into(),
        ite(
            boolv(false),
            node(Sort::Bv(8), "unsupported", vec![]),
            bv(8, 0),
        ),
    );
    for context in [bad_sort, bad_context] {
        let out =
            super::super::solve_with_hint(&formula, &context, Limits::default(), SearchHint::Unsat);
        assert_eq!(out.verdict, Verdict::Unknown);
        assert!(!out.original_formula_validated);
    }
    let malformed = and(
        formula,
        ite(
            boolv(false),
            node(Sort::Bool, "unsupported", vec![]),
            boolv(true),
        ),
    );
    let out =
        super::super::solve_with_hint(&malformed, &context, Limits::default(), SearchHint::Unsat);
    assert_eq!(out.verdict, Verdict::Unknown);
    assert!(!out.original_formula_validated);
}

#[test]
fn subset_scoring_is_deterministic_for_shared_and_negated_guards() {
    let (mut formula, context) = many_controls(7, false);
    let repeated = eq(
        ite(not(var("control06".into(), Sort::Bool)), bv(8, 3), bv(8, 7)),
        bv(8, 7),
    );
    for _ in 0..8 {
        formula = and(formula, repeated.clone());
    }
    formula = and(
        formula,
        eq(
            ite(
                not(var("control06".into(), Sort::Bool)),
                bv(8, 5),
                bv(8, 11),
            ),
            bv(8, 11),
        ),
    );
    let first =
        super::super::solve_with_hint(&formula, &context, Limits::default(), SearchHint::Unsat);
    let second =
        super::super::solve_with_hint(&formula, &context, Limits::default(), SearchHint::Unsat);
    assert_eq!(first.verdict, Verdict::Sat);
    assert!(first.original_formula_validated);
    // The mandatory control is propagated, so cannot consume a split slot.
    assert!(
        !first
            .stats
            .control_variables
            .contains(&"control06".to_string())
    );
    assert_eq!(
        first.stats.control_variables,
        second.stats.control_variables
    );
    assert_eq!(first.stats.work, second.stats.work);
    assert_eq!(first.stats.control_cases_closed, 7);
}

#[test]
fn control_splitting_never_bypasses_unsupported_terms_or_zero_limits() {
    let (formula, context) = fixture(false);
    for limits in [
        Limits {
            max_clauses: 0,
            ..Limits::default()
        },
        Limits {
            max_variables: 0,
            ..Limits::default()
        },
        Limits {
            timeout_ms: 0,
            ..Limits::default()
        },
    ] {
        assert_eq!(
            super::super::solve_with_hint(&formula, &context, limits, SearchHint::Unsat).verdict,
            Verdict::Unknown
        );
    }
    let malformed = and(formula, node(Sort::Bool, "unsupported", vec![]));
    assert_eq!(
        super::super::solve_with_hint(&malformed, &context, Limits::default(), SearchHint::Unsat)
            .verdict,
        Verdict::Unknown
    );
}
