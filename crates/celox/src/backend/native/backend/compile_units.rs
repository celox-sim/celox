//! Compiling execution units to native functions and appending their code.

use super::*;

pub(in crate::backend::native) struct CompiledNativeFunction {
    pub(super) code: Vec<u8>,
    pub(super) symbols: Vec<jit_mem::JitSymbol>,
    pub(super) trace: Option<emit::NativeFunctionTrace>,
    pub(in crate::backend::native) required_state_size: usize,
    pub(in crate::backend::native) required_native_features: u8,
}

#[cfg_attr(
    any(
        all(feature = "arm64-codegen", not(target_arch = "aarch64")),
        all(feature = "x86_64-codegen", not(target_arch = "x86_64"))
    ),
    allow(dead_code)
)]
pub(crate) struct NativeCodegenTrace {
    pub optimized_sir: String,
    pub mir: String,
    pub reactive_graph: String,
    pub state_layout: String,
}

/// Optimization context of a merged native function. Diagnostic labels do
/// not describe whether pending values can be produced outside this function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::backend::native) enum NativeMergeKind {
    /// A complete sequential group of execution units.
    Complete,
    /// Combinational settling followed by an FF event in the same function.
    FusedCombAndFf,
    /// One task of a partitioned kernel; other tasks may define pending state.
    Partial,
}

pub(super) fn prepare_merged_sir(
    units: &[&crate::ir::ExecutionUnit<crate::ir::RegionedAbsoluteAddr>],
    layout: &MemoryLayout,
    four_state: bool,
    label: &str,
    merge_kind: NativeMergeKind,
    first_ff_unit: Option<usize>,
    diagnostics: &crate::optimizer::SirDiagnostics,
    cancel: Option<&CompileCancel>,
    baseline: bool,
    calls_functions: bool,
) -> Result<crate::ir::ExecutionUnit<crate::ir::RegionedAbsoluteAddr>, SimulatorError> {
    let verify_enabled = cfg!(debug_assertions) || diagnostics.verify_boundaries;
    let mut phase_start = diagnostics.pass_timing.then(crate::timing::now);
    let mut report_phase = |phase: &str| {
        if let Some(start) = phase_start {
            tracing::debug!(
                "[native-sir-timing] label={label} phase={phase} elapsed={:?}",
                start.elapsed()
            );
            phase_start = Some(crate::timing::now());
        }
    };
    if verify_enabled {
        for (unit_index, unit) in units.iter().enumerate() {
            if let Err(error) = unit.verify_result() {
                return Err(codegen_err(CodegenError::SirVerification {
                    phase: format!(
                        "invalid SIR before x86 source-unit merge: {label} source unit {unit_index}"
                    ),
                    source: error,
                }));
            }
        }
    }

    let (mut sir_eu, merge_provenance) = celox_sir::merge_sir_eu_refs_with_provenance(units);
    let boundaries = merge_provenance.unit_entries[1..].to_vec();
    let verify = |eu: &crate::ir::ExecutionUnit<crate::ir::RegionedAbsoluteAddr>,
                  phase: &'static str| {
        if verify_enabled {
            eu.verify_result().map_err(|source| {
                codegen_err(CodegenError::SirVerification {
                    phase: phase.to_string(),
                    source,
                })
            })
        } else {
            Ok(())
        }
    };

    verify(&sir_eu, "before x86 merged-SIR optimization")?;
    report_phase("merge");
    if cancelled(cancel) {
        return Err(cancelled_error());
    }
    // The baseline tier keeps the finalized source-unit order and every
    // state store; the optional cross-unit transforms belong to the
    // optimizing tier. ISel/legalize/regalloc/emit still run below.
    if baseline {
        return Ok(sir_eu);
    }
    // These rewrites read the unit's own loads and stores, not the effects
    // of the functions it calls, nor frame slots, which no other unit sees.
    if calls_functions {
        return Ok(sir_eu);
    }
    if let Some(first_ff_unit) = first_ff_unit {
        let removed = crate::optimizer::sir::eliminate_unobserved_comb_state_stores(
            &mut sir_eu,
            &merge_provenance,
            first_ff_unit,
        )
        .map_err(|source| {
            codegen_err(CodegenError::Optimization {
                context: "comb/FF state-publication DSE",
                source,
            })
        })?;
        if removed != 0 {
            crate::optimizer::sir::remove_dead_sir_definitions(&mut sir_eu);
            verify(&sir_eu, "after comb/FF state-publication DSE")?;
        }
    }
    report_phase("comb_state_dse");
    if merge_kind == NativeMergeKind::FusedCombAndFf
        && crate::optimizer::sir::promote_fused_comb_static_slots(&mut sir_eu).map_err(
            |source| {
                codegen_err(CodegenError::Optimization {
                    context: "final fused comb StateSSA promotion",
                    source,
                })
            },
        )?
    {
        crate::optimizer::sir::remove_dead_sir_definitions(&mut sir_eu);
        verify(&sir_eu, "after final fused comb StateSSA promotion")?;
    }
    report_phase("comb_state_ssa");
    match merge_kind {
        NativeMergeKind::Partial => {
            crate::optimizer::sir::pass_eliminate_working_round_trip::eliminate_partial_working_round_trip(&mut sir_eu);
        }
        NativeMergeKind::Complete | NativeMergeKind::FusedCombAndFf => {
            crate::optimizer::sir::pass_eliminate_working_round_trip::eliminate_working_round_trip(
                &mut sir_eu,
                &boundaries,
            );
        }
    }
    verify(&sir_eu, "after x86 direct working rewrite")?;
    if cancelled(cancel) {
        return Err(cancelled_error());
    }
    let promoted_working =
        crate::optimizer::sir::promote_eval_apply_working_round_trips(&mut sir_eu);
    if promoted_working {
        verify(&sir_eu, "after x86 working StateSSA")?;
        crate::optimizer::sir::remove_dead_sir_definitions(&mut sir_eu);
        verify(&sir_eu, "after x86 working StateSSA DCE")?;
    }
    report_phase("working_state_ssa");
    if cancelled(cancel) {
        return Err(cancelled_error());
    }
    crate::optimizer::sir::optimize_native_merged_chain(
        &mut sir_eu,
        layout,
        four_state,
        merge_kind == NativeMergeKind::FusedCombAndFf,
        diagnostics,
        || cancelled(cancel),
    )
    .map_err(|source| {
        if source.kind() == celox_sir_opt::OptimizationErrorKind::Cancelled {
            codegen_err(CodegenError::Cancelled)
        } else {
            codegen_err(CodegenError::Optimization {
                context: "native merged-chain optimization",
                source,
            })
        }
    })?;
    verify(&sir_eu, "after x86 merged-chain cleanup")?;
    report_phase("merged_chain");
    Ok(sir_eu)
}

