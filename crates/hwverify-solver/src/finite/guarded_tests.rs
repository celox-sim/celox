use super::*;
use hwverify_ir::{and, boolv, bv, eq, ite, node, not, var};
fn imp(a: Term, b: Term) -> Term {
    node(Sort::Bool, "=>", vec![a, b])
}
fn or(a: Term, b: Term) -> Term {
    node(Sort::Bool, "or", vec![a, b])
}
fn plus(a: Term) -> Term {
    node(Sort::Bv(2), "bvadd", vec![a, bv(2, 1)])
}
fn vars() -> (Term, Term, Term, Term, Term) {
    (
        var("g".into(), Sort::Bool),
        var("h".into(), Sort::Bool),
        var("x".into(), Sort::Bv(2)),
        var("y".into(), Sort::Bv(2)),
        var("z".into(), Sort::Bv(2)),
    )
}
fn both(q: &Term, ctx: &Env, expected: Verdict) -> Outcome {
    let mut last = None;
    for hint in [SearchHint::Unsat, SearchHint::Sat] {
        let r = solve_with_hint(q, ctx, Limits::default(), hint);
        assert_eq!(r.verdict, expected, "{hint:?}: {:?}", r.reason);
        if expected == Verdict::Sat {
            assert!(r.original_formula_validated)
        }
        last = Some(r)
    }
    last.unwrap()
}
#[test]
fn positive_guarded_branch_collapses_and_negated_or_missing_guard_does_not() {
    let (g, h, x, y, z) = vars();
    let rhs = plus(y.clone());
    let fact = imp(g.clone(), eq(rhs.clone(), x.clone()));
    let changed = not(eq(
        ite(and(g.clone(), h.clone()), x.clone(), z.clone()),
        ite(and(g.clone(), h.clone()), rhs.clone(), z.clone()),
    ));
    let r = both(&and(fact.clone(), changed), &Env::new(), Verdict::Unsat);
    assert!(r.stats.guarded_rewrites > 0);
    for guard in [not(g.clone()), h.clone(), or(g.clone(), h.clone())] {
        let q = and(
            fact.clone(),
            not(eq(
                ite(guard.clone(), x.clone(), z.clone()),
                ite(guard, rhs.clone(), z.clone()),
            )),
        );
        both(
            &q,
            &Env::from([
                ("x".into(), x.clone()),
                ("original fact".into(), fact.clone()),
            ]),
            Verdict::Sat,
        );
    }
    let nested = imp(g.clone(), imp(h.clone(), eq(x.clone(), rhs.clone())));
    let guarded = and(g.clone(), h.clone());
    let q = and(
        nested.clone(),
        not(eq(
            ite(guarded.clone(), x.clone(), z.clone()),
            ite(guarded, rhs.clone(), z.clone()),
        )),
    );
    assert!(both(&q, &Env::new(), Verdict::Unsat).stats.guarded_rewrites > 0);
    let q = and(
        nested,
        not(eq(ite(g.clone(), x.clone(), z.clone()), ite(g, rhs, z))),
    );
    both(&q, &Env::new(), Verdict::Sat);
}
#[test]
fn disjunction_negation_context_and_conditional_cycles_never_supply_facts() {
    let (g, h, x, y, z) = vars();
    let fact = imp(g.clone(), eq(x.clone(), plus(y.clone())));
    let bad = not(eq(
        ite(g.clone(), x.clone(), z.clone()),
        ite(g.clone(), plus(y.clone()), z.clone()),
    ));
    for source in [
        or(fact.clone(), h.clone()),
        not(fact.clone()),
        imp(h.clone(), fact.clone()),
    ] {
        both(&and(source, bad.clone()), &Env::new(), Verdict::Sat);
    }
    let r = both(&bad, &Env::from([("fact".into(), fact)]), Verdict::Sat);
    assert_eq!(r.stats.guarded_equalities, 0);
    let q = and(
        imp(g.clone(), eq(x.clone(), plus(y.clone()))),
        and(imp(g.clone(), eq(y.clone(), plus(x.clone()))), g.clone()),
    );
    let r = both(&q, &Env::new(), Verdict::Unsat);
    assert_eq!(r.stats.guarded_equalities, 1);
    let q = and(
        eq(x.clone(), z.clone()),
        and(imp(g.clone(), eq(z, plus(x.clone()))), g.clone()),
    );
    let r = both(&q, &Env::new(), Verdict::Unsat);
    assert_eq!(r.stats.guarded_equalities, 0);
    let self_guard = eq(x.clone(), bv(2, 0));
    let q = imp(self_guard, eq(x, bv(2, 1)));
    let r = both(&q, &Env::new(), Verdict::Sat);
    assert_eq!(r.stats.guarded_equalities, 0);
}
#[test]
fn guarded_rewrite_checks_all_sources_and_exact_work_threshold() {
    let (g, h, x, y, z) = vars();
    let fact = imp(g.clone(), eq(x.clone(), plus(y.clone())));
    let q = and(
        fact,
        not(eq(
            ite(and(g.clone(), h.clone()), x.clone(), z.clone()),
            ite(and(g.clone(), h.clone()), plus(y.clone()), z.clone()),
        )),
    );
    for hint in [SearchHint::Unsat, SearchHint::Sat] {
        for bad in [
            node(Sort::Bv(2), "unsupported", vec![]),
            var("x".into(), Sort::Bv(3)),
        ] {
            let ctx = Env::from([("dead".into(), ite(boolv(true), bv(2, 0), bad))]);
            assert_eq!(
                solve_with_hint(&q, &ctx, Limits::default(), hint).verdict,
                Verdict::Unknown
            );
        }
        let full = solve_with_hint(&q, &Env::new(), Limits::default(), hint);
        assert_eq!(full.verdict, Verdict::Unsat);
        let lim = Limits {
            max_work: full.stats.work - 1,
            ..Limits::default()
        };
        assert_eq!(
            solve_with_hint(&q, &Env::new(), lim, hint).verdict,
            Verdict::Unknown
        );
        let lim = Limits {
            max_work: full.stats.work,
            ..Limits::default()
        };
        assert_eq!(
            solve_with_hint(&q, &Env::new(), lim, hint).verdict,
            Verdict::Unsat
        );
        for lim in [
            Limits {
                max_terms: 1,
                ..Limits::default()
            },
            Limits {
                max_depth: 1,
                ..Limits::default()
            },
            Limits {
                max_clauses: 0,
                ..Limits::default()
            },
            Limits {
                timeout_ms: 0,
                ..Limits::default()
            },
        ] {
            assert_eq!(
                solve_with_hint(&q, &Env::new(), lim, hint).verdict,
                Verdict::Unknown
            )
        }
    }
}
#[test]
fn small_guarded_formulas_match_exhaustive_independent_boolean_word_oracle() {
    let (g, h, x, y, z) = vars();
    for seed in 0u64..240 {
        let c = (seed / 7) % 4;
        let rhs = node(Sort::Bv(2), "bvxor", vec![y.clone(), bv(2, c)]);
        let fact = imp(g.clone(), eq(x.clone(), rhs.clone()));
        let guard = match seed % 6 {
            0 => g.clone(),
            1 => and(g.clone(), h.clone()),
            2 => or(g.clone(), h.clone()),
            3 => not(g.clone()),
            4 => h.clone(),
            _ => and(not(g.clone()), h.clone()),
        };
        let source = match (seed / 6) % 5 {
            0 => fact.clone(),
            1 => imp(h.clone(), fact.clone()),
            2 => or(h.clone(), fact.clone()),
            3 => not(fact.clone()),
            _ => and(fact.clone(), eq(x.clone(), z.clone())),
        };
        let cmp = eq(
            ite(guard.clone(), x.clone(), z.clone()),
            ite(guard, rhs.clone(), bv(2, (seed / 11) % 4)),
        );
        let q = and(source, if seed & 1 == 0 { cmp } else { not(cmp) });
        let mut any = false;
        for gv in [false, true] {
            for hv in [false, true] {
                for xv in 0..4 {
                    for yv in 0..4 {
                        for zv in 0..4 {
                            let fv = !gv || xv == (yv ^ c);
                            let guard = match seed % 6 {
                                0 => gv,
                                1 => gv && hv,
                                2 => gv || hv,
                                3 => !gv,
                                4 => hv,
                                _ => !gv && hv,
                            };
                            let source = match (seed / 6) % 5 {
                                0 => fv,
                                1 => !hv || fv,
                                2 => hv || fv,
                                3 => !fv,
                                _ => fv && xv == zv,
                            };
                            let cmp = (if guard { xv } else { zv })
                                == (if guard { yv ^ c } else { (seed / 11) % 4 });
                            any |= source && (if seed & 1 == 0 { cmp } else { !cmp });
                        }
                    }
                }
            }
        }
        both(
            &q,
            &Env::from([
                ("x".into(), x.clone()),
                ("y".into(), y.clone()),
                ("z".into(), z.clone()),
            ]),
            if any { Verdict::Sat } else { Verdict::Unsat },
        );
    }
}

