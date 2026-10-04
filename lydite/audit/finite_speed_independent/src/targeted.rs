//! Optimization-focused independent regressions. No backend evaluator or Z3 oracle.
use super::*;

fn balanced(mut xs: Vec<Term>, join: fn(Term, Term) -> Term, identity: bool) -> Term {
    if xs.is_empty() {
        return boolv(identity);
    }
    while xs.len() > 1 {
        xs = xs
            .chunks(2)
            .map(|p| {
                if p.len() == 1 {
                    p[0].clone()
                } else {
                    join(p[0].clone(), p[1].clone())
                }
            })
            .collect();
    }
    xs.pop().unwrap()
}
fn disjoin(a: Term, b: Term) -> Term {
    node(Sort::Bool, "or", vec![a, b])
}

pub fn run(a: &mut Audit) {
    // Variable ordering and activity changes must not alter satisfiability. Each
    // formula is enumerated independently; reverse clauses and literals to
    // perturb traversal/variable IDs and revisit the same logical predicate.
    let mut rng = R(0x0f_11_5eed_abcd_2026);
    for n in 3..=9 {
        for case in 0..64 {
            let mut clauses = Vec::<Vec<Term>>::new();
            for _ in 0..(2 * n + rng.next(5 * n)) {
                let mut clause = vec![];
                for _ in 0..(2 + rng.next(3)) {
                    let term = bvar(&format!("r{}", rng.next(n)));
                    clause.push(if rng.next(2) == 0 { term } else { not(term) });
                }
                clauses.push(clause);
            }
            let t = balanced(
                clauses
                    .iter()
                    .map(|c| balanced(c.clone(), disjoin, false))
                    .collect(),
                and,
                true,
            );
            let (sat, rows) = exhaustive(&t);
            a.truth_rows += rows;
            a.expected(&format!("optimization_cnf/{n}/{case}"), t, Env::new(), sat);
            clauses.reverse();
            for c in &mut clauses {
                c.reverse();
            }
            let reversed = balanced(
                clauses
                    .into_iter()
                    .map(|c| balanced(c, disjoin, false))
                    .collect(),
                and,
                true,
            );
            a.expected(
                &format!("optimization_cnf_reordered/{n}/{case}"),
                reversed,
                Env::new(),
                sat,
            );
        }
    }
    // Long watch lists, repeated watch migration, and the all-assigned terminal
    // path, with the only satisfying literal at either end of a long clause.
    for n in [2, 3, 17, 64, 257] {
        for winner in [0, n - 1] {
            let vars: Vec<_> = (0..n).map(|i| bvar(&format!("watch_{i:04}"))).collect();
            let constraints: Vec<_> = vars
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != winner)
                .map(|(_, v)| not(v.clone()))
                .collect();
            let demand = balanced(vars.clone(), disjoin, false);
            let t = and(demand, balanced(constraints, and, true));
            let ctx = Env::from([("winner".into(), vars[winner].clone())]);
            a.expected(
                "optimization_watch/single_survivor",
                t.clone(),
                ctx.clone(),
                true,
            );
            a.expected(
                "optimization_watch/no_survivor",
                and(t, not(vars[winner].clone())),
                ctx,
                false,
            );
        }
    }
    // Context-only, initially zero-activity variables must still be assigned.
    for n in [0, 1, 2, 31, 255, 1024] {
        let ctx = (0..n)
            .map(|i| (format!("extra_{i:04}"), bvar(&format!("context_{i:04}"))))
            .collect();
        a.expected("optimization_context/zero_activity", boolv(true), ctx, true);
    }
    for (p, h) in [(5, 5), (6, 5), (6, 6), (7, 6)] {
        a.expected(
            "optimization_search/pigeonhole",
            pigeonhole(p, h),
            Env::new(),
            p <= h,
        );
    }
    // Exact work thresholds catch accounting accidentally removed by batching
    // clock reads or replaced decision selection. A decisive answer may use no
    // more than the configured budget, and one unit less must fail closed.
    for (label, t, expect) in [
        (
            "sat",
            eq(op(8, "bvadd", x(8), y(8)), bv(8, 73)),
            Verdict::Sat,
        ),
        ("unsat", pigeonhole(5, 4), Verdict::Unsat),
        ("constant_sat", boolv(true), Verdict::Sat),
        ("constant_unsat", boolv(false), Verdict::Unsat),
    ] {
        let full = finite::solve(&t, &Env::new(), Limits::default());
        assert_eq!(full.verdict, expect);
        assert!(full.stats.work > 0);
        for delta in [0, 1] {
            let lim = Limits {
                max_work: full.stats.work - delta,
                ..Limits::default()
            };
            a.check(
                &format!("optimization_work/{label}/minus{delta}"),
                t.clone(),
                Env::new(),
                if delta == 0 { expect } else { Verdict::Unknown },
                lim,
            );
        }
        for max_work in [0, 1, 2, 3, 7, 15, 31, 63, 127, 255, 511, 1023, u64::MAX] {
            let lim = Limits {
                max_work,
                ..Limits::default()
            };
            let probe = finite::solve(&t, &Env::new(), lim.clone());
            assert!(probe.verdict == Verdict::Unknown || probe.verdict == expect);
            if probe.verdict != Verdict::Unknown {
                assert!(probe.stats.work <= max_work);
            }
            a.check(
                "optimization_work/sweep",
                t.clone(),
                Env::new(),
                probe.verdict,
                lim,
            );
        }
    }
    // Sequential calls with differing budgets must not leak queued decisions,
    // phases, watchers, or terminal outcomes between fresh solver instances.
    for i in 0..32 {
        a.expected(
            "optimization_repeated/sat",
            pigeonhole(3, 3),
            Env::new(),
            true,
        );
        a.unknown(
            "optimization_repeated/zero_work",
            pigeonhole(3, 2),
            Env::new(),
            Limits {
                max_work: 0,
                ..Limits::default()
            },
        );
        a.expected(
            "optimization_repeated/unsat",
            pigeonhole(3, 2),
            Env::new(),
            false,
        );
        let ctx = Env::from([(format!("replay{i}"), y(64))]);
        a.expected(
            "optimization_repeated/context",
            eq(x(4), bv(4, i % 16)),
            ctx,
            true,
        );
    }
}