pub(super) fn compile_units(
    units: &[crate::ir::ExecutionUnit<crate::ir::RegionedAbsoluteAddr>],
    layout: &MemoryLayout,
    four_state: bool,
    label: &str,
    x86_options: &crate::backend::X86BackendOptions,
    capture_trace: bool,
    diagnostics: &crate::optimizer::SirDiagnostics,
    cancel: Option<&CompileCancel>,
) -> Result<CompiledNativeFunction, SimulatorError> {
    let units = units.iter().collect::<Vec<_>>();
    compile_unit_refs(
        &units,
        layout,
        four_state,
        label,
        NativeMergeKind::Complete,
        None,
        x86_options,
        capture_trace,
        diagnostics,
        cancel,
    )
}

pub(in crate::backend::native) fn compile_unit_refs(
    units: &[&crate::ir::ExecutionUnit<crate::ir::RegionedAbsoluteAddr>],
    layout: &MemoryLayout,
    four_state: bool,
    label: &str,
    merge_kind: NativeMergeKind,
    first_ff_unit: Option<usize>,
    x86_options: &crate::backend::X86BackendOptions,
    capture_trace: bool,
    diagnostics: &crate::optimizer::SirDiagnostics,
    cancel: Option<&CompileCancel>,
) -> Result<CompiledNativeFunction, SimulatorError> {
    compile_unit_refs_inspected(
        units,
        layout,
        four_state,
        label,
        merge_kind,
        first_ff_unit,
        x86_options,
        capture_trace,
        diagnostics,
        cancel,
        &mut |_| {},
    )
}

