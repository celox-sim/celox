use super::*;
use crate::{node, not, var};
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
// Enumerating all masks covers unchanged explicit rows and implicit defaults.
fn partial_fixture(mask: u64, constants: bool) -> Term {
    let rows = (0..4)
        .map(|k| {
            let old = if constants && mask & (1 << k) == 0 {
                bv(2, (k + 1) % 4)
            } else {
                w(&format!("r{k}"), 2)
            };
            if mask & (1 << k) == 0 {
                old
            } else {
                ite(
                    and(var("en".into(), Sort::Bool), eq(w("rd", 2), bv(2, k))),
                    w("v", 2),
                    old,
                )
            }
        })
        .collect::<Vec<_>>();
    selector(
        &w("rs", 2),
        &[
            (0, rows[0].clone()),
            (1, rows[1].clone()),
            (2, rows[2].clone()),
        ],
        rows[3].clone(),
    )
}
#[test]
fn partial_writes_exhaustive_independent_truth_tables() {
    for mask in 1..15 {
        for constants in [false, true] {
            let t = partial_fixture(mask, constants);
            let candidate = Rewrite::default().read_write(&t, &mut b()).unwrap();
            assert_eq!(candidate.is_some(), mask.count_ones() >= 2);
            let n = candidate.unwrap_or_else(|| t.clone());
            for cells in 0..256 {
                let mut a = BTreeMap::new();
                for i in 0..4 {
                    a.insert(format!("r{i}"), (cells >> (i * 2)) & 3);
                }
                for rd in 0..4 {
                    for rs in 0..4 {
                        for v in 0..4 {
                            for en in 0..2 {
                                a.extend([
                                    ("rd".into(), rd),
                                    ("rs".into(), rs),
                                    ("v".into(), v),
                                    ("en".into(), en),
                                ]);
                                assert_eq!(
                                    eval(&t, &a),
                                    eval(&n, &a),
                                    "mask={mask} constants={constants} {a:?}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}
#[test]
fn sparse_rom_is_unchanged_but_dense_zero_row_bank_factors() {
    let make = |width: u32, sparse: bool| {
        let count = 1u64 << width;
        let rd = w("rd", width);
        let values = (0..count)
            .map(|k| {
                let old = if !sparse && k == 0 {
                    bv(32, 0)
                } else {
                    w(&format!("r{k}"), 32)
                };
                if (sparse && k != count - 1) || (!sparse && k == 0) {
                    old
                } else {
                    ite(
                        and(var("en".into(), Sort::Bool), eq(rd.clone(), bv(width, k))),
                        w("v", 32),
                        old,
                    )
                }
            })
            .collect::<Vec<_>>();
        selector(
            &w("rs", width),
            &(0..count - 1)
                .map(|k| (k, values[k as usize].clone()))
                .collect::<Vec<_>>(),
            values[count as usize - 1].clone(),
        )
    };
    let sparse = make(6, true);
    let mut state = Rewrite::default();
    assert!(state.read_write(&sparse, &mut b()).unwrap().is_none());
    assert_eq!(state.created, 0);
    let dense = make(5, false);
    let factored = Rewrite::default()
        .read_write(&dense, &mut b())
        .unwrap()
        .expect("one zero row and31 writable rows remain applicable");
    let mut values = (0..32)
        .map(|k| (format!("r{k}"), k * 13 + 7))
        .collect::<BTreeMap<_, _>>();
    for rs in 0..32 {
        for rd in 0..32 {
            for en in 0..2 {
                for value in [0, 0x8000_0000, 0xffff_ffff] {
                    values.extend([
                        ("rs".into(), rs),
                        ("rd".into(), rd),
                        ("en".into(), en),
                        ("v".into(), value),
                    ]);
                    assert_eq!(eval(&dense, &values), eval(&factored, &values));
                }
            }
        }
    }
}

#[test]
fn common_base_mux_both_polarities_exhaustive_and_bounded() {
    for negative in [false, true] {
        let c = var("c".into(), Sort::Bool);
        let d = var("d".into(), Sort::Bool);
        let base = w("base", 2);
        let inner = ite(d, w("value", 2), base.clone());
        let t = if negative {
            ite(c, base, inner)
        } else {
            ite(c, inner, base)
        };
        let (n, state) = normalized(&t);
        assert_eq!(state.word_rewrites, 1);
        assert_eq!(state.created, if negative { 3 } else { 2 });
        for c in 0..2 {
            for d in 0..2 {
                for base in 0..4 {
                    for value in 0..4 {
                        let a = BTreeMap::from([
                            ("c".into(), c),
                            ("d".into(), d),
                            ("base".into(), base),
                            ("value".into(), value),
                        ]);
                        assert_eq!(eval(&t, &a), eval(&n, &a));
                    }
                }
            }
        }
        let source = validate_source(&t, &Env::new(), &mut b()).unwrap();
        for allowance in 0..=3 {
            let mut budget = b();
            budget.limits.max_terms = source + allowance;
            let mut state = Rewrite {
                source_nodes: source,
                ..Rewrite::default()
            };
            let result = state.normalize(&t, &mut budget, 0).unwrap();
            assert!(state.created <= allowance);
            if allowance < if negative { 3 } else { 2 } {
                assert_eq!(result, t);
            }
        }
    }
}
#[test]
fn nested_negative_guards_expose_partial_bank_writes() {
    let rows = (0..4)
        .map(|k| {
            let old = w(&format!("r{k}"), 2);
            if k == 0 {
                return old;
            }
            ite(
                var("stall".into(), Sort::Bool),
                old.clone(),
                ite(
                    var("halted".into(), Sort::Bool),
                    old.clone(),
                    ite(
                        var("valid".into(), Sort::Bool),
                        ite(
                            var("fault".into(), Sort::Bool),
                            old.clone(),
                            ite(
                                and(var("en".into(), Sort::Bool), eq(w("rd", 2), bv(2, k))),
                                w("v", 2),
                                old.clone(),
                            ),
                        ),
                        old,
                    ),
                ),
            )
        })
        .collect::<Vec<_>>();
    let t = selector(
        &w("rs", 2),
        &[
            (0, rows[0].clone()),
            (1, rows[1].clone()),
            (2, rows[2].clone()),
        ],
        rows[3].clone(),
    );
    let (n, state) = normalized(&t);
    assert!(state.word_rewrites >= 13);
    // A single factored write condition must be exposed outside the selector.
    assert_eq!(n.0.args[1], w("v", 2));
    for controls in 0..32 {
        for cells in 0..256 {
            let mut a = BTreeMap::new();
            for (i, name) in ["stall", "halted", "valid", "fault", "en"]
                .iter()
                .enumerate()
            {
                a.insert((*name).into(), (controls >> i) & 1);
            }
            for k in 0..4 {
                a.insert(format!("r{k}"), (cells >> (k * 2)) & 3);
            }
            for rd in 0..4 {
                for rs in 0..4 {
                    for v in 0..4 {
                        a.extend([("rd".into(), rd), ("rs".into(), rs), ("v".into(), v)]);
                        assert_eq!(eval(&t, &a), eval(&n, &a));
                    }
                }
            }
        }
    }
    for hint in [SearchHint::Sat, SearchHint::Unsat] {
        let result = super::super::solve_with_hint(
            &not(eq(t.clone(), n.clone())),
            &Env::new(),
            Limits::default(),
            hint,
        );
        assert_eq!(result.verdict, Verdict::Unsat, "{}", result.diagnostics());
    }
}
#[test]
fn partial_writes_reject_nonuniform_writes_and_incomplete_domains() {
    let t = partial_fixture(7, false);
    let mut bads = vec![t.0.args[2].clone(), partial_fixture(0, false)];
    for (guard, value) in [
        (
            and(var("other".into(), Sort::Bool), eq(w("rd", 2), bv(2, 0))),
            w("v", 2),
        ),
        (
            and(var("en".into(), Sort::Bool), eq(w("other_rd", 2), bv(2, 0))),
            w("v", 2),
        ),
        (
            and(var("en".into(), Sort::Bool), eq(w("rd", 2), bv(2, 1))),
            w("v", 2),
        ),
        (
            and(var("en".into(), Sort::Bool), eq(w("rd", 2), bv(2, 0))),
            w("other_v", 2),
        ),
        (
            and(var("en".into(), Sort::Bool), eq(w("rd_wide", 3), bv(3, 0))),
            w("v", 2),
        ),
    ] {
        let mut args = t.0.args.clone();
        args[1] = ite(guard, value, w("r0", 2));
        bads.push(node(t.0.sort.clone(), "ite", args));
    }
    let mut args = t.0.args.clone();
    args[0] = eq(w("rs", 2), bv(2, 1));
    bads.push(node(t.0.sort.clone(), "ite", args));
    // An arbitrary ITE default is not silently classified as an unchanged cell.
    let mut rows = vec![
        (0, t.0.args[1].clone()),
        (1, t.0.args[2].0.args[1].clone()),
        (2, t.0.args[2].0.args[2].0.args[1].clone()),
    ];
    bads.push(selector(
        &w("rs", 2),
        &rows,
        ite(var("g".into(), Sort::Bool), bv(2, 1), bv(2, 2)),
    ));
    rows.insert(0, (0, bv(2, 3)));
    bads.push(selector(&w("rs", 2), &rows, w("r3", 2)));
    for t in bads {
        assert!(
            Rewrite::default()
                .read_write(&t, &mut b())
                .unwrap()
                .is_none()
        );
    }
}
#[test]
fn partial_writes_original_sat_context_replay_and_budgets() {
    let t = partial_fixture(6, false);
    let (n, _) = normalized(&t);
    for hint in [SearchHint::Sat, SearchHint::Unsat] {
        let q = not(eq(t.clone(), n.clone()));
        let result = super::super::solve_with_hint(&q, &Env::new(), Limits::default(), hint);
        assert_eq!(result.verdict, Verdict::Unsat, "{}", result.diagnostics());
        for context in [Env::new(), Env::from([("table".into(), t.clone())])] {
            let lhs = if context.is_empty() {
                t.clone()
            } else {
                w("table", 2)
            };
            // rd selects an unchanged nonzero-valued default, even with enable true.
            let q = and(
                and(eq(w("rd", 2), bv(2, 3)), eq(w("rs", 2), bv(2, 3))),
                and(
                    var("en".into(), Sort::Bool),
                    and(eq(w("r3", 2), bv(2, 2)), eq(lhs, bv(2, 2))),
                ),
            );
            let result = super::super::solve_with_hint(&q, &context, Limits::default(), hint);
            assert_eq!(result.verdict, Verdict::Sat, "{}", result.diagnostics());
            assert!(result.original_formula_validated);
            for limits in [
                Limits {
                    max_work: 1,
                    ..Limits::default()
                },
                Limits {
                    max_clauses: 1,
                    ..Limits::default()
                },
                Limits {
                    timeout_ms: 0,
                    ..Limits::default()
                },
            ] {
                let result = super::super::solve_with_hint(&q, &context, limits, hint);
                assert_eq!(result.verdict, Verdict::Unknown);
                assert!(!result.original_formula_validated);
            }
        }
    }
    let source = validate_source(&t, &Env::new(), &mut b()).unwrap();
    for allowance in [0, 1, 16, 32, 64] {
        let mut budget = b();
        budget.limits.max_terms = source + allowance;
        let mut r = Rewrite {
            source_nodes: source,
            ..Rewrite::default()
        };
        let _ = r.read_write(&t, &mut budget).unwrap();
        assert!(r.created <= allowance);
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
