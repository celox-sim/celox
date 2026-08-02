use crate::HashSet;

use veryl_parser::resource_table::{self, StrId};

pub(crate) use celox_frontend_veryl::BuildConfig;
pub(crate) mod loop_provenance;
#[cfg(test)]
pub mod module;
use crate::ir::{RegionedAbsoluteAddr, RuntimeProgram, STABLE_REGION, SirProgram, UnoptimizedSir};

pub use celox_frontend_veryl::ParserError;

#[cfg(test)]
pub use celox_frontend_veryl::parse_ir;
use celox_frontend_veryl::parse_ir_with_loop_provenance;

fn apply_fused_optimization_hints(
    scheduled: &mut celox_frontend_veryl::ScheduledRtl,
    hints: celox_frontend_veryl::FusedSirOptimizationHints,
) -> Result<(), ParserError> {
    for (event, direct_ff_writes) in hints.direct_ff_writes {
        let Some(units) = scheduled.sir.eval_comb_apply_ffs.get_mut(&event) else {
            return Err(ParserError::illegal_context(
                "fused comb/FF optimization hints",
                format!("event {event} has hints but no scheduled SIR"),
                None,
            ));
        };
        for unit in units {
            let removed =
                crate::optimizer::sir::eliminate_shared_comb_state_stores(unit, &direct_ff_writes)
                    .map_err(|error| {
                        ParserError::illegal_context(
                            "shared comb/FF state-publication DSE",
                            error.to_string(),
                            None,
                        )
                    })?;
            if removed != 0 {
                crate::optimizer::sir::remove_dead_sir_definitions(unit);
            }
            if crate::optimizer::sir::promote_fused_comb_static_slots(unit).map_err(|error| {
                ParserError::illegal_context(
                    "fused comb StateSSA promotion",
                    error.to_string(),
                    None,
                )
            })? {
                crate::optimizer::sir::remove_dead_sir_definitions(unit);
            }
        }
    }
    Ok(())
}

fn dump_addr_map_if_requested(program: &RuntimeProgram, diagnostics: &crate::RuntimeDiagnostics) {
    let Some(raw_filter) = diagnostics.address_map_filter.as_deref() else {
        return;
    };

    let filter = parse_addr_map_filter(raw_filter);
    let mut entries = Vec::new();
    for (&instance_id, &module_id) in &program.frontend.instance_module {
        let Some(vars) = program.frontend.module_variables.get(&module_id) else {
            continue;
        };
        for (&var_id, info) in vars {
            let inst_key = instance_id.0.to_string();
            let var_key = normalized_addr_id(&var_id.to_string());
            if let Some(filter) = &filter
                && !filter.contains(&(inst_key, var_key))
            {
                continue;
            }
            entries.push((instance_id, module_id, var_id, info));
        }
    }

    entries.sort_by(|(a_inst, _, a_var, _), (b_inst, _, b_var, _)| {
        (a_inst.0, a_var.to_string()).cmp(&(b_inst.0, b_var.to_string()))
    });

    for (instance_id, module_id, var_id, info) in entries {
        let module_name = program
            .frontend
            .module_names
            .get(&module_id)
            .and_then(|name| resource_table::get_str_value(*name))
            .unwrap_or_default();
        let Some(addr) = program.state_address_for_source(instance_id, var_id) else {
            continue;
        };
        tracing::debug!(
            "[addr-map] inst={} var={} module={} path={} width={} array_dims={:?} 4state={} kind={:?} var_kind={}",
            instance_id,
            var_id,
            module_name,
            program.get_path(&addr),
            info.width,
            info.array_dims,
            info.is_4state,
            info.kind,
            info.var_kind.description(),
        );
    }
}

fn parse_addr_map_filter(raw: &str) -> Option<HashSet<(String, String)>> {
    if raw.is_empty() {
        return None;
    }
    let mut filter = HashSet::default();
    for item in raw
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
    {
        let Some((inst, var)) = item.split_once(':') else {
            continue;
        };
        filter.insert((normalized_addr_id(inst), normalized_addr_id(var)));
    }
    (!filter.is_empty()).then_some(filter)
}

fn normalized_addr_id(raw: &str) -> String {
    raw.trim()
        .trim_start_matches("inst")
        .trim_start_matches("var")
        .to_string()
}

