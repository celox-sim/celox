//! Generic microstep/stuttering and finite-rank proof obligations.
use hwverify_ir::*;
use hwverify_solver::Check;
use serde_json::{json, Value};
use std::path::PathBuf;
pub fn check(doc: &Value, z3: String, out: PathBuf) -> Res<Value> {
    let design = Design::from_json(doc).map_err(|e| e.to_string())?;
    check_design(&design, z3, out)
}

/// Verify a fully validated model. A frontend cannot manufacture this type.
pub fn check_design(design: &Design, z3: String, out: PathBuf) -> Res<Value> {
    let doc = design.document();
    let mut l = Lower {
        rules: design.machine_normalization().clone(),
    };
    let i = design.inputs().clone();
    let rst = i[text(&doc["reset_input"])?].clone();
    let spec = design.spec();
    let implementation = design.implementation();
    let (s, sr, sn, so) = (
        spec.state.clone(),
        spec.reset.clone(),
        spec.next.clone(),
        spec.outputs.clone(),
    );
    let (t, tr, tn, io) = (
        implementation.state.clone(),
        implementation.reset.clone(),
        implementation.next.clone(),
        implementation.outputs.clone(),
    );
    let c = require_bool(
        io.get(text(&doc["commit"])?)
            .ok_or("missing commit output")?
            .clone(),
    )?;
    let can_step = require_bool(
        so.get(text(&doc["can_step"])?)
            .ok_or("missing spec can_step output")?
            .clone(),
    )?;
    let r = require_bool(l.expr(&doc["binding"], &relation_env(&s, &t, &Env::new()))?)?;
    let rr = require_bool(l.expr(&doc["binding"], &relation_env(&sr, &tr, &Env::new()))?)?;
    let next_s = sn
        .iter()
        .map(|(n, v)| {
            (
                n.clone(),
                ite(
                    rst.clone(),
                    sr[n].clone(),
                    ite(c.clone(), v.clone(), s[n].clone()),
                ),
            )
        })
        .collect::<Env>();
    let next_t = tn
        .iter()
        .map(|(n, v)| (n.clone(), ite(rst.clone(), tr[n].clone(), v.clone())))
        .collect::<Env>();
    let rn = require_bool(l.expr(
        &doc["binding"],
        &relation_env(&next_s, &next_t, &Env::new()),
    )?)?;
    let mut ctx = relation_env(&s, &t, &i);
    ctx.extend(
        next_s
            .iter()
            .map(|(n, v)| (format!("spec_next.{n}"), v.clone())),
    );
    ctx.extend(
        next_t
            .iter()
            .map(|(n, v)| (format!("impl_next.{n}"), v.clone())),
    );
    ctx.insert("commit".into(), c.clone());
    ctx.insert("binding_before".into(), r.clone());
    ctx.insert("binding_after".into(), rn.clone());
    let mut q = Check {
        z3,
        out,
        reports: vec![],
    };
    q.query("binding_nonempty", r.clone(), true, &Env::new())?;
    q.query("reset_binding", not(rr), false, &ctx)?;
    q.query("microstep_refinement", and(r.clone(), not(rn)), false, &ctx)?;
    q.query(
        "commit_eligible",
        and(
            r.clone(),
            and(not(rst.clone()), and(c.clone(), not(can_step.clone()))),
        ),
        false,
        &ctx,
    )?;
    if let Some(h) = doc.get("hold_when") {
        let hold = require_bool(l.expr(h, &relation_env(&s, &t, &i))?)?;
        let stable = t.iter().fold(boolv(true), |acc, (n, v)| {
            and(acc, eq(v.clone(), next_t[n].clone()))
        });
        q.query(
            "hold_contract",
            and(r.clone(), and(not(rst.clone()), and(hold, not(stable)))),
            false,
            &ctx,
        )?;
    }
    let p = &doc["progress"];
    keys(p, &["enabled", "rank"], &[])?;
    let env = relation_env(&s, &t, &i);
    let enabled = require_bool(l.expr(&p["enabled"], &env)?)?;
    let rank = l.expr(&p["rank"], &relation_env(&s, &t, &Env::new()))?;
    if !matches!(rank.0.sort, Sort::Bv(_)) {
        return Err("rank must be unsigned word".into());
    }
    let rankn = l.expr(&p["rank"], &relation_env(&next_s, &next_t, &Env::new()))?;
    let dec = node(Sort::Bool, "bvult", vec![rankn.clone(), rank.clone()]);
    let live = and(r.clone(), and(not(rst.clone()), enabled.clone()));
    ctx.insert("rank".into(), rank);
    ctx.insert("rank_next".into(), rankn);
    ctx.insert("progress_enabled".into(), enabled);
    q.query("progress_nonvacuity", live.clone(), true, &Env::new())?;
    q.query(
        "commit_reachable_in_relation",
        and(live.clone(), c.clone()),
        true,
        &Env::new(),
    )?;
    q.query(
        "noncommit_rank_decreases",
        and(live, and(not(c), not(dec))),
        false,
        &ctx,
    )?;
    let partition_plan = if let Some(contract) = doc.get("program_contract") {
        Some(crate::program::obligations(
            contract,
            &s,
            &sr,
            &sn,
            &i,
            can_step,
            rst.clone(),
            &mut l,
            &mut q,
        )?)
    } else {
        None
    };
    let statuses = q
        .reports
        .iter()
        .map(|x| x["status"].as_str().unwrap())
        .collect::<Vec<_>>();
    let status = if statuses.contains(&"counterexample") {
        "counterexample"
    } else if statuses.contains(&"unknown") {
        "unknown"
    } else if statuses.contains(&"failed_nonvacuity") {
        "inadequate_contract"
    } else if doc.get("program_contract").is_some() {
        "program_and_refinement_verified"
    } else {
        "stuttering_refinement_verified"
    };
    let engine_summary = json!({
        "custom_closed":q.reports.iter().filter(|r|r["backend"]=="structural_kernel").count(),
        "z3_queries":q.reports.iter().filter(|r|r["backend"]=="z3").count(),
        "finite_queries":q.reports.iter().filter(|r|r["backend"]=="finite_bv").count(),
        "not_run":q.reports.iter().filter(|r|r["solver_result"]=="not_run").count(),
        "query_seconds":q.reports.iter().filter_map(|r|r["seconds"].as_f64()).sum::<f64>(),
        "kernel_compute_seconds":q.reports.iter().filter_map(|r|r["kernel"]["seconds"].as_f64()).sum::<f64>(),
        "z3_seconds":q.reports.iter().filter_map(|r|r["z3_seconds"].as_f64()).sum::<f64>(),
        "finite_seconds":q.reports.iter().filter_map(|r|r["finite_seconds"].as_f64()).sum::<f64>(),
        "scoring_seconds":partition_plan.as_ref().and_then(|p|p["scoring_seconds"].as_f64()).unwrap_or(0.0),
        "timing_note":"query_seconds includes emission and evidence I/O; scoring is additional; whole-process wall time must be measured externally"
    });
    Ok(
        json!({"status":status,"name":doc.get("name"),"obligations":q.reports,"normalization":l.rules,"partition_plan":partition_plan,"engine_summary":engine_summary,"claim":"Reset-established inductive microstep/ISA-step correspondence; spec steps iff implementation commit, otherwise stutters; unsigned rank strictly decreases on enabled noncommit transitions", "limitations":["Rust structural-kernel UNSAT, the selected finite Bool/BV solver or Z3 fallback, Rust lowering, specification and binding are trusted; diagnostics are not independent proof certificates; existing Lean memory theorems do not certify these solvers","Progress is conditional on enabled at every noncommit step; external stalls may continue forever","Supplied binding adequacy is not inferred; no RTL import or synthesis claim","Nonvacuity checks are satisfiability in the relation, not reset reachability"],"program_contract":doc.get("program_contract").map(|_| "ISA total correctness from precondition with immutable parameters; implementation transfer assumes no subsequent reset and continuously enabled progress until termination"),"reset":"active-high synchronous priority; reset expressions may use shared inputs"}),
    )
}
