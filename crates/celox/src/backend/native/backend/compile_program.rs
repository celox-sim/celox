//! Compiling a whole program to native code.

use super::*;

pub(super) fn compile_program(
    laid_out: &LaidOutProgram,
    options: &SimulatorOptions,
    capture_trace: bool,
    cancel: Option<&CompileCancel>,
) -> Result<(NativeProgramImage, Option<NativeCodegenTrace>), SimulatorError> {
    compile_program_with_outlining_threshold(laid_out, options, capture_trace, cancel, 32_768)
}

pub(super) fn compile_program_with_outlining_threshold(
    laid_out: &LaidOutProgram,
    options: &SimulatorOptions,
    capture_trace: bool,
    cancel: Option<&CompileCancel>,
    outline_threshold: usize,
) -> Result<(NativeProgramImage, Option<NativeCodegenTrace>), SimulatorError> {
    laid_out.reject_unsupported_callee_stores("native", true)?;
    let sir = laid_out;
    let layout = laid_out.layout();
    // Function units take the extern address table slots after the DPI
    // imports, before the generated kernels. Every unit may call them, so
    // every compile sees their frames; a program without them compiles with
    // the options it was given.
    let extern_count = sir.runtime().runtime_schema.extern_functions.len();
    let function_options;
    let options = if sir.sir.functions.is_empty() {
        options
    } else {
        let mut with_functions = options.clone();
        with_functions.x86_options.functions =
            Some(std::sync::Arc::new(crate::backend::NativeFunctions {
                frames: sir
                    .sir
                    .functions
                    .iter()
                    .map(|function| function.frame.clone())
                    .collect(),
                first_slot: u32::try_from(extern_count)
                    .map_err(|_| codegen_message("too many native extern functions"))?,
            }));
        function_options = with_functions;
        &function_options
    };
    let kernel_slots = extern_count + sir.sir.functions.len();
    let function_jits = compile_function_units(sir, layout, options, cancel)?;
    let force_bindings =
        build_force_bindings(sir.runtime(), layout, &options.signal_access.force_signals)?;
    let (compile_tasks, task_bindings) = collect_ff_compile_tasks(sir);
    let comb_blocks = sir
        .sir
        .eval_comb
        .iter()
        .map(|unit| unit.blocks.len())
        .sum::<usize>();
    let size = |units: &[&ExecutionUnit<RegionedAbsoluteAddr>]| {
        (
            units.iter().map(|unit| unit.blocks.len()).sum::<usize>(),
            units
                .iter()
                .flat_map(|unit| unit.blocks.values())
                .map(|block| block.instructions.len())
                .sum::<usize>(),
        )
    };
    let comb_size = size(&sir.sir.eval_comb.iter().collect::<Vec<_>>());
    let task_sizes = compile_tasks
        .iter()
        .map(|task| size(&task.units))
        .collect::<Vec<_>>();
    let max_task_blocks = task_sizes.iter().map(|size| size.0).max().unwrap_or(0);
    let max_task_instructions = task_sizes.iter().map(|size| size.1).max().unwrap_or(0);
    let max_instructions = comb_size.1.max(max_task_instructions);
    let large = |size: (usize, usize)| size.0 >= 65_536 || size.1 >= 131_072;
    let release_pages = || {
        // Measure usable capacity after returning unused large-compiler pages.
        // Small single-worker builds must not trim unrelated process heaps.
        #[cfg(all(target_os = "linux", target_env = "gnu"))]
        if large(comb_size) || large((max_task_blocks, max_task_instructions)) {
            unsafe {
                libc::malloc_trim(0);
            }
        }
    };
    // Outlining amortizes the dominant circuit across all derived edges.
    // Keep small circuits inline, where cross-boundary optimization is cheap.
    let comb_instructions = sir
        .sir
        .eval_comb
        .iter()
        .flat_map(|unit| unit.blocks.values())
        .map(|block| block.instructions.len())
        .sum::<usize>();
    let outline_comb = cfg!(any(
        feature = "x86_64-codegen",
        all(target_arch = "x86_64", not(feature = "arm64-codegen"))
    )) && !sir.deferred_derived_edges.is_empty()
        && comb_instructions >= outline_threshold;
    let outlined_comb = if outline_comb {
        Some(outlined_comb_caller(kernel_slots)?)
    } else {
        None
    };
    let derived_model = (!sir.deferred_derived_edges.is_empty()).then(|| {
        celox_design::schedule::ScheduleModel::derive(&sir.design, &sir.runtime().runtime_schema)
    });
    let process_sizes = expanded_process_sizes(sir, outline_comb, comb_blocks, comb_instructions);
    let max_process_blocks = process_sizes.iter().map(|size| size.0).max().unwrap_or(0);
    let max_process_instructions = process_sizes.iter().map(|size| size.1).max().unwrap_or(0);
    // The resource budget already includes overlapping SIR/MIR copies.
    // Use the expanded size, rather than the tiny reserved process entry.
    let mut process_order = (0..process_sizes.len()).collect::<Vec<_>>();
    process_order.sort_by_key(|&index| std::cmp::Reverse(process_sizes[index]));
    // Without an outlined comb the process units depend on no other compiled
    // code: they join the first parallel phase instead of waiting for it.
    let pool_processes = !outline_comb;
    let compile_process = |index: usize, process_options: &crate::backend::X86BackendOptions| {
        let unit = &sir.sir.processes[index];
        if cancelled(cancel) {
            return Err(cancelled_error());
        }
        // Expand, compile, then drop just this edge's SIR. Retaining every
        // expansion makes memory scale with circuit size times edge count.
        let generated = sir
            .deferred_derived_edges
            .get(&index)
            .map(|edge| {
                let model = derived_model.as_ref().expect("deferred edge model");
                if let Some(comb) = &outlined_comb {
                    edge.generate_with_comb(sir, model, comb)
                } else {
                    edge.generate(sir, model)
                }
                .map_err(codegen_message)
            })
            .transpose()?;
        let unit = generated.as_ref().unwrap_or(unit);
        compile_unit_refs(
            &[unit],
            layout,
            options.four_state,
            &format!("process[{index}]"),
            NativeMergeKind::Complete,
            None,
            process_options,
            capture_trace,
            &options.optimize_options.diagnostics,
            cancel,
        )
    };
    release_pages();
    let pooled = if pool_processes {
        process_sizes.len()
    } else {
        0
    };
    let pool_blocks = max_task_blocks.max(if pool_processes {
        max_process_blocks
    } else {
        0
    });
    let pool_instructions = max_task_instructions.max(if pool_processes {
        max_process_instructions
    } else {
        0
    });
    let workers = super::super::compile_resources::workers_with_primary(
        comb_size,
        (pool_blocks, pool_instructions),
        compile_tasks.len() + pooled,
    );
    let serial = workers == 1;
    // Start expensive functions early so the final fused function does not
    // sit behind short FF entries. Stable task identities preserve image order.
    // Pool entries at and past `ff_count` are process units.
    let ff_count = compile_tasks.len();
    let mut task_order = (0..ff_count + pooled).collect::<Vec<_>>();
    task_order.sort_by_key(|&id| {
        std::cmp::Reverse(if id < ff_count {
            compile_tasks[id]
                .units
                .iter()
                .map(|unit| unit.blocks.len())
                .sum::<usize>()
        } else {
            process_sizes[id - ff_count].0
        })
    });
    if options.x86_options.diagnostics.phase_timing {
        tracing::debug!(
            "[native-timing] compile_program serial={serial} workers={workers} comb_blocks={comb_blocks} max_task_blocks={max_task_blocks} max_instructions={max_instructions} ff_tasks={} settle_partitions={} settle_check={}",
            compile_tasks.len(),
            sir.runtime()
                .runtime_schema
                .settle_partitions
                .as_ref()
                .map_or(0, |s| s.partitions),
            layout.settle_check(),
        );
    }
    // Execution units already communicate through state. Keep large units
    // separate instead of rebuilding a single allocator problem, and dispatch
    // them through the same bounded pool as the FF functions.
    let split_comb = cfg!(any(
        feature = "x86_64-codegen",
        all(target_arch = "x86_64", not(feature = "arm64-codegen"))
    )) && !capture_trace
        && sir.sir.eval_comb.len() > 1
        && comb_size.1 >= outline_threshold;
    let mut comb_chunk_jits = Vec::new();
    let next_task = AtomicUsize::new(0);
    let mut pooled_process_jits = None;
    let (comb_jit, compiled_ff_codes) = if split_comb {
        compile_split_comb(
            sir,
            layout,
            options,
            cancel,
            &compile_tasks,
            &task_sizes,
            &size,
            kernel_slots,
            &mut comb_chunk_jits,
        )?
    } else if serial {
        let comb_jit = compile_units(
            &sir.sir.eval_comb,
            layout,
            options.four_state,
            "eval_comb",
            &options.x86_options,
            capture_trace,
            &options.optimize_options.diagnostics,
            cancel,
        )?;
        let mut compiled = HashMap::default();
        let mut previous_large = large(comb_size);
        for (task_id, task) in compile_tasks.iter().enumerate() {
            if previous_large {
                release_pages();
            }
            previous_large = large(task_sizes[task_id]);
            let code = compile_unit_refs(
                &task.units,
                layout,
                options.four_state,
                task.label,
                task.merge_kind,
                task.first_ff_unit,
                &options.x86_options,
                capture_trace,
                &options.optimize_options.diagnostics,
                cancel,
            )?;
            compiled.insert(task_id, code);
        }
        (comb_jit, compiled)
    } else {
        compile_comb_and_tasks_threaded(
            sir,
            layout,
            options,
            capture_trace,
            cancel,
            &compile_tasks,
            &task_order,
            workers,
            ff_count,
            pooled,
            &compile_process,
            &next_task,
            &mut pooled_process_jits,
        )?
    };
    release_pages();
    // A foreign-interface image can request per-unit entries so force/release
    // can reapply overrides between procedural store boundaries. Ordinary
    // images do not compile or retain this duplicate combinational code.
    let comb_unit_jits = compile_force_comb_units(sir, layout, options, cancel)?;
    // Return completed codegen's allocator pages before budgeting the process
    // workers. Small designs leave unrelated process heaps alone.
    release_pages();
    let process_jits = match pooled_process_jits {
        Some(jits) => jits,
        None => compile_remaining_processes(
            options,
            outline_comb,
            &comb_jit,
            max_process_blocks,
            max_process_instructions,
            &process_sizes,
            &process_order,
            &compile_process,
        )?,
    };
    let lane_kernels = super::super::lanes::compile_lane_kernels(sir, options, cancel)?;
    let clock_alias_jits = compile_clock_aliases(sir, layout, options, cancel, &force_bindings)?;
    assemble_program_image(
        sir,
        layout,
        options,
        capture_trace,
        comb_jit,
        compiled_ff_codes,
        compile_tasks,
        task_bindings,
        force_bindings,
        function_jits,
        comb_chunk_jits,
        comb_unit_jits,
        process_jits,
        clock_alias_jits,
        lane_kernels,
        outline_comb,
        split_comb,
    )
}

