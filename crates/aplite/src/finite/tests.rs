use super::*;
use crate::{and, boolv, bv, eq, ite, node, not, var};
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
    let unsupported = Env::from([("array".into(), node(Sort::Mem(2, 2), "unsupported", vec![]))]);
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
        assert!(
            split_choices(&opaque, &blast, &mut budget)
                .unwrap()
                .is_empty()
        );
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
                assert!(
                    clauses
                        .iter()
                        .all(|c| c.iter().any(|&p| truth(&sat.values, p) > 0))
                );
            }
        }
    }
}