/// [`compile_unit_refs`] that shows the merged SIR, after every SIR rewrite
/// of the native pipeline, to `inspect_prepared` before instruction
/// selection. It is not called for an empty unit list.
#[allow(clippy::too_many_arguments)]
pub(in crate::backend::native) fn compile_unit_refs_inspected(
    units: &[&crate::ir::ExecutionUnit<crate::ir::RegionedAbsoluteAddr>],
    layout: &MemoryLayout,
    four_state: bool,
    label: &str,
    merge_kind: NativeMergeKind,
    first_ff_unit: Option<usize>,
    x86_options: &crate::backend::X86BackendOptions,
    capture_trace: bool,
    diagnostics: &crate::optimizer::SirDiagnostics,
    cancel: Option<&CompileCancel>,
    inspect_prepared: &mut dyn FnMut(&crate::ir::ExecutionUnit<crate::ir::RegionedAbsoluteAddr>),
) -> Result<CompiledNativeFunction, SimulatorError> {
    if cancelled(cancel) {
        return Err(cancelled_error());
    }
    let timing = x86_options.diagnostics.phase_timing;
    if units.is_empty() {
        // Empty function: just return 0
        #[cfg(any(
            feature = "x86_64-codegen",
            all(target_arch = "x86_64", not(feature = "arm64-codegen"))
        ))]
        let (empty_result, empty_mir) = {
            let mut empty_func =
                super::super::mir::MFunction::new(super::super::mir::VRegAllocator::new(), vec![]);
            let mut block = super::super::mir::MBlock::new(super::super::mir::BlockId(0));
            block.push(super::super::mir::MInst::Return);
            empty_func.push_block(block);
            let empty_result = emit::emit(&empty_func, &regalloc::AssignmentMap::default(), 0)
                .map_err(|source| codegen_err(CodegenError::NativeEmission { source }))?;
            let empty_mir = empty_func.to_string();
            (empty_result, empty_mir)
        };
        #[cfg(any(
            feature = "arm64-codegen",
            all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
        ))]
        let (empty_result, empty_mir) = {
            let state_size = layout
                .merged_total_size
                .checked_add(layout.triggered_bits_total_size)
                .ok_or_else(|| {
                    codegen_err(CodegenError::NativeEmission {
                        source: emit::EmitError::Range("AArch64 simulation-state size overflow"),
                    })
                })?;
            let result = emit::emit_empty(state_size)
                .map_err(|source| codegen_err(CodegenError::NativeEmission { source }))?;
            (result, "bb0:\n  Return\n".to_string())
        };
        let trace = capture_trace.then(|| emit::NativeFunctionTrace {
            optimized_sir: "<empty native function>\n".into(),
            reactive_graph: String::new(),
            state_layout: String::new(),
            mir_before_regalloc: empty_mir.clone(),
            mir_after_late_memory_folds: empty_mir.clone(),
            mir_after_scheduling: empty_mir.clone(),
            mir_after_regalloc: empty_mir,
            register_assignment: String::new(),
            spill_frame_size: 0,
            disassembly: emit::disassemble(&empty_result.code[..empty_result.text_size], 0),
        });
        let symbols = perf_symbols_for_emit_result(label, &empty_result);
        return Ok(CompiledNativeFunction {
            code: empty_result.code,
            symbols,
            trace,
            required_state_size: empty_result.required_state_size as usize,
            #[cfg(any(
                feature = "x86_64-codegen",
                all(target_arch = "x86_64", not(feature = "arm64-codegen"))
            ))]
            required_native_features: empty_result.required_image_features,
            #[cfg(any(
                feature = "arm64-codegen",
                all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
            ))]
            required_native_features: 0,
        });
    }

    // Merge all EUs and compile the exact SIR/MIR function used at runtime.
    if timing {
        tracing::debug!(
            "[native-timing] compile_units start label={label} eus={}",
            units.len()
        );
    }
    let start = timing.then(crate::timing::now);
    // Phase timing also reports the merged-SIR phases.
    let timed_diagnostics = (timing && !diagnostics.pass_timing).then(|| {
        let mut diagnostics = diagnostics.clone();
        diagnostics.pass_timing = true;
        diagnostics
    });
    let sir_eu = prepare_merged_sir(
        units,
        layout,
        four_state,
        label,
        merge_kind,
        first_ff_unit,
        timed_diagnostics.as_ref().unwrap_or(diagnostics),
        cancel,
        x86_options.baseline,
        // A function unit, or a unit that calls one.
        x86_options.function.is_some()
            || units.iter().any(|unit| {
                unit.blocks.values().any(|block| {
                    block
                        .instructions
                        .iter()
                        .any(|instruction| matches!(instruction, SIRInstruction::Call { .. }))
                })
            }),
    )?;
    inspect_prepared(&sir_eu);
    if cancelled(cancel) {
        return Err(cancelled_error());
    }
    let mut trace = capture_trace.then(emit::NativeFunctionTrace::default);
    #[cfg(any(
        feature = "x86_64-codegen",
        all(target_arch = "x86_64", not(feature = "arm64-codegen"))
    ))]
    let emit_result = emit::emit_owned_prepared_eu(
        sir_eu,
        layout,
        four_state,
        label,
        x86_options,
        trace.as_mut(),
        || cancelled(cancel),
    )
    .map_err(|source| {
        if matches!(source, emit::ChainedEmitError::Cancelled) {
            codegen_err(CodegenError::Cancelled)
        } else {
            codegen_err(CodegenError::NativePipeline { source })
        }
    })?;
    #[cfg(any(
        feature = "arm64-codegen",
        all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
    ))]
    let emit_result = emit::emit_prepared_eu_with_functions(
        &sir_eu,
        layout,
        four_state,
        label,
        x86_options.native_tick_loop,
        x86_options.arena_base,
        x86_options
            .functions
            .as_deref()
            .map(|functions| celox_backend_arm64::FunctionCalls {
                frames: &functions.frames,
                first_slot: functions.first_slot,
                function: x86_options.function,
            }),
        trace.as_mut(),
        || cancelled(cancel),
    )
    .map_err(|source| {
        if matches!(source, emit::ChainedEmitError::Cancelled) {
            codegen_err(CodegenError::Cancelled)
        } else {
            codegen_err(CodegenError::NativePipeline { source })
        }
    })?;
    if let Some(start) = start {
        tracing::debug!(
            "[native-timing] compile_units done label={label} bytes={} elapsed={:?}",
            emit_result.code.len(),
            start.elapsed()
        );
    }
    let symbols = perf_symbols_for_emit_result(label, &emit_result);
    let required_state_size = emit_result.required_state_size as usize;
    Ok(CompiledNativeFunction {
        code: emit_result.code,
        symbols,
        trace,
        required_state_size,
        #[cfg(any(
            feature = "x86_64-codegen",
            all(target_arch = "x86_64", not(feature = "arm64-codegen"))
        ))]
        required_native_features: emit_result.required_image_features,
        #[cfg(any(
            feature = "arm64-codegen",
            all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
        ))]
        required_native_features: 0,
    })
}