/// Compile the function units of a program, in the order of their slots.
fn compile_function_units(
    sir: &LaidOutProgram,
    layout: &MemoryLayout,
    options: &SimulatorOptions,
    cancel: Option<&CompileCancel>,
) -> Result<Vec<CompiledNativeFunction>, SimulatorError> {
    if sir.sir.functions.is_empty() {
        return Ok(Vec::new());
    }
    let functions = &sir.sir.functions;
    let sizes = functions
        .iter()
        .map(|function| {
            (
                function.unit.blocks.len(),
                function
                    .unit
                    .blocks
                    .values()
                    .map(|block| block.instructions.len())
                    .sum::<usize>(),
            )
        })
        .collect::<Vec<_>>();
    let max_blocks = sizes.iter().map(|size| size.0).max().unwrap_or(0);
    let max_instructions = sizes.iter().map(|size| size.1).max().unwrap_or(0);
    // Each worker compiles with a copy of the layout (below).
    let workers = super::super::compile_resources::workers_with_state(
        max_blocks,
        max_instructions,
        functions.len(),
        super::super::compile_resources::layout_bytes(layout),
    );
    if options.x86_options.diagnostics.phase_timing {
        tracing::debug!(
            "[native-timing] function_compile workers={workers} units={} max_blocks={max_blocks} max_instructions={max_instructions}",
            functions.len()
        );
    }
    // The costliest first, so that the last functions a worker takes are
    // small, by the cost the worker count is sized by.
    let mut order = (0..functions.len()).collect::<Vec<_>>();
    order.sort_by_key(|&index| {
        let (blocks, instructions) = sizes[index];
        std::cmp::Reverse(super::super::compile_resources::compile_cost(
            blocks,
            instructions,
        ))
    });
    // One layout serves every function a worker compiles in turn: each places
    // the objects of its frame slots in it while it is compiled. A copy per
    // function would copy the whole state's layout once per function.
    super::super::compile_resources::compile_tasks_with(
        &order,
        workers,
        || layout.clone(),
        |function_layout, index| {
            let mut function_options = options.x86_options.clone();
            function_options.function = Some(celox_sir::FunctionId(
                u32::try_from(index).map_err(|_| codegen_message("too many function units"))?,
            ));
            let frame = &function_options
                .functions
                .as_deref()
                .ok_or_else(|| codegen_message("function units without their frames"))?
                .frames[index];
            let placed =
                emit::place_frame_slots(function_layout, frame).map_err(codegen_message)?;
            let compiled = compile_unit_refs(
                &[&functions[index].unit],
                function_layout,
                options.four_state,
                &format!("function[{index}]"),
                NativeMergeKind::Complete,
                None,
                &function_options,
                false,
                &options.optimize_options.diagnostics,
                cancel,
            );
            emit::remove_frame_slots(function_layout, &placed);
            compiled
        },
    )
    .map_err(|_| codegen_message("native function compile thread panicked"))?
}

