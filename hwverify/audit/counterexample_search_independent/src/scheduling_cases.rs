use super::*;

fn or(x: Term, y: Term) -> Term {
    node(Sort::Bool, "or", vec![x, y])
}
fn any(mut xs: Vec<Term>) -> Term {
    if xs.is_empty() {
        return boolv(false);
    }
    while xs.len() > 1 {
        xs = xs.chunks(2).map(|x| {
            if x.len() == 1 { x[0].clone() } else { or(x[0].clone(), x[1].clone()) }
        }).collect();
    }
    xs.pop().unwrap()
}

// A separate mathematical oracle: a total assignment of pigeons to distinct
// holes exists exactly when pigeons <= holes. The generator has no dependency
// on the production solver's private representation or search order.
fn pigeonholes(prefix: &str, pigeons: usize, holes: usize) -> Term {
    let cell = |p, h| b(&format!("{prefix}_{p}_{h}"));
    let mut clauses = Vec::new();
    for p in 0..pigeons {
        clauses.push(any((0..holes).map(|h| cell(p,h)).collect()));
    }
    for h in 0..holes {
        for p in 0..pigeons {
            for q in p+1..pigeons {
                clauses.push(or(not(cell(p,h)), not(cell(q,h))));
            }
        }
    }
    all(clauses)
}

fn branches(n: usize, winner: Option<usize>, hard_holes: usize) -> (Term, Env) {
    let choices = (0..n).map(|i| b(&format!("selected_{i:03}"))).collect::<Vec<_>>();
    let constraints = choices.iter().enumerate().map(|(i, choice)| {
        let condition = if winner == Some(i) {
            // Keep this branch nontrivial without imposing an UNSAT obligation.
            pigeonholes(&format!("pigeon_{i:03}"), 2, 2)
        } else {
            pigeonholes(&format!("pigeon_{i:03}"), hard_holes + 1, hard_holes)
        };
        node(Sort::Bool, "=>", vec![choice.clone(), condition])
    }).collect();
    let t = and(all(constraints), any(choices.clone()));
    let mut ctx = Env::from([
        ("original_formula".into(), t.clone()),
        ("unrelated_context".into(), w("context_only", 64)),
    ]);
    for (i, choice) in choices.into_iter().enumerate() {
        ctx.insert(format!("selected_{i:03}"), choice);
    }
    (t, ctx)
}

fn checkpoint(a: &mut Audit, label: &str, t: Term, ctx: Env, expected: Verdict) -> finite::Outcome {
    let out = a.check(label, t, ctx, expected, Limits::default());
    a.probes.push(json!({"case":label,"outcome":out.diagnostics()}));
    out
}

fn complete_unsat_coverage(o: &finite::Outcome) {
    let d = o.diagnostics();
    if d["probe_result"] == "unsat" {
        assert_eq!(o.stats.split_completed, 0);
        assert_eq!(o.stats.split_unsat, 0);
    } else {
        assert_eq!(o.stats.split_completed, o.stats.split_alternatives);
        assert_eq!(o.stats.split_unsat, o.stats.split_alternatives);
    }
}

pub fn run(a: &mut Audit) {
    for n in [2, 3, 5, 9] {
        for winner in 0..n {
            let (t, ctx) = branches(n, Some(winner), 3);
            let o = checkpoint(a, "pigeonhole_every_sat_branch", t.clone(), ctx.clone(), Verdict::Sat);
            assert_eq!(o.context_values[&format!("selected_{winner:03}")], Scalar::Bool(true));
            // Each exact-cap check is per query. Altering only the cap must not
            // silently erase a paused branch or return partial UNSAT coverage.
            for delta in [0, 1] {
                a.check("pigeonhole_exact_work", t.clone(), ctx.clone(),
                    if delta == 0 { Verdict::Sat } else { Verdict::Unknown },
                    Limits { max_work: o.stats.work - delta, ..Limits::default() });
                a.check("pigeonhole_exact_clauses", t.clone(), ctx.clone(),
                    if delta == 0 { Verdict::Sat } else { Verdict::Unknown },
                    Limits { max_clauses: o.stats.clauses - delta as usize, ..Limits::default() });
            }
        }
        let (t, ctx) = branches(n, None, 3);
        let o = checkpoint(a, "pigeonhole_all_branches_unsat", t, ctx, Verdict::Unsat);
        complete_unsat_coverage(&o);
    }
    for holes in [4, 5, 6] {
        for winner in [Some(0), Some(3), None] {
            let (t, ctx) = branches(4, winner, holes);
            let expected = if winner.is_some() { Verdict::Sat } else { Verdict::Unsat };
            let o = checkpoint(a, "hard_early_unsat_late_sat", t.clone(), ctx.clone(), expected);
            a.oracle_case("pigeonhole_original_query_oracle", t.clone(), ctx.clone(), expected);
            let d = o.diagnostics();
            if winner == Some(3) && holes >= 5 {
                // The implementation reports these additive diagnostics. The
                // stored counters let review establish actual yield coverage.
                a.probes.push(json!({"case":"hard_late_sat_scheduler_coverage","holes":holes,"diagnostics":d}));
            }
            for fraction in [1, 2, 3] {
                a.check("hard_branch_partial_work_unknown", t.clone(), ctx.clone(), Verdict::Unknown,
                    Limits { max_work: o.stats.work * fraction / 4, ..Limits::default() });
            }
        }
    }
    // Original formula/context validation happens even if an easy selected
    // alternative is found before the hard siblings have been searched.
    let (t, mut ctx) = branches(4, Some(3), 5);
    ctx.insert("malformed_unvisited_context".into(), node(Sort::Bool,"or",vec![b("x")]));
    a.check("late_sat_cannot_skip_bad_context", t, ctx, Verdict::Unknown, Limits::default());

    let mut timeout_after_probe_yield = false;
    for holes in [4, 5, 6, 7, 8] {
        let (t, ctx) = branches(4, None, holes);
        for timeout_ms in [0, 1, 2, 4, 8, 12, 16, 24, 32, 48, 64] {
            let start = Instant::now();
            let out = finite::solve(&t, &ctx, Limits { timeout_ms, ..Limits::default() });
            assert_ne!(out.verdict, Verdict::Sat);
            assert!(!out.original_formula_validated && out.assignments.is_empty() && out.context_values.is_empty());
            if out.verdict == Verdict::Unknown { assert!(out.reason.is_some()); }
            if out.verdict == Verdict::Unsat { complete_unsat_coverage(&out); }
            let d = out.diagnostics();
            timeout_after_probe_yield |= out.verdict == Verdict::Unknown
                && out.reason.as_ref().unwrap().contains("time budget")
                && d["probe_result"] == "unknown"
                && d["search_slices"].as_u64().unwrap_or(0) > 1
                && out.stats.split_completed < out.stats.split_alternatives;
            *a.counts.entry("scheduling_deadline_checks".into()).or_default() += 1;
            a.probes.push(json!({"case":"sliced_cumulative_deadline","holes":holes,"timeout_ms":timeout_ms,"elapsed_seconds":start.elapsed().as_secs_f64(),"outcome":out.diagnostics()}));
        }
    }
    fs::write(a.out.join("scheduling-deadline-coverage.json"), serde_json::to_string_pretty(&json!({
        "timeout_after_probe_yield":timeout_after_probe_yield,
        "controls":a.probes.iter().filter(|p|p["case"]=="sliced_cumulative_deadline").collect::<Vec<_>>()
    })).unwrap()).unwrap();
    assert!(timeout_after_probe_yield, "need genuine global timeout after incomplete probe yields to branch scheduling");
}