/// A compact DAG can incur an expensive structural-equality lookup even with
/// fewer than 256 work checkpoints. Deadline checks must also guard terminal
/// SAT and UNSAT outcomes, not depend exclusively on a periodic counter.
pub fn deadline_boundaries(a: &mut Audit) {
    let mut rows = vec![];
    for depth in [18, 20, 22] {
        let mut left = bvar("deadline_shared");
        let mut right = bvar("deadline_shared");
        for _ in 0..depth {
            left = eq(left.clone(), left);
            right = eq(right.clone(), right);
        }
        for unsat in [false, true] {
            let equivalent = eq(left.clone(), right.clone());
            let formula = if unsat { not(equivalent) } else { equivalent };
            let start = Instant::now();
            let out = finite::solve(
                &formula,
                &Env::new(),
                Limits {
                    timeout_ms: 1,
                    ..Limits::default()
                },
            );
            let seconds = start.elapsed().as_secs_f64();
            assert_eq!(
                out.verdict,
                Verdict::Unknown,
                "compact expensive DAG must fail closed at one millisecond"
            );
            assert!(out.reason.as_ref().unwrap().contains("time budget"));
            assert!(!out.original_formula_validated);
            assert!(out.assignments.is_empty());
            assert!(out.context_values.is_empty());
            rows.push(json!({"depth":depth,"logical_expected":if unsat {"unsat"} else {"sat"},"actual":"unknown","reason":out.reason,"elapsed_seconds":seconds,"work":out.stats.work,"terms":out.stats.terms,"decisions":out.stats.decisions,"conflicts":out.stats.conflicts}));
            *a.counts
                .entry("deadline_fail_closed_checks".into())
                .or_default() += 1;
        }
    }
    fs::write(
        output("deadline-boundaries.json"),
        serde_json::to_string_pretty(&rows).unwrap(),
    )
    .unwrap();
}