fn verify_program_sir(
    sir: &SirProgram,
    program: &RuntimeProgram,
    phase: &'static str,
) -> Result<(), ParserError> {
    program.verify_design_projection().map_err(|error| {
        ParserError::illegal_context("elaborated design projection", error.to_string(), None)
    })?;
    let units = sir
        .eval_comb
        .iter()
        .enumerate()
        .map(|(unit, eu)| ("eval_comb", unit, eu))
        .chain(
            sir.eval_apply_ffs
                .values()
                .flatten()
                .enumerate()
                .map(|(unit, eu)| ("eval_apply_ffs", unit, eu)),
        )
        .chain(
            sir.eval_comb_apply_ffs
                .values()
                .flatten()
                .enumerate()
                .map(|(unit, eu)| ("eval_comb_apply_ffs", unit, eu)),
        )
        .chain(
            sir.eval_only_ffs
                .values()
                .flatten()
                .enumerate()
                .map(|(unit, eu)| ("eval_only_ffs", unit, eu)),
        )
        .chain(
            sir.apply_ffs
                .values()
                .flatten()
                .enumerate()
                .map(|(unit, eu)| ("apply_ffs", unit, eu)),
        );
    for (group, unit, eu) in units {
        verify_memory_offset_contract(program, eu).map_err(|error| ParserError::SirVerify {
            phase,
            group,
            unit,
            error,
        })?;
        verify_region_contract(group, eu).map_err(|error| ParserError::SirVerify {
            phase,
            group,
            unit,
            error,
        })?;
        eu.verify_result().map_err(|error| ParserError::SirVerify {
            phase,
            group,
            unit,
            error,
        })?;
    }
    Ok(())
}

pub(crate) fn verify_memory_offset_contract(
    program: &RuntimeProgram,
    eu: &crate::ir::ExecutionUnit<RegionedAbsoluteAddr>,
) -> Result<(), crate::ir::verify::SirVerifyError> {
    celox_sir_opt::verify_memory_offset_contract(&program.design, eu)
}

