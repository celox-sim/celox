use super::*;

fn or(x: Term, y: Term) -> Term {
    node(Sort::Bool, "or", vec![x, y])
}
fn any(xs: Vec<Term>) -> Term {
    xs.into_iter().fold(boolv(false), or)
}
fn implication(x: Term, y: Term) -> Term {
    node(Sort::Bool, "=>", vec![x, y])
}

pub fn truth_tables(a: &mut Audit) {
    let pool = vec![
        boolv(false),
        boolv(true),
        b("p"),
        not(b("p")),
        b("q"),
        not(b("q")),
        b("r"),
        not(b("r")),
    ];
    for pre in &pool {
        for x in &pool {
            for y in &pool {
                for z in &pool {
                    let conjunction = and(x.clone(), and(y.clone(), z.clone()));
                    let disjunction = or(x.clone(), or(y.clone(), z.clone()));
                    for (phase, post) in [
                        ("negative_conjunction", not(conjunction.clone())),
                        ("positive_disjunction", disjunction.clone()),
                        ("double_negative_disjunction", not(not(disjunction))),
                        (
                            "negative_or_stays_atom",
                            not(or(x.clone(), and(y.clone(), z.clone()))),
                        ),
                        (
                            "nested_polarity",
                            not(and(not(x.clone()), not(or(y.clone(), z.clone())))),
                        ),
                    ] {
                        a.exact(phase, and(pre.clone(), post), Env::new());
                    }
                }
            }
        }
    }
    // Similar syntactic regions are not mandatory top-level factors. Evaluate
    // original semantics exhaustively so an over-broad split cannot hide SAT.
    for x in &pool {
        for y in &pool {
            for z in &pool {
                let bad = not(and(x.clone(), y.clone()));
                for t in [
                    not(and(z.clone(), bad.clone())),
                    implication(bad.clone(), z.clone()),
                    implication(z.clone(), bad.clone()),
                    ite(z.clone(), bad.clone(), not(bad.clone())),
                    eq(z.clone(), bad.clone()),
                    node(Sort::Bool, "xor", vec![z.clone(), bad]),
                ] {
                    a.exact("nonmandatory_regions", t, Env::new());
                }
            }
        }
    }
}

fn padded_disjunction(xs: Vec<Term>) -> Term {
    // Constant identities, duplicates and binary nesting remain ordinary IR;
    // they are deliberately not simplified by the audit before the solver call.
    any(xs
        .into_iter()
        .enumerate()
        .map(|(i, x)| {
            if i % 2 == 0 {
                or(boolv(false), x)
            } else {
                not(and(boolv(true), not(x)))
            }
        })
        .collect())
}

pub fn coverage(a: &mut Audit) {
    for n in [0, 1, 2, 3, 7, 16, 31, 65] {
        // For nonempty domains, only one selected branch is satisfiable. Every
        // position, including the last, must be covered; deduplication is safe
        // only for exact literals. No model-specific naming convention is used.
        for winner in 0..n {
            let vars = (0..n)
                .map(|i| b(&format!("choice_{i:03}")))
                .collect::<Vec<_>>();
            let pre = all(vars
                .iter()
                .enumerate()
                .map(|(i, x)| {
                    if i == winner {
                        x.clone()
                    } else {
                        not(x.clone())
                    }
                })
                .collect());
            let mut alternatives = vars.clone();
            if winner % 2 == 0 {
                alternatives.reverse()
            }
            if winner % 3 == 0 {
                alternatives.extend(alternatives.clone())
            }
            let t = and(pre, padded_disjunction(alternatives));
            let ctx = Env::from([
                ("extra".into(), w("unconstrained_extra", 64)),
                ("original_disjunction".into(), any(vars)),
            ]);
            let o = a.check(
                "coverage_every_branch_position",
                t,
                ctx,
                Verdict::Sat,
                Limits::default(),
            );
            if winner == n - 1 {
                a.probes.push(json!({"case":"last_only_sat","branches_requested":n,"outcome":o.diagnostics()}));
            }
        }
        let vars = (0..n)
            .map(|i| b(&format!("choice_{i:03}")))
            .collect::<Vec<_>>();
        let t = and(
            all(vars.iter().cloned().map(not).collect()),
            padded_disjunction(vars),
        );
        let o = a.check(
            "coverage_all_unsat",
            t,
            Env::new(),
            Verdict::Unsat,
            Limits::default(),
        );
        a.probes
            .push(json!({"case":"all_unsat","branches_requested":n,"outcome":o.diagnostics()}));
    }
    for flag in [false, true] {
        let eqxy = eq(w("x", 4), w("y", 4));
        let t = and(not(eqxy.clone()), or(eqxy.clone(), boolv(flag)));
        a.exact(
            "false_true_factor_alternatives",
            t,
            Env::from([("original_equality".into(), eqxy)]),
        );
    }
    // Many occurrences of the same disjunction DAG must preserve coverage and
    // remain bounded. Both syntactic and CNF-literal duplicate routes are tested.
    let base = or(b("p"), b("q"));
    let mut repeated = base.clone();
    for _ in 0..10 {
        repeated = or(repeated.clone(), repeated)
    }
    a.exact(
        "repeated_disjunctive_dag",
        and(not(b("p")), and(b("q"), repeated)),
        Env::from([("base".into(), base)]),
    );
}

