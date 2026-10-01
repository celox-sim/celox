//! Same original formulas under both declared hints. Hints are never oracles.
mod semantics;
use hwverify_ir::*;
use hwverify_solver::finite::{self, Limits, Scalar, SearchHint, Verdict};
use hwverify_solver::{Check, QueryOptions};
use semantics::*;
use serde_json::{json, Value};
use std::{collections::BTreeMap, fs, path::PathBuf, time::Instant};
fn b(n: &str) -> Term {
    var(n.into(), Sort::Bool)
}
fn w(n: &str, width: u32) -> Term {
    var(n.into(), Sort::Bv(width))
}
fn any(xs: Vec<Term>) -> Term {
    xs.into_iter()
        .fold(boolv(false), |a, b| node(Sort::Bool, "or", vec![a, b]))
}
fn all(xs: Vec<Term>) -> Term {
    xs.into_iter().fold(boolv(true), and)
}
fn hint_name(h: SearchHint) -> &'static str {
    match h {
        SearchHint::Sat => "sat",
        SearchHint::Unsat => "unsat",
    }
}
fn ph(prefix: &str, pigeons: usize, holes: usize) -> Term {
    let cell = |p, h| b(&format!("{prefix}_{p}_{h}"));
    let mut xs = vec![];
    for p in 0..pigeons {
        xs.push(any((0..holes).map(|h| cell(p, h)).collect()));
    }
    for h in 0..holes {
        for p in 0..pigeons {
            for q in p + 1..pigeons {
                xs.push(any(vec![not(cell(p, h)), not(cell(q, h))]));
            }
        }
    }
    all(xs)
}
fn branches(n: usize, winner: Option<usize>, holes: usize) -> (Term, Env) {
    let choices = (0..n)
        .map(|i| b(&format!("choice_{i:03}")))
        .collect::<Vec<_>>();
    let t = and(
        all(choices
            .iter()
            .enumerate()
            .map(|(i, g)| {
                node(
                    Sort::Bool,
                    "=>",
                    vec![
                        g.clone(),
                        if winner == Some(i) {
                            ph(&format!("p{i}"), 2, 2)
                        } else {
                            ph(&format!("p{i}"), holes + 1, holes)
                        },
                    ],
                )
            })
            .collect()),
        any(choices.clone()),
    );
    let mut ctx = Env::from([
        ("original".into(), t.clone()),
        ("unrelated".into(), w("context_only", 64)),
    ]);
    for (i, t) in choices.into_iter().enumerate() {
        ctx.insert(format!("choice_{i:03}"), t);
    }
    (t, ctx)
}
struct Audit {
    rows: Vec<Value>,
    counts: BTreeMap<String, u64>,
    out: PathBuf,
}
impl Audit {
    fn check(
        &mut self,
        label: &str,
        t: &Term,
        ctx: &Env,
        h: SearchHint,
        expect: Verdict,
        limits: Limits,
    ) -> finite::Outcome {
        let o = finite::solve_with_hint(t, ctx, limits.clone(), h);
        assert_eq!(o.verdict, expect, "{label}/{h:?}: {}", o.diagnostics());
        assert_eq!(o.diagnostics()["search_hint"], hint_name(h));
        *self
            .counts
            .entry(format!("{}_{}", hint_name(h), expect.as_str()))
            .or_default() += 1;
        if o.verdict == Verdict::Unknown {
            assert!(o.reason.is_some());
        } else {
            assert!(o.stats.work <= limits.max_work);
            assert!(o.stats.clauses <= limits.max_clauses);
            assert!(o.stats.variables <= limits.max_variables);
            assert!(o.stats.terms <= limits.max_terms);
        }
        if h == SearchHint::Unsat {
            assert_eq!(o.stats.probe_work, 0, "UNSAT hint paid probe work");
            assert!(o.stats.probe_result.is_none());
        }
        if o.verdict == Verdict::Sat {
            assert!(o.original_formula_validated);
            let a: Assignment = o
                .assignments
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        match v {
                            Scalar::Bool(v) => V::B(*v),
                            Scalar::Bv { value, .. } => V::W(*value),
                        },
                    )
                })
                .collect();
            assert!(boolean(eval(t, &a)), "original formula witness failed");
            let mut vars = BTreeMap::new();
            variables(t, &mut vars);
            for t in ctx.values() {
                variables(t, &mut vars);
            }
            for (n, s) in vars {
                match (s, &o.assignments[&n]) {
                    (Sort::Bool, Scalar::Bool(_)) => {}
                    (Sort::Bv(w), Scalar::Bv { width, value }) => {
                        assert_eq!(w, *width);
                        assert!(*value <= mask(w));
                    }
                    _ => panic!("bad witness sort"),
                }
            }
            for (k, t) in ctx {
                let v = match &o.context_values[k] {
                    Scalar::Bool(v) => V::B(*v),
                    Scalar::Bv { value, .. } => V::W(*value),
                };
                assert_eq!(eval(t, &a), v);
            }
            *self
                .counts
                .entry("independent_sat_replays".into())
                .or_default() += 1;
        } else {
            assert!(!o.original_formula_validated);
            assert!(o.assignments.is_empty());
            assert!(o.context_values.is_empty());
        }
        if o.verdict == Verdict::Unsat
            && o.stats.split_alternatives > 0
            && o.stats.probe_result != Some(Verdict::Unsat)
        {
            assert_eq!(o.stats.split_completed, o.stats.split_alternatives);
            assert_eq!(o.stats.split_unsat, o.stats.split_alternatives);
        }
        self.rows.push(json!({"case":label,"declared_hint":hint_name(h),"actual":expect.as_str(),"outcome":o.diagnostics()}));
        o
    }
    fn pair(&mut self, label: &str, t: &Term, ctx: &Env, expected: Verdict) {
        for h in [SearchHint::Sat, SearchHint::Unsat] {
            self.check(label, t, ctx, h, expected, Limits::default());
        }
    }
}
fn direct(a: &mut Audit) {
    let p = b("p");
    let q = b("q");
    let mut pool = vec![
        boolv(true),
        boolv(false),
        p.clone(),
        q.clone(),
        not(p.clone()),
        not(q.clone()),
    ];
    for x in [p.clone(), not(p.clone())] {
        for y in [q.clone(), not(q.clone())] {
            pool.extend([
                and(x.clone(), y.clone()),
                any(vec![x.clone(), y.clone()]),
                eq(x.clone(), y.clone()),
                ite(x.clone(), y.clone(), not(y.clone())),
            ]);
        }
    }
    for x in &pool {
        for y in &pool {
            for t in [
                and(x.clone(), y.clone()),
                any(vec![x.clone(), y.clone()]),
                not(eq(x.clone(), y.clone())),
            ] {
                let (sat, _) = exhaustive(&t);
                a.pair(
                    "same_bool_truth_formula",
                    &t,
                    &Env::new(),
                    if sat { Verdict::Sat } else { Verdict::Unsat },
                );
            }
        }
    }
    for n in [2, 3, 5, 9] {
        for winner in (0..n).map(Some).chain(std::iter::once(None)) {
            let (t, ctx) = branches(n, winner, 3);
            let expected = if winner.is_some() {
                Verdict::Sat
            } else {
                Verdict::Unsat
            };
            for h in [SearchHint::Sat, SearchHint::Unsat] {
                let o = a.check(
                    "branch_mismatched_or_matching_hint",
                    &t,
                    &ctx,
                    h,
                    expected,
                    Limits::default(),
                );
                for delta in [0, 1] {
                    a.check(
                        "exact_global_work",
                        &t,
                        &ctx,
                        h,
                        if delta == 0 {
                            expected
                        } else {
                            Verdict::Unknown
                        },
                        Limits {
                            max_work: o.stats.work - delta,
                            ..Limits::default()
                        },
                    );
                    a.check(
                        "exact_global_clauses",
                        &t,
                        &ctx,
                        h,
                        if delta == 0 {
                            expected
                        } else {
                            Verdict::Unknown
                        },
                        Limits {
                            max_clauses: o.stats.clauses - delta as usize,
                            ..Limits::default()
                        },
                    );
                }
            }
        }
    }
    for h in [SearchHint::Sat, SearchHint::Unsat] {
        let (t, ctx) = branches(4, Some(3), 4);
        for limits in [
            Limits {
                max_work: 0,
                ..Limits::default()
            },
            Limits {
                max_terms: 0,
                ..Limits::default()
            },
            Limits {
                max_variables: 0,
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
            Limits {
                max_depth: 0,
                ..Limits::default()
            },
        ] {
            a.check(
                "exhaustion_never_trusted_hint",
                &t,
                &ctx,
                h,
                Verdict::Unknown,
                limits,
            );
        }
        for bad in [
            node(Sort::Bool, "or", vec![p.clone()]),
            node(Sort::Bool, "unsupported", vec![]),
            w("x", 65),
            node(Sort::Mem(4, 4), "@array", vec![]),
        ] {
            a.check(
                "unsupported_never_trusted_hint",
                &bad,
                &Env::new(),
                h,
                Verdict::Unknown,
                Limits::default(),
            );
            let mut ctx = ctx.clone();
            ctx.insert("malformed_hidden_context".into(), bad);
            a.check(
                "bad_context_not_skipped",
                &t,
                &ctx,
                h,
                Verdict::Unknown,
                Limits::default(),
            );
        }
    }
    let (t, ctx) = branches(3, Some(2), 3);
    let default = finite::solve(&t, &ctx, Limits::default());
    let unsat = finite::solve_with_hint(&t, &ctx, Limits::default(), SearchHint::Unsat);
    assert_eq!(default.diagnostics(), unsat.diagnostics());
    a.counts.insert("default_is_unsat_strategy".into(), 1);
}
fn integration(a: &mut Audit) {
    std::env::set_var("HWVERIFY_SOLVER", "finite");
    std::env::set_var("HWVERIFY_KERNEL", "off");
    let out = a.out.join("check-api");
    fs::create_dir_all(&out).unwrap();
    let mut c = Check {
        z3: "/independent-audit-must-not-call-z3".into(),
        out: out.clone(),
        reports: vec![],
    };
    let mut originals: BTreeMap<bool, String> = BTreeMap::new();
    for actual in [false, true] {
        let t = if actual {
            b("x")
        } else {
            and(b("x"), not(b("x")))
        };
        for logical in [false, true] {
            for env in ["query", "sat", "unsat"] {
                for api in [None, Some(SearchHint::Sat), Some(SearchHint::Unsat)] {
                    std::env::set_var("HWVERIFY_FINITE_SEARCH_HINT", env);
                    let id = format!(
                        "actual_{actual}_logical_{logical}_env_{env}_api_{}",
                        api.map(hint_name).unwrap_or("query")
                    );
                    let hint = api.unwrap_or(match env {
                        "sat" => SearchHint::Sat,
                        "unsat" => SearchHint::Unsat,
                        _ => {
                            if logical {
                                SearchHint::Sat
                            } else {
                                SearchHint::Unsat
                            }
                        }
                    });
                    c.query_with_options(
                        &id,
                        t.clone(),
                        logical,
                        &Env::from([
                            ("x".into(), b("x")),
                            ("unrelated".into(), w("free_context", 16)),
                        ]),
                        QueryOptions {
                            timeout_ms: 10_000,
                            capture_sat: false,
                            finite_search_hint: api,
                        },
                    )
                    .unwrap();
                    let r = c.reports.last().unwrap();
                    assert_eq!(r["solver_result"], if actual { "sat" } else { "unsat" });
                    assert_eq!(r["finite"]["search_hint"], hint_name(hint));
                    assert_eq!(
                        r["logical_expectation"],
                        if logical { "sat" } else { "unsat" }
                    );
                    assert_eq!(
                        r["status"],
                        match (actual, logical) {
                            (true, true) | (false, false) => "passed",
                            (true, false) => "counterexample",
                            (false, true) => "failed_nonvacuity",
                        }
                    );
                    assert_eq!(r["z3_seconds"], 0.0);
                    assert_eq!(r["concrete_model"], actual);
                    let smt = fs::read_to_string(out.join(format!("{id}.smt2")))
                        .unwrap()
                        .split("(check-sat)")
                        .next()
                        .unwrap()
                        .to_string();
                    if let Some(old) = originals.get(&actual) {
                        assert_eq!(old, &smt, "hint/logical goal contaminated formula");
                    } else {
                        originals.insert(actual, smt);
                    }
                    *a.counts
                        .entry("logical_goal_hint_actual_api_matrix".into())
                        .or_default() += 1;
                }
            }
        }
    }
    std::env::remove_var("HWVERIFY_FINITE_SEARCH_HINT");
    for h in [SearchHint::Sat, SearchHint::Unsat] {
        for bad in [boolv(true), boolv(false)] {
            c.query_with_options(
                &format!("timeout_{h:?}_{:?}", bad.0.op),
                bad,
                true,
                &Env::new(),
                QueryOptions {
                    timeout_ms: 0,
                    capture_sat: true,
                    finite_search_hint: Some(h),
                },
            )
            .unwrap();
            assert_eq!(c.reports.last().unwrap()["status"], "unknown");
        }
    }
    for env in ["sat", "unsat", "query"] {
        std::env::set_var("HWVERIFY_FINITE_SEARCH_HINT", env);
        for kind in [
            hwverify_solver::BinderKind::Exists,
            hwverify_solver::BinderKind::Forall,
        ] {
            for logical in [false, true] {
                let formula = hwverify_solver::QuantifiedFormula::Atom(b("bound"))
                    .bind(kind, vec![b("bound")]);
                c.query_quantified(
                    &format!("quantified_{}_{}_{logical}", env, c.reports.len()),
                    &formula,
                    logical,
                    &Env::new(),
                    10_000,
                )
                .unwrap();
                let r = c.reports.last().unwrap();
                assert_eq!(r["status"], "unknown");
                assert_eq!(r["solver_result"], "unknown");
                assert_eq!(r["z3_seconds"], 0.0);
                assert_eq!(r["kernel"]["enabled"], false);
                *a.counts
                    .entry("quantified_hint_does_not_bypass_unsupported".into())
                    .or_default() += 1;
            }
        }
    }
    assert!(hwverify_solver::solver("/no-z3", "(check-sat)")
        .unwrap_err()
        .contains("disabled"));
    fs::write(
        a.out.join("check-api-reports.json"),
        serde_json::to_string_pretty(&c.reports).unwrap(),
    )
    .unwrap();
    for name in [
        "HWVERIFY_FINITE_SEARCH_HINT",
        "HWVERIFY_SOLVER",
        "HWVERIFY_KERNEL",
    ] {
        std::env::remove_var(name);
    }
}
fn main() {
    let out = PathBuf::from(std::env::var("EXPECTED_AUDIT_OUT").expect("fresh EXPECTED_AUDIT_OUT"));
    fs::create_dir_all(&out).unwrap();
    let start = Instant::now();
    let mut a = Audit {
        rows: vec![],
        counts: BTreeMap::new(),
        out,
    };
    direct(&mut a);
    integration(&mut a);
    fs::write(a.out.join("summary.json"),serde_json::to_string_pretty(&json!({"status":"pass","counts":a.counts,"rows":a.rows,"seconds_not_benchmark":start.elapsed().as_secs_f64()})).unwrap()).unwrap();
    println!("{}", json!({"status":"pass","counts":a.counts}));
}