pub(super) fn perf_symbols_for_emit_result(
    label: &str,
    result: &emit::EmitResult,
) -> Vec<jit_mem::JitSymbol> {
    let code_len = result.text_size;
    if result.block_offsets.is_empty() {
        return Vec::new();
    }

    let mut blocks = result.block_offsets.clone();
    blocks.sort_by_key(|(_, offset)| *offset);

    let mut symbols = Vec::with_capacity(blocks.len() + 2);
    let first_offset = blocks[0].1 as usize;
    if first_offset > 0 {
        symbols.push(jit_mem::JitSymbol {
            offset: 0,
            size: first_offset,
            name: format!("{label}.prologue"),
        });
    }

    for (idx, (block_id, offset)) in blocks.iter().enumerate() {
        let start = *offset as usize;
        let end = blocks
            .get(idx + 1)
            .map(|(_, next)| *next as usize)
            .unwrap_or(code_len);
        if end > start {
            symbols.push(jit_mem::JitSymbol {
                offset: start,
                size: end - start,
                name: format!("{label}.bb{}", block_id.0),
            });
        }
    }

    symbols
}

pub(super) const NATIVE_CODE_ENTRY_ALIGNMENT: usize = 16;

pub(in crate::backend::native) fn append_native_code(
    image: &mut Vec<u8>,
    entries: &mut Vec<NativeCodeEntry>,
    image_symbols: &mut Vec<NativeCodeSymbol>,
    name: String,
    compiled: &CompiledNativeFunction,
) -> Result<usize, SimulatorError> {
    let offset = image
        .len()
        .checked_add(NATIVE_CODE_ENTRY_ALIGNMENT - 1)
        .map(|value| value & !(NATIVE_CODE_ENTRY_ALIGNMENT - 1))
        .ok_or_else(|| codegen_message("packed native code image alignment overflow"))?;
    image.resize(offset, 0);
    let end = offset
        .checked_add(compiled.code.len())
        .ok_or_else(|| codegen_message("packed native code image size overflow"))?;
    image.extend_from_slice(&compiled.code);

    if compiled.symbols.is_empty() {
        image_symbols.push(NativeCodeSymbol {
            offset,
            size: compiled.code.len(),
            name: name.clone(),
        });
    } else {
        for symbol in &compiled.symbols {
            let symbol_end = symbol
                .offset
                .checked_add(symbol.size)
                .ok_or_else(|| codegen_message("native function symbol range overflow"))?;
            if symbol_end > compiled.code.len() {
                return Err(codegen_message(format!(
                    "native function symbol `{}` exceeds its emitted code",
                    symbol.name
                )));
            }
            image_symbols.push(NativeCodeSymbol {
                offset: offset + symbol.offset,
                size: symbol.size,
                name: format!("{name}.{}", symbol.name),
            });
        }
    }

    entries.push(NativeCodeEntry {
        name,
        offset,
        size: end - offset,
    });
    Ok(offset)
}