pub fn words(a: &mut Audit) {
    for x in 0..4 {
        for y in 0..4 {
            for p in [false, true] {
                a.pinned(
                    "pinned_word_original_violations",
                    and(
                        b("p"),
                        not(and(
                            eq(w("x", 2), w("y", 2)),
                            eq(binary("bvadd", w("x", 2), bv(2, 1)), w("y", 2)),
                        )),
                    ),
                    Assignment::from([
                        ("x".into(), V::W(x)),
                        ("y".into(), V::W(y)),
                        ("p".into(), V::B(p)),
                    ]),
                );
            }
        }
    }
    for width in 1..=4 {
        let x = w("x", width);
        let y = w("y", width);
        for offset in 0..1u64 << width {
            let predicates = vec![
                eq(x.clone(), y.clone()),
                eq(binary("bvadd", x.clone(), bv(width, offset)), y.clone()),
                eq(unary("bvnot", x.clone()), y.clone()),
            ];
            for reverse in [false, true] {
                let mut p = predicates.clone();
                if reverse {
                    p.reverse()
                }
                let t = and(eq(x.clone(), bv(width, offset)), not(all(p)));
                a.exact(
                    "word_original_query_enumeration",
                    t,
                    Env::from([("x".into(), x.clone()), ("y".into(), y.clone())]),
                );
            }
        }
    }
    for width in [1, 4, 8, 16, 32, 64] {
        let x = w("x", width);
        let y = w("y", width);
        let z = w("z", width);
        let pre = all(vec![eq(x.clone(), y.clone()), eq(y.clone(), z.clone())]);
        let goals = vec![
            eq(
                binary("bvadd", x.clone(), bv(width, 1)),
                binary("bvadd", y.clone(), bv(width, 1)),
            ),
            eq(unary("bvnot", y.clone()), unary("bvnot", z.clone())),
            eq(ite(b("p"), x.clone(), z.clone()), y.clone()),
        ];
        a.oracle_case(
            "wide_conjunctive_violations_unsat",
            and(pre.clone(), not(all(goals.clone()))),
            Env::new(),
            Verdict::Unsat,
        );
        let mut broken = goals;
        broken.push(not(eq(x.clone(), y.clone())));
        a.oracle_case(
            "wide_late_violation_sat",
            and(pre, not(all(broken))),
            Env::from([
                ("original_x".into(), x),
                ("original_y".into(), y),
                ("original_z".into(), z),
                ("original_guard".into(), b("p")),
                ("unrelated".into(), w("unrelated", 64)),
            ]),
            Verdict::Sat,
        );
    }
}

pub fn malformed(a: &mut Audit) {
    let invalids = vec![
        node(Sort::Bool, "uninterpreted", vec![]),
        node(Sort::Bool, "and", vec![]),
        node(Sort::Bool, "or", vec![]),
        node(Sort::Bool, "and", vec![b("p")]),
        node(Sort::Bool, "or", vec![b("p"), b("q"), b("r")]),
        node(Sort::Bool, "not", vec![]),
        node(Sort::Bool, "or", vec![b("p"), bv(4, 1)]),
    ];
    for bad in invalids {
        for t in [
            or(boolv(true), bad.clone()),
            or(bad.clone(), boolv(true)),
            and(boolv(false), bad.clone()),
            not(and(boolv(false), bad.clone())),
            and(or(b("p"), b("q")), not(and(boolv(true), bad))),
        ] {
            a.check(
                "malformed_sibling_never_skipped",
                t,
                Env::new(),
                Verdict::Unknown,
                Limits::default(),
            );
        }
    }
    let t = or(b("p"), b("q"));
    for ctx in [
        Env::from([(
            "unsupported".into(),
            node(Sort::Bv(4), "bvudiv", vec![bv(4, 1), bv(4, 1)]),
        )]),
        Env::from([("same_name_conflicting_sort".into(), w("p", 8))]),
    ] {
        a.check(
            "malformed_context_before_branch",
            t.clone(),
            ctx,
            Verdict::Unknown,
            Limits::default(),
        );
    }
}

fn distribution_formula(n: usize, sat: bool) -> Term {
    let xs = (0..n)
        .map(|i| b(&format!("branch_{i:03}")))
        .collect::<Vec<_>>();
    // Each equivalence is intentionally kept below OR so the separate alias
    // optimization cannot turn the entire domain into a single literal.
    let constraints = xs
        .iter()
        .enumerate()
        .map(|(i, x)| {
            let require = if sat && i == n - 1 {
                x.clone()
            } else {
                not(x.clone())
            };
            or(
                and(b("guard"), require.clone()),
                and(not(b("guard")), require),
            )
        })
        .collect();
    and(all(constraints), padded_disjunction(xs))
}

