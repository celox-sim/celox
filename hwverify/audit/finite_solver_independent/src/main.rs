mod semantics;
use hwverify_ir::*;
use hwverify_solver::finite::{self, Limits, Scalar, Verdict};
use semantics::*;
use serde_json::{json, Value};
use std::{collections::BTreeMap, fs, time::Instant};
const BV_OPS: [&str; 9] = [
    "bvnot", "bvand", "bvor", "bvxor", "bvadd", "bvsub", "bvmul", "bvshl", "bvlshr",
];
const CMP_OPS: [&str; 4] = ["bvult", "bvule", "bvslt", "bvsle"];
struct Audit {
    counts: BTreeMap<String, u64>,
    phases: BTreeMap<String, u64>,
    truth_rows: u64,
    checks: Vec<Value>,
    start: Instant,
}
impl Audit {
    fn new() -> Self {
        Self {
            counts: BTreeMap::new(),
            phases: BTreeMap::new(),
            truth_rows: 0,
            checks: vec![],
            start: Instant::now(),
        }
    }
    fn check(&mut self, label: &str, t: Term, ctx: Env, expect: Verdict, limits: Limits) {
        let out = finite::solve(&t, &ctx, limits);
        *self.counts.entry(out.verdict.as_str().into()).or_default() += 1;
        *self.counts.entry("cdcl_decisions".into()).or_default() += out.stats.decisions;
        *self.counts.entry("cdcl_conflicts".into()).or_default() += out.stats.conflicts;
        *self
            .phases
            .entry(label.split('/').next().unwrap().into())
            .or_default() += 1;
        if out.verdict != expect {
            self.fail(
                label,
                &t,
                &out,
                format!("verdict mismatch: expected {expect:?}"),
            );
        }
        if out.verdict == Verdict::Sat {
            let model: Assignment = out
                .assignments
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        match v {
                            Scalar::Bool(b) => V::B(*b),
                            Scalar::Bv { value, .. } => V::W(*value),
                        },
                    )
                })
                .collect();
            let mut vars = BTreeMap::new();
            variables(&t, &mut vars);
            for v in ctx.values() {
                variables(v, &mut vars)
            }
            for (k, s) in vars {
                let got = out
                    .assignments
                    .get(&k)
                    .unwrap_or_else(|| panic!("{label}: missing model {k}"));
                match (&s, got) {
                    (Sort::Bool, Scalar::Bool(_)) => {}
                    (Sort::Bv(w), Scalar::Bv { width, value })
                        if w == width && *value <= mask(*w) => {}
                    _ => self.fail(label, &t, &out, format!("ill-typed assignment {k}")),
                }
            }
            if !boolean(eval(&t, &model)) {
                self.fail(
                    label,
                    &t,
                    &out,
                    "independent original formula replay false".into(),
                );
            }
            if !out.original_formula_validated {
                self.fail(label, &t, &out, "SAT lacks original validation flag".into());
            }
            for (name, term) in &ctx {
                let got = out
                    .context_values
                    .get(name)
                    .unwrap_or_else(|| panic!("{label}: missing context {name}"));
                let got = match got {
                    Scalar::Bool(b) => V::B(*b),
                    Scalar::Bv { value, .. } => V::W(*value),
                };
                if got != eval(term, &model) {
                    self.fail(label, &t, &out, format!("context value mismatch {name}"));
                }
            }
            *self
                .counts
                .entry("independent_sat_replays".into())
                .or_default() += 1;
        }
        if expect == Verdict::Unknown {
            assert!(out.reason.is_some(), "unknown must say why");
            assert!(out.assignments.is_empty());
            assert!(!out.original_formula_validated);
            self.checks
                .push(json!({"name":label,"result":"unknown","reason":out.reason,"decisions":out.stats.decisions,"conflicts":out.stats.conflicts,"terms":out.stats.terms}));
        }
    }
    fn fail(&self, label: &str, t: &Term, out: &finite::Outcome, reason: String) -> ! {
        let result = json!({"status":"FAIL","label":label,"reason":reason,"term":format!("{t:?}"),"outcome":out.diagnostics(),"counts_so_far":self.counts});
        fs::write(
            "results/finite_solver_independent/failure.json",
            serde_json::to_string_pretty(&result).unwrap(),
        )
        .unwrap();
        panic!("{result}")
    }
    fn expected(&mut self, label: &str, t: Term, ctx: Env, sat: bool) {
        self.check(
            label,
            t,
            ctx,
            if sat { Verdict::Sat } else { Verdict::Unsat },
            Limits::default(),
        )
    }
    fn exact(&mut self, label: &str, t: Term, ctx: Env) {
        let (sat, rows) = exhaustive(&t);
        self.truth_rows += rows;
        self.expected(label, t, ctx, sat)
    }
    fn unknown(&mut self, label: &str, t: Term, ctx: Env, limits: Limits) {
        self.check(label, t, ctx, Verdict::Unknown, limits)
    }
    fn pinned(&mut self, label: &str, expression: Term, assignment: Assignment) {
        let result = eval(&expression, &assignment);
        let literal = match result {
            V::B(b) => boolv(b),
            V::W(v) => {
                let Sort::Bv(w) = expression.0.sort else {
                    panic!()
                };
                bv(w, v)
            }
        };
        let mut vars = BTreeMap::new();
        variables(&expression, &mut vars);
        let guard = vars
            .into_iter()
            .map(|(name, sort)| {
                let v = assignment[&name].clone();
                let c = match v {
                    V::B(b) => boolv(b),
                    V::W(v) => {
                        let Sort::Bv(w) = sort else { panic!() };
                        bv(w, v)
                    }
                };
                eq(var(name, sort), c)
            })
            .fold(boolv(true), and);
        let relation = eq(expression.clone(), literal);
        let ctx = Env::from([("expression".into(), expression)]);
        self.expected(
            &format!("{label}/positive"),
            and(guard.clone(), relation.clone()),
            ctx.clone(),
            true,
        );
        self.expected(
            &format!("{label}/negative"),
            and(guard, not(relation)),
            ctx,
            false,
        );
    }
}
fn x(w: u32) -> Term {
    var("x".into(), Sort::Bv(w))
}
fn y(w: u32) -> Term {
    var("y".into(), Sort::Bv(w))
}
fn bvar(n: &str) -> Term {
    var(n.into(), Sort::Bool)
}
fn op(w: u32, name: &str, a: Term, b: Term) -> Term {
    node(
        if CMP_OPS.contains(&name) {
            Sort::Bool
        } else {
            Sort::Bv(w)
        },
        name,
        if name == "bvnot" { vec![a] } else { vec![a, b] },
    )
}
fn inputs(a: u64, b: u64) -> Assignment {
    Assignment::from([("x".into(), V::W(a)), ("y".into(), V::W(b))])
}
fn all(ts: Vec<Term>) -> Term {
    ts.into_iter().fold(boolv(true), and)
}
fn any(ts: Vec<Term>) -> Term {
    ts.into_iter()
        .fold(boolv(false), |a, b| node(Sort::Bool, "or", vec![a, b]))
}
struct R(u64);
impl R {
    fn next(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 as usize % n
    }
}
fn randword(r: &mut R, w: u32, d: usize) -> Term {
    if d == 0 {
        return match r.next(4) {
            0 => x(w),
            1 => y(w),
            _ => bv(w, r.next(1usize << w) as u64),
        };
    }
    let i = r.next(12);
    let a = randword(r, w, d - 1);
    let b = randword(r, w, d - 1);
    match i {
        0..=8 => op(w, BV_OPS[i], a, b),
        9 => ite(randpred(r, w, d - 1), a, b),
        10 => node(
            Sort::Bv(w),
            format!("(_ extract {} 0)", w - 1),
            vec![node(Sort::Bv(2 * w), "concat", vec![a, b])],
        ),
        _ => a,
    }
}
fn randpred(r: &mut R, w: u32, d: usize) -> Term {
    if d == 0 {
        return match r.next(4) {
            0 => bvar("p"),
            1 => bvar("q"),
            _ => node(
                Sort::Bool,
                ["=", "bvult", "bvule", "bvslt", "bvsle"][r.next(5)],
                vec![randword(r, w, 0), randword(r, w, 0)],
            ),
        };
    }
    match r.next(8) {
        0 => not(randpred(r, w, d - 1)),
        1..=3 => node(
            Sort::Bool,
            ["and", "or", "xor", "=>"][r.next(4)],
            vec![randpred(r, w, d - 1), randpred(r, w, d - 1)],
        ),
        4 => ite(
            randpred(r, w, d - 1),
            randpred(r, w, d - 1),
            randpred(r, w, d - 1),
        ),
        _ => node(
            Sort::Bool,
            ["=", "bvult", "bvule", "bvslt", "bvsle"][r.next(5)],
            vec![randword(r, w, d - 1), randword(r, w, d - 1)],
        ),
    }
}
fn main() {
    fs::create_dir_all("results/finite_solver_independent").unwrap();
    let mut a = Audit::new();
    for w in 1..=4 {
        for name in BV_OPS.into_iter().chain(CMP_OPS) {
            for xv in 0..1u64 << w {
                for yv in 0..1u64 << w {
                    if name == "bvnot" && yv != 0 {
                        continue;
                    }
                    a.pinned(
                        &format!("small_exhaustive/{name}/w{w}/x{xv}/y{yv}"),
                        op(w, name, x(w), y(w)),
                        inputs(xv, yv),
                    );
                }
            }
        }
    }
    eprintln!("small exhaustive complete: {:?}", a.counts);
    for w in 1..=4 {
        for name in BV_OPS.into_iter().chain(CMP_OPS) {
            let term = op(w, name, x(w), y(w));
            let values: Vec<Term> = if term.0.sort == Sort::Bool {
                vec![boolv(false), boolv(true)]
            } else {
                (0..1u64 << w).map(|v| bv(w, v)).collect()
            };
            for target in values {
                a.exact(
                    &format!("unbound_ranges/{name}/w{w}"),
                    eq(term.clone(), target),
                    Env::from([("value".into(), term.clone())]),
                );
            }
        }
    }
    for name in ["and", "or", "xor", "=>", "=", "ite", "not"] {
        for p in [false, true] {
            for q in [false, true] {
                for r in [false, true] {
                    let args = match name {
                        "not" => vec![bvar("p")],
                        "ite" => vec![bvar("p"), bvar("q"), bvar("r")],
                        _ => vec![bvar("p"), bvar("q")],
                    };
                    a.pinned(
                        &format!("boolean/{name}/{p}/{q}/{r}"),
                        node(Sort::Bool, name, args),
                        Assignment::from([
                            ("p".into(), V::B(p)),
                            ("q".into(), V::B(q)),
                            ("r".into(), V::B(r)),
                        ]),
                    );
                }
            }
        }
    }
    for w in 1..=4 {
        for xv in 0..1u64 << w {
            for lo in 0..w {
                for hi in lo..w {
                    a.pinned(
                        "extract",
                        node(
                            Sort::Bv(hi - lo + 1),
                            format!("(_ extract {hi} {lo})"),
                            vec![x(w)],
                        ),
                        inputs(xv, 0),
                    );
                }
            }
            for target in [w, w + 1, 8, 32, 64] {
                for name in ["zero_extend", "sign_extend"] {
                    a.pinned(
                        &format!("extensions/{name}/w{w}/target{target}"),
                        node(
                            Sort::Bv(target),
                            format!("(_ {name} {})", target - w),
                            vec![x(w)],
                        ),
                        inputs(xv, 0),
                    );
                }
            }
        }
    }
    for hi in 1..=4 {
        for lo in 1..=4 {
            for xv in 0..1u64 << hi {
                for yv in 0..1u64 << lo {
                    a.pinned(
                        "concat",
                        node(Sort::Bv(hi + lo), "concat", vec![x(hi), y(lo)]),
                        inputs(xv, yv),
                    );
                }
            }
        }
    }
    for w in [5, 7, 8, 16, 31, 32, 63, 64] {
        let vals = [
            0,
            1,
            2,
            (1u64 << (w - 1)) - 1,
            1u64 << (w - 1),
            mask(w) - 1,
            mask(w),
        ];
        for name in BV_OPS.into_iter().chain(CMP_OPS) {
            for (i, xv) in vals.iter().enumerate() {
                for (j, yv) in vals.iter().enumerate() {
                    if w > 16 && (i + j) % 3 != 0 {
                        continue;
                    }
                    if name == "bvnot" && j != 0 {
                        continue;
                    }
                    a.pinned(
                        &format!("width_boundaries/{name}/w{w}/x{xv}/y{yv}"),
                        op(w, name, x(w), y(w)),
                        inputs(*xv, *yv),
                    );
                }
            }
        }
        for shift in [0, 1, w as u64 - 1, w as u64, w as u64 + 1, mask(w)] {
            for xv in vals {
                for name in ["bvshl", "bvlshr"] {
                    a.pinned(
                        &format!("shift_boundaries/{name}/w{w}/shift{shift}"),
                        op(w, name, x(w), y(w)),
                        inputs(xv, shift),
                    );
                }
            }
        }
    }
    for (hi, lo) in [(1, 63), (31, 33), (32, 32), (63, 1)] {
        for xv in [0, 1, mask(hi)] {
            for yv in [0, 1, mask(lo)] {
                a.pinned(
                    "concat64",
                    node(Sort::Bv(64), "concat", vec![x(hi), y(lo)]),
                    inputs(xv, yv),
                );
            }
        }
    }
    let mut r = R(0x4c30ed1789);
    for w in 1..=4 {
        for i in 0..160 {
            let term = randpred(&mut r, w, 3);
            let term = match i % 4 {
                0 => and(term.clone(), not(term)),
                1 => and(
                    term,
                    eq(
                        op(w, "bvadd", x(w), y(w)),
                        bv(w, r.next(1usize << w) as u64),
                    ),
                ),
                2 => not(term),
                _ => term,
            };
            a.exact(&format!("random_formulas/w{w}/{i}"), term, Env::new());
        }
    }
    for i in 0..600 {
        let n = 3 + r.next(8);
        let k = 1 + r.next(n * 8);
        let mut clauses = vec![];
        for _ in 0..k {
            let mut lits = vec![];
            for _ in 0..(2 + r.next(2)) {
                let v = bvar(&format!("b{}", r.next(n)));
                lits.push(if r.next(2) == 0 { v } else { not(v) })
            }
            clauses.push(any(lits));
        }
        a.exact(&format!("random_cnf/{i}"), all(clauses), Env::new());
    }
    for (p, h) in [(3, 2), (3, 3), (4, 3), (5, 4)] {
        let var = |i, j| bvar(&format!("p{i}h{j}"));
        let mut c = vec![];
        for i in 0..p {
            c.push(any((0..h).map(|j| var(i, j)).collect()));
        }
        for j in 0..h {
            for i in 0..p {
                for k in i + 1..p {
                    c.push(not(and(var(i, j), var(k, j))))
                }
            }
        }
        a.expected(&format!("pigeonhole/{p}/{h}"), all(c), Env::new(), p <= h);
    }
    for flag in [false, true] {
        let term = eq(x(4), bv(4, 7));
        let ctx = Env::from([
            ("unconstrained".into(), y(64)),
            ("boolean".into(), bvar("extra")),
            ("derived".into(), ite(boolv(flag), x(4), bv(4, 15))),
        ]);
        a.expected("context_only_variables", term, ctx, true);
    }
    // Negative replay control: a deliberately corrupted witness must be rejected by our evaluator.
    let t = eq(x(4), bv(4, 7));
    assert!(boolean(eval(&t, &inputs(7, 0))));
    assert!(!boolean(eval(&t, &inputs(6, 0))));
    a.counts.insert("corrupt_model_negative_controls".into(), 1);
    additional_word_shapes(&mut a);
    systematic_cnf(&mut a);
    unsupported_and_limits(&mut a);
    search_exhaustion(&mut a);
    integration(&mut a);
    let summary = json!({"status":"PASS","counts":a.counts,"phases":a.phases,"exhaustive_formula_assignment_rows":a.truth_rows,"inconclusive_checks":a.checks,"elapsed_seconds":a.start.elapsed().as_secs_f64(),"oracle":"Audit-owned u64/i128 recursive evaluator plus exhaustive finite assignment enumeration; no Z3 invoked","scope":"Exhaustive small-width operation input valuations, exhaustive truth tables of generated formulas, selected large-width boundaries, bounded CNF search and fail-closed controls; not a formal proof"});
    fs::write(
        "results/finite_solver_independent/summary.json",
        serde_json::to_string_pretty(&summary).unwrap(),
    )
    .unwrap();
    println!("{summary}");
}
fn unsupported_and_limits(a: &mut Audit) {
    for op in [
        "bvashr", "bvudiv", "bvsdiv", "bvurem", "bvsrem", "bvsmod", "bvnand", "bvnor", "bvxnor",
    ] {
        let t = eq(node(Sort::Bv(4), op, vec![x(4), y(4)]), bv(4, 0));
        a.unknown(
            &format!("unsupported/{op}"),
            t,
            Env::new(),
            Limits::default(),
        );
    }
    let mem = var("memory".into(), Sort::Mem(2, 4));
    let read = node(Sort::Bv(4), "select", vec![mem.clone(), bv(2, 0)]);
    a.unknown(
        "unsupported/memory_read",
        eq(read, bv(4, 0)),
        Env::new(),
        Limits::default(),
    );
    a.unknown(
        "unsupported/memory_context",
        boolv(true),
        Env::from([("memory".into(), mem)]),
        Limits::default(),
    );
    a.unknown(
        "unsupported/dead_ite",
        ite(
            boolv(true),
            boolv(true),
            node(Sort::Bool, "quantifier", vec![]),
        ),
        Env::new(),
        Limits::default(),
    );
    a.unknown(
        "unsupported/false_and",
        and(boolv(false), node(Sort::Bool, "quantifier", vec![])),
        Env::new(),
        Limits::default(),
    );
    for (name, t) in [
        ("root_word", bv(4, 0)),
        (
            "zero_width",
            eq(
                var("bad".into(), Sort::Bv(0)),
                var("bad".into(), Sort::Bv(0)),
            ),
        ),
        (
            "wide_word",
            eq(
                var("bad".into(), Sort::Bv(65)),
                var("bad".into(), Sort::Bv(65)),
            ),
        ),
        ("mixed_equality", eq(x(4), x(3))),
        (
            "result_sort",
            eq(node(Sort::Bv(3), "bvadd", vec![x(4), y(4)]), bv(3, 0)),
        ),
        ("missing_operand", node(Sort::Bool, "bvult", vec![x(4)])),
        (
            "extra_operand",
            node(Sort::Bool, "not", vec![boolv(true), boolv(false)]),
        ),
        (
            "same_name_conflicting_sort",
            and(eq(x(4), bv(4, 0)), eq(x(3), bv(3, 0))),
        ),
        (
            "bad_extract",
            eq(node(Sort::Bv(4), "(_ extract 6 3)", vec![x(4)]), bv(4, 0)),
        ),
        (
            "bad_extension",
            eq(node(Sort::Bv(8), "(_ sign_extend 3)", vec![x(4)]), bv(8, 0)),
        ),
        (
            "malformed_literal",
            eq(node(Sort::Bv(4), "(_ bv7 8)", vec![]), bv(4, 7)),
        ),
    ] {
        a.unknown(
            &format!("malformed/{name}"),
            t,
            Env::new(),
            Limits::default(),
        );
    }
    let t = eq(op(8, "bvadd", x(8), y(8)), bv(8, 99));
    for budget in ["timeout", "work", "terms", "variables", "clauses", "depth"] {
        let mut limits = Limits::default();
        match budget {
            "timeout" => limits.timeout_ms = 0,
            "work" => limits.max_work = 0,
            "terms" => limits.max_terms = 0,
            "variables" => limits.max_variables = 0,
            "clauses" => limits.max_clauses = 0,
            "depth" => limits.max_depth = 0,
            _ => unreachable!(),
        };
        a.unknown(&format!("limits/{budget}"), t.clone(), Env::new(), limits);
    }
}

