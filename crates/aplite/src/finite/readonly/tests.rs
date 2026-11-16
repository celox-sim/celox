use super::*;
use crate::bv;
fn mem(n: &str) -> Term {
    var(n.into(), Sort::Mem(1, 1))
}
fn read(a: Term, i: Term) -> Term {
    node(Sort::Bv(1), "select", vec![a, i])
}
fn check(q: Term, ctx: Env, expected: Verdict) -> Outcome {
    let mut last = None;
    for hint in [SearchHint::Sat, SearchHint::Unsat] {
        let r = solve_with_hint(&q, &ctx, Limits::default(), hint);
        assert_eq!(r.verdict, expected, "{}: {:?}", hint.as_str(), r.reason);
        assert_eq!(r.original_formula_validated, expected == Verdict::Sat);
        last = Some(r);
    }
    last.unwrap()
}
fn write(a: Term, i: Term, v: Term) -> Term {
    node(a.0.sort.clone(), "store", vec![a, i, v])
}
#[test]
fn store_only_equalities_observe_every_changed_cell() {
    let a = mem("a");
    check(
        and(
            eq(
                write(a.clone(), bv(1, 0), bv(1, 1)),
                write(a.clone(), bv(1, 1), bv(1, 1)),
            ),
            eq(
                write(a.clone(), bv(1, 0), bv(1, 0)),
                write(a, bv(1, 1), bv(1, 0)),
            ),
        ),
        Env::new(),
        Verdict::Unsat,
    );
}
#[test]
fn stores_replay_zero_deletion_and_conditional_nested_writes() {
    let (a, b) = (mem("a"), mem("b"));
    let c = var("cond".into(), Sort::Bool);
    let modified = ite(
        c.clone(),
        write(write(a.clone(), bv(1, 0), bv(1, 1)), bv(1, 0), bv(1, 0)),
        b.clone(),
    );
    let q = and(
        c,
        and(
            eq(a.clone(), b),
            and(
                eq(read(a.clone(), bv(1, 0)), bv(1, 1)),
                eq(read(a, bv(1, 1)), bv(1, 0)),
            ),
        ),
    );
    let r = check(
        q,
        Env::from([
            ("changed".into(), modified.clone()),
            ("cell".into(), read(modified, bv(1, 0))),
        ]),
        Verdict::Sat,
    );
    assert!(r.array_context_values["changed"].entries.is_empty());
    assert_eq!(r.context_values["cell"], Scalar::Bv { width: 1, value: 0 });
    // Include independent array replay in the same whole-query work limit.
    let ctx = Env::from([("changed".into(), write(mem("a"), bv(1, 0), bv(1, 1)))]);
    for hint in [SearchHint::Sat, SearchHint::Unsat] {
        let full = solve_with_hint(&boolv(true), &ctx, Limits::default(), hint);
        assert_eq!(full.verdict, Verdict::Sat);
        let limited = solve_with_hint(
            &boolv(true),
            &ctx,
            Limits {
                max_work: full.stats.work - 1,
                ..Limits::default()
            },
            hint,
        );
        assert_eq!(limited.verdict, Verdict::Unknown);
        assert!(!limited.original_formula_validated);
        assert!(limited.array_context_values.is_empty());
    }
}
#[test]
fn stores_cover_full_width_and_distinct_sorts() {
    let a = var("a".into(), Sort::Mem(64, 64));
    let b = var("b".into(), Sort::Mem(64, 1));
    let wa = write(a, bv(64, u64::MAX), bv(64, u64::MAX));
    let wb = write(b, bv(64, u64::MAX), bv(1, 1));
    let r = check(
        boolv(true),
        Env::from([("wide".into(), wa), ("narrow".into(), wb)]),
        Verdict::Sat,
    );
    assert_eq!(r.array_context_values["wide"].read(u64::MAX), u64::MAX);
    assert_eq!(r.array_context_values["narrow"].read(u64::MAX), 1);
}
#[test]
fn store_limits_and_dead_malformed_children_fail_closed() {
    let a = mem("a");
    let malformed = node(Sort::Bv(1), "unsupported", vec![]);
    let invalid = [
        node(Sort::Mem(1, 1), "store", vec![a.clone(), bv(1, 0)]),
        write(a.clone(), bv(1, 0), bv(2, 0)),
        write(a.clone(), bv(1, 0), malformed),
        write(var("wide".into(), Sort::Mem(65, 1)), bv(65, 0), bv(1, 0)),
    ];
    for bad in invalid {
        check(
            boolv(false),
            Env::from([("dead".into(), bad)]),
            Verdict::Unknown,
        );
    }
    let q = eq(write(a.clone(), bv(1, 0), bv(1, 1)), a);
    for limits in [
        Limits {
            max_work: 1,
            ..Limits::default()
        },
        Limits {
            max_terms: 8,
            ..Limits::default()
        },
        Limits {
            max_depth: 1,
            ..Limits::default()
        },
    ] {
        let r = solve_with_hint(&q, &Env::new(), limits, SearchHint::Unsat);
        assert_eq!(r.verdict, Verdict::Unknown);
        assert!(!r.original_formula_validated);
    }
}
#[test]
fn equality_is_extensional_and_transitive() {
    let (a, b, c) = (mem("a"), mem("b"), mem("c"));
    check(
        and(
            eq(a.clone(), b.clone()),
            and(eq(b.clone(), c.clone()), not(eq(a.clone(), c.clone()))),
        ),
        Env::new(),
        Verdict::Unsat,
    );
    let r = check(not(eq(a.clone(), b.clone())), Env::new(), Verdict::Sat);
    assert_ne!(r.array_assignments["a"], r.array_assignments["b"]);
    check(
        and(
            not(eq(a.clone(), b.clone())),
            and(not(eq(a.clone(), c.clone())), eq(b.clone(), c.clone())),
        ),
        Env::new(),
        Verdict::Sat,
    );
    let cond = var("cnd".into(), Sort::Bool);
    check(
        and(
            eq(ite(cond.clone(), a.clone(), b.clone()), a.clone()),
            and(not(cond), not(eq(a, b))),
        ),
        Env::new(),
        Verdict::Unsat,
    );
}
#[test]
fn all_read_addresses_are_congruent_but_distinct_arrays_are_not_aliased() {
    let (a, b) = (mem("a"), mem("b"));
    let (x, y) = (var("x".into(), Sort::Bv(1)), var("y".into(), Sort::Bv(1)));
    let neq = not(eq(read(a.clone(), x.clone()), read(a.clone(), y.clone())));
    check(
        and(eq(x.clone(), y.clone()), neq),
        Env::new(),
        Verdict::Unsat,
    );
    check(
        not(eq(read(a.clone(), x.clone()), read(b.clone(), x.clone()))),
        Env::new(),
        Verdict::Sat,
    );
    check(
        and(
            eq(a.clone(), b.clone()),
            and(
                eq(x.clone(), y.clone()),
                not(eq(read(a.clone(), x.clone()), read(b.clone(), y.clone()))),
            ),
        ),
        Env::new(),
        Verdict::Unsat,
    );
    check(
        not(eq(
            read(a.clone(), read(b.clone(), x.clone())),
            read(a, read(b, x)),
        )),
        Env::new(),
        Verdict::Unsat,
    );
}
#[test]
fn context_arrays_reads_and_fresh_symbol_collisions_are_replayed() {
    let a = mem("__lydite_readonly_0");
    let x = var("__lydite_readonly_1".into(), Sort::Bv(1));
    let ctx = Env::from([
        ("array".into(), a.clone()),
        ("read".into(), read(a.clone(), x.clone())),
        ("eq".into(), eq(a.clone(), a)),
    ]);
    let r = check(eq(x, bv(1, 1)), ctx, Verdict::Sat);
    assert!(r.array_context_values.contains_key("array"));
    assert_eq!(r.context_values["eq"], Scalar::Bool(true));
    assert_eq!(
        r.context_values["read"],
        Scalar::Bv {
            width: 1,
            value: r.array_context_values["array"].read(1)
        }
    );
    assert_eq!(r.assignments.len(), 1);
}
#[test]
fn malformed_hidden_terms_are_rejected_before_optimization() {
    let a = mem("a");
    let invalid = [
        node(Sort::Bv(1), "select", vec![a.clone(), bv(2, 0)]),
        node(Sort::Bv(2), "select", vec![a.clone(), bv(1, 0)]),
        node(Sort::Bv(1), "select", vec![a.clone()]),
        read(
            node(
                Sort::Mem(1, 1),
                "store",
                vec![a.clone(), bv(2, 0), bv(1, 0)],
            ),
            bv(1, 0),
        ),
        read(var("bad".into(), Sort::Mem(0, 1)), bv(0, 0)),
    ];
    for bad in invalid {
        check(
            boolv(false),
            Env::from([("hidden".into(), bad)]),
            Verdict::Unknown,
        );
    }
    check(
        boolv(false),
        Env::from([("x".into(), a), ("y".into(), var("a".into(), Sort::Bv(1)))]),
        Verdict::Unknown,
    );
}
#[test]
fn whole_query_work_timeout_and_expansion_limits_fail_closed() {
    let q = not(eq(mem("a"), mem("b")));
    let r = check(q.clone(), Env::new(), Verdict::Sat);
    for limits in [
        Limits {
            max_work: r.stats.work - 1,
            ..Limits::default()
        },
        Limits {
            timeout_ms: 0,
            ..Limits::default()
        },
        Limits {
            max_terms: 8,
            ..Limits::default()
        },
        Limits {
            max_variables: 1,
            ..Limits::default()
        },
        Limits {
            max_clauses: 1,
            ..Limits::default()
        },
    ] {
        let r = solve_with_hint(&q, &Env::new(), limits, SearchHint::Unsat);
        assert_eq!(r.verdict, Verdict::Unknown);
        assert!(!r.original_formula_validated);
        assert!(r.array_assignments.is_empty());
    }
}
#[test]
fn arrays_with_large_or_distinct_widths_remain_symbolic() {
    let a = var("a".into(), Sort::Mem(64, 64));
    let b = var("b".into(), Sort::Mem(64, 1));
    let ar = node(Sort::Bv(64), "select", vec![a.clone(), bv(64, u64::MAX)]);
    let br = node(Sort::Bv(1), "select", vec![b.clone(), bv(64, u64::MAX)]);
    let r = check(
        and(eq(ar, bv(64, 17)), eq(br, bv(1, 1))),
        Env::from([("a".into(), a), ("b".into(), b)]),
        Verdict::Sat,
    );
    assert_eq!(r.array_assignments["a"].read(u64::MAX), 17);
    assert_eq!(r.array_assignments["b"].read(u64::MAX), 1);
    assert_eq!(r.array_assignments["a"].entries.len(), 1);
}
#[test]
fn equality_under_or_not_and_context_never_becomes_an_asserted_alias() {
    let (a, b) = (mem("a"), mem("b"));
    let e = eq(a.clone(), b.clone());
    let q = node(Sort::Bool, "or", vec![e.clone(), not(e.clone())]);
    check(
        and(q, not(e.clone())),
        Env::from([("eq".into(), e)]),
        Verdict::Sat,
    );
}
#[test]
fn congruence_promotion_never_uses_its_own_conclusion_or_guarded_facts() {
    let x = var("x".into(), Sort::Bv(1));
    let y = var("y".into(), Sort::Bv(1));
    let z = var("z".into(), Sort::Bv(1));
    let e = eq(x.clone(), y.clone());
    let cond = var("cond".into(), Sort::Bool);
    let a = mem("a");
    let scenarios = [
        and(
            node(Sort::Bool, "=>", vec![e.clone(), e.clone()]),
            not(e.clone()),
        ),
        and(
            node(Sort::Bool, "=>", vec![cond.clone(), e.clone()]),
            and(not(cond), not(e.clone())),
        ),
        and(
            eq(x.clone(), node(Sort::Bv(1), "bvnot", vec![y.clone()])),
            node(Sort::Bool, "=>", vec![e.clone(), eq(x.clone(), z)]),
        ),
    ];
    for q in scenarios {
        check(q, Env::from([("array".into(), a.clone())]), Verdict::Sat);
    }
    // Promoting x=y after collecting x=not(y) must rebuild definitions and
    // reject a newly cyclic definition, rather than recursively assume it.
    let q = and(
        eq(x.clone(), node(Sort::Bv(1), "bvnot", vec![y.clone()])),
        and(
            node(Sort::Bool, "=>", vec![eq(bv(1, 0), bv(1, 0)), e]),
            eq(read(a, bv(1, 0)), bv(1, 0)),
        ),
    );
    check(q, Env::new(), Verdict::Unsat);
}
#[test]
fn exhaustive_total_array_differential() {
    // Enumerate every total 1-bit-address/1-bit-value array pair and every
    // index pair. Fix complete arrays by their two cells, then test equality,
    // read congruence, nested reads, and array ITE against concrete semantics.
    let (a, b) = (mem("a"), mem("b"));
    let (x, y) = (var("x".into(), Sort::Bv(1)), var("y".into(), Sort::Bv(1)));
    for av in 0..4u64 {
        for bv_ in 0..4u64 {
            for xv in 0..2 {
                for yv in 0..2 {
                    let mut fixed = and(eq(x.clone(), bv(1, xv)), eq(y.clone(), bv(1, yv)));
                    for i in 0..2 {
                        fixed = and(
                            fixed,
                            and(
                                eq(read(a.clone(), bv(1, i)), bv(1, (av >> i) & 1)),
                                eq(read(b.clone(), bv(1, i)), bv(1, (bv_ >> i) & 1)),
                            ),
                        );
                    }
                    let stored = write(a.clone(), x.clone(), read(b.clone(), y.clone()));
                    let value = (bv_ >> yv) & 1;
                    let updated = (av & !(1 << xv)) | (value << xv);
                    let nested = write(
                        stored.clone(),
                        read(a.clone(), x.clone()),
                        read(b.clone(), x.clone()),
                    );
                    let ni = (av >> xv) & 1;
                    let nv = (bv_ >> xv) & 1;
                    let nested_value = (updated & !(1 << ni)) | (nv << ni);
                    let cases = [
                        (eq(stored.clone(), b.clone()), updated == bv_),
                        (
                            eq(read(stored, y.clone()), bv(1, 1)),
                            ((updated >> yv) & 1) == 1,
                        ),
                        (eq(nested, a.clone()), nested_value == av),
                        (
                            eq(
                                ite(
                                    eq(x.clone(), bv(1, 0)),
                                    write(a.clone(), y.clone(), bv(1, 1)),
                                    b.clone(),
                                ),
                                a.clone(),
                            ),
                            if xv == 0 {
                                (av | (1 << yv)) == av
                            } else {
                                bv_ == av
                            },
                        ),
                        (eq(a.clone(), b.clone()), av == bv_),
                        (
                            eq(read(a.clone(), x.clone()), read(a.clone(), y.clone())),
                            ((av >> xv) & 1) == ((av >> yv) & 1),
                        ),
                        (
                            eq(read(a.clone(), read(b.clone(), x.clone())), bv(1, 1)),
                            ((av >> ((bv_ >> xv) & 1)) & 1) == 1,
                        ),
                        (
                            eq(
                                ite(eq(x.clone(), bv(1, 0)), a.clone(), b.clone()),
                                a.clone(),
                            ),
                            xv == 0 || av == bv_,
                        ),
                    ];
                    for (q, want) in cases {
                        check(
                            and(fixed.clone(), q.clone()),
                            Env::new(),
                            if want { Verdict::Sat } else { Verdict::Unsat },
                        );
                        check(
                            and(fixed.clone(), not(q)),
                            Env::new(),
                            if want { Verdict::Unsat } else { Verdict::Sat },
                        );
                    }
                }
            }
        }
    }
}
