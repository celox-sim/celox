//! Generic microstep/stuttering and finite-rank proof obligations.
use hwverify_ir::*;
use hwverify_solver::Check;
use serde_json::{Value, json};
use std::path::PathBuf;
pub fn check(doc: &Value, z3: String, out: PathBuf) -> Res<Value> {
    let design = Design::from_json(doc).map_err(|e| e.to_string())?;
    check_design(&design, z3, out)
}

/// Exact current/next model environment; shared by authoring validation and live execution.
fn proof_context(design: &Design) -> Res<(Env, Env, Env)> {
    let doc = design.document();
    let mut l = Lower {
        rules: design.machine_normalization().clone(),
    };
    let i = design.inputs();
    let rst = i[text(&doc["reset_input"])?].clone();
    let spec = design.spec();
    let imp = design.implementation();
    let (s, sr, sn) = (&spec.state, &spec.reset, &spec.next);
    let (t, tr, tn) = (&imp.state, &imp.reset, &imp.next);
    let c = require_bool(imp.outputs[text(&doc["commit"])?].clone())?;
    let r = require_bool(l.expr(&doc["binding"], &relation_env(s, t, &Env::new()))?)?;
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
    let mut ctx = relation_env(s, t, i);
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
    Ok((next_s, next_t, ctx))
}
/// Type-check proof proposals without running or accepting a solver proof.
pub fn validate_proof_metadata(design: &Design) -> Res<()> {
    if let Some(metadata) = design.document().get("proof_programs") {
        let (_, _, ctx) = proof_context(design)?;
        crate::proof_program::ProofPrograms::from_json(metadata, &ctx)?;
    }
    Ok(())
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
    let (t, tr, io) = (
        implementation.state.clone(),
        implementation.reset.clone(),
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
    let (next_s, next_t, mut ctx) = proof_context(design)?;
    let rn = ctx["binding_after"].clone();
    // Proof metadata is untrusted search input. Resolve every expression in
    // this exact Design context before any solver query can succeed.
    let proof_programs = doc
        .get("proof_programs")
        .map(|metadata| {
            if std::env::var("HWVERIFY_SOLVER").as_deref() != Ok("finite") {
                return Err("proof programs require finite-only mode".into());
            }
            crate::proof_program::ProofPrograms::from_json(metadata, &ctx)
        })
        .transpose()?;
    let mut q = Check {
        z3,
        out,
        reports: vec![],
    };
    q.query("binding_nonempty", r.clone(), true, &Env::new())?;
    q.query("reset_binding", not(rr), false, &ctx)?;
    if let Some(programs) = &proof_programs {
        let mut callback = |checker: &mut Check, name: &str, bad: &Term, context: &Env| {
            programs.try_query(checker, name, bad, context)
        };
        if programs.is_independent() {
            q.query_implication_with_fallback(
                "microstep_refinement",
                r.clone(),
                rn,
                &ctx,
                &mut callback,
            )?;
        } else {
            q.query_implication_with_callback(
                "microstep_refinement",
                r.clone(),
                rn,
                &ctx,
                &mut callback,
            )?;
        }
    } else {
        q.query_implication("microstep_refinement", r.clone(), rn, &ctx)?;
    }
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
    let dec = crate::progress::decreases(rankn.clone(), rank.clone());
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
    let primitive_reports = hwverify_solver::primitive_query_reports(&q.reports);
    let engine_summary = json!({
        "conjunctive_bundles":q.reports.iter().filter(|r|r["backend"]=="conjunctive_lemmas").count(),
        "source_validation_work":primitive_reports.iter().filter_map(|r|r["original_source_validation"]["work"].as_u64()).sum::<u64>(),
        "source_validation_seconds":primitive_reports.iter().filter_map(|r|r["original_source_validation"]["seconds"].as_f64()).sum::<f64>(),
        "finite_total_work":primitive_reports.iter().filter_map(|r|r["finite"]["work"].as_u64()).sum::<u64>(),
        "custom_closed":primitive_reports.iter().filter(|r|r["backend"]=="structural_kernel").count(),
        "z3_queries":primitive_reports.iter().filter(|r|r["backend"]=="z3").count(),
        "finite_queries":primitive_reports.iter().filter(|r|r["backend"]=="finite_bv").count(),
        "not_run":primitive_reports.iter().filter(|r|r["solver_result"]=="not_run").count(),
        "query_seconds":primitive_reports.iter().filter_map(|r|r["seconds"].as_f64()).sum::<f64>(),
        "kernel_compute_seconds":primitive_reports.iter().filter_map(|r|r["kernel"]["seconds"].as_f64()).sum::<f64>(),
        "z3_seconds":primitive_reports.iter().filter_map(|r|r["z3_seconds"].as_f64()).sum::<f64>(),
        "finite_seconds":primitive_reports.iter().filter_map(|r|r["finite_seconds"].as_f64()).sum::<f64>(),
        "scoring_seconds":partition_plan.as_ref().and_then(|p|p["scoring_seconds"].as_f64()).unwrap_or(0.0),
        "timing_note":"query_seconds includes emission and evidence I/O; scoring is additional; whole-process wall time must be measured externally"
    });
    Ok(
        json!({"status":status,"name":doc.get("name"),"obligations":q.reports,"normalization":l.rules,"partition_plan":partition_plan,"engine_summary":engine_summary,"claim":"Reset-established inductive microstep/ISA-step correspondence; spec steps iff implementation commit, otherwise stutters; unsigned rank strictly decreases on enabled noncommit transitions", "limitations":["Rust structural-kernel UNSAT, the selected finite Bool/BV solver or Z3 fallback, Rust lowering, specification and binding are trusted; diagnostics are not independent proof certificates; existing Lean memory theorems do not certify these solvers","Progress is conditional on enabled at every noncommit step; external stalls may continue forever","Supplied binding adequacy is not inferred; no RTL import or synthesis claim","Nonvacuity checks are satisfiability in the relation, not reset reachability"],"program_contract":doc.get("program_contract").map(|_| "ISA total correctness from precondition with immutable parameters; implementation transfer assumes no subsequent reset and continuously enabled progress until termination"),"reset":"active-high synchronous priority; reset expressions may use shared inputs"}),
    )
}

/// Explicit editor proof request for one source target, or its prefix through a
/// candidate/use. Results are diagnostics only; all handles die in this call.
pub fn check_editor_request(
    design: &Design,
    program: &str,
    step: Option<&str>,
    branch: Option<usize>,
    out: PathBuf,
) -> Res<Value> {
    let (_, _, ctx) = proof_context(design)?;
    let metadata = design
        .document()
        .get("proof_programs")
        .ok_or("design has no proof declarations")?;
    let programs = crate::proof_program::ProofPrograms::from_json(metadata, &ctx)?;
    let whole = and(
        ctx["binding_before"].clone(),
        not(ctx["binding_after"].clone()),
    );
    let queries = if programs.matches_target(program, &whole) {
        vec![whole]
    } else {
        hwverify_solver::implication_queries(
            ctx["binding_before"].clone(),
            ctx["binding_after"].clone(),
        )?
        .into_iter()
        .filter(|q| programs.matches_target(program, q))
        .collect()
    };
    if queries.is_empty() {
        return Err("target rhs does not match a current refinement query".into());
    }
    if branch.is_none() && queries.len() != 1 {
        return Err(format!(
            "target matches {} branches; supply a zero-based branch index",
            queries.len()
        ));
    }
    let index = branch.unwrap_or(0);
    let bad = queries
        .get(index)
        .ok_or("branch index outside current target")?;
    let mut q = Check {
        out,
        z3: "EDITOR_EXTERNAL_SOLVER_FORBIDDEN".into(),
        reports: vec![],
    };
    let result = if let Some(step) = step {
        programs.check_through_step(&mut q, "editor", bad, &ctx, program, step)
    } else {
        programs.check_target(&mut q, "editor", bad, &ctx, program)
    };
    if q.reports.is_empty() {
        return Err(result.err().unwrap_or("no matching proof target".into()));
    }
    let mut budget = 512;
    let query = editor_term(bad, &ctx, &mut budget, 0);
    Ok(
        json!({"query":query,"query_display_truncated":budget==0,"reports":q.reports,"error":result.err(),"branch":index,"matching_branches":queries.len(),"scope":if step.is_some(){"program_prefix"}else{"target_program"},"saved_reports_are_authority":false}),
    )
}

// Bounded explanatory rendering only. Never parsed back or used as evidence.
fn editor_term(term: &Term, context: &Env, budget: &mut usize, depth: usize) -> String {
    if *budget == 0 || depth > 64 {
        *budget = 0;
        return "…".into();
    }
    *budget -= 1;
    if let Some((name, _)) = context.iter().find(|(_, value)| *value == term) {
        return name.clone();
    }
    if term.0.args.is_empty() {
        return term.0.op.clone();
    }
    let args = term
        .0
        .args
        .iter()
        .map(|a| editor_term(a, context, budget, depth + 1))
        .collect::<Vec<_>>();
    format!("({} {})", term.0.op, args.join(" "))
}