fn verify_region_contract(
    group: &'static str,
    eu: &crate::ir::ExecutionUnit<RegionedAbsoluteAddr>,
) -> Result<(), crate::ir::verify::SirVerifyError> {
    use crate::ir::{SIRInstruction, SIROffset, SPARSE_WORKING_REGION};

    for block in eu.blocks.values() {
        for (index, inst) in block.instructions.iter().enumerate() {
            match inst {
                SIRInstruction::Load(_, addr, _, _)
                | SIRInstruction::Store(addr, _, _, _, _, _)
                    if addr.region > SPARSE_WORKING_REGION =>
                {
                    return Err(crate::ir::verify::SirVerifyError::instruction(
                        "REGION.KNOWN_MEMORY_REGION",
                        block.id,
                        index,
                        format!("unknown memory region {}", addr.region),
                    ));
                }
                SIRInstruction::Commit(src, dst, _, _, _)
                    if src.region > SPARSE_WORKING_REGION || dst.region > SPARSE_WORKING_REGION =>
                {
                    return Err(crate::ir::verify::SirVerifyError::instruction(
                        "REGION.KNOWN_MEMORY_REGION",
                        block.id,
                        index,
                        format!("unknown Commit region pair {}→{}", src.region, dst.region),
                    ));
                }
                SIRInstruction::Load(_, addr, _, _) if addr.region == SPARSE_WORKING_REGION => {
                    return Err(crate::ir::verify::SirVerifyError::instruction(
                        "REGION.SPARSE_IS_NOT_READABLE",
                        block.id,
                        index,
                        "FF sparse next-state storage cannot be loaded; FF RHS values read STABLE",
                    ));
                }
                SIRInstruction::Store(addr, _, _, _, _, _)
                    if addr.region == SPARSE_WORKING_REGION
                        && !matches!(
                            group,
                            "eval_only_ffs" | "eval_apply_ffs" | "eval_comb_apply_ffs"
                        ) =>
                {
                    return Err(crate::ir::verify::SirVerifyError::instruction(
                        "REGION.SPARSE_STORE_IN_EVALUATOR",
                        block.id,
                        index,
                        format!("SPARSE Store is not valid in {group}"),
                    ));
                }
                SIRInstruction::Commit(src, dst, offset, _, triggers)
                    if src.region == SPARSE_WORKING_REGION =>
                {
                    if dst.region != STABLE_REGION
                        || !matches!(
                            group,
                            "apply_ffs" | "eval_apply_ffs" | "eval_comb_apply_ffs"
                        )
                        || !matches!(offset, SIROffset::Static(0))
                        || !triggers.is_empty()
                    {
                        return Err(crate::ir::verify::SirVerifyError::instruction(
                            "REGION.SPARSE_COMMIT_FORM",
                            block.id,
                            index,
                            "SPARSE Commit must be an untriggered full-range SPARSE→STABLE apply",
                        ));
                    }
                }
                SIRInstruction::Commit(_, dst, _, _, _) if dst.region == SPARSE_WORKING_REGION => {
                    return Err(crate::ir::verify::SirVerifyError::instruction(
                        "REGION.SPARSE_IS_NOT_COMMIT_DESTINATION",
                        block.id,
                        index,
                        "SPARSE is populated by Store, not by Commit",
                    ));
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn finalize_scheduled_rtl(
    mut scheduled: celox_frontend_veryl::ScheduledRtlOutput,
    four_state: bool,
    trace_opts: &crate::debug::TraceOptions,
    mut trace: Option<&mut crate::debug::CompilationTrace>,
    optimize_options: &crate::optimizer::OptimizeOptions,
    diagnostics: &crate::RuntimeDiagnostics,
    preserve_element_storage_layout: bool,
) -> Result<crate::ir::OptimizedSir, ParserError> {
    let phase_timing = diagnostics.phase_timing;
    macro_rules! timed_phase {
        ($label:expr, $body:expr) => {{
            if phase_timing {
                let start = crate::timing::now();
                let result = $body;
                tracing::debug!("[phase-timing] {}: {:?}", $label, start.elapsed());
                result
            } else {
                $body
            }
        }};
    }

    apply_fused_optimization_hints(&mut scheduled.scheduled, scheduled.fused_optimization_hints)?;
    scheduled.scheduled.inject_triggers();
    let (sir, mut runtime, testbench_source) = RuntimeProgram::from_scheduled(scheduled.scheduled);
    crate::testbench_compile::project_observability(&mut runtime, &testbench_source);
    runtime.testbench =
        crate::testbench_compile::compile_semantic_testbench(&runtime, &testbench_source);
    dump_addr_map_if_requested(&runtime, diagnostics);
    let mut program = UnoptimizedSir::new(sir, runtime);
    if let Some(t) = trace.as_deref_mut()
        && trace_opts.pre_optimized_sir
    {
        t.pre_optimized_sir = Some(program.clone());
    }

    timed_phase!(
        "verify_sir_before_optimize",
        verify_program_sir(&program.sir, &program.runtime, "before optimization")
    )?;
    timed_phase!("optimize", {
        if preserve_element_storage_layout {
            crate::optimizer::optimize_preserving_element_storage(
                &mut program,
                four_state,
                optimize_options,
            )
        } else {
            crate::optimizer::optimize(&mut program, four_state, optimize_options)
        }
    });
    timed_phase!(
        "verify_sir_after_optimize",
        verify_program_sir(&program.sir, &program.runtime, "after optimization")
    )?;

    let program = program.into_optimized();
    if let Some(t) = trace
        && trace_opts.post_optimized_sir
    {
        t.post_optimized_sir = Some(program.clone());
    }
    Ok(program)
}

pub fn parse(
    top: &StrId,
    ir: &veryl_analyzer::ir::Ir,
    loop_provenance: &loop_provenance::LoopProvenance,
    config: &BuildConfig,
    ignored_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
    )],
    true_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
        usize,
    )],
    four_state: bool,
    trace_opts: &crate::debug::TraceOptions,
    mut trace: Option<&mut crate::debug::CompilationTrace>,
    optimize_options: &crate::optimizer::OptimizeOptions,
    diagnostics: &crate::RuntimeDiagnostics,
    preserve_element_storage_layout: bool,
) -> Result<crate::ir::OptimizedSir, ParserError> {
    debug_assert!(
        loop_provenance.is_consistent_with(ir),
        "loop provenance must describe the analyzer IR passed to the parser"
    );
    let phase_timing = diagnostics.phase_timing;

    macro_rules! timed_phase {
        ($label:expr, $body:expr) => {{
            if phase_timing {
                let start = crate::timing::now();
                let result = $body;
                tracing::debug!("[phase-timing] {}: {:?}", $label, start.elapsed());
                result
            } else {
                $body
            }
        }};
    }

    let result = timed_phase!(
        "parse_ir",
        parse_ir_with_loop_provenance(ir, loop_provenance, config, top)
    )?;
    if let Some(t) = trace.as_deref_mut()
        && trace_opts.analyzer_ir
    {
        t.analyzer_ir = Some(ir.to_string());
    }
    let frontend_trace_options = trace_opts.frontend(diagnostics);
    let mut frontend_trace = celox_frontend_veryl::FrontendTrace::default();
    let scheduled = timed_phase!(
        "flatten",
        celox_frontend_veryl::schedule_symbolic_rtl(
            result,
            config,
            ignored_loops,
            true_loops,
            four_state,
            &frontend_trace_options,
            trace.is_some().then_some(&mut frontend_trace),
        )
    );
    if let Some(trace) = trace.as_deref_mut() {
        trace.absorb_frontend(frontend_trace);
    }
    finalize_scheduled_rtl(
        scheduled?,
        four_state,
        trace_opts,
        trace,
        optimize_options,
        diagnostics,
        preserve_element_storage_layout,
    )
}

pub fn parse_sv(
    sources: &[(&str, &std::path::Path)],
    top: &str,
    config: &BuildConfig,
    ignored_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
    )],
    true_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
        usize,
    )],
    four_state: bool,
    trace_opts: &crate::debug::TraceOptions,
    mut trace: Option<&mut crate::debug::CompilationTrace>,
    optimize_options: &crate::optimizer::OptimizeOptions,
    diagnostics: &crate::RuntimeDiagnostics,
    preserve_element_storage_layout: bool,
) -> Result<crate::ir::OptimizedSir, ParserError> {
    let frontend_trace_options = trace_opts.frontend(diagnostics);
    let mut frontend_trace = celox_frontend_veryl::FrontendTrace::default();
    let scheduled = crate::frontend_sv::schedule_sources(
        sources,
        top,
        config,
        ignored_loops,
        true_loops,
        four_state,
        &frontend_trace_options,
        trace.is_some().then_some(&mut frontend_trace),
    )
    .map_err(|error| match error {
        crate::frontend_sv::FrontendError::Lowering(error) => error,
        crate::frontend_sv::FrontendError::Analyzer(error) => ParserError::unsupported(
            64,
            celox_frontend_veryl::LoweringPhase::SimulatorParser,
            "systemverilog analysis",
            error.to_string(),
            None,
        ),
    })?;
    if let Some(trace) = trace.as_deref_mut() {
        trace.absorb_frontend(frontend_trace);
    }
    finalize_scheduled_rtl(
        scheduled,
        four_state,
        trace_opts,
        trace,
        optimize_options,
        diagnostics,
        preserve_element_storage_layout,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn parse_mixed(
    top: &StrId,
    ir: &veryl_analyzer::ir::Ir,
    loop_provenance: &loop_provenance::LoopProvenance,
    sv_sources: &[(&str, &std::path::Path)],
    config: &BuildConfig,
    ignored_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
    )],
    true_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
        usize,
    )],
    four_state: bool,
    trace_opts: &crate::debug::TraceOptions,
    mut trace: Option<&mut crate::debug::CompilationTrace>,
    optimize_options: &crate::optimizer::OptimizeOptions,
    diagnostics: &crate::RuntimeDiagnostics,
    preserve_element_storage_layout: bool,
) -> Result<crate::ir::OptimizedSir, ParserError> {
    let external =
        crate::frontend_sv::prepare_external_hierarchy(sv_sources).map_err(
            |error| match error {
                crate::frontend_sv::FrontendError::Lowering(error) => error,
                crate::frontend_sv::FrontendError::Analyzer(error) => ParserError::unsupported(
                    64,
                    celox_frontend_veryl::LoweringPhase::SimulatorParser,
                    "systemverilog analysis",
                    error.to_string(),
                    None,
                ),
            },
        )?;
    let symbolic = celox_frontend_veryl::parse_ir_with_external_hierarchy(
        ir,
        loop_provenance,
        &external,
        config,
        top,
    )?;
    if let Some(trace) = trace.as_deref_mut()
        && trace_opts.analyzer_ir
    {
        trace.analyzer_ir = Some(ir.to_string());
    }
    let frontend_trace_options = trace_opts.frontend(diagnostics);
    let mut frontend_trace = celox_frontend_veryl::FrontendTrace::default();
    let scheduled = celox_frontend_veryl::schedule_symbolic_rtl(
        symbolic,
        config,
        ignored_loops,
        true_loops,
        four_state,
        &frontend_trace_options,
        trace.is_some().then_some(&mut frontend_trace),
    )?;
    if let Some(trace) = trace.as_deref_mut() {
        trace.absorb_frontend(frontend_trace);
    }
    finalize_scheduled_rtl(
        scheduled,
        four_state,
        trace_opts,
        trace,
        optimize_options,
        diagnostics,
        preserve_element_storage_layout,
    )
}