/// The unit a derived edge inlines instead of combinational logic outlined
/// into a kernel of its own: a call of that kernel.
fn outlined_comb_caller(
    kernel_slots: usize,
) -> Result<[ExecutionUnit<RegionedAbsoluteAddr>; 1], SimulatorError> {
    let slot = u32::try_from(kernel_slots)
        .ok()
        .filter(|&slot| slot < 0x1000_0000)
        .ok_or_else(|| codegen_message("too many native extern functions"))?;
    let mut builder = celox_sir::SIRBuilder::new();
    builder.emit(SIRInstruction::ExternCall {
        dst: None,
        func: celox_sir::extern_abi::NATIVE_KERNEL_CALL_TAG | slot,
        args: Vec::new(),
    });
    builder.seal_block(SIRTerminator::Return);
    let (blocks, register_map, _) = builder.drain();
    Ok([ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    }])
}

/// Blocks and instructions of each process unit as it is compiled: a
/// deferred derived edge counts expanded.
fn expanded_process_sizes(
    sir: &LaidOutProgram,
    outline_comb: bool,
    comb_blocks: usize,
    comb_instructions: usize,
) -> Vec<(usize, usize)> {
    sir.sir
        .processes
        .iter()
        .enumerate()
        .map(|(index, unit)| {
            sir.deferred_derived_edges.get(&index).map_or_else(
                || {
                    (
                        unit.blocks.len(),
                        unit.blocks
                            .values()
                            .map(|block| block.instructions.len())
                            .sum(),
                    )
                },
                |edge| {
                    if outline_comb {
                        // The recipe estimate includes two complete comb
                        // copies. Both now use a single call; its fixed CFG
                        // allowance already covers those call instructions.
                        (
                            edge.blocks.saturating_sub(comb_blocks.saturating_mul(2)),
                            edge.instructions
                                .saturating_sub(comb_instructions.saturating_mul(2)),
                        )
                    } else {
                        (edge.blocks, edge.instructions)
                    }
                },
            )
        })
        .collect::<Vec<_>>()
}

