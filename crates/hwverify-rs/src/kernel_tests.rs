use crate::{frontend::*, ir::*, kernel, solver::Check};
use serde_json::json;
use std::{collections::BTreeMap, fs, path::PathBuf};
fn dir(name: &str) -> PathBuf {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/test-evidence")
        .join(name);
    fs::create_dir_all(&p).unwrap();
    p
}
#[test]
fn kernel_closes_without_starting_external_solver() {
    let x = var("kernel_x".into(), Sort::Bv(8));
    let y = var("kernel_y".into(), Sort::Bv(8));
    let expression = node(Sort::Bv(8), "bvadd", vec![x.clone(), y.clone()]);
    let mut q = Check {
        z3: "MUST_NOT_BE_EXECUTED".into(),
        out: dir("custom_only"),
        reports: vec![],
    };
    q.query(
        "commutative_add",
        not(eq(expression, node(Sort::Bv(8), "bvadd", vec![y, x]))),
        false,
        &Env::new(),
    )
    .unwrap();
    assert_eq!(q.reports[0]["backend"], "structural_kernel");
    assert!(q.out.join("commutative_add.smt2").exists());
    assert!(q.out.join("commutative_add.kernel.json").exists());
}
#[test]
fn kernel_does_not_turn_unknown_into_sat_or_unsat() {
    let x = var("x".into(), Sort::Bv(8));
    assert!(!kernel::refute(&boolv(true)).closed);
    assert!(!kernel::refute(&eq(x.clone(), bv(8, 5))).closed);
    // No domain restriction from a disjunctive or negated comparison.
    let q = node(
        Sort::Bool,
        "or",
        vec![eq(x.clone(), bv(8, 0)), eq(x.clone(), bv(8, 1))],
    );
    assert!(!kernel::refute(&and(q, eq(x, bv(8, 1)))).closed);
}
#[test]
fn contextual_substitution_keeps_types_and_checks_cycles() {
    let x = var("x".into(), Sort::Bv(4));
    let y = var("y".into(), Sort::Bv(4));
    let impossible = vec![eq(x.clone(), bv(4, 0)), eq(x.clone(), bv(4, 1))];
    assert_eq!(kernel::simplify(&x, &impossible).0.sort, Sort::Bv(4));
    let cycle = and(
        eq(x.clone(), y.clone()),
        eq(
            y.clone(),
            node(Sort::Bv(4), "bvadd", vec![x.clone(), bv(4, 0)]),
        ),
    );
    assert!(!kernel::refute(&cycle).closed);
    let bad_cycle = and(
        eq(x.clone(), y.clone()),
        eq(y, node(Sort::Bv(4), "bvadd", vec![x, bv(4, 1)])),
    );
    // Acyclic substitution may leave this unsupported; it must terminate.
    let _ = kernel::refute(&bad_cycle);
}
#[test]
fn symbolic_memory_alias_is_split_not_assumed_unequal() {
    let m = var("m".into(), Sort::Mem(2, 4));
    let a = var("a".into(), Sort::Bv(2));
    let b = var("b".into(), Sort::Bv(2));
    let v = bv(4, 9);
    let write = node(
        Sort::Mem(2, 4),
        "store",
        vec![m.clone(), a.clone(), v.clone()],
    );
    let read = node(Sort::Bv(4), "select", vec![write, b.clone()]);
    let unchanged = node(Sort::Bv(4), "select", vec![m, b.clone()]);
    assert!(!kernel::refute(&not(eq(read.clone(), unchanged.clone()))).closed);
    assert!(kernel::refute(&and(eq(a.clone(), b.clone()), not(eq(read.clone(), v)))).closed);
    assert!(kernel::refute(&and(not(eq(a, b)), not(eq(read, unchanged)))).closed);
}
#[test]
fn structural_trial_scores_control_not_incidental_literals() {
    let s = symbols(
        &json!({"control":{"bv":4},"distract":{"bv":8},"memory":{"mem":[4,8]},"output":{"bv":8}}),
        "spec",
    )
    .unwrap();
    let mut clauses = vec![];
    let mut goal = boolv(true);
    for v in 0..6 {
        let c = eq(s["control"].clone(), bv(4, v));
        clauses.push(c.clone());
        let load = node(Sort::Bv(8), "select", vec![s["memory"].clone(), bv(4, v)]);
        goal = ite(c, eq(s["output"].clone(), load), goal);
    }
    let inv = and(
        node(
            Sort::Bool,
            "or",
            vec![clauses[0].clone(), clauses[1].clone()],
        ),
        node(Sort::Bool, "bvule", vec![s["distract"].clone(), bv(8, 255)]),
    );
    let sn = BTreeMap::from([("goal".into(), goal.clone())]);
    let (parts, plan) = crate::program::infer_partitions(&s, &inv, &sn, &goal);
    assert!(parts.is_some(), "{plan}");
    assert!(plan["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .any(|x| x["state"] == "control" && x["decision"] == "selected"));
    assert!(!plan["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .any(|x| x["state"] == "distract" && x["decision"] == "selected"));
    let mut covered = boolv(false);
    for (_, g) in parts.unwrap() {
        covered = node(Sort::Bool, "or", vec![covered, g]);
    }
    // Exact exhaustive 4-bit controls, all scalar values, checks the generated
    // complete complement rather than inferring a bound from the invariant.
    for control in 0..16 {
        for distract in 0..256 {
            assert_eq!(
                kernel::simplify(
                    &covered,
                    &[
                        eq(s["control"].clone(), bv(4, control)),
                        eq(s["distract"].clone(), bv(8, distract))
                    ]
                ),
                boolv(true)
            );
        }
    }
}
