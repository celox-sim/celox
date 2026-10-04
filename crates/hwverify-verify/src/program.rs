//! Reusable total-correctness obligations over an ISA transition system.
//! Parameters are universally symbolic, immutable ghost values, never live inputs.
use hwverify_ir::*;
use hwverify_solver::{
    Check,
    partition::{budgeted_query, infer_partitions},
};
use serde_json::{Value, json};
use std::time::Instant;

// Keep the explicit state environments visible at this proof-obligation boundary.
#[allow(clippy::too_many_arguments)]
pub fn obligations(
    contract: &Value,
    s: &Env,
    sr: &Env,
    sn: &Env,
    i: &Env,
    can_step: Term,
    reset: Term,
    l: &mut Lower,
    q: &mut Check,
) -> Res<Value> {
    keys(
        contract,
        &[
            "parameters",
            "precondition",
            "invariant",
            "terminal",
            "postcondition",
            "rank",
        ],
        &["cases", "split", "partitioning"],
    )?;
    let parameters = symbols(&contract["parameters"], "parameter")?;
    let env = |state: &Env, inputs: &Env| {
        let mut e = scope(state, inputs);
        e.extend(
            parameters
                .iter()
                .map(|(n, t)| (format!("p.{n}"), t.clone())),
        );
        e
    };
    let e = env(s, &Env::new());
    let pre = and(
        reset,
        require_bool(l.expr(&contract["precondition"], &env(&Env::new(), i))?)?,
    );
    let inv = require_bool(l.expr(&contract["invariant"], &e)?)?;
    let initial_inv = require_bool(l.expr(&contract["invariant"], &env(sr, &Env::new()))?)?;
    let next_inv = require_bool(l.expr(&contract["invariant"], &env(sn, &Env::new()))?)?;
    let terminal = require_bool(l.expr(&contract["terminal"], &e)?)?;
    let post = require_bool(l.expr(&contract["postcondition"], &e)?)?;
    let rank = l.expr(&contract["rank"], &e)?;
    if !matches!(rank.0.sort, Sort::Bv(_)) {
        return Err("program rank must be unsigned word".into());
    }
    let rank_next = l.expr(&contract["rank"], &env(sn, &Env::new()))?;
    let decreases = node(Sort::Bool, "bvult", vec![rank_next.clone(), rank.clone()]);
    let active = and(inv.clone(), not(terminal.clone()));
    let mut ctx = env(s, i);
    ctx.extend(sn.iter().map(|(n, t)| (format!("next.{n}"), t.clone())));
    ctx.insert("program_rank".into(), rank);
    ctx.insert("program_rank_next".into(), rank_next);
    // No assumptions about live inputs are carried across transitions.
    q.query("program_pre_nonempty", pre.clone(), true, &Env::new())?;
    q.query(
        "program_initialization",
        and(pre, not(initial_inv)),
        false,
        &ctx,
    )?;
    q.query("program_active_nonempty", active.clone(), true, &Env::new())?;
    q.query(
        "program_step_available",
        and(active.clone(), not(can_step.clone())),
        false,
        &ctx,
    )?;
    if contract.get("cases").is_some() && contract.get("split").is_some() {
        return Err("program cases and split are mutually exclusive".into());
    }
    let mode = contract
        .get("partitioning")
        .map(text)
        .transpose()?
        .unwrap_or("auto");
    if !["auto", "none"].contains(&mode) {
        return Err("partitioning must be auto or none".into());
    }
    if contract.get("partitioning").is_some()
        && (contract.get("split").is_some() || contract.get("cases").is_some())
    {
        return Err("partitioning cannot be combined with split or cases".into());
    }
    let automatic =
        mode == "auto" && contract.get("split").is_none() && contract.get("cases").is_none();
    let mut plan = json!({"strategy": "none", "partition_count": 1});
    let partitions = if let Some(cases) = contract.get("cases") {
        plan = json!({"strategy":"manual_cases"});
        Some(
            named(cases)?
                .iter()
                .map(|(name, guard)| Ok((name.clone(), require_bool(l.expr(guard, &e)?)?)))
                .collect::<Res<Vec<_>>>()?,
        )
    } else if let Some(split) = contract.get("split") {
        plan = json!({"strategy":"manual_split"});
        // Deterministic finite partitions plus a complement: hints cannot silently
        // exclude states. Bounds limit tool cost, not the semantic state space.
        let mut partitions = vec![(String::new(), boolv(true))];
        for (name, hint) in named(split)? {
            keys(hint, &["expr", "min", "max"], &[])?;
            let expr = l.expr(&hint["expr"], &e)?;
            let Sort::Bv(width) = expr.0.sort else {
                return Err("split expression must be word".into());
            };
            let min = hint["min"].as_u64().ok_or("invalid split min")?;
            let max = hint["max"].as_u64().ok_or("invalid split max")?;
            if min > max
                || (width < 64 && max >= (1u64 << width))
                || (max as u128 - min as u128 + 2) * partitions.len() as u128 > 256
            {
                return Err("split bounds invalid or partition count exceeds 256".into());
            }
            let mut choices = vec![];
            let mut covered = boolv(false);
            for value in min..=max {
                let guard = eq(expr.clone(), bv(width, value));
                covered = node(Sort::Bool, "or", vec![covered, guard.clone()]);
                choices.push((format!("{name}_{value}"), guard));
            }
            choices.push((format!("{name}_other"), not(covered)));
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
        }
        Some(partitions)
    } else if automatic {
        let (partitions, inferred) = infer_partitions(s, &inv, sn, &next_inv);
        plan = inferred;
        partitions
    } else {
        None
    };
    if let Some(partitions) = partitions {
        plan["partition_count"] = json!(partitions.len());
        let started = Instant::now();
        let mut covered = boolv(false);
        for (name, guard) in partitions {
            covered = node(Sort::Bool, "or", vec![covered, guard.clone()]);
            let query_name = format!("program_invariant_preserved_{name}");
            let bad = and(active.clone(), and(guard, not(next_inv.clone())));
            if automatic {
                budgeted_query(q, &query_name, bad, &ctx, started, 30000)?;
            } else {
                q.query(&query_name, bad, false, &ctx)?;
            }
        }
        q.query(
            "program_cases_cover",
            and(active.clone(), not(covered)),
            false,
            &ctx,
        )?;
    } else {
        q.query(
            "program_invariant_preserved",
            and(active.clone(), not(next_inv)),
            false,
            &ctx,
        )?;
    }
    q.query(
        "program_rank_decreases",
        and(active, not(decreases)),
        false,
        &ctx,
    )?;
    q.query(
        "program_postcondition",
        and(inv.clone(), and(terminal.clone(), not(post))),
        false,
        &ctx,
    )?;
    // Needed to transfer a final-state claim to all later implementation cycles.
    q.query(
        "program_terminal_quiescent",
        and(inv, and(terminal, can_step)),
        false,
        &ctx,
    )?;
    Ok(plan)
}