/// Compile each combinational unit as a kernel of its own, on one pool with
/// the FF tasks, and the `eval_comb` that calls them. Returns that caller
/// and the FF functions by task; the kernels go to `comb_chunk_jits`.
fn compile_split_comb(
    sir: &LaidOutProgram,
    layout: &MemoryLayout,
    options: &SimulatorOptions,
    cancel: Option<&CompileCancel>,
    compile_tasks: &[NativeCompileTask<'_>],
    task_sizes: &[(usize, usize)],
    size: &impl Fn(&[&ExecutionUnit<RegionedAbsoluteAddr>]) -> (usize, usize),
    kernel_slots: usize,
    comb_chunk_jits: &mut Vec<CompiledNativeFunction>,
) -> Result<
    (
        CompiledNativeFunction,
        HashMap<usize, CompiledNativeFunction>,
    ),
    SimulatorError,
> {
    let ff_count = compile_tasks.len();
    let sizes = task_sizes
        .iter()
        .copied()
        .chain(sir.sir.eval_comb.iter().map(|unit| size(&[unit])))
        .collect::<Vec<_>>();
    let mut order = (0..sizes.len()).collect::<Vec<_>>();
    order.sort_by_key(|&index| std::cmp::Reverse(sizes[index]));
    let workers = super::super::compile_resources::workers(
        sizes.iter().map(|s| s.0).max().unwrap_or(0),
        sizes.iter().map(|s| s.1).max().unwrap_or(0),
        sizes.len(),
    );
    if options.x86_options.diagnostics.phase_timing {
        tracing::debug!(
            "[native-timing] comb_chunks={} workers={workers}",
            sir.sir.eval_comb.len()
        );
    }
    let codes = super::super::compile_resources::compile_tasks(&order, workers, |index| {
        if cancelled(cancel) {
            return Err(cancelled_error());
        }
        if index < ff_count {
            let task = &compile_tasks[index];
            compile_unit_refs(
                &task.units,
                layout,
                options.four_state,
                task.label,
                task.merge_kind,
                task.first_ff_unit,
                &options.x86_options,
                false,
                &options.optimize_options.diagnostics,
                cancel,
            )
        } else {
            compile_unit_refs(
                &[&sir.sir.eval_comb[index - ff_count]],
                layout,
                options.four_state,
                "eval_comb_chunk",
                NativeMergeKind::Partial,
                None,
                &options.x86_options,
                false,
                &options.optimize_options.diagnostics,
                cancel,
            )
        }
    })
    .map_err(|_| codegen_message("native compile worker panicked"))??;
    let mut compiled = HashMap::default();
    for (index, code) in codes.into_iter().enumerate() {
        if index < ff_count {
            compiled.insert(index, code);
        } else {
            comb_chunk_jits.push(code);
        }
    }
    let first_slot = kernel_slots + 1;
    let mut builder = celox_sir::SIRBuilder::new();
    for index in 0..comb_chunk_jits.len() {
        let slot = u32::try_from(first_slot + index)
            .ok()
            .filter(|&slot| slot < 0x1000_0000)
            .ok_or_else(|| codegen_message("too many native extern functions"))?;
        builder.emit(SIRInstruction::ExternCall {
            dst: None,
            func: celox_sir::extern_abi::NATIVE_KERNEL_CALL_TAG | slot,
            args: Vec::new(),
        });
    }
    builder.seal_block(SIRTerminator::Return);
    let caller_options = options
        .x86_options
        .clone()
        .with_native_callees(comb_chunk_jits.iter().map(|code| code.required_state_size));
    let (blocks, register_map, _) = builder.drain();
    let comb = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    };
    let root = compile_unit_refs(
        &[&comb],
        layout,
        options.four_state,
        "eval_comb",
        NativeMergeKind::Complete,
        None,
        &caller_options,
        false,
        &options.optimize_options.diagnostics,
        cancel,
    )?;
    Ok((root, compiled))
}

