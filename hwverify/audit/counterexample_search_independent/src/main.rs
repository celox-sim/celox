//! Independent original-query decomposition coverage and shared-budget audit.
//! No access to the encoder, SAT state, learned clauses, or production evaluator.
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
    probes: Vec<Value>,
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
        let o = finite::solve(&t, &ctx, limits.clone());
        if o.verdict != Verdict::Unknown {
            assert!(o.stats.work <= limits.max_work);
            assert!(o.stats.variables <= limits.max_variables);
            assert!(o.stats.clauses <= limits.max_clauses);
            assert!(o.stats.terms <= limits.max_terms);
        }
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
mod decomposition_cases;
mod scheduling_cases;
fn main() {
    let out = PathBuf::from(std::env::var("COUNTEREXAMPLE_AUDIT_OUT").expect("COUNTEREXAMPLE_AUDIT_OUT"));
    fs::create_dir_all(&out).unwrap();
    let started = Instant::now();
    let mut a = Audit {
        out,
        counts: BTreeMap::new(),
        phases: BTreeMap::new(),
        truth_rows: 0,
        oracle: vec![],
        probes: vec![],
    };
    if std::env::var_os("COUNTEREXAMPLE_INCLUDE_DECOMPOSITION").is_some() {
        decomposition_cases::truth_tables(&mut a);
        decomposition_cases::coverage(&mut a);
        decomposition_cases::words(&mut a);
        decomposition_cases::malformed(&mut a);
        decomposition_cases::budgets(&mut a);
    }
    scheduling_cases::run(&mut a);
    let summary = json!({"status":"pass","counts":a.counts,"phases":a.phases,"exhaustive_or_pinned_assignment_rows":a.truth_rows,"oracle_cases":a.oracle,"probes":a.probes,"seconds_not_benchmark":started.elapsed().as_secs_f64(),"oracle":"Audit-owned original integer/Boolean semantics and exhaustive enumeration; Z3 original-query checks run separately"});
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
