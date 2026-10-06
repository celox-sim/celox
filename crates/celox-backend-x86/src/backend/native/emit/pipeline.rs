//! Prepared execution-unit compilation through selection, optimization, allocation, and emission.

use super::*;

// ────────────────────────────────────────────────────────────────
// Multi-EU chained emission
// ────────────────────────────────────────────────────────────────

/// Lower one fully prepared SIR function through x86 ISel, MIR optimization,
/// register allocation, and emission.
///
/// SIR merging and backend-boundary SIR optimization happen before this call;
/// the x86 crate therefore consumes an immutable backend-independent artifact.
pub fn emit_prepared_eu(
    sir_eu: &crate::ExecutionUnit<crate::RegionedAbsoluteAddr>,
    layout: &crate::MemoryLayout,
    four_state: bool,
    label: &str,
    options: &crate::X86BackendOptions,
    trace: Option<&mut NativeFunctionTrace>,
    is_cancelled: impl Fn() -> bool,
) -> Result<EmitResult, ChainedEmitError> {
    emit_prepared_eu_inner(
        std::borrow::Cow::Borrowed(sir_eu),
        layout,
        four_state,
        label,
        options,
        trace,
        is_cancelled,
    )
}

/// Consume the prepared SIR so its storage can be released after instruction
/// selection, before MIR optimization and register allocation allocate analyses.
pub fn emit_owned_prepared_eu(
    sir_eu: crate::ExecutionUnit<crate::RegionedAbsoluteAddr>,
    layout: &crate::MemoryLayout,
    four_state: bool,
    label: &str,
    options: &crate::X86BackendOptions,
    trace: Option<&mut NativeFunctionTrace>,
    is_cancelled: impl Fn() -> bool,
) -> Result<EmitResult, ChainedEmitError> {
    emit_prepared_eu_inner(
        std::borrow::Cow::Owned(sir_eu),
        layout,
        four_state,
        label,
        options,
        trace,
        is_cancelled,
    )
}