/// Compile `eval_comb` on a thread of its own while a pool compiles the FF
/// tasks and, when they are pooled, the process units, which then go to
/// `pooled_process_jits`. Returns `eval_comb` and the FF functions by task.
fn compile_comb_and_tasks_threaded(
    sir: &LaidOutProgram,
    layout: &MemoryLayout,
    options: &SimulatorOptions,
    capture_trace: bool,
    cancel: Option<&CompileCancel>,
    compile_tasks: &[NativeCompileTask<'_>],
    task_order: &[usize],
    workers: usize,
    ff_count: usize,
    pooled: usize,
    compile_process: &(
         impl Fn(
        usize,
        &crate::backend::X86BackendOptions,
    ) -> Result<CompiledNativeFunction, SimulatorError>
         + Sync
     ),
    next_task: &AtomicUsize,
    pooled_process_jits: &mut Option<Vec<CompiledNativeFunction>>,
) -> Result<
    (
        CompiledNativeFunction,
        HashMap<usize, CompiledNativeFunction>,
    ),
    SimulatorError,
> {
    std::thread::scope(|scope| {
        let four_state = options.four_state;
        let x86_options = &options.x86_options;
        let comb_handle = scope.spawn(move || {
            if cancelled(cancel) {
                return Err(cancelled_error());
            }
            compile_units(
                &sir.sir.eval_comb,
                layout,
                four_state,
                "eval_comb",
                x86_options,
                capture_trace,
                &options.optimize_options.diagnostics,
                cancel,
            )
        });
        let task_worker_count = task_order.len().min(workers.saturating_sub(1));
        let task_handles = (0..task_worker_count)
            .map(|_| {
                let next_task = &next_task;
                let compile_tasks = &compile_tasks;
                let task_order = &task_order;
                let compile_process = &compile_process;
                scope.spawn(move || {
                    let mut compiled = Vec::new();
                    loop {
                        if cancelled(cancel) {
                            return Err(cancelled_error());
                        }
                        let index = next_task.fetch_add(1, Ordering::Relaxed);
                        let Some(&task_id) = task_order.get(index) else {
                            break;
                        };
                        if task_id >= ff_count {
                            let code = compile_process(task_id - ff_count, x86_options)?;
                            compiled.push((task_id, code));
                            continue;
                        }
                        let task = &compile_tasks[task_id];
                        let code = compile_unit_refs(
                            &task.units,
                            layout,
                            four_state,
                            task.label,
                            task.merge_kind,
                            task.first_ff_unit,
                            x86_options,
                            capture_trace,
                            &options.optimize_options.diagnostics,
                            cancel,
                        )?;
                        compiled.push((task_id, code));
                    }
                    Ok::<_, SimulatorError>(compiled)
                })
            })
            .collect::<Vec<_>>();

        let comb_jit = comb_handle
            .join()
            .map_err(|_| codegen_message("native eval_comb compile thread panicked"))??;
        let mut compiled_ff_codes = HashMap::default();
        let mut process_codes = Vec::new();
        for handle in task_handles {
            let compiled = handle
                .join()
                .map_err(|_| codegen_message("native FF compile thread panicked"))??;
            for (task_id, code) in compiled {
                if task_id < ff_count {
                    compiled_ff_codes.insert(task_id, code);
                } else {
                    process_codes.push((task_id - ff_count, code));
                }
            }
        }
        if pooled > 0 {
            process_codes.sort_unstable_by_key(|(index, _)| *index);
            *pooled_process_jits = Some(
                process_codes
                    .into_iter()
                    .map(|(_, code)| code)
                    .collect::<Vec<_>>(),
            );
        }
        Ok::<_, SimulatorError>((comb_jit, compiled_ff_codes))
    })
}

/// Compile the per-unit combinational entries a force-capable image
/// reapplies overrides between; an ordinary image has none.
fn compile_force_comb_units(
    sir: &LaidOutProgram,
    layout: &MemoryLayout,
    options: &SimulatorOptions,
    cancel: Option<&CompileCancel>,
) -> Result<Vec<CompiledNativeFunction>, SimulatorError> {
    let force_store_boundaries =
        options.native_force_support && options.optimize_options.opt_level() == crate::OptLevel::O0;
    let comb_runtime_units = if options.native_force_support {
        sir.sir
            .eval_comb
            .iter()
            .flat_map(|unit| {
                if force_store_boundaries {
                    split_comb_execution_unit(unit)
                } else {
                    vec![unit.clone()]
                }
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    comb_runtime_units
        .iter()
        .enumerate()
        .map(|(index, unit)| {
            if cancelled(cancel) {
                return Err(cancelled_error());
            }
            compile_unit_refs(
                &[unit],
                layout,
                options.four_state,
                &format!("eval_comb_unit[{index}]"),
                NativeMergeKind::Complete,
                None,
                &options.x86_options,
                false,
                &options.optimize_options.diagnostics,
                cancel,
            )
        })
        .collect::<Result<Vec<_>, _>>()
}

/// Compile the process units the first pool did not: with an outlined
/// `eval_comb` they call it, so they wait for `comb_jit`.
fn compile_remaining_processes(
    options: &SimulatorOptions,
    outline_comb: bool,
    comb_jit: &CompiledNativeFunction,
    max_process_blocks: usize,
    max_process_instructions: usize,
    process_sizes: &[(usize, usize)],
    process_order: &[usize],
    compile_process: &(
         impl Fn(
        usize,
        &crate::backend::X86BackendOptions,
    ) -> Result<CompiledNativeFunction, SimulatorError>
         + Sync
     ),
) -> Result<Vec<CompiledNativeFunction>, SimulatorError> {
    let mut process_options = options.x86_options.clone();
    if outline_comb {
        // Nested functions must not overwrite the caller's spills, saved
        // registers, scratch values, or extern call area.
        process_options = process_options.with_native_callees([comb_jit.required_state_size]);
    }
    let process_workers = super::super::compile_resources::workers(
        max_process_blocks,
        max_process_instructions,
        process_sizes.len(),
    );
    if options.x86_options.diagnostics.phase_timing {
        tracing::debug!(
            "[native-timing] process_compile workers={process_workers} units={} max_blocks={max_process_blocks} max_instructions={max_process_instructions}",
            process_sizes.len()
        );
    }
    super::super::compile_resources::compile_tasks(process_order, process_workers, |index| {
        compile_process(index, &process_options)
    })
    .map_err(|_| codegen_message("native process compile thread panicked"))?
}

/// Compile the clock alias kernels of a two-state program without forces.
fn compile_clock_aliases(
    sir: &LaidOutProgram,
    layout: &MemoryLayout,
    options: &SimulatorOptions,
    cancel: Option<&CompileCancel>,
    force_bindings: &[NativeForceBinding],
) -> Result<Vec<(String, CompiledNativeFunction)>, SimulatorError> {
    let mut clock_alias_jits = Vec::new();
    if !options.four_state && !options.native_force_support && force_bindings.is_empty() {
        let (_, clocks) = crate::backend::event_ids(sir);
        for (id, clock) in clocks.into_iter().enumerate() {
            if let Some(units) = crate::clock_aliases::extract(sir, clock) {
                let name = format!("{}{}]", crate::clock_aliases::EXPORT_PREFIX, id);
                let compiled = compile_units(
                    &units,
                    layout,
                    false,
                    &name,
                    &options.x86_options,
                    false,
                    &options.optimize_options.diagnostics,
                    cancel,
                )?;
                clock_alias_jits.push((name, compiled));
            }
        }
    }
    Ok(clock_alias_jits)
}

/// Pack every compiled function into one image and bind the events, the
/// kernels and the runtime schema of the program to their offsets in it.
fn assemble_program_image(
    sir: &LaidOutProgram,
    layout: &MemoryLayout,
    options: &SimulatorOptions,
    capture_trace: bool,
    comb_jit: CompiledNativeFunction,
    compiled_ff_codes: HashMap<usize, CompiledNativeFunction>,
    compile_tasks: Vec<NativeCompileTask<'_>>,
    task_bindings: NativeTaskBindings,
    force_bindings: Vec<NativeForceBinding>,
    function_jits: Vec<CompiledNativeFunction>,
    comb_chunk_jits: Vec<CompiledNativeFunction>,
    comb_unit_jits: Vec<CompiledNativeFunction>,
    process_jits: Vec<CompiledNativeFunction>,
    clock_alias_jits: Vec<(String, CompiledNativeFunction)>,
    lane_kernels: Option<super::super::lanes::CompiledLaneKernels>,
    outline_comb: bool,
    split_comb: bool,
) -> Result<(NativeProgramImage, Option<NativeCodegenTrace>), SimulatorError> {
    let codegen_trace = capture_trace.then(|| {
        format_native_codegen_trace(&comb_jit, &compiled_ff_codes, &compile_tasks, &process_jits)
    });
    let semantic_memory_size = layout
        .merged_total_size
        .checked_add(layout.triggered_bits_total_size)
        .expect("native semantic-memory size overflow");
    let native_memory_size = std::iter::once(comb_jit.required_state_size)
        .chain(function_jits.iter().map(|code| code.required_state_size))
        .chain(
            clock_alias_jits
                .iter()
                .map(|(_, code)| code.required_state_size),
        )
        .chain(comb_chunk_jits.iter().map(|code| code.required_state_size))
        .chain(
            compiled_ff_codes
                .values()
                .map(|compiled| compiled.required_state_size),
        )
        .chain(
            comb_unit_jits
                .iter()
                .chain(&process_jits)
                .map(|compiled| compiled.required_state_size),
        )
        .chain(
            lane_kernels
                .iter()
                .flat_map(|kernels| kernels.functions())
                .map(|compiled| compiled.required_state_size),
        )
        .fold(semantic_memory_size, usize::max);
    let required_native_features = std::iter::once(comb_jit.required_native_features)
        .chain(
            function_jits
                .iter()
                .map(|code| code.required_native_features),
        )
        .chain(
            clock_alias_jits
                .iter()
                .map(|(_, code)| code.required_native_features),
        )
        .chain(
            comb_chunk_jits
                .iter()
                .map(|code| code.required_native_features),
        )
        .chain(
            compiled_ff_codes
                .values()
                .map(|compiled| compiled.required_native_features),
        )
        .chain(
            comb_unit_jits
                .iter()
                .chain(&process_jits)
                .map(|compiled| compiled.required_native_features),
        )
        .chain(
            lane_kernels
                .iter()
                .flat_map(|kernels| kernels.functions())
                .map(|compiled| compiled.required_native_features),
        )
        .fold(0, |features, required| features | required);
    let mut packed_image = Vec::new();
    let mut code_entries = Vec::with_capacity(1 + compiled_ff_codes.len());
    let mut image_symbols = Vec::new();
    let comb_offset = append_native_code(
        &mut packed_image,
        &mut code_entries,
        &mut image_symbols,
        "eval_comb".into(),
        &comb_jit,
    )?;
    // The function units' slots come first, then the generated kernels'.
    let mut native_kernel_offsets = Vec::with_capacity(function_jits.len() + 1);
    for (index, code) in function_jits.iter().enumerate() {
        native_kernel_offsets.push(append_native_code(
            &mut packed_image,
            &mut code_entries,
            &mut image_symbols,
            format!("function[{index}]"),
            code,
        )?);
    }
    if outline_comb || split_comb {
        native_kernel_offsets.push(comb_offset);
    }
    for (index, code) in comb_chunk_jits.iter().enumerate() {
        native_kernel_offsets.push(append_native_code(
            &mut packed_image,
            &mut code_entries,
            &mut image_symbols,
            format!("eval_comb_chunk[{index}]"),
            code,
        )?);
    }
    let mut comb_unit_offsets = Vec::with_capacity(comb_unit_jits.len());
    for (index, compiled) in comb_unit_jits.iter().enumerate() {
        comb_unit_offsets.push(append_native_code(
            &mut packed_image,
            &mut code_entries,
            &mut image_symbols,
            format!("eval_comb_unit[{index}]"),
            compiled,
        )?);
    }
    let mut process_offsets = Vec::with_capacity(process_jits.len());
    for (index, compiled) in process_jits.iter().enumerate() {
        process_offsets.push(append_native_code(
            &mut packed_image,
            &mut code_entries,
            &mut image_symbols,
            format!("process[{index}]"),
            compiled,
        )?);
    }
    let mut compiled_ff_keys = compiled_ff_codes.keys().copied().collect::<Vec<_>>();
    compiled_ff_keys.sort_unstable();
    let mut task_offsets = HashMap::default();
    let mut label_indices = HashMap::<&str, usize>::default();
    for &task_id in &compiled_ff_keys {
        let task = &compile_tasks[task_id];
        let index = label_indices.entry(task.label).or_default();
        let name = format!("{}[{index}]", task.label);
        *index += 1;
        let offset = append_native_code(
            &mut packed_image,
            &mut code_entries,
            &mut image_symbols,
            name,
            &compiled_ff_codes[&task_id],
        )?;
        task_offsets.insert(task_id, offset);
    }
    for (name, compiled) in &clock_alias_jits {
        append_native_code(
            &mut packed_image,
            &mut code_entries,
            &mut image_symbols,
            name.clone(),
            compiled,
        )?;
    }
    let lane_image = lane_kernels
        .map(|kernels| kernels.pack(&mut packed_image, &mut code_entries, &mut image_symbols))
        .transpose()?;
    let NativeEventBindings {
        event_map,
        eval_only_event_map,
        apply_event_map,
        id_to_addr,
        id_to_event,
    } = bind_native_events(sir, &task_bindings, &task_offsets)?;
    let four_state_inits = four_state_init_regions(layout, options);

    Ok((
        NativeProgramImage {
            code: packed_image.into(),
            code_entries,
            symbols: image_symbols,
            comb_offset,
            comb_unit_offsets,
            process_offsets,
            required_native_features,
            event_map,
            eval_only_event_map,
            apply_event_map,
            id_to_addr,
            id_to_event,
            reflection: sir.runtime().build_design_reflection(layout),
            design: sir.runtime().design.clone(),
            testbench: sir.runtime().testbench.clone(),
            runtime_schema: native_runtime_schema(sir, force_bindings, native_kernel_offsets),
            layout: layout.clone(),
            native_memory_size,
            options: NativeRuntimeOptions {
                four_state: options.four_state,
                native_tick_loop: options.x86_options.native_tick_loop,
                native_force_support: options.native_force_support,
                perf_map: options.x86_options.diagnostics.perf_map,
                kernel_selection: crate::backend::lanes::kernel_selection(options),
            },
            four_state_inits,
            lanes: lane_image,
        },
        codegen_trace,
    ))
}

/// Semantic event identities bound to image-relative function offsets.
struct NativeEventBindings {
    event_map: HashMap<AbsoluteAddr, NativeEventImageRef>,
    eval_only_event_map: HashMap<AbsoluteAddr, NativeEventImageRef>,
    apply_event_map: HashMap<AbsoluteAddr, NativeEventImageRef>,
    id_to_addr: Vec<AbsoluteAddr>,
    id_to_event: Vec<NativeEventImageRef>,
}

fn bind_native_events(
    sir: &LaidOutProgram,
    task_bindings: &NativeTaskBindings,
    task_offsets: &HashMap<usize, usize>,
) -> Result<NativeEventBindings, SimulatorError> {
    // Bind semantic event identities to image-relative function offsets. The
    // precompiled runtime turns these into process-local pointers after it has
    // copied the image into executable memory.
    let (mut addr_to_id, mut id_to_addr) = crate::backend::event_ids(sir);
    let mut next_id = id_to_addr.len();
    let mut id_to_event = Vec::new();
    let mut event_map = HashMap::default();
    let mut eval_only_event_map = HashMap::default();
    let mut apply_event_map = HashMap::default();
    let compile_ff_group = |ff_map: &HashMap<
        AbsoluteAddr,
        Vec<crate::ir::ExecutionUnit<crate::ir::RegionedAbsoluteAddr>>,
    >,
                            label: &'static str,
                            event_map_out: &mut HashMap<AbsoluteAddr, NativeEventImageRef>,
                            addr_to_id: &mut HashMap<AbsoluteAddr, usize>,
                            compiled_ff_cache: &HashMap<usize, usize>,
                            comb_apply_label: Option<&'static str>,
                            next_id: &mut usize,
                            id_to_addr: &mut Vec<AbsoluteAddr>,
                            id_to_event: &mut Vec<NativeEventImageRef>|
     -> Result<(), SimulatorError> {
        for addr in ff_map.keys() {
            let canonical = sir.design.events.canonical(*addr);
            if let Some(&event) = event_map_out.get(&canonical) {
                event_map_out.insert(*addr, event);
                continue;
            }

            let task_id = task_bindings[&(label, *addr)];
            let func_offset = compiled_ff_cache[&task_id];
            let comb_apply_offset = comb_apply_label.and_then(|label| {
                let task_id = task_bindings.get(&(label, *addr))?;
                Some(compiled_ff_cache[task_id])
            });

            let (id, is_new_id) = if let Some(&id) = addr_to_id.get(&canonical) {
                (id, false)
            } else {
                let id = *next_id;
                *next_id += 1;
                addr_to_id.insert(canonical, id);
                id_to_addr.push(canonical);
                (id, true)
            };

            let event = NativeEventImageRef {
                func_offset,
                comb_apply_offset,
                addr: canonical,
                id,
            };
            event_map_out.insert(canonical, event);
            if *addr != canonical {
                event_map_out.insert(*addr, event);
            }
            if is_new_id {
                id_to_event.push(event);
            }
        }
        Ok(())
    };

    compile_ff_group(
        &sir.sir.eval_apply_ffs,
        "eval_apply_ff",
        &mut event_map,
        &mut addr_to_id,
        task_offsets,
        Some("eval_comb_apply_ff"),
        &mut next_id,
        &mut id_to_addr,
        &mut id_to_event,
    )?;
    compile_ff_group(
        &sir.sir.eval_only_ffs,
        "eval_only_ff",
        &mut eval_only_event_map,
        &mut addr_to_id,
        task_offsets,
        None,
        &mut next_id,
        &mut id_to_addr,
        &mut id_to_event,
    )?;
    compile_ff_group(
        &sir.sir.apply_ffs,
        "apply_ff",
        &mut apply_event_map,
        &mut addr_to_id,
        task_offsets,
        None,
        &mut next_id,
        &mut id_to_addr,
        &mut id_to_event,
    )?;
    id_to_event = id_to_addr
        .iter()
        .map(|address| {
            event_map
                .get(address)
                .or_else(|| eval_only_event_map.get(address))
                .or_else(|| apply_event_map.get(address))
                .copied()
                .expect("every scheduled event address resolves to an event")
        })
        .collect();
    Ok(NativeEventBindings {
        event_map,
        eval_only_event_map,
        apply_event_map,
        id_to_addr,
        id_to_event,
    })
}

/// Offset and size of every four-state region to initialize to X.
fn four_state_init_regions(
    layout: &MemoryLayout,
    options: &SimulatorOptions,
) -> Vec<(usize, usize)> {
    // Pre-compute 4-state initialization regions
    let mut four_state_inits = Vec::new();
    if options.four_state {
        for (addr, &offset) in &layout.offsets {
            let is_4state = layout.is_4states.get(addr).copied().unwrap_or(false);
            if is_4state {
                let allocated_size = layout.plane_size(addr);
                four_state_inits.push((offset, allocated_size));
            }
        }
        for (addr, &rel_offset) in &layout.working_offsets {
            let offset = layout.working_base_offset + rel_offset;
            let is_4state = layout.is_4states.get(addr).copied().unwrap_or(false);
            if is_4state {
                let allocated_size = layout.plane_size(addr);
                four_state_inits.push((offset, allocated_size));
            }
        }
    }
    four_state_inits
}

/// The runtime schema of the image: the program's, with the force bindings
/// and the kernel offsets of this compile.
fn native_runtime_schema(
    sir: &LaidOutProgram,
    force_bindings: Vec<NativeForceBinding>,
    native_kernel_offsets: Vec<usize>,
) -> NativeRuntimeSchema {
    NativeRuntimeSchema {
        force_bindings,
        runtime_errors: sir.runtime().runtime_schema.runtime_errors.clone(),
        runtime_event_sites: sir.runtime().runtime_schema.runtime_event_sites.clone(),
        extern_functions: sir.runtime().runtime_schema.extern_functions.clone(),
        native_kernel_offsets,
        comb_observers: sir.runtime().runtime_schema.comb_observers.clone(),
        testbench_read_roots: sir.runtime().runtime_schema.testbench_read_roots.clone(),
        rtl_writes: sir.runtime().runtime_schema.rtl_writes.clone(),
        rtl_accesses: sir.runtime().rtl_accesses.iter().copied().collect(),
        persistent_comb_state: sir
            .runtime()
            .runtime_schema
            .persistent_comb_state
            .iter()
            .copied()
            .collect(),
        comb_writes: sir
            .runtime()
            .runtime_schema
            .comb_writes
            .iter()
            .copied()
            .collect(),
        dead_store_policy: sir.runtime().runtime_schema.dead_store_policy,
        live_signals: sir
            .runtime()
            .runtime_schema
            .live_signals
            .iter()
            .copied()
            .collect(),
        processes: sir.runtime().runtime_schema.processes.clone(),
        process_origins: sir.runtime().runtime_schema.process_origins.clone(),
        process_frames: sir.runtime().runtime_schema.process_frames.clone(),
        process_call_resumes: sir.runtime().runtime_schema.process_call_resumes.clone(),
        function_frames: sir.runtime().runtime_schema.function_frames.clone(),
        watches: sir.runtime().runtime_schema.watches.clone(),
        occurrence_probes: sir.runtime().runtime_schema.occurrence_probes.clone(),
        wait_sites: sir.runtime().runtime_schema.wait_sites.clone(),
        nba_sites: sir.runtime().runtime_schema.nba_sites.clone(),
        host_request_sites: sir.runtime().runtime_schema.host_request_sites.clone(),
        declaration_startup: sir.runtime().runtime_schema.declaration_startup.clone(),
        active_commits: sir.runtime().runtime_schema.active_commits.clone(),
        time_slots: sir.runtime().runtime_schema.time_slots.clone(),
        sample_shadows: sir.runtime().runtime_schema.sample_shadows.clone(),
        initial_copies: sir.runtime().runtime_schema.initial_copies.clone(),
        scheduler: sir.runtime().runtime_schema.scheduler.clone(),
        derived_edges: sir.runtime().runtime_schema.derived_edges.clone(),
    }
}
