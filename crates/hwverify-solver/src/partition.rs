//! Sound complete partitions selected by typed-expression trial scoring.
use crate::Check;
use hwverify_ir::*;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    time::Instant,
};
/// Performance hints only: occurrences under negation/disjunction are deliberately
/// not interpreted as bounds. Every selected value set gets its full complement.
/// No algebra, reachability, alias or signed-range assumption is introduced.
pub fn infer_partitions(
    s: &Env,
    inv: &Term,
    sn: &Env,
    goal: &Term,
) -> (Option<Vec<(String, Term)>>, Value) {
    let scoring_started = Instant::now();
    fn literal(t: &Term) -> Option<u64> {
        t.0.op
            .strip_prefix("(_ bv")?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    }
    fn collect(
        t: &Term,
        states: &BTreeMap<String, String>,
        found: &mut BTreeMap<String, BTreeSet<u64>>,
        seen: &mut HashSet<Term>,
    ) {
        if !seen.insert(t.clone()) {
            return;
        }
        if ["=", "bvult", "bvule", "bvslt", "bvsle"].contains(&t.0.op.as_str())
            && t.0.args.len() == 2
        {
            for (a, b) in [(0, 1), (1, 0)] {
                if let (Some(name), Some(value)) =
                    (states.get(&t.0.args[a].0.op), literal(&t.0.args[b]))
                {
                    found.entry(name.clone()).or_default().insert(value);
                }
            }
        }
        for arg in &t.0.args {
            collect(arg, states, found, seen);
        }
    }
    let states = s
        .iter()
        .filter(|(_, t)| matches!(t.0.sort, Sort::Bv(_)))
        .map(|(n, t)| (t.0.op.clone(), n.clone()))
        .collect();
    let mut found = BTreeMap::new();
    let mut seen = HashSet::new();
    collect(inv, &states, &mut found, &mut seen);
    let invariant_states = found.keys().cloned().collect::<BTreeSet<_>>();
    for t in sn.values() {
        collect(t, &states, &mut found, &mut seen);
    }
    // Compare the actual next-invariant expression under sound trial assumptions.
    // The invariant is assumed only here as the antecedent of preservation. A
    // trial is a performance estimate, never an added verification assumption.
    let mut candidates = found.into_iter().collect::<Vec<_>>();
    candidates.sort_by(|(an, _), (bn, _)| an.cmp(bn));
    let mut partitions = vec![(String::new(), boolv(true))];
    let mut selected = 0;
    let mut decisions = vec![];
    let mut trial_count = 0usize;
    let mut baseline = vec![crate::kernel::complexity(&crate::kernel::simplify(
        goal,
        std::slice::from_ref(inv),
    ))];
    fn choices(name: &str, expr: &Term, values: &BTreeSet<u64>) -> Vec<(String, Term)> {
        let Sort::Bv(width) = expr.0.sort else {
            unreachable!()
        };
        let mut covered = boolv(false);
        let mut choices = vec![];
        for value in values {
            let guard = eq(expr.clone(), bv(width, *value));
            covered = node(Sort::Bool, "or", vec![covered, guard.clone()]);
            choices.push((format!("auto_{name}_{value}"), guard));
        }
        choices.push((format!("auto_{name}_other"), not(covered)));
        choices
    }
    while !candidates.is_empty() {
        let before = baseline.iter().sum::<u64>();
        let mut trials = vec![];
        for (idx, (name, values)) in candidates.iter().enumerate() {
            let cases = values.len() + 1;
            let reason = if values.len() > 32 {
                "too_many_values"
            } else if selected >= 3 {
                "axis_budget"
            } else if partitions.len() * cases > 128 {
                "product_budget"
            } else if trial_count + partitions.len() * cases > 512 {
                "trial_budget"
            } else {
                "scored"
            };
            let mut costs = vec![];
            if reason == "scored" {
                for (_, previous) in &partitions {
                    for (_, guard) in choices(name, &s[name], values) {
                        let reduced =
                            crate::kernel::simplify(goal, &[inv.clone(), previous.clone(), guard]);
                        costs.push(crate::kernel::complexity(&reduced));
                        trial_count += 1;
                    }
                }
            }
            let after = costs.iter().sum::<u64>();
            let reduction = if costs.is_empty() || before == 0 {
                0.0
            } else {
                1.0 - after as f64 / (before as f64 * cases as f64)
            };
            // Compare fractional simplification with growth in case count. The
            // square-root penalty tolerates a useful wider control/index axis
            // without granting every small incidental comparison a free split.
            let score = reduction / (cases as f64).sqrt();
            let worthwhile = reduction >= 0.15 && before as f64 * reduction >= 8.0;
            trials.push((idx, reason, costs, reduction, score, worthwhile));
        }
        let best = trials
            .iter()
            .filter(|t| t.1 == "scored" && t.5)
            .max_by(|a, b| a.4.total_cmp(&b.4).then_with(|| b.0.cmp(&a.0)))
            .map(|t| t.0);
        for (idx, reason, costs, reduction, score, worthwhile) in &trials {
            let (name, values) = &candidates[*idx];
            decisions.push(json!({"round":selected,"state":name,"values":values,
                "source":if invariant_states.contains(name) {"invariant comparison (possibly also model)"} else {"model-only comparison"},
                "decision":if *reason!="scored" {*reason} else if Some(*idx)==best {"selected"} else if !worthwhile {"low_marginal_benefit"} else {"lower_score"},
                "baseline_cost_sum":before,"trial_cost_sum":costs.iter().sum::<u64>(),
                "trial_cases":costs.len(),"case_growth":values.len()+1,"fractional_reduction":reduction,"score":score}));
        }
        let Some(best) = best else { break };
        let (_, _, costs, _, _, _) = trials.swap_remove(best);
        baseline = costs;
        let (name, values) = candidates.remove(best);
        let choices = choices(&name, &s[&name], &values);
        let mut product = vec![];
        for (prefix, previous) in partitions {
            for (suffix, guard) in &choices {
                product.push((
                    if prefix.is_empty() {
                        suffix.clone()
                    } else {
                        format!("{prefix}_{suffix}")
                    },
                    and(previous.clone(), guard.clone()),
                ));
            }
        }
        partitions = product;
        selected += 1;
    }
    let plan = json!({"strategy":"structural_trial_simplification_v1", "candidates":decisions,
        "selected_axes":selected,"partition_count":partitions.len(),"max_partitions":128,
        "max_values_per_axis":32,"max_axes":3,"max_trial_cases":512,"trial_cases":trial_count,
        "scoring_seconds":scoring_started.elapsed().as_secs_f64(),"partition_query_timeout_ms":1000,
        "partition_scheduling_budget_ms":30000,
        "ordering":"greedy marginal trial simplification / sqrt(case growth), state name only for ties",
        "metric":"weighted unique next-invariant DAG after contextual rewrite; min 15% and 8 weighted nodes saved",
        "soundness":"trials select hints only; each selected axis includes complement; all partitions and coverage must pass"});
    (
        if selected == 0 {
            None
        } else {
            Some(partitions)
        },
        plan,
    )
}

/// Budget exhaustion is an explicit unproved obligation, never an empty proof.
pub fn budgeted_query(
    q: &mut Check,
    name: &str,
    bad: Term,
    ctx: &Env,
    started: Instant,
    budget_ms: u128,
) -> Res<()> {
    if started.elapsed().as_millis() >= budget_ms {
        q.reports.push(
            json!({"name":name,"status":"unknown", "solver_result":"not_run",
            "reason":"automatic partition scheduling budget exhausted"}),
        );
        Ok(())
    } else {
        q.query_with_timeout(name, bad, false, ctx, 1000)
    }
}