// All subsets of the 12 non-tautological, two-variable binary clauses over 3 variables.
fn systematic_cnf(a: &mut Audit) {
    let mut clauses = vec![];
    for i in 0..3 {
        for j in i + 1..3 {
            for si in [false, true] {
                for sj in [false, true] {
                    let v = bvar(&format!("c{i}"));
                    let w = bvar(&format!("c{j}"));
                    clauses.push(any(vec![
                        if si { v } else { not(v) },
                        if sj { w } else { not(w) },
                    ]));
                }
            }
        }
    }
    assert_eq!(clauses.len(), 12);
    for subset in 0..1usize << clauses.len() {
        a.exact(
            "systematic_2cnf",
            all(clauses
                .iter()
                .enumerate()
                .filter(|(i, _)| subset & (1 << i) != 0)
                .map(|(_, t)| t.clone())
                .collect()),
            Env::new(),
        );
    }
}
fn pigeonhole(p: usize, h: usize) -> Term {
    let var = |i, j| bvar(&format!("p{i}h{j}"));
    let mut c = vec![];
    for i in 0..p {
        c.push(any((0..h).map(|j| var(i, j)).collect()));
    }
    for j in 0..h {
        for i in 0..p {
            for k in i + 1..p {
                c.push(not(and(var(i, j), var(k, j))))
            }
        }
    }
    all(c)
}
fn search_exhaustion(a: &mut Audit) {
    a.unknown(
        "limits/ancestor_terms_regression",
        not(bvar("single")),
        Env::new(),
        Limits {
            max_terms: 1,
            ..Limits::default()
        },
    );
    a.check(
        "limits/exact_term_budget",
        not(bvar("single")),
        Env::new(),
        Verdict::Sat,
        Limits {
            max_terms: 2,
            ..Limits::default()
        },
    );
    let t = pigeonhole(5, 4);
    let baseline = finite::solve(&t, &Env::new(), Limits::default());
    assert_eq!(baseline.verdict, Verdict::Unsat);
    assert!(baseline.stats.conflicts > 1);
    let limits = Limits {
        max_clauses: baseline.stats.clauses - 1,
        ..Limits::default()
    };
    let probe = finite::solve(&t, &Env::new(), limits.clone());
    assert_eq!(probe.verdict, Verdict::Unknown);
    assert!(probe.reason.as_ref().unwrap().contains("learned-clause"));
    assert!(probe.stats.decisions > 0);
    a.unknown(
        "search_limits/learned_clauses",
        t.clone(),
        Env::new(),
        limits,
    );
    let mut work_limit = None;
    for pct in [90, 80, 70, 60, 50, 40, 30, 20, 10] {
        let limits = Limits {
            max_work: baseline.stats.work * pct / 100,
            ..Limits::default()
        };
        let probe = finite::solve(&t, &Env::new(), limits.clone());
        if probe.verdict == Verdict::Unknown
            && probe.stats.decisions > 0
            && probe.reason.as_ref().unwrap().contains("work budget")
        {
            work_limit = Some(limits);
            break;
        }
    }
    a.unknown(
        "search_limits/work_after_decisions",
        t,
        Env::new(),
        work_limit.expect("find genuine search exhaustion"),
    );
    a.unknown(
        "search_limits/nonzero_timeout",
        pigeonhole(10, 9),
        Env::new(),
        Limits {
            timeout_ms: 1,
            ..Limits::default()
        },
    );
}
fn integration(a: &mut Audit) {
    std::env::set_var("HWVERIFY_SOLVER", "finite");
    std::env::set_var("HWVERIFY_KERNEL", "off");
    let out = std::path::PathBuf::from("results/finite_solver_independent/integration");
    fs::create_dir_all(&out).unwrap();
    // A nonexistent executable makes any attempted external fallback fail visibly.
    let mut checker = hwverify_solver::Check {
        z3: "/finite-audit-no-z3-allowed".into(),
        out: out.clone(),
        reports: vec![],
    };
    for (name, term, expect, verdict, status, timeout) in [
        ("sat", eq(x(4), bv(4, 7)), true, "sat", "passed", 10000),
        (
            "counterexample",
            eq(x(4), bv(4, 7)),
            false,
            "sat",
            "counterexample",
            10000,
        ),
        (
            "unsat",
            and(eq(x(4), bv(4, 7)), eq(x(4), bv(4, 6))),
            false,
            "unsat",
            "passed",
            10000,
        ),
        (
            "failed_nonvacuity",
            boolv(false),
            true,
            "unsat",
            "failed_nonvacuity",
            10000,
        ),
        (
            "unsupported",
            node(Sort::Bool, "uninterpreted", vec![]),
            false,
            "unknown",
            "unknown",
            10000,
        ),
        ("timeout", boolv(true), false, "unknown", "unknown", 0),
    ] {
        checker
            .query_with_timeout(name, term, expect, &Env::new(), timeout)
            .unwrap();
        let report = checker.reports.last().unwrap();
        assert_eq!(report["backend"], "finite_bv");
        assert_eq!(report["solver_result"], verdict);
        assert_eq!(report["status"], status);
        assert_eq!(report["z3_seconds"], 0.0);
        *a.phases.entry("reporting_integration".into()).or_default() += 1;
    }
    std::env::set_var("HWVERIFY_KERNEL", "on");
    for (name, kind) in [
        ("forall_unsupported", hwverify_solver::BinderKind::Forall),
        ("exists_unsupported", hwverify_solver::BinderKind::Exists),
    ] {
        let formula =
            hwverify_solver::QuantifiedFormula::Atom(eq(x(4), x(4))).bind(kind, vec![x(4)]);
        checker
            .query_quantified(name, &formula, true, &Env::new(), 10000)
            .unwrap();
        let report = checker.reports.last().unwrap();
        assert_eq!(report["solver_result"], "unknown");
        assert_eq!(report["status"], "unknown");
        assert_eq!(report["z3_seconds"], 0.0);
        assert_eq!(report["kernel"]["enabled"], false);
        *a.phases.entry("reporting_integration".into()).or_default() += 1;
    }
    let refusal =
        hwverify_solver::solver("/finite-audit-no-z3-allowed", "(check-sat)").unwrap_err();
    assert!(refusal.contains("disabled by HWVERIFY_SOLVER=finite"));
    a.counts.insert("external_solver_guard".into(), 1);
    fs::write(
        out.join("reports.json"),
        serde_json::to_string_pretty(&checker.reports).unwrap(),
    )
    .unwrap();
    std::env::remove_var("HWVERIFY_SOLVER");
    std::env::remove_var("HWVERIFY_KERNEL");
}

