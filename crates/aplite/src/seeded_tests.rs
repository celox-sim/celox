use crate::{
    Env, Limits, Scalar, Sort, Verdict, and, boolv, bv, eq, node, not, solve, solve_seeded, var,
};
use std::collections::BTreeSet;

#[test]
fn seeds_reproduce_and_vary_unconstrained_inputs() {
    let x = var("x".into(), Sort::Bv(64));
    let flag = var("flag".into(), Sort::Bool);
    let context = Env::from([("x".into(), x), ("flag".into(), flag)]);
    let mut words = BTreeSet::new();
    let mut flags = BTreeSet::new();
    for seed in 0..64 {
        let a = solve_seeded(&boolv(true), &context, Limits::default(), seed);
        let b = solve_seeded(&boolv(true), &context, Limits::default(), seed);
        assert_eq!(a.verdict, Verdict::Sat);
        assert!(a.original_formula_validated);
        assert_eq!(a.assignments, b.assignments);
        assert_eq!(a.context_values, b.context_values);
        let Scalar::Bv { value, .. } = a.assignments["x"] else {
            panic!()
        };
        let Scalar::Bool(flag) = a.assignments["flag"] else {
            panic!()
        };
        words.insert(value);
        flags.insert(flag);
    }
    assert_eq!(words.len(), 64);
    assert_eq!(flags.len(), 2);
}

#[test]
fn constrained_models_preserve_relations_and_context_expressions() {
    let x = var("x".into(), Sort::Bv(4));
    let y = var("y".into(), Sort::Bv(4));
    let sum = node(Sort::Bv(4), "bvadd", vec![x.clone(), y.clone()]);
    let formula = and(
        eq(sum.clone(), bv(4, 9)),
        node(Sort::Bool, "bvult", vec![x.clone(), bv(4, 4)]),
    );
    let context = Env::from([("sum".into(), sum)]);
    let mut seen = BTreeSet::new();
    for seed in 0..128 {
        let result = solve_seeded(&formula, &context, Limits::default(), seed);
        assert_eq!(result.verdict, Verdict::Sat);
        assert!(result.original_formula_validated);
        let Scalar::Bv { value: x, .. } = result.assignments["x"] else {
            panic!()
        };
        let Scalar::Bv { value: y, .. } = result.assignments["y"] else {
            panic!()
        };
        assert!(x < 4);
        assert_eq!((x + y) & 15, 9);
        assert_eq!(
            result.context_values["sum"],
            Scalar::Bv { width: 4, value: 9 }
        );
        seen.insert(x);
    }
    assert_eq!(seen.len(), 4);
}

#[test]
fn seeds_cannot_change_unsatisfiability_or_unique_models() {
    let x = var("x".into(), Sort::Bv(8));
    let fixed = eq(x.clone(), bv(8, 37));
    let impossible = and(fixed.clone(), not(fixed.clone()));
    for seed in [0, 1, 42, u64::MAX] {
        assert_eq!(
            solve_seeded(&impossible, &Env::new(), Limits::default(), seed).verdict,
            Verdict::Unsat
        );
        let result = solve_seeded(&fixed, &Env::new(), Limits::default(), seed);
        assert_eq!(result.verdict, Verdict::Sat);
        assert_eq!(
            result.assignments["x"],
            Scalar::Bv {
                width: 8,
                value: 37
            }
        );
    }
    assert_eq!(
        solve(&impossible, &Env::new(), Limits::default()).verdict,
        Verdict::Unsat
    );
}

#[test]
fn unsupported_input_and_exhausted_budgets_are_unknown_without_models() {
    let x = var("x".into(), Sort::Bv(8));
    let context = Env::from([("x".into(), x)]);
    for limits in [
        Limits {
            max_work: 0,
            ..Limits::default()
        },
        Limits {
            max_variables: 1,
            ..Limits::default()
        },
        Limits {
            max_clauses: 0,
            ..Limits::default()
        },
    ] {
        let result = solve_seeded(&boolv(true), &context, limits, 0);
        assert_eq!(result.verdict, Verdict::Unknown);
        assert!(!result.original_formula_validated);
        assert!(result.assignments.is_empty());
    }
    let memory = Env::from([("m".into(), var("m".into(), Sort::Mem(4, 8)))]);
    let result = solve_seeded(&boolv(true), &memory, Limits::default(), 0);
    assert_eq!(result.verdict, Verdict::Unknown);
    assert!(result.reason.unwrap().contains("scalar"));
    let invalid = node(Sort::Bool, "unsupported", vec![]);
    assert_eq!(
        solve_seeded(&invalid, &Env::new(), Limits::default(), 0).verdict,
        Verdict::Unknown
    );
}

#[test]
fn seeded_search_matches_every_two_boolean_truth_table() {
    let x = var("x".into(), Sort::Bool);
    let y = var("y".into(), Sort::Bool);
    let context = Env::from([("x".into(), x.clone()), ("y".into(), y.clone())]);
    for mask in 0..16u8 {
        let mut formula = boolv(false);
        for row in 0..4u8 {
            if mask & (1 << row) != 0 {
                let case = and(
                    eq(x.clone(), boolv(row & 1 != 0)),
                    eq(y.clone(), boolv(row & 2 != 0)),
                );
                formula = node(Sort::Bool, "or", vec![formula, case]);
            }
        }
        for seed in 0..16 {
            let result = solve_seeded(&formula, &context, Limits::default(), seed);
            if mask == 0 {
                assert_eq!(result.verdict, Verdict::Unsat);
            } else {
                assert_eq!(result.verdict, Verdict::Sat);
                assert!(result.original_formula_validated);
                let Scalar::Bool(x) = result.assignments["x"] else {
                    panic!()
                };
                let Scalar::Bool(y) = result.assignments["y"] else {
                    panic!()
                };
                let row = u8::from(x) | (u8::from(y) << 1);
                assert_ne!(mask & (1 << row), 0);
            }
        }
    }
}
