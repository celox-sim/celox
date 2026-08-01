use crate::HashSet;

use veryl_parser::resource_table::{self, StrId};

pub(crate) use celox_frontend_veryl::BuildConfig;
pub(crate) mod loop_provenance;
#[cfg(test)]
pub mod module;
use crate::ir::{Program, RegionedAbsoluteAddr, STABLE_REGION};

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
            let removed = crate::optimizer::coalescing::eliminate_shared_comb_state_stores(
                unit,
                &direct_ff_writes,
            )
            .map_err(|message| {
                ParserError::illegal_context("shared comb/FF state-publication DSE", message, None)
            })?;
            if removed != 0 {
                crate::optimizer::coalescing::remove_dead_sir_definitions(unit);
            }
            if crate::optimizer::coalescing::promote_fused_comb_static_slots(unit).map_err(
                |message| {
                    ParserError::illegal_context("fused comb StateSSA promotion", message, None)
                },
            )? {
                crate::optimizer::coalescing::remove_dead_sir_definitions(unit);
            }
        }
    }
    Ok(())
}

fn dump_addr_map_if_requested(program: &Program) {
    if std::env::var_os("CELOX_ADDR_MAP_DUMP").is_none() {
        return;
    }

    let filter = parse_addr_map_filter();
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
        eprintln!(
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

fn parse_addr_map_filter() -> Option<HashSet<(String, String)>> {
    let raw = std::env::var_os("CELOX_ADDR_MAP_FILTER")?;
    let raw = raw.to_string_lossy();
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
    Some(filter)
}

fn normalized_addr_id(raw: &str) -> String {
    raw.trim()
        .trim_start_matches("inst")
        .trim_start_matches("var")
        .to_string()
}

fn verify_program_sir(program: &Program, phase: &'static str) -> Result<(), ParserError> {
    program.verify_design_projection().map_err(|detail| {
        ParserError::illegal_context("elaborated design projection", detail, None)
    })?;
    let units = program
        .sir
        .eval_comb
        .iter()
        .enumerate()
        .map(|(unit, eu)| ("eval_comb", unit, eu))
        .chain(
            program
                .sir
                .eval_apply_ffs
                .values()
                .flatten()
                .enumerate()
                .map(|(unit, eu)| ("eval_apply_ffs", unit, eu)),
        )
        .chain(
            program
                .sir
                .eval_comb_apply_ffs
                .values()
                .flatten()
                .enumerate()
                .map(|(unit, eu)| ("eval_comb_apply_ffs", unit, eu)),
        )
        .chain(
            program
                .sir
                .eval_only_ffs
                .values()
                .flatten()
                .enumerate()
                .map(|(unit, eu)| ("eval_only_ffs", unit, eu)),
        )
        .chain(
            program
                .sir
                .apply_ffs
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
    program: &Program,
    eu: &crate::ir::ExecutionUnit<RegionedAbsoluteAddr>,
) -> Result<(), crate::ir::verify::SirVerifyError> {
    use crate::ir::SIRInstruction;

    for block in eu.blocks.values() {
        for (index, inst) in block.instructions.iter().enumerate() {
            let (addr, offset, width, operation, explicit_memory_copy) = match inst {
                SIRInstruction::Load(_, addr, offset, width) => {
                    (addr, offset, *width, "Load", false)
                }
                SIRInstruction::Store(addr, offset, width, _, _, _) => {
                    (addr, offset, *width, "Store", false)
                }
                SIRInstruction::Commit(src, dst, offset, width, _) => {
                    verify_memory_offset_for_addr(
                        program,
                        block.id,
                        index,
                        dst,
                        offset,
                        *width,
                        "Commit destination",
                        true,
                    )?;
                    (src, offset, *width, "Commit source", true)
                }
                _ => continue,
            };
            verify_memory_offset_for_addr(
                program,
                block.id,
                index,
                addr,
                offset,
                width,
                operation,
                explicit_memory_copy,
            )?;
        }
    }
    Ok(())
}

fn verify_memory_offset_for_addr(
    program: &Program,
    block: crate::ir::BlockId,
    index: usize,
    addr: &RegionedAbsoluteAddr,
    offset: &crate::ir::SIROffset,
    width: usize,
    operation: &'static str,
    explicit_memory_copy: bool,
) -> Result<(), crate::ir::verify::SirVerifyError> {
    use crate::ir::SIROffset;

    let Some(info) = program.get_variable_info(&addr.absolute_addr()) else {
        return Err(crate::ir::verify::SirVerifyError::instruction(
            "MEMORY.ADDRESS_HAS_DECLARATION",
            block,
            index,
            format!("no variable declaration for memory address {addr:?}"),
        ));
    };
    let element_count = info
        .array_dims
        .iter()
        .try_fold(1usize, |count, &dimension| count.checked_mul(dimension));
    let declared_element_width = (!info.array_dims.is_empty())
        .then_some(element_count)
        .flatten()
        .filter(|&count| count != 0 && info.width % count == 0)
        .map(|count| info.width / count);

    match offset {
        SIROffset::Dynamic(_) if !info.array_dims.is_empty() => {
            let absolute_addr = addr.absolute_addr();
            return Err(crate::ir::verify::SirVerifyError::instruction(
                "MEMORY.UNPACKED_OFFSET_IS_ELEMENT",
                block,
                index,
                format!(
                    "{operation} addresses unpacked array {} with dimensions {:?} by an arbitrary dynamic bit offset; preserve the element index as SIROffset::Element",
                    program.get_path(&absolute_addr),
                    info.array_dims,
                ),
            ));
        }
        SIROffset::Element { element_width, .. } => {
            if info.array_dims.is_empty() {
                return Err(crate::ir::verify::SirVerifyError::instruction(
                    "MEMORY.ELEMENT_OFFSET_REQUIRES_UNPACKED_ARRAY",
                    block,
                    index,
                    "SIROffset::Element used for a variable without unpacked dimensions",
                ));
            }
            let Some(declared_element_width) = declared_element_width else {
                return Err(crate::ir::verify::SirVerifyError::instruction(
                    "MEMORY.UNPACKED_DECLARATION_HAS_ELEMENT_WIDTH",
                    block,
                    index,
                    format!(
                        "array dimensions {:?} do not divide declared width {}",
                        info.array_dims, info.width
                    ),
                ));
            };
            if *element_width != declared_element_width {
                return Err(crate::ir::verify::SirVerifyError::instruction(
                    "MEMORY.ELEMENT_WIDTH_MATCHES_DECLARATION",
                    block,
                    index,
                    format!(
                        "SIR element width {element_width} does not match declared element width {declared_element_width}"
                    ),
                ));
            }
            let SIROffset::Element { bit_offset, .. } = offset else {
                unreachable!()
            };
            if bit_offset
                .checked_add(width)
                .is_none_or(|end| end > declared_element_width)
            {
                return Err(crate::ir::verify::SirVerifyError::instruction(
                    "MEMORY.ACCESS_STAYS_WITHIN_UNPACKED_ELEMENT",
                    block,
                    index,
                    format!(
                        "{operation} range [{bit_offset} +: {width}] exceeds unpacked element width {declared_element_width}"
                    ),
                ));
            }
        }
        SIROffset::PackedElements {
            bit_offset,
            element_width,
        } => {
            let Some(declared_element_width) = declared_element_width else {
                return Err(crate::ir::verify::SirVerifyError::instruction(
                    "MEMORY.PACKED_ELEMENTS_REQUIRE_UNPACKED_ARRAY",
                    block,
                    index,
                    format!("{operation} uses packed-elements addressing on a non-array variable"),
                ));
            };
            let valid_range = *element_width == declared_element_width
                && bit_offset.is_multiple_of(declared_element_width)
                && width.is_multiple_of(declared_element_width)
                && bit_offset
                    .checked_add(width)
                    .is_some_and(|end| end <= info.width);
            if !valid_range {
                return Err(crate::ir::verify::SirVerifyError::instruction(
                    "MEMORY.PACKED_ELEMENTS_MATCH_DECLARATION",
                    block,
                    index,
                    format!(
                        "{operation} packed-elements range [{bit_offset} +: {width}] with element width {element_width} does not match declared element width {declared_element_width} and total width {}",
                        info.width
                    ),
                ));
            }
        }
        SIROffset::Static(start)
            if !explicit_memory_copy
                && let Some(element_width) = declared_element_width
                && width != 0 =>
        {
            let Some(end) = start.checked_add(width) else {
                return Err(crate::ir::verify::SirVerifyError::instruction(
                    "MEMORY.ACCESS_STAYS_WITHIN_UNPACKED_ELEMENT",
                    block,
                    index,
                    format!("{operation} range overflows usize"),
                ));
            };
            if *start / element_width != end.saturating_sub(1) / element_width {
                return Err(crate::ir::verify::SirVerifyError::instruction(
                    "MEMORY.ACCESS_STAYS_WITHIN_UNPACKED_ELEMENT",
                    block,
                    index,
                    format!(
                        "{operation} at {addr:?} ({}) range [{start} +: {width}] crosses unpacked element width {element_width}; use an explicit array-copy operation for a multi-element transfer",
                        program.get_path(&addr.absolute_addr()),
                    ),
                ));
            }
        }
        SIROffset::Static(_) | SIROffset::Dynamic(_) => {}
    }
    Ok(())
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
    preserve_element_storage_layout: bool,
) -> Result<crate::ir::OptimizedSir, ParserError> {
    debug_assert!(
        loop_provenance.is_consistent_with(ir),
        "loop provenance must describe the analyzer IR passed to the parser"
    );
    let phase_timing = std::env::var("CELOX_PHASE_TIMING").is_ok();

    macro_rules! timed_phase {
        ($label:expr, $body:expr) => {{
            if phase_timing {
                let start = crate::timing::now();
                let result = $body;
                eprintln!("[phase-timing] {}: {:?}", $label, start.elapsed());
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
    let frontend_trace_options = trace_opts.frontend();
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
    let mut scheduled = scheduled?;
    apply_fused_optimization_hints(&mut scheduled.scheduled, scheduled.fused_optimization_hints)?;
    scheduled.scheduled.inject_triggers();
    let scheduled = scheduled.scheduled;
    let (mut program, testbench_source) = Program::from_scheduled(scheduled);
    crate::testbench::project_observability(&mut program, &testbench_source);
    program.testbench = crate::testbench::compile_semantic_testbench(&program, &testbench_source);
    dump_addr_map_if_requested(&program);
    if let Some(t) = trace.as_deref_mut()
        && trace_opts.pre_optimized_sir
    {
        t.pre_optimized_sir = Some(program.clone());
    }

    timed_phase!(
        "verify_sir_before_optimize",
        verify_program_sir(&program, "before optimization")
    )?;

    // Always run the SIR pipeline so required canonicalization and explicit
    // per-pass overrides are applied consistently. Concrete backend planning
    // (including Cranelift function splitting) happens after this phase.
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
        verify_program_sir(&program, "after optimization")
    )?;

    if let Some(t) = trace
        && trace_opts.post_optimized_sir
    {
        t.post_optimized_sir = Some(program.clone());
    }

    Ok(crate::ir::OptimizedSir::new(program))
}
