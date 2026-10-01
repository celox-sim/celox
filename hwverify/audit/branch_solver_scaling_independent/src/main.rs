//! Independent public-API tests of canonical mux encoding and original semantics.
//! No access to the encoder, SAT state, learned clauses, or production evaluator.
#[path = "../../finite_speed_independent/src/semantics.rs"]
mod semantics;
use hwverify_ir::*;
use hwverify_solver::finite::{self, Limits, Scalar, Verdict};
use semantics::*;
use serde_json::{json, Value};
use std::{collections::BTreeMap, fs, path::PathBuf, time::Instant};

fn b(n: &str) -> Term {
    var(n.into(), Sort::Bool)
}
fn w(n: &str, width: u32) -> Term {
    var(n.into(), Sort::Bv(width))
}
fn unary(op: &str, x: Term) -> Term {
    node(x.0.sort.clone(), op, vec![x])
}
fn binary(op: &str, x: Term, y: Term) -> Term {
    node(x.0.sort.clone(), op, vec![x, y])
}
fn all(mut ts: Vec<Term>) -> Term {
    if ts.is_empty() {
        return boolv(true);
    }
    while ts.len() > 1 {
        ts = ts
            .chunks(2)
            .map(|x| {
                if x.len() == 1 {
                    x[0].clone()
                } else {
                    and(x[0].clone(), x[1].clone())
                }
            })
            .collect();
    }
    ts.pop().unwrap()
}
fn literal(v: &V, s: &Sort) -> Term {
    match (v, s) {
        (V::B(v), Sort::Bool) => boolv(*v),
        (V::W(v), Sort::Bv(w)) => bv(*w, *v),
        _ => panic!("bad literal"),
    }
}
fn pin(a: &Assignment, t: &Term) -> Term {
    let mut vars = BTreeMap::new();
    variables(t, &mut vars);
    all(vars
        .into_iter()
        .map(|(n, s)| eq(var(n.clone(), s.clone()), literal(&a[&n], &s)))
        .collect())
}
fn smt(t: &Term) -> String {
    if let Some(n) = t.0.op.strip_prefix('@') {
        return format!("|{n}|");
    }
    if t.0.args.is_empty() {
        return t.0.op.clone();
    }
    format!(
        "({} {})",
        t.0.op,
        t.0.args.iter().map(smt).collect::<Vec<_>>().join(" ")
    )
}
struct Audit {
    out: PathBuf,
    counts: BTreeMap<String, u64>,
    phases: BTreeMap<String, u64>,
    truth_rows: u64,
    oracle: Vec<Value>,
}
impl Audit {
    fn check(
        &mut self,
        phase: &str,
        t: Term,
        ctx: Env,
        expected: Verdict,
        limits: Limits,
    ) -> finite::Outcome {
        let o = finite::solve(&t, &ctx, limits);
        assert_eq!(
            o.verdict,
            expected,
            "{phase}: {}\n{}",
            smt(&t),
            o.diagnostics()
        );
        *self.counts.entry(o.verdict.as_str().into()).or_default() += 1;
        *self.phases.entry(phase.into()).or_default() += 1;
        if o.verdict == Verdict::Sat {
            assert!(o.original_formula_validated);
            let a: Assignment = o
                .assignments
                .iter()
                .map(|(n, v)| {
                    (
                        n.clone(),
                        match v {
                            Scalar::Bool(v) => V::B(*v),
                            Scalar::Bv { value, .. } => V::W(*value),
                        },
                    )
                })
                .collect();
            let mut vars = BTreeMap::new();
            variables(&t, &mut vars);
            for t in ctx.values() {
                variables(t, &mut vars)
            }
            for (n, s) in vars {
                match (&s, &o.assignments[&n]) {
                    (Sort::Bool, Scalar::Bool(_)) => {}
                    (Sort::Bv(w), Scalar::Bv { width, value }) => {
                        assert_eq!(w, width);
                        assert!(*value <= mask(*w));
                    }
                    _ => panic!("model sort"),
                }
            }
            assert!(
                boolean(eval(&t, &a)),
                "original-query replay failed: {phase}"
            );
            for (n, t) in ctx {
                let v = match &o.context_values[&n] {
                    Scalar::Bool(v) => V::B(*v),
                    Scalar::Bv { value, .. } => V::W(*value),
                };
                assert_eq!(eval(&t, &a), v, "context {n}");
            }
            *self
                .counts
                .entry("independent_sat_replays".into())
                .or_default() += 1;
        } else {
            assert!(!o.original_formula_validated);
            assert!(o.assignments.is_empty() && o.context_values.is_empty());
            if o.verdict == Verdict::Unknown {
                assert!(o.reason.is_some());
            }
        }
        o
    }
    fn exact(&mut self, phase: &str, t: Term, ctx: Env) {
        let (sat, rows) = exhaustive(&t);
        self.truth_rows += rows;
        self.check(
            phase,
            t,
            ctx,
            if sat { Verdict::Sat } else { Verdict::Unsat },
            Limits::default(),
        );
    }
    fn pinned(&mut self, phase: &str, t: Term, a: Assignment) {
        let truth = eval(&t, &a);
        let claim = eq(t.clone(), literal(&truth, &t.0.sort));
        let guard = pin(&a, &t);
        let ctx = Env::from([("expression".into(), t)]);
        self.check(
            phase,
            and(guard.clone(), claim.clone()),
            ctx.clone(),
            Verdict::Sat,
            Limits::default(),
        );
        self.check(
            phase,
            and(guard, not(claim)),
            ctx,
            Verdict::Unsat,
            Limits::default(),
        );
        self.truth_rows += 1;
    }
    fn oracle_case(&mut self, label: &str, t: Term, ctx: Env, expect: Verdict) {
        let o = self.check(label, t.clone(), ctx, expect, Limits::default());
        let id = self.oracle.len();
        let path = self.out.join("original-smt").join(format!("{id:04}.smt2"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut vars = BTreeMap::new();
        variables(&t, &mut vars);
        let decl = vars
            .iter()
            .map(|(n, s)| format!("(declare-fun |{n}| () {})\n", s.smt()))
            .collect::<String>();
        fs::write(
            &path,
            format!(
                "(set-logic QF_BV)\n{decl}(assert {})\n(check-sat)\n",
                smt(&t)
            ),
        )
        .unwrap();
        self.oracle.push(json!({"label":label,"file":format!("original-smt/{id:04}.smt2"),"finite_result":o.verdict.as_str(),"diagnostics":o.diagnostics()}));
    }
}
fn bool_mux(a: &mut Audit) {
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
    for g in &pool {
        for x in &pool {
            for y in &pool {
                let t = ite(g.clone(), x.clone(), y.clone());
                for bits in 0..8 {
                    a.pinned(
                        "mux_complete_polarity_alias_truth_table",
                        t.clone(),
                        Assignment::from([
                            ("p".into(), V::B(bits & 1 != 0)),
                            ("q".into(), V::B(bits & 2 != 0)),
                            ("r".into(), V::B(bits & 4 != 0)),
                        ]),
                    );
                }
                // Simultaneously encoding related nodes tests cache key normalization and
                // sharing of output complements, guard complements, and exchanged arms.
                for equivalent in [
                    ite(not(g.clone()), y.clone(), x.clone()),
                    not(ite(g.clone(), not(x.clone()), not(y.clone()))),
                ] {
                    a.exact(
                        "mux_canonical_cache_equivalence",
                        not(eq(t.clone(), equivalent)),
                        Env::new(),
                    );
                }
            }
        }
    }
}
struct R(u64);
impl R {
    fn n(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 as usize % n
    }
}
fn random_word(r: &mut R, width: u32, depth: u32) -> Term {
    if depth == 0 {
        return match r.n(3) {
            0 => w("x", width),
            1 => w("y", width),
            _ => bv(width, r.n(16) as u64),
        };
    }
    let x = random_word(r, width, depth - 1);
    let y = random_word(r, width, depth - 1);
    match r.n(8) {
        0 => binary("bvadd", x, y),
        1 => binary("bvsub", x, y),
        2 => binary("bvxor", x, y),
        3 => binary("bvand", x, y),
        4 => unary("bvnot", x),
        _ => ite(if r.n(2) == 0 { b("p") } else { not(b("q")) }, x, y),
    }
}
fn nested(a: &mut Audit) {
    let mut r = R(0x11ba_5e5e_c0de_2026);
    for width in 1..=3 {
        for _ in 0..96 {
            let x = random_word(&mut r, width, 3);
            let y = random_word(&mut r, width, 2);
            let t = if r.n(2) == 0 {
                eq(x.clone(), y.clone())
            } else {
                not(eq(x.clone(), y.clone()))
            };
            a.exact(
                "nested_mux_arithmetic_exhaustive",
                t,
                Env::from([("lhs".into(), x), ("rhs".into(), y)]),
            );
        }
    }
    for width in [1, 2, 3, 4, 8, 16, 32, 64] {
        let values = [0, 1, mask(width), mask(width) / 2, 1u64 << (width - 1)];
        let x = w("x", width);
        let y = w("y", width);
        let left = ite(
            b("p"),
            binary("bvadd", x.clone(), y.clone()),
            binary("bvsub", x.clone(), y.clone()),
        );
        let right = ite(
            not(b("p")),
            binary("bvsub", x.clone(), y.clone()),
            binary("bvadd", x.clone(), y.clone()),
        );
        a.oracle_case(
            "wide_mux_swapped_guard",
            not(eq(left.clone(), right)),
            Env::new(),
            Verdict::Unsat,
        );
        let identity = eq(
            ite(b("p"), ite(b("q"), x.clone(), y.clone()), x.clone()),
            ite(and(b("p"), not(b("q"))), y.clone(), x.clone()),
        );
        a.oracle_case(
            "wide_nested_mux_identity",
            not(identity),
            Env::new(),
            Verdict::Unsat,
        );
        for xv in values {
            for yv in values {
                for bits in 0..4 {
                    let m = Assignment::from([
                        ("x".into(), V::W(xv)),
                        ("y".into(), V::W(yv)),
                        ("p".into(), V::B(bits & 1 != 0)),
                        ("q".into(), V::B(bits & 2 != 0)),
                    ]);
                    let t = ite(
                        not(b("q")),
                        left.clone(),
                        unary("bvnot", ite(b("p"), y.clone(), x.clone())),
                    );
                    a.pinned("wide_nested_mux_boundary", t, m);
                }
            }
        }
        for kind in ["bvshl", "bvlshr"] {
            let t = eq(
                binary(kind, ite(b("p"), x.clone(), y.clone()), bv(width, 1)),
                ite(
                    b("p"),
                    binary(kind, x.clone(), bv(width, 1)),
                    binary(kind, y.clone(), bv(width, 1)),
                ),
            );
            a.oracle_case("mux_shift_distribution", not(t), Env::new(), Verdict::Unsat);
        }
        // Deliberately inequivalent nested muxes must retain counterexamples.
        a.oracle_case(
            "wide_mux_adversarial_sat",
            and(
                not(eq(x.clone(), y.clone())),
                not(eq(ite(b("p"), x.clone(), y.clone()), ite(b("p"), y, x))),
            ),
            Env::from([("unused_word".into(), w("extra", 64))]),
            Verdict::Sat,
        );
    }
}
fn controls(a: &mut Audit) {
    let t = eq(ite(b("p"), w("x", 8), w("y", 8)), bv(8, 73));
    let ctx = Env::from([(
        "context_mux".into(),
        ite(b("q"), w("extra", 64), bv(64, u64::MAX)),
    )]);
    let full = a.check(
        "control_full",
        t.clone(),
        ctx.clone(),
        Verdict::Sat,
        Limits::default(),
    );
    for n in [
        0,
        1,
        2,
        3,
        7,
        15,
        31,
        63,
        127,
        255,
        511,
        1023,
        full.stats.work - 1,
        full.stats.work,
    ] {
        let limits = Limits {
            max_work: n,
            ..Limits::default()
        };
        a.check(
            "control_exact_work",
            t.clone(),
            ctx.clone(),
            if n < full.stats.work {
                Verdict::Unknown
            } else {
                Verdict::Sat
            },
            limits,
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
            max_depth: 1,
            ..Limits::default()
        },
        Limits {
            max_clauses: 1,
            ..Limits::default()
        },
        Limits {
            max_variables: 1,
            ..Limits::default()
        },
    ] {
        a.check(
            "control_resource_unknown",
            t.clone(),
            ctx.clone(),
            Verdict::Unknown,
            limits,
        );
    }
    let bad = [
        node(Sort::Bool, "uninterpreted", vec![]),
        node(Sort::Bool, "ite", vec![b("p"), b("q")]),
        node(Sort::Bool, "ite", vec![bv(1, 0), b("p"), b("q")]),
    ];
    for invalid in bad {
        for formula in [
            ite(boolv(true), boolv(true), invalid.clone()),
            ite(boolv(false), invalid.clone(), boolv(false)),
            and(boolv(false), invalid.clone()),
        ] {
            a.check(
                "control_dead_malformed_branch",
                formula,
                Env::new(),
                Verdict::Unknown,
                Limits::default(),
            );
        }
    }
    let ctx = Env::from([(
        "bad".into(),
        node(Sort::Bv(8), "bvudiv", vec![bv(8, 1), bv(8, 1)]),
    )]);
    a.check(
        "control_unsupported_context",
        boolv(false),
        ctx,
        Verdict::Unknown,
        Limits::default(),
    );
    for _ in 0..16 {
        a.check(
            "control_interleaved_sat",
            t.clone(),
            Env::new(),
            Verdict::Sat,
            Limits::default(),
        );
        a.check(
            "control_interleaved_unknown",
            t.clone(),
            Env::new(),
            Verdict::Unknown,
            Limits {
                max_work: 0,
                ..Limits::default()
            },
        );
        a.check(
            "control_interleaved_unsat",
            not(eq(
                ite(b("p"), b("q"), b("r")),
                ite(not(b("p")), b("r"), b("q")),
            )),
            Env::new(),
            Verdict::Unsat,
            Limits::default(),
        );
    }
}
fn equality_aliasing(a: &mut Audit) {
    // Equality is mandatory only along a positive conjunction spine. Every
    // alternative below permits x!=y for some valuations and must retain them.
    for width in 1..=4 {
        let x = w("x", width);
        let y = w("y", width);
        let z = w("z", width);
        let equal = eq(x.clone(), y.clone());
        let different = not(equal.clone());
        let alternatives = vec![
            node(Sort::Bool, "or", vec![equal.clone(), b("p")]),
            node(Sort::Bool, "=>", vec![equal.clone(), boolv(false)]),
            node(Sort::Bool, "=>", vec![b("p"), equal.clone()]),
            not(and(equal.clone(), b("p"))),
            ite(b("p"), equal.clone(), boolv(true)),
            ite(b("p"), boolv(true), equal.clone()),
            eq(equal.clone(), b("p")),
            node(Sort::Bool, "xor", vec![equal.clone(), b("p")]),
            node(
                Sort::Bool,
                "or",
                vec![and(equal.clone(), b("p")), not(b("p"))],
            ),
            not(not(node(Sort::Bool, "or", vec![equal.clone(), b("p")]))),
        ];
        for condition in alternatives {
            a.exact(
                "equality_nonmandatory_polarity",
                and(condition, different.clone()),
                Env::from([("alias_x".into(), x.clone()), ("alias_y".into(), y.clone())]),
            );
        }
        let sumx = binary("bvadd", x.clone(), z.clone());
        let sumy = binary("bvadd", y.clone(), z.clone());
        for relation in [
            and(equal.clone(), eq(y.clone(), z.clone())),
            and(eq(z.clone(), y.clone()), eq(x.clone(), z.clone())),
            all(vec![
                eq(x.clone(), y.clone()),
                eq(y.clone(), z.clone()),
                eq(z.clone(), x.clone()),
            ]),
        ] {
            a.exact(
                "equality_transitive_cycle",
                and(relation.clone(), not(eq(sumx.clone(), sumy.clone()))),
                Env::new(),
            );
            a.exact(
                "equality_transitive_sat",
                and(relation, eq(x.clone(), bv(width, mask(width)))),
                Env::from([
                    ("x".into(), x.clone()),
                    ("y".into(), y.clone()),
                    ("z".into(), z.clone()),
                    (
                        "nontrivial_context".into(),
                        ite(b("p"), sumx.clone(), sumy.clone()),
                    ),
                ]),
            );
        }
        // Equality in a context has no assertion status whatsoever.
        a.exact(
            "equality_context_not_assumed",
            different,
            Env::from([("only_context".into(), equal)]),
        );
    }
    for width in [1, 4, 8, 16, 32, 64] {
        let mut conjuncts = vec![];
        let mut ctx = Env::new();
        for i in 0..65 {
            let left = w(&format!("alias_{i:03}"), width);
            let right = w(&format!("alias_{:03}", (i + 1) % 65), width);
            conjuncts.push(eq(left.clone(), right));
            ctx.insert(format!("member_{i:03}"), left);
        }
        conjuncts.push(eq(w("alias_000", width), bv(width, mask(width))));
        a.oracle_case(
            "equality_large_cycle_original_names",
            all(conjuncts),
            ctx,
            Verdict::Sat,
        );
    }
    for bool_cycle in [
        all(vec![
            eq(b("p"), b("q")),
            eq(b("q"), b("r")),
            eq(b("r"), b("p")),
            b("p"),
        ]),
        all(vec![
            eq(b("p"), b("q")),
            eq(b("q"), b("r")),
            eq(b("r"), b("p")),
            b("p"),
            not(b("r")),
        ]),
    ] {
        a.exact(
            "equality_boolean_cycle",
            bool_cycle,
            Env::from([
                ("p".into(), b("p")),
                ("q".into(), b("q")),
                ("r".into(), b("r")),
            ]),
        );
    }
    for (formula, ctx) in [
        (
            eq(w("x", 4), w("y", 4)),
            Env::from([("wrong_sort".into(), b("y"))]),
        ),
        (
            and(eq(w("x", 4), w("y", 4)), eq(w("y", 8), w("z", 8))),
            Env::new(),
        ),
        (eq(w("x", 4), w("y", 8)), Env::new()),
        (
            eq(node(Sort::Bv(4), "@x", vec![bv(4, 0)]), w("y", 4)),
            Env::new(),
        ),
        (eq(node(Sort::Bv(4), "@", vec![]), w("y", 4)), Env::new()),
        (eq(w("x", 0), w("y", 0)), Env::new()),
        (eq(w("x", 65), w("y", 65)), Env::new()),
        (
            all(vec![
                eq(w("x", 4), w("y", 4)),
                eq(w("y", 4), w("z", 4)),
                eq(b("z"), b("x")),
            ]),
            Env::new(),
        ),
    ] {
        a.check(
            "equality_invalid_signature_unknown",
            formula,
            ctx,
            Verdict::Unknown,
            Limits::default(),
        );
    }
    let t = all(vec![
        eq(w("x", 8), w("y", 8)),
        eq(w("y", 8), w("z", 8)),
        eq(w("z", 8), bv(8, 21)),
    ]);
    let ctx = Env::from([
        ("whole_class".into(), binary("bvadd", w("x", 8), w("y", 8))),
        ("context_only".into(), w("other", 16)),
    ]);
    let full = a.check(
        "equality_full_budget",
        t.clone(),
        ctx.clone(),
        Verdict::Sat,
        Limits::default(),
    );
    for delta in [0, 1] {
        a.check(
            "equality_exact_work_budget",
            t.clone(),
            ctx.clone(),
            if delta == 0 {
                Verdict::Sat
            } else {
                Verdict::Unknown
            },
            Limits {
                max_work: full.stats.work - delta,
                ..Limits::default()
            },
        );
        a.check(
            "equality_exact_variable_budget",
            t.clone(),
            ctx.clone(),
            if delta == 0 {
                Verdict::Sat
            } else {
                Verdict::Unknown
            },
            Limits {
                max_variables: full.stats.variables - delta as usize,
                ..Limits::default()
            },
        );
    }
}
fn nested_cofactoring(a: &mut Audit) {
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
    for g in [b("p"), not(b("p"))] {
        for x in &pool {
            for y in &pool {
                for z in &pool {
                    let same = ite(g.clone(), x.clone(), y.clone());
                    let opposite = ite(not(g.clone()), x.clone(), y.clone());
                    let pairs = [
                        (
                            ite(g.clone(), same.clone(), z.clone()),
                            ite(g.clone(), x.clone(), z.clone()),
                        ),
                        (
                            ite(g.clone(), opposite.clone(), z.clone()),
                            ite(g.clone(), y.clone(), z.clone()),
                        ),
                        (
                            ite(g.clone(), not(same.clone()), z.clone()),
                            ite(g.clone(), not(x.clone()), z.clone()),
                        ),
                        (
                            ite(g.clone(), not(opposite.clone()), z.clone()),
                            ite(g.clone(), not(y.clone()), z.clone()),
                        ),
                        (
                            ite(g.clone(), z.clone(), same.clone()),
                            ite(g.clone(), z.clone(), y.clone()),
                        ),
                        (
                            ite(g.clone(), z.clone(), opposite.clone()),
                            ite(g.clone(), z.clone(), x.clone()),
                        ),
                        (
                            ite(g.clone(), z.clone(), not(same)),
                            ite(g.clone(), z.clone(), not(y.clone())),
                        ),
                        (
                            ite(g.clone(), z.clone(), not(opposite)),
                            ite(g.clone(), z.clone(), not(x.clone())),
                        ),
                    ];
                    for (left, right) in pairs {
                        a.exact(
                            "nested_cofactoring_complete_polarity",
                            not(eq(left, right)),
                            Env::new(),
                        );
                    }
                }
            }
        }
    }
    // A different guard, even when its name is similar or an equality is merely
    // conditional, must not be treated as the outer guard.
    for g in [b("p"), not(b("p"))] {
        for h in [b("q"), not(b("q"))] {
            for x in &pool {
                for y in &pool {
                    let inner = ite(h.clone(), x.clone(), y.clone());
                    a.exact(
                        "nested_cofactoring_different_guard",
                        not(eq(
                            ite(g.clone(), inner, b("r")),
                            ite(g.clone(), x.clone(), b("r")),
                        )),
                        Env::new(),
                    );
                }
            }
        }
    }
}
fn main() {
    let out = PathBuf::from(std::env::var("BRANCH_AUDIT_OUT").expect("BRANCH_AUDIT_OUT"));
    fs::create_dir_all(&out).unwrap();
    let start = Instant::now();
    let mut a = Audit {
        out,
        counts: BTreeMap::new(),
        phases: BTreeMap::new(),
        truth_rows: 0,
        oracle: vec![],
    };
    bool_mux(&mut a);
    nested(&mut a);
    controls(&mut a);
    equality_aliasing(&mut a);
    nested_cofactoring(&mut a);
    let summary = json!({"status":"pass","counts":a.counts,"phases":a.phases,"exhaustive_or_pinned_assignment_rows":a.truth_rows,"oracle_cases":a.oracle,"seconds_not_benchmark":start.elapsed().as_secs_f64(),"oracle":"Audit-owned integer evaluator and enumeration; saved original SMT checked separately"});
    fs::write(
        a.out.join("summary.json"),
        serde_json::to_string_pretty(&summary).unwrap(),
    )
    .unwrap();
    println!(
        "{}",
        json!({"status":"pass","counts":a.counts,"phases":a.phases,"truth_rows":a.truth_rows})
    );
}