fn emit_prepared_eu_inner(
    sir_eu: std::borrow::Cow<'_, crate::ExecutionUnit<crate::RegionedAbsoluteAddr>>,
    layout: &crate::MemoryLayout,
    four_state: bool,
    label: &str,
    options: &crate::X86BackendOptions,
    mut trace: Option<&mut NativeFunctionTrace>,
    is_cancelled: impl Fn() -> bool,
) -> Result<EmitResult, ChainedEmitError> {
    use crate::native::{isel, regalloc};
    // Shared borrow so the checkpoint closure and downstream callees observe
    // the same predicate without moving it.
    let is_cancelled = &is_cancelled;
    let diagnostics = &options.diagnostics;
    let timing = diagnostics.phase_timing;
    let mir_stats = diagnostics.mir_stats;
    let copy_stats =
        timing || mir_stats || diagnostics.regalloc_timing || diagnostics.regalloc_stats;
    let total_start = timing.then(crate::timing::now);
    // Observed between emission stages so a cancelled compile unwinds at the
    // next boundary instead of finishing the remaining phases. Callers
    // without cancellation pass `|| false`, which folds away after inlining.
    let checkpoint = || -> Result<(), ChainedEmitError> {
        if is_cancelled() {
            Err(ChainedEmitError::Cancelled)
        } else {
            Ok(())
        }
    };

    if cfg!(debug_assertions) || diagnostics.verify_sir {
        sir_eu
            .verify_result()
            .map_err(|error| ChainedEmitError::Sir {
                phase: "at x86 backend boundary",
                error,
            })?;
    }
    let verify_mir = |mfunc: &MFunction, phase| {
        if cfg!(debug_assertions) || diagnostics.verify_mir {
            mfunc
                .verify_result()
                .map_err(|error| ChainedEmitError::Mir { phase, error })
        } else {
            Ok(())
        }
    };
    if let Some(trace) = trace.as_deref_mut() {
        trace.optimized_sir = sir_eu.to_string();
    }
    if timing {
        log_sir_width_stats(&sir_eu);
    }

    // Single ISel + optimize + regalloc + emit
    let isel_start = timing.then(crate::timing::now);
    let mut mfunc =
        isel::lower_execution_unit_with_diagnostics(&sir_eu, layout, four_state, diagnostics);
    if let Some(start) = isel_start {
        tracing::debug!(
            "[native-timing] emit_chained isel mir_blocks={} mir_insts={} vregs={} elapsed={:?}",
            mfunc.blocks.len(),
            mir_inst_count(&mfunc),
            mfunc.vregs.count(),
            start.elapsed()
        );
    }
    dump_native_block_context(label, "after_isel", &sir_eu, &mfunc, diagnostics);
    let check_runtime_events = label == "eval_comb_apply_ff"
        && options.native_tick_loop
        && sir_eu.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction,
                    crate::SIRInstruction::RuntimeEvent { .. }
                        | crate::SIRInstruction::CombCaptureEvent { .. }
                )
            })
        });
    // Later SIR access is only for an explicitly requested block dump. Traces
    // already own their textual SIR, and runtime-event presence is captured above.
    let sir_eu = if diagnostics.dump.is_some() {
        Some(sir_eu)
    } else {
        drop(sir_eu);
        None
    };
    if timing {
        tracing::debug!("[native-timing] emit_chained verify after_isel label={label}");
    }
    verify_mir(&mfunc, "after native instruction selection")?;
    checkpoint()?;
    let legalize_start = timing.then(crate::timing::now);
    crate::native::mir_legalize::legalize(&mut mfunc);
    if let Some(start) = legalize_start {
        tracing::debug!(
            "[native-timing] emit_chained legalize mir_blocks={} mir_insts={} vregs={} elapsed={:?}",
            mfunc.blocks.len(),
            mir_inst_count(&mfunc),
            mfunc.vregs.count(),
            start.elapsed()
        );
    }
    if let Some(sir_eu) = sir_eu.as_deref() {
        dump_native_block_context(label, "after_legalize", sir_eu, &mfunc, diagnostics);
    }
    if timing {
        tracing::debug!("[native-timing] emit_chained verify after_legalize label={label}");
    }
    verify_mir(&mfunc, "after MIR legalization")?;
    checkpoint()?;
    let opt_start = timing.then(crate::timing::now);
    if options.baseline {
        crate::native::mir_opt::optimize_baseline(&mut mfunc, diagnostics);
    } else {
        crate::native::mir_opt::optimize_with_diagnostics(&mut mfunc, diagnostics);
    }
    if let Some(start) = opt_start {
        tracing::debug!(
            "[native-timing] emit_chained mir_opt label={label} mir_blocks={} mir_insts={} vregs={} elapsed={:?}",
            mfunc.blocks.len(),
            mir_inst_count(&mfunc),
            mfunc.vregs.count(),
            start.elapsed()
        );
    }
    verify_mir(&mfunc, "after MIR optimization before x86 SLP")?;
    checkpoint()?;
    let slp_stats = if options.slp && !options.baseline {
        crate::native::x86_slp::select(&mut mfunc)
    } else {
        crate::native::x86_slp::SlpStats::default()
    };
    verify_mir(&mfunc, "after x86 SLP before VReg compaction")?;
    if timing {
        tracing::debug!(
            "[native-timing] emit_chained x86_slp vector_zeroes={} vector_packs={} vector_loads={} vector_binary_ops={} vector_stores={} scalar_instructions_removed={}",
            slp_stats.vector_zeroes,
            slp_stats.vector_packs,
            slp_stats.vector_loads,
            slp_stats.vector_binary_ops,
            slp_stats.vector_stores,
            slp_stats.scalar_instructions_removed,
        );
    }
    let compact_start = timing.then(crate::timing::now);
    let compacted = crate::native::mir_opt::compact_vregs(&mut mfunc);
    if let Some(start) = compact_start {
        tracing::debug!(
            "[native-timing] emit_chained compact_vregs before={} after={} removed={} elapsed={:?}",
            compacted.before,
            compacted.after,
            compacted.before - compacted.after,
            start.elapsed()
        );
    }
    if mir_stats {
        log_mir_stats(label, "after_mir_opt", &mfunc);
    }
    if diagnostics.mir_block_stats {
        log_mir_block_stats(label, "after_mir_opt", &mfunc);
    }
    if let Some(sir_eu) = sir_eu.as_deref() {
        dump_native_block_context(label, "after_mir_opt", sir_eu, &mfunc, diagnostics);
    }
    if timing {
        tracing::debug!("[native-timing] emit_chained verify after_mir_opt label={label}");
    }
    verify_mir(&mfunc, "after MIR optimization")?;
    checkpoint()?;
    if let Some(trace) = trace.as_deref_mut() {
        trace.mir_before_regalloc = mfunc.to_string();
    }
    let regalloc_start = timing.then(crate::timing::now);
    let mut regalloc_trace = trace.as_ref().map(|_| regalloc::RegallocTrace::default());
    // Unique first-tier spill slots avoid global stack liveness/coloring.
    // Bound their frame below half the tiered image's reserved headroom;
    // larger frames fall back to exact slot reuse, including on tiny designs.
    let baseline_spill_budget = options
        .baseline
        .then_some((layout.merged_total_size / 8).max(4096));
    let ra = regalloc::run_regalloc_for_codegen(
        &mut mfunc,
        label,
        regalloc_trace.as_mut(),
        diagnostics,
        options.native_tick_loop,
        baseline_spill_budget,
        is_cancelled,
    )
    .map_err(|error| {
        if regalloc::is_cancellation(&error) {
            ChainedEmitError::Cancelled
        } else {
            ChainedEmitError::Regalloc(error)
        }
    })?;
    if let (Some(trace), Some(regalloc_trace)) = (trace.as_deref_mut(), regalloc_trace.as_mut()) {
        trace.mir_after_late_memory_folds =
            std::mem::take(&mut regalloc_trace.mir_after_late_memory_folds);
        trace.mir_after_scheduling = std::mem::take(&mut regalloc_trace.mir_after_scheduling);
    }
    if let Some(start) = regalloc_start {
        tracing::debug!(
            "[native-timing] emit_chained regalloc mir_blocks={} mir_insts={} vregs={} spill_frame={} elapsed={:?}",
            mfunc.blocks.len(),
            mir_inst_count(&mfunc),
            mfunc.vregs.count(),
            ra.spill_frame_size,
            start.elapsed()
        );
    }
    let post_regalloc_start = timing.then(crate::timing::now);
    crate::native::mir_opt::post_regalloc_peephole(&mut mfunc, &ra.assignment);
    crate::native::mir_opt::post_regalloc_cleanup(&mut mfunc);
    crate::native::mir_opt::post_regalloc_direct_load_cse(&mut mfunc, &ra.assignment);
    if cfg!(debug_assertions) || diagnostics.verify_regalloc {
        regalloc::verify_assignment(&mfunc, &ra.assignment)?;
    }
    if let Some(start) = post_regalloc_start {
        tracing::debug!(
            "[native-timing] emit_chained post_regalloc_cleanup mir_blocks={} mir_insts={} vregs={} elapsed={:?}",
            mfunc.blocks.len(),
            mir_inst_count(&mfunc),
            mfunc.vregs.count(),
            start.elapsed()
        );
    }
    verify_mir(&mfunc, "after post-allocation MIR peepholes")?;
    checkpoint()?;
    if let Some(trace) = trace.as_deref_mut() {
        trace.mir_after_regalloc = mfunc.to_string();
        trace.register_assignment.clear();
        for (vreg, preg) in ra.assignment.sorted_entries() {
            trace
                .register_assignment
                .push_str(&format!("  {vreg} -> {preg}\n"));
        }
        trace.spill_frame_size = ra.spill_frame_size;
    }
    if mir_stats {
        log_mir_stats(label, "after_regalloc", &mfunc);
    }
    if diagnostics.mir_block_stats {
        log_mir_block_stats(label, "after_regalloc", &mfunc);
    }
    if let Some(sir_eu) = sir_eu.as_deref() {
        dump_native_block_context(label, "after_regalloc", sir_eu, &mfunc, diagnostics);
    }
    // Post-allocation peepholes and CFG cleanup can change the physical value
    // present on a phi edge. Build the edge-copy plan from this final MIR, not
    // from the pre-cleanup allocation input.
    let ssa_destruction = SsaDestructionPlan::build(&mfunc, &ra.assignment)?;
    if cfg!(debug_assertions) || diagnostics.verify_regalloc {
        ssa_destruction.verify(&mfunc, &ra.assignment, ra.spill_frame_size)?;
    }
    if copy_stats {
        let stats = ssa_destruction.stats();
        tracing::debug!(
            "[native-edge-copy-stats] label={label} edges={} rows={} identity_rows={} effective_copies={} identity_only_edges={} direct_moves={} register_swaps={} cycle_breaks={} temporary_cycle_breaks={} ready_pops={} dependency_releases={} max_effective_per_edge={}",
            stats.edges,
            stats.rows,
            stats.identity_rows,
            stats.effective_copies,
            stats.identity_only_edges,
            stats.direct_moves,
            stats.register_swaps,
            stats.cycle_breaks,
            stats.temporary_cycle_breaks,
            stats.ready_queue_pops,
            stats.dependency_releases,
            stats.max_effective_copies_per_edge,
        );
    }
    checkpoint()?;
    let emit_start = timing.then(crate::timing::now);
    let semantic_size = layout
        .merged_total_size
        .checked_add(layout.triggered_bits_total_size)
        .expect("native simulation-state size overflow");
    let state_size = options
        .arena_base
        .map_or(semantic_size, |base| base.max(semantic_size));
    let result = if label == "eval_comb_apply_ff" && options.native_tick_loop {
        emit_with_plan_tick_loop(
            &mfunc,
            &ra.assignment,
            ra.spill_frame_size,
            state_size,
            &ssa_destruction,
            check_runtime_events,
        )?
    } else {
        emit_with_plan(
            &mfunc,
            &ra.assignment,
            ra.spill_frame_size,
            state_size,
            &ssa_destruction,
        )?
    };
    if let Some(trace) = trace {
        trace.disassembly = disassemble_with_block_offsets(
            &result.code[..result.text_size],
            0,
            &result.block_offsets,
        );
    }
    if let Some(start) = emit_start {
        tracing::debug!(
            "[native-timing] emit_chained emit bytes={} elapsed={:?}",
            result.code.len(),
            start.elapsed()
        );
    }
    if let Some(start) = total_start {
        tracing::debug!(
            "[native-timing] emit_chained total elapsed={:?}",
            start.elapsed()
        );
    }
    Ok(result)
}