fn entailment_budget() -> Budget {
    Budget {
        limits: Limits::default(),
        start: Instant::now(),
        work: 0,
        time_check_in: 0,
    }
}
#[test]
fn guard_entailment_truth_tables_do_not_invent_conjuncts_or_disjuncts() {
    let g = var("selector_a".into(), Sort::Bool);
    let h = var("selector_b".into(), Sort::Bool);
    // Each table is calculated directly with Rust Boolean operations, without
    // using solver normalization or its expression evaluator as the oracle.
    let atoms = vec![
        (g.clone(), [false, true, false, true]),
        (h.clone(), [false, false, true, true]),
        (not(g.clone()), [true, false, true, false]),
        (not(h.clone()), [true, true, false, false]),
        (boolv(true), [true; 4]),
        (boolv(false), [false; 4]),
    ];
    let mut forms = atoms.clone();
    for (a, av) in &atoms {
        for (c, cv) in &atoms {
            forms.push((
                and(a.clone(), c.clone()),
                std::array::from_fn(|i| av[i] && cv[i]),
            ));
            forms.push((
                or(a.clone(), c.clone()),
                std::array::from_fn(|i| av[i] || cv[i]),
            ));
        }
    }
    for (fact_a, av) in &forms {
        for (fact_b, bv) in &forms {
            let facts = HashSet::from([fact_a.clone(), fact_b.clone()]);
            let mut memo = HashMap::new();
            let mut budget = entailment_budget();
            for (target, tv) in &forms {
                if guard_entailed(target, &facts, &mut memo, &mut budget, 0).unwrap() {
                    for i in 0..4 {
                        assert!(
                            !(av[i] && bv[i]) || tv[i],
                            "entailed guard was false in a satisfying assignment"
                        );
                    }
                }
            }
        }
    }
    let facts = HashSet::from([or(g.clone(), h.clone())]);
    assert!(!guard_entailed(&g, &facts, &mut HashMap::new(), &mut entailment_budget(), 0).unwrap());
    assert!(
        !guard_entailed(
            &and(g.clone(), h.clone()),
            &HashSet::from([g]),
            &mut HashMap::new(),
            &mut entailment_budget(),
            0
        )
        .unwrap()
    );
}
#[test]
fn guard_entailment_enables_broader_guards_but_not_partial_guards() {
    let (g, h, x, y, z) = vars();
    let k = var("third_guard".into(), Sort::Bool);
    for polarity in [false, true] {
        let atom = if polarity { g.clone() } else { not(g.clone()) };
        let requirement = and(or(atom.clone(), h.clone()), or(atom.clone(), k.clone()));
        let fact = imp(requirement, eq(x.clone(), plus(y.clone())));
        for (branch, expected) in [
            (atom.clone(), Verdict::Unsat),
            (and(h.clone(), k.clone()), Verdict::Unsat),
            (h.clone(), Verdict::Sat),
            (or(atom.clone(), h.clone()), Verdict::Sat),
            (not(atom.clone()), Verdict::Sat),
        ] {
            let q = and(
                fact.clone(),
                not(eq(
                    ite(branch.clone(), x.clone(), z.clone()),
                    ite(branch, plus(y.clone()), z.clone()),
                )),
            );
            let out = both(&q, &Env::from([("original x".into(), x.clone())]), expected);
            if expected == Verdict::Unsat {
                assert!(out.stats.guarded_rewrites > 0);
            }
        }
    }
}
#[test]
fn guard_entailment_memoizes_shared_dags_and_obeys_all_limits() {
    let g = var("fact".into(), Sort::Bool);
    let facts = HashSet::from([g.clone()]);
    let mut target = g;
    for _ in 0..24 {
        target = and(target.clone(), target);
    }
    let mut budget = entailment_budget();
    assert!(guard_entailed(&target, &facts, &mut HashMap::new(), &mut budget, 0).unwrap());
    assert!(budget.work < 100);
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
        let mut budget = Budget {
            limits,
            ..entailment_budget()
        };
        assert!(guard_entailed(&target, &facts, &mut HashMap::new(), &mut budget, 0).is_err());
    }
}