pub(in crate::backend::native) fn native_function_at(
    image: &jit_mem::JitCode,
    offset: usize,
) -> Result<NativeSimFunc, SimulatorError> {
    let ptr = image
        .entry_ptr(offset)
        .ok_or_else(|| codegen_message("native function entry exceeds packed code image"))?;
    // Safety: `offset` was returned by `append_native_code` for a complete
    // function emitted with `NativeSimFunc`'s target ABI. `image` owns the
    // executable allocation for at least as long as the returned pointer.
    Ok(unsafe { std::mem::transmute::<*const u8, NativeSimFunc>(ptr) })
}

pub(super) struct NativeCompileTask<'a> {
    pub(super) units: Vec<&'a crate::ir::ExecutionUnit<crate::ir::RegionedAbsoluteAddr>>,
    pub(super) label: &'static str,
    pub(super) merge_kind: NativeMergeKind,
    pub(super) first_ff_unit: Option<usize>,
    pub(super) bindings: Vec<String>,
}

pub(super) type NativeTaskBindings = HashMap<(&'static str, AbsoluteAddr), usize>;

pub(super) type NativeUnit = crate::ir::ExecutionUnit<crate::ir::RegionedAbsoluteAddr>;

/// Classes of structurally equal execution units. Each distinct unit is
/// hashed once and compared only against the units of its hash, so tasks
/// whose unit lists share whole programs, such as every unfused comb-apply
/// task starting with all of `eval_comb`, are told apart by their class
/// lists instead of by comparing the programs again for each trigger.
#[derive(Default)]
pub(super) struct NativeUnitClasses<'a> {
    pub(super) by_unit: HashMap<*const NativeUnit, usize>,
    pub(super) by_hash: HashMap<u64, Vec<(&'a NativeUnit, usize)>>,
    pub(super) count: usize,
}

impl<'a> NativeUnitClasses<'a> {
    pub(super) fn class(&mut self, unit: &'a NativeUnit) -> usize {
        if let Some(class) = self.by_unit.get(&std::ptr::from_ref(unit)) {
            return *class;
        }
        let bucket = self.by_hash.entry(native_unit_hash(unit)).or_default();
        let class = match bucket.iter().find(|(other, _)| *other == unit) {
            Some((_, class)) => *class,
            None => {
                let class = self.count;
                self.count += 1;
                bucket.push((unit, class));
                class
            }
        };
        self.by_unit.insert(std::ptr::from_ref(unit), class);
        class
    }

    pub(super) fn classes(&mut self, units: &[&'a NativeUnit]) -> Vec<usize> {
        units.iter().map(|unit| self.class(unit)).collect()
    }
}

/// A hash of `unit` that equal units share, independent of the iteration
/// order of its maps.
pub(super) fn native_unit_hash(unit: &NativeUnit) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = fxhash::FxHasher::default();
    unit.entry_block_id.hash(&mut hasher);
    let mut blocks = unit.blocks.iter().collect::<Vec<_>>();
    blocks.sort_unstable_by_key(|(id, _)| **id);
    for (id, block) in blocks {
        id.hash(&mut hasher);
        block.id.hash(&mut hasher);
        block.params.hash(&mut hasher);
        block.instructions.hash(&mut hasher);
        block.terminator.hash(&mut hasher);
    }
    let mut registers = unit.register_map.iter().collect::<Vec<_>>();
    registers.sort_unstable_by_key(|(id, _)| **id);
    registers.hash(&mut hasher);
    hasher.finish()
}

pub(super) fn collect_ff_compile_tasks(
    sir: &LaidOutProgram,
) -> (Vec<NativeCompileTask<'_>>, NativeTaskBindings) {
    let mut tasks = Vec::new();
    let mut task_bindings = HashMap::default();
    let mut classes = NativeUnitClasses::default();
    // Edge tasks of every label share one index: a task is reused for any
    // edge trigger with equal units.
    let mut ff_tasks = HashMap::default();
    for (ff_map, label) in [
        (&sir.sir.eval_apply_ffs, "eval_apply_ff"),
        (&sir.sir.eval_only_ffs, "eval_only_ff"),
        (&sir.sir.apply_ffs, "apply_ff"),
    ] {
        collect_ff_compile_tasks_from(
            sir,
            ff_map,
            label,
            &mut classes,
            &mut ff_tasks,
            &mut tasks,
            &mut task_bindings,
        );
    }
    collect_comb_apply_compile_tasks(sir, &mut classes, &mut tasks, &mut task_bindings);
    (tasks, task_bindings)
}

pub(super) fn collect_comb_apply_compile_tasks<'a>(
    sir: &'a LaidOutProgram,
    classes: &mut NativeUnitClasses<'a>,
    tasks: &mut Vec<NativeCompileTask<'a>>,
    task_bindings: &mut NativeTaskBindings,
) {
    const LABEL: &str = "eval_comb_apply_ff";
    let mut comb_tasks = HashMap::<(Option<usize>, Vec<usize>), usize>::default();
    for addr in sir.sir.eval_apply_ffs.keys() {
        // A shared tick calls the comb and FF functions compiled anyway.
        let (unit_refs, first_ff_unit) = match sir.sir.comb_apply(addr) {
            Some(celox_sir::CombApply::Fused(fused_units)) => {
                (fused_units.iter().collect::<Vec<_>>(), None)
            }
            Some(celox_sir::CombApply::Merged { comb, ff }) => {
                let mut unit_refs = comb.iter().collect::<Vec<_>>();
                let first_ff_unit =
                    (!unit_refs.is_empty() && !ff.is_empty()).then_some(unit_refs.len());
                unit_refs.extend(ff);
                (unit_refs, first_ff_unit)
            }
            Some(celox_sir::CombApply::Shared) | None => continue,
        };
        let binding = format!("{LABEL} trigger={}", sir.get_path(addr));
        let key = (first_ff_unit, classes.classes(&unit_refs));
        let index = if let Some(&index) = comb_tasks.get(&key) {
            tasks[index].bindings.push(binding);
            index
        } else {
            let index = tasks.len();
            tasks.push(NativeCompileTask {
                units: unit_refs,
                label: LABEL,
                merge_kind: NativeMergeKind::FusedCombAndFf,
                first_ff_unit,
                bindings: vec![binding],
            });
            comb_tasks.insert(key, index);
            index
        };
        task_bindings.insert((LABEL, *addr), index);
    }
}

pub(super) fn collect_ff_compile_tasks_from<'a>(
    sir: &LaidOutProgram,
    ff_map: &'a HashMap<
        AbsoluteAddr,
        Vec<crate::ir::ExecutionUnit<crate::ir::RegionedAbsoluteAddr>>,
    >,
    label: &'static str,
    classes: &mut NativeUnitClasses<'a>,
    ff_tasks: &mut HashMap<Vec<usize>, usize>,
    tasks: &mut Vec<NativeCompileTask<'a>>,
    task_bindings: &mut NativeTaskBindings,
) {
    for (addr, units) in ff_map {
        let unit_refs = units.iter().collect::<Vec<_>>();
        let binding = format!("{label} trigger={}", sir.get_path(addr));
        let key = classes.classes(&unit_refs);
        let index = if let Some(&index) = ff_tasks.get(&key) {
            tasks[index].bindings.push(binding);
            index
        } else {
            let index = tasks.len();
            tasks.push(NativeCompileTask {
                units: unit_refs,
                label,
                merge_kind: NativeMergeKind::Complete,
                first_ff_unit: None,
                bindings: vec![binding],
            });
            ff_tasks.insert(key, index);
            index
        };
        task_bindings.insert((label, *addr), index);
    }
}