fn additional_word_shapes(a: &mut Audit) {
    for w in 1..=4 {
        for xv in 0..1u64 << w {
            for yv in 0..1u64 << w {
                for p in [false, true] {
                    let mut assignment = inputs(xv, yv);
                    assignment.insert("p".into(), V::B(p));
                    a.pinned("word_ite", ite(bvar("p"), x(w), y(w)), assignment);
                }
            }
        }
    }
    for w in [8, 31, 32, 63, 64] {
        for xv in [0, 1, (1u64 << (w - 1)) - 1, 1u64 << (w - 1), mask(w)] {
            for lo in [0, w / 2, w - 1] {
                for hi in [lo, w - 1] {
                    a.pinned(
                        "extract_boundaries",
                        node(
                            Sort::Bv(hi - lo + 1),
                            format!("(_ extract {hi} {lo})"),
                            vec![x(w)],
                        ),
                        inputs(xv, 0),
                    );
                }
            }
            for target in [w, 64] {
                for name in ["zero_extend", "sign_extend"] {
                    a.pinned(
                        "extension_boundaries",
                        node(
                            Sort::Bv(target),
                            format!("(_ {name} {})", target - w),
                            vec![x(w)],
                        ),
                        inputs(xv, 0),
                    );
                }
            }
        }
    }
    let mut deep = bvar("deep");
    for _ in 0..600 {
        deep = not(deep)
    }
    a.unknown("limits/default_depth", deep, Env::new(), Limits::default());
}