pub fn budgets(a: &mut Audit) {
    for sat in [false, true] {
        let t = distribution_formula(12, sat);
        let ctx = Env::from([
            ("post".into(), t.clone()),
            ("wide_context_only".into(), w("ctx", 64)),
        ]);
        let expected = if sat { Verdict::Sat } else { Verdict::Unsat };
        let full = a.check(
            "budget_full",
            t.clone(),
            ctx.clone(),
            expected,
            Limits::default(),
        );
        a.probes.push(json!({"case":"shared_budget_full","expected":expected.as_str(),"outcome":full.diagnostics()}));
        for delta in [0, 1] {
            a.check(
                "budget_exact_work",
                t.clone(),
                ctx.clone(),
                if delta == 0 {
                    expected
                } else {
                    Verdict::Unknown
                },
                Limits {
                    max_work: full.stats.work - delta,
                    ..Limits::default()
                },
            );
            a.check(
                "budget_exact_clauses",
                t.clone(),
                ctx.clone(),
                if delta == 0 {
                    expected
                } else {
                    Verdict::Unknown
                },
                Limits {
                    max_clauses: full.stats.clauses - delta as usize,
                    ..Limits::default()
                },
            );
        }
        for work in [
            0,
            1,
            2,
            3,
            7,
            31,
            127,
            511,
            1023,
            full.stats.work / 4,
            full.stats.work / 2,
            full.stats.work * 3 / 4,
        ] {
            let o = a.check(
                "budget_shared_work_sweep",
                t.clone(),
                ctx.clone(),
                Verdict::Unknown,
                Limits {
                    max_work: work,
                    ..Limits::default()
                },
            );
            a.probes.push(json!({"case":"shared_work_unknown","expected":expected.as_str(),"max_work":work,"outcome":o.diagnostics()}));
        }
        for clauses in [
            0,
            1,
            2,
            3,
            7,
            31,
            full.stats.clauses / 4,
            full.stats.clauses / 2,
            full.stats.clauses * 3 / 4,
        ] {
            a.check(
                "budget_shared_clause_sweep",
                t.clone(),
                ctx.clone(),
                Verdict::Unknown,
                Limits {
                    max_clauses: clauses,
                    ..Limits::default()
                },
            );
        }
        for limits in [
            Limits {
                timeout_ms: 0,
                ..Limits::default()
            },
            Limits {
                max_terms: 1,
                ..Limits::default()
            },
            Limits {
                max_variables: 1,
                ..Limits::default()
            },
            Limits {
                max_depth: 1,
                ..Limits::default()
            },
        ] {
            a.check(
                "budget_other_limits",
                t.clone(),
                ctx.clone(),
                Verdict::Unknown,
                limits,
            );
        }
    }
    for i in 0..24 {
        let t = distribution_formula(5, i % 2 == 0);
        a.check(
            "fresh_instance_after_unknown",
            t.clone(),
            Env::new(),
            Verdict::Unknown,
            Limits {
                max_work: 7,
                ..Limits::default()
            },
        );
        a.check(
            "fresh_instance_decisive",
            t,
            Env::new(),
            if i % 2 == 0 {
                Verdict::Sat
            } else {
                Verdict::Unsat
            },
            Limits::default(),
        );
    }
    // One monotonic deadline spans extraction, all completed alternatives, and
    // the current alternative. A fast baseline may finish these unsplit queries;
    // a partitioning candidate must expose at least one genuine timeout after
    // finishing a branch but before covering the remaining alternatives.
    let mut has_partition_diagnostics = false;
    let mut middle_deadline_observed = false;
    for n in [64, 128, 256] {
        for timeout_ms in [1, 2, 4] {
            let t = distribution_formula(n, false);
            let start = Instant::now();
            let out = finite::solve(
                &t,
                &Env::new(),
                Limits {
                    timeout_ms,
                    ..Limits::default()
                },
            );
            let elapsed = start.elapsed().as_secs_f64();
            assert_ne!(out.verdict, Verdict::Sat, "known UNSAT deadline control");
            assert!(
                out.assignments.is_empty()
                    && out.context_values.is_empty()
                    && !out.original_formula_validated
            );
            if out.verdict == Verdict::Unknown {
                assert!(out.reason.is_some());
            }
            let diagnostics = out.diagnostics();
            let alternatives = diagnostics["split_alternatives"].as_u64().unwrap_or(0);
            let completed = diagnostics["split_completed"].as_u64().unwrap_or(0);
            has_partition_diagnostics |= diagnostics.get("split_alternatives").is_some();
            middle_deadline_observed |= out.verdict == Verdict::Unknown
                && out.reason.as_ref().unwrap().contains("time budget")
                && completed > 0
                && completed < alternatives;
            *a.counts
                .entry("deadline_control_checks".into())
                .or_default() += 1;
            *a.phases
                .entry("cumulative_nonzero_deadline".into())
                .or_default() += 1;
            a.probes.push(json!({"case":"cumulative_nonzero_deadline","size":n,"timeout_ms":timeout_ms,"elapsed_seconds":elapsed,"outcome":diagnostics}));
        }
    }
    if has_partition_diagnostics {
        assert!(
            middle_deadline_observed,
            "need a deadline reached between completion of some and all split alternatives"
        );
    }
}
