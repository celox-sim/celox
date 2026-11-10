//! Relocating the units of every instance into the global design.

use super::*;

pub(super) fn elaborated_scope_name(
    root_name: &str,
    path: &InstancePath,
    expanded: &HashMap<InstancePath, InstanceId>,
    indexed_instances: &HashSet<InstanceId>,
) -> String {
    let mut prefix = Vec::with_capacity(path.0.len());
    let segments = path
        .0
        .iter()
        .map(|(name, index)| {
            prefix.push((name.clone(), *index));
            let indexed = expanded
                .get(&InstancePath(prefix.clone()))
                .is_some_and(|id| indexed_instances.contains(id));
            if indexed {
                format!("{name}[{index}]")
            } else {
                name.clone()
            }
        })
        .collect::<Vec<_>>();
    if segments.is_empty() {
        root_name.to_string()
    } else {
        format!("{root_name}.{}", segments.join("."))
    }
}

pub(super) fn flatten_with_trace(
    module: &SimModule,
    path: &InstancePath,
    instance_ids: &HashMap<InstancePath, InstanceId>,
    global_boundaries: &HashMap<AbsoluteAddr, BTreeSet<usize>>,
    unpacked_element_widths: &HashMap<AbsoluteAddr, usize>,
    arena: &mut SLTNodeArena<AbsoluteAddr>,
    bindings: &PackageBindings,
    trace_opts: &FrontendTraceOptions,
    mut trace: Option<&mut FrontendTrace>,
) -> Result<RelocationModule, celox_slt::SLTNodeFactsError> {
    let flattened = flattening::flatten_module(
        module,
        path,
        instance_ids,
        global_boundaries,
        unpacked_element_widths,
        arena,
        &|instance_id, var_id| bindings.locate(instance_id, var_id),
    )?;

    if let Some(trace) = trace.as_deref_mut()
        && trace_opts.pre_atomized_comb_blocks
    {
        match &mut trace.pre_atomized_comb_blocks {
            Some((blocks, trace_arena)) => {
                blocks.extend(flattened.pre_atomized_comb_blocks);
                *trace_arena = arena.clone();
            }
            slot @ None => *slot = Some((flattened.pre_atomized_comb_blocks, arena.clone())),
        }
    }

    if let Some(trace) = trace
        && trace_opts.atomized_comb_blocks
    {
        match &mut trace.atomized_comb_blocks {
            Some((blocks, trace_arena)) => {
                blocks.extend(flattened.relocation.comb_blocks.iter().cloned());
                *trace_arena = arena.clone();
            }
            slot @ None => {
                *slot = Some((flattened.relocation.comb_blocks.clone(), arena.clone()));
            }
        }
    }

    Ok(flattened.relocation)
}

pub(super) fn remap_for_fold_runtime_event_sites<A: std::hash::Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    start: usize,
    runtime_event_site_map: &HashMap<u32, u32>,
) -> Result<(), ParserError> {
    arena
        .remap_for_fold_effect_sites(start..arena.len(), |site_id, fatal_error_code| {
            Ok(runtime_event_site_map.get(&site_id).map(|&global_site| {
                (
                    global_site,
                    fatal_error_code.map(|_| i64::from(global_site)),
                )
            }))
        })
        .map_err(|error| {
            ParserError::illegal_context(
                "ForFold runtime-event relocation",
                error.to_string(),
                None,
            )
        })
}

pub(super) fn relocate_executation_unit_with_errors<A, B>(
    eu: &ExecutionUnit<A>,
    f: &impl Fn(&A) -> B,
    runtime_error_codes: &HashMap<i64, i64>,
    runtime_event_sites: &HashMap<u32, u32>,
    extern_functions: &HashMap<u32, u32>,
    function_base: u32,
) -> ExecutionUnit<B> {
    ExecutionUnit {
        entry_block_id: eu.entry_block_id,
        blocks: eu
            .blocks
            .iter()
            .map(|(id, block)| {
                (
                    *id,
                    BasicBlock {
                        id: block.id,
                        instructions: block
                            .instructions
                            .iter()
                            .map(|inst| match inst {
                                SIRInstruction::RuntimeEvent { site_id, args } => {
                                    SIRInstruction::RuntimeEvent {
                                        site_id: runtime_event_sites
                                            .get(site_id)
                                            .copied()
                                            .unwrap_or(*site_id),
                                        args: args.clone(),
                                    }
                                }
                                SIRInstruction::CombCaptureEvent {
                                    site_id,
                                    args,
                                    fatal_error_code,
                                    consume_enabled,
                                } => SIRInstruction::CombCaptureEvent {
                                    site_id: runtime_event_sites
                                        .get(site_id)
                                        .copied()
                                        .unwrap_or(*site_id),
                                    args: args.clone(),
                                    fatal_error_code: *fatal_error_code,
                                    consume_enabled: *consume_enabled,
                                },
                                SIRInstruction::ExternCall { dst, func, args } => {
                                    SIRInstruction::ExternCall {
                                        dst: *dst,
                                        // The heap helper is outside every
                                        // module's extern function table.
                                        func: if *func == celox_sir::extern_abi::HEAP_HELPER_ID {
                                            *func
                                        } else {
                                            extern_functions[func]
                                        },
                                        args: args.clone(),
                                    }
                                }
                                // A module's units name its functions by their index
                                // in the module; each instance has its own copies,
                                // from `function_base` on.
                                SIRInstruction::Call { .. } => {
                                    let mut call = inst.map_addr(f);
                                    if let SIRInstruction::Call { function, .. } = &mut call {
                                        function.0 += function_base;
                                    }
                                    call
                                }
                                _ => inst.map_addr(f),
                            })
                            .collect(),
                        params: block.params.clone(),
                        terminator: match block.terminator {
                            SIRTerminator::Error(code) => SIRTerminator::Error(
                                runtime_error_codes.get(&code).copied().unwrap_or(code),
                            ),
                            ref terminator => terminator.clone(),
                        },
                    },
                )
            })
            .collect(),
        register_map: eu.register_map.clone(),
    }
}

pub(super) fn relocate_units(
    bindings: &PackageBindings,
    expanded: &HashMap<InstancePath, InstanceId>,
    instance_modules: &HashMap<InstanceId, ModuleId>,
    modules: &HashMap<ModuleId, SimModule>,
    root_name: &str,
    indexed_instances: &HashSet<InstanceId>,
    global_boundaries: &HashMap<AbsoluteAddr, std::collections::BTreeSet<usize>>,
    unpacked_element_widths: &HashMap<AbsoluteAddr, usize>,
    clock_domains: &HashMap<AbsoluteAddr, AbsoluteAddr>,
    parallel_ff_units: Option<
        &mut HashMap<AbsoluteAddr, Vec<(InstanceId, FfPart<RegionedAbsoluteAddr>)>>,
    >,
    split_commits: &mut SplitCommits,
    trace_opts: &crate::FrontendTraceOptions,
    trace: &mut Option<&mut crate::FrontendTrace>,
    startup_ready: Option<AbsoluteAddr>,
) -> Result<
    (
        SLTNodeArena<AbsoluteAddr>,
        HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
        HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
        HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
        HashMap<AbsoluteAddr, Vec<FfAccessSummary<RegionedAbsoluteAddr>>>,
        HashMap<InstanceId, FfRuntimeRelocation>,
        Vec<celox_slt::LogicPath<AbsoluteAddr>>,
        Vec<CombObserver<AbsoluteAddr>>,
        HashMap<i64, RuntimeErrorInfo<AbsoluteAddr>>,
        Vec<RuntimeEventSite>,
        Vec<ExternFunction>,
        i64,
        Vec<RelocatedProcess>,
        (
            Vec<celox_sir::FunctionUnit<RegionedAbsoluteAddr>>,
            Vec<celox_design::FunctionFrame>,
        ),
    ),
    ParserError,
> {
    let mut global_arena = SLTNodeArena::<AbsoluteAddr>::new();
    let mut processes = Vec::new();
    let mut eval_apply_ffs: HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>> =
        HashMap::default();
    let mut eval_only_ffs: HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>> =
        HashMap::default();
    let mut apply_ffs: HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>> =
        HashMap::default();
    let mut ff_access_summaries: HashMap<AbsoluteAddr, Vec<FfAccessSummary<RegionedAbsoluteAddr>>> =
        HashMap::default();
    let mut ff_runtime_relocations = HashMap::default();
    let mut comb_blocks = Vec::new();
    let mut comb_observers = Vec::new();
    let mut runtime_errors = HashMap::default();
    let mut runtime_event_sites = Vec::new();
    let mut extern_functions = Vec::<ExternFunction>::new();
    let mut extern_function_indices = HashMap::<String, usize>::default();
    let mut next_runtime_error_code = 2000;
    let mut functions = Vec::new();
    let mut function_frames = Vec::new();
    let mut parallel_ff_units = parallel_ff_units;
    let mut startup_statuses: Vec<_> = expanded
        .values()
        .flat_map(|id| {
            modules[&instance_modules[id]]
                .processes
                .iter()
                // Only automatically started procedures participate in the
                // time-zero barrier (IEEE 1800-2023 9.2.2.2). Final procedures
                // wait for termination, and callable kernels wait for a caller;
                // neither can release a startup dependency.
                .filter(|process| match process.slots.kind {
                    celox_design::ProcessKind::Initializer | celox_design::ProcessKind::Regular => {
                        true
                    }
                    celox_design::ProcessKind::Final | celox_design::ProcessKind::Callable => false,
                })
                .map(|process| bindings.locate(*id, process.slots.status))
        })
        .collect();
    startup_statuses.sort();
    startup_statuses.dedup();
    let mut gated_startup = false;

    for (path, id) in expanded {
        let module_id = &instance_modules[id];
        let sim_module = &modules[module_id];
        let runtime_event_site_base = u32::try_from(runtime_event_sites.len()).map_err(|_| {
            ParserError::illegal_context(
                "FF runtime-event relocation",
                "runtime event site count exceeds u32",
                None,
            )
        })?;
        let relocate_ff_summary = |summary: &FfAccessSummary<RegionedVarAddr>| {
            let relocate_addr =
                |addr: RegionedVarAddr| bindings.locate_regioned(addr.region, *id, addr.var_id);
            FfAccessSummary {
                reads: summary
                    .reads
                    .iter()
                    .map(|read| VarAtomBase {
                        id: relocate_addr(read.id),
                        access: read.access,
                    })
                    .collect(),
                writes: summary
                    .writes
                    .iter()
                    .map(|write| VarAtomBase {
                        id: relocate_addr(write.id),
                        access: write.access,
                    })
                    .collect(),
                dynamic_writes: summary
                    .dynamic_writes
                    .iter()
                    .copied()
                    .map(relocate_addr)
                    .collect(),
            }
        };
        for (trigger_set, summary) in &sim_module.ff_access_summaries {
            let clock_addr = bindings.locate(*id, trigger_set.clock);
            let canonical_clock = clock_domains
                .get(&clock_addr)
                .copied()
                .unwrap_or(clock_addr);
            ff_access_summaries
                .entry(canonical_clock)
                .or_default()
                .push(relocate_ff_summary(summary));
            for &reset in &trigger_set.resets {
                let reset_addr = bindings.locate(*id, reset);
                let canonical_reset = clock_domains
                    .get(&reset_addr)
                    .copied()
                    .unwrap_or(reset_addr);
                ff_access_summaries
                    .entry(canonical_reset)
                    .or_default()
                    .push(relocate_ff_summary(summary));
            }
        }
        let mut runtime_error_codes = HashMap::default();
        for (&local_code, info) in &sim_module.runtime_errors {
            let global_code = next_runtime_error_code;
            next_runtime_error_code += 1;
            runtime_error_codes.insert(local_code, global_code);
            runtime_errors.insert(
                global_code,
                RuntimeErrorInfo {
                    message: info.message.clone(),
                    signals: info
                        .signals
                        .iter()
                        .filter(|var_id| sim_module.variables.contains_key(var_id))
                        .map(|&var_id| bindings.locate(*id, var_id))
                        .collect(),
                },
            );
        }
        ff_runtime_relocations.insert(
            *id,
            FfRuntimeRelocation {
                error_codes: runtime_error_codes.clone(),
                event_site_base: runtime_event_site_base,
            },
        );
        // Extern functions are shared by C symbol name across instances, and
        // every import of one name must declare the same prototype.
        let mut extern_function_map = HashMap::default();
        for (local, function) in sim_module.extern_functions.iter().enumerate() {
            let global = match extern_function_indices.get(&function.name).copied() {
                Some(global) if extern_functions[global] != *function => {
                    return Err(ParserError::ExternSignatureMismatch {
                        name: function.name.clone(),
                        first: extern_functions[global].to_string(),
                        second: function.to_string(),
                    });
                }
                Some(global) => global,
                None => {
                    extern_function_indices.insert(function.name.clone(), extern_functions.len());
                    extern_functions.push(function.clone());
                    extern_functions.len() - 1
                }
            };
            extern_function_map.insert(local as u32, global as u32);
        }
        let mut runtime_event_site_map = HashMap::default();
        let scope = elaborated_scope_name(root_name, path, expanded, indexed_instances);
        let site_base = runtime_event_sites.len() as u32;
        for (local_site, site) in sim_module.runtime_event_sites.iter().enumerate() {
            let global_site = runtime_event_sites.len() as u32;
            runtime_event_site_map.insert(local_site as u32, global_site);
            let mut site = site.clone();
            site.scope = Some(scope.clone());
            // A deferred action names the flush point of its process in the
            // same instance.
            if let Some(celox_design::DeferredSite::Action { queue, .. }) = &mut site.deferred {
                *queue += site_base;
            }
            runtime_event_sites.push(site);
        }
        let function_base = u32::try_from(functions.len()).map_err(|_| {
            ParserError::illegal_context("function relocation", "function count exceeds u32", None)
        })?;
        for function in &sim_module.functions {
            // Frame slots are no state: each copy of a function names its
            // own, unique in the design because the instance is part of the
            // address, and package or hierarchical bindings do not apply.
            let locate = |addr: &RegionedVarAddr| {
                if addr.region == celox_design::FRAME_REGION {
                    RegionedAbsoluteAddr {
                        region: addr.region,
                        instance_id: *id,
                        var_id: addr.var_id,
                    }
                } else {
                    bindings.locate_regioned(addr.region, *id, addr.var_id)
                }
            };
            functions.push(celox_sir::FunctionUnit {
                unit: relocate_executation_unit_with_errors(
                    &function.unit,
                    &locate,
                    &runtime_error_codes,
                    &runtime_event_site_map,
                    &extern_function_map,
                    function_base,
                ),
                frame: celox_sir::FrameLayout {
                    size: function.frame.size,
                    slots: function
                        .frame
                        .slots
                        .iter()
                        .map(|slot| celox_sir::FrameSlot {
                            addr: locate(&slot.addr),
                            offset: slot.offset,
                            width: slot.width,
                            four_state: slot.four_state,
                            handle: slot.handle,
                            element_width: slot.element_width,
                        })
                        .collect(),
                },
                effects: Default::default(),
            });
        }
        // The callees of each copy are the copies in the same instance.
        let rebase = |calls: &[celox_design::CallResume]| {
            calls
                .iter()
                .map(|call| celox_design::CallResume {
                    function: call.function + function_base,
                    ..*call
                })
                .collect::<Vec<_>>()
        };
        function_frames.extend(sim_module.function_frames.iter().map(|frame| {
            celox_design::FunctionFrame {
                calls: rebase(&frame.calls),
                ..frame.clone()
            }
        }));

        // Lane-partitioning units of every trigger group: its independently
        // lowered parts when present, otherwise its whole evaluate/apply pair.
        if let Some(parallel_ff_units) = parallel_ff_units.as_deref_mut() {
            let relocate = |unit: &ExecutionUnit<RegionedVarAddr>| {
                relocate_executation_unit_with_errors(
                    unit,
                    &|addr| bindings.locate_regioned(addr.region, *id, addr.var_id),
                    &runtime_error_codes,
                    &runtime_event_site_map,
                    &extern_function_map,
                    function_base,
                )
            };
            let mut trigger_sets = sim_module.eval_only_ff_blocks.keys().collect::<Vec<_>>();
            trigger_sets.sort_unstable();
            for trigger_set in trigger_sets {
                let parts = match sim_module.parallel_ff_parts.get(trigger_set) {
                    Some(parts) => parts
                        .iter()
                        .map(|part| part.map_units(relocate))
                        .collect::<Vec<_>>(),
                    None => {
                        let Some(apply) = sim_module.apply_ff_blocks.get(trigger_set) else {
                            continue;
                        };
                        vec![FfPart {
                            evaluate: relocate(&sim_module.eval_only_ff_blocks[trigger_set]),
                            apply: relocate(apply),
                        }]
                    }
                };
                let canonical = |var_id| {
                    let address = bindings.locate(*id, var_id);
                    clock_domains.get(&address).copied().unwrap_or(address)
                };
                let events = std::iter::once(canonical(trigger_set.clock))
                    .chain(trigger_set.resets.iter().map(|&reset| canonical(reset)))
                    .collect::<BTreeSet<_>>();
                for event in events {
                    parallel_ff_units
                        .entry(event)
                        .or_default()
                        .extend(parts.iter().cloned().map(|part| (*id, part)));
                }
            }
        }

        let arena_start = global_arena.len();
        let mut relocated_module = flatten_with_trace(
            sim_module,
            path,
            expanded,
            global_boundaries,
            unpacked_element_widths,
            &mut global_arena,
            bindings,
            trace_opts,
            trace.as_deref_mut(),
        )?;
        remap_for_fold_runtime_event_sites(
            &mut global_arena,
            arena_start,
            &runtime_event_site_map,
        )?;
        for observer in &mut relocated_module.comb_observers {
            observer.site_id = runtime_event_site_map[&observer.site_id];
            observer.activation_group = runtime_event_site_map[&observer.activation_group];
        }
        for path in &mut relocated_module.comb_blocks {
            if let celox_slt::LogicPathTarget::Process {
                unit,
                after_initials,
                ..
            } = &mut path.target
            {
                if let Some(ready) = startup_ready.filter(|_| *after_initials)
                    && !startup_statuses.is_empty()
                {
                    path.sources.insert(VarAtomBase::new(ready, 0, 0));
                }
                **unit = relocate_executation_unit_with_errors(
                    unit,
                    &|addr| *addr,
                    &runtime_error_codes,
                    &runtime_event_site_map,
                    &extern_function_map,
                    function_base,
                );
                if let Some(ready) = startup_ready.filter(|_| *after_initials)
                    && !startup_statuses.is_empty()
                {
                    **unit = after_initial_processes(unit, ready);
                    gated_startup = true;
                }
            }
        }
        comb_blocks.extend(relocated_module.comb_blocks);
        comb_observers.extend(relocated_module.comb_observers);

        // Relocate sequential blocks for this instance
        for (trigger_set, eu) in &sim_module.eval_apply_ff_blocks {
            let clock_addr = bindings.locate(*id, trigger_set.clock);
            let canonical_addr = clock_domains
                .get(&clock_addr)
                .copied()
                .unwrap_or(clock_addr);

            eval_apply_ffs.entry(canonical_addr).or_default().push(
                relocate_executation_unit_with_errors(
                    eu,
                    &|addr| bindings.locate_regioned(addr.region, *id, addr.var_id),
                    &runtime_error_codes,
                    &runtime_event_site_map,
                    &extern_function_map,
                    function_base,
                ),
            );

            for &reset in &trigger_set.resets {
                let reset_addr = bindings.locate(*id, reset);
                let canonical_addr = clock_domains
                    .get(&reset_addr)
                    .copied()
                    .unwrap_or(reset_addr);
                eval_apply_ffs.entry(canonical_addr).or_default().push(
                    relocate_executation_unit_with_errors(
                        eu,
                        &|addr| bindings.locate_regioned(addr.region, *id, addr.var_id),
                        &runtime_error_codes,
                        &runtime_event_site_map,
                        &extern_function_map,
                        function_base,
                    ),
                );
            }
        }

        for (trigger_set, eu) in &sim_module.eval_only_ff_blocks {
            let clock_addr = bindings.locate(*id, trigger_set.clock);
            let canonical_addr = clock_domains
                .get(&clock_addr)
                .copied()
                .unwrap_or(clock_addr);
            eval_only_ffs.entry(canonical_addr).or_default().push(
                relocate_executation_unit_with_errors(
                    eu,
                    &|addr| bindings.locate_regioned(addr.region, *id, addr.var_id),
                    &runtime_error_codes,
                    &runtime_event_site_map,
                    &extern_function_map,
                    function_base,
                ),
            );

            for &reset in &trigger_set.resets {
                let reset_addr = bindings.locate(*id, reset);
                let canonical_addr = clock_domains
                    .get(&reset_addr)
                    .copied()
                    .unwrap_or(reset_addr);
                eval_only_ffs.entry(canonical_addr).or_default().push(
                    relocate_executation_unit_with_errors(
                        eu,
                        &|addr| bindings.locate_regioned(addr.region, *id, addr.var_id),
                        &runtime_error_codes,
                        &runtime_event_site_map,
                        &extern_function_map,
                        function_base,
                    ),
                );
            }
        }

        for (index, process) in sim_module.processes.iter().enumerate() {
            processes.push((
                (*id, index),
                RelocatedProcess {
                    kernel: relocate_executation_unit_with_errors(
                        &process.kernel,
                        &|addr| bindings.locate_regioned(addr.region, *id, addr.var_id),
                        &runtime_error_codes,
                        &runtime_event_site_map,
                        &extern_function_map,
                        function_base,
                    ),
                    slots: process.slots.map(|var_id| bindings.locate(*id, var_id)),
                    frame: process
                        .frame
                        .clone()
                        .map(|frame| frame.map(|var_id| bindings.locate(*id, var_id))),
                    origin: celox_design::ProcessOrigin {
                        scope: Some(scope.clone()),
                        ..process.origin.clone()
                    },
                    call_resumes: rebase(&process.call_resumes),
                    key: (*id, index),
                },
            ));
        }

        for (trigger_set, eu) in &sim_module.apply_ff_blocks {
            let clock_addr = bindings.locate(*id, trigger_set.clock);
            let canonical_addr = clock_domains
                .get(&clock_addr)
                .copied()
                .unwrap_or(clock_addr);
            // The split commit of each apply unit, in the same order.
            let split = sim_module
                .split_commit_ff_blocks
                .get(trigger_set)
                .map(|split| SplitCommit {
                    active: relocate_executation_unit_with_errors(
                        &split.active,
                        &|addr| bindings.locate_regioned(addr.region, *id, addr.var_id),
                        &runtime_error_codes,
                        &runtime_event_site_map,
                        &extern_function_map,
                        function_base,
                    ),
                    nba: relocate_executation_unit_with_errors(
                        &split.nba,
                        &|addr| bindings.locate_regioned(addr.region, *id, addr.var_id),
                        &runtime_error_codes,
                        &runtime_event_site_map,
                        &extern_function_map,
                        function_base,
                    ),
                });
            // Pushed like the apply units below: the clock, then each reset.
            split_commits
                .entry(canonical_addr)
                .or_default()
                .push(split.clone());
            for &reset in &trigger_set.resets {
                let reset_addr = bindings.locate(*id, reset);
                let canonical_addr = clock_domains
                    .get(&reset_addr)
                    .copied()
                    .unwrap_or(reset_addr);
                split_commits
                    .entry(canonical_addr)
                    .or_default()
                    .push(split.clone());
            }
            apply_ffs.entry(canonical_addr).or_default().push(
                relocate_executation_unit_with_errors(
                    eu,
                    &|addr| bindings.locate_regioned(addr.region, *id, addr.var_id),
                    &runtime_error_codes,
                    &runtime_event_site_map,
                    &extern_function_map,
                    function_base,
                ),
            );

            for &reset in &trigger_set.resets {
                let reset_addr = bindings.locate(*id, reset);
                let canonical_addr = clock_domains
                    .get(&reset_addr)
                    .copied()
                    .unwrap_or(reset_addr);
                apply_ffs.entry(canonical_addr).or_default().push(
                    relocate_executation_unit_with_errors(
                        eu,
                        &|addr| bindings.locate_regioned(addr.region, *id, addr.var_id),
                        &runtime_error_codes,
                        &runtime_event_site_map,
                        &extern_function_map,
                        function_base,
                    ),
                );
            }
        }
    }
    // A canonical event may collect multiple reset groups or instances. The
    // ordinary tick path must evaluate every group against pre-edge state
    // before any group publishes its nonblocking assignments. Concatenating
    // each module's eval+apply unit instead makes behavior depend on map order.
    for (event, units) in &mut eval_apply_ffs {
        if units.len() > 1 {
            let evaluations = eval_only_ffs.get(event).ok_or_else(|| {
                ParserError::illegal_context(
                    "FF event assembly",
                    "shared event has no evaluation phase",
                    None,
                )
            })?;
            let applications = apply_ffs.get(event).ok_or_else(|| {
                ParserError::illegal_context(
                    "FF event assembly",
                    "shared event has no application phase",
                    None,
                )
            })?;
            let phases = evaluations.iter().chain(applications).collect::<Vec<_>>();
            // Keep the staging stores and their commits in the same optimizer
            // unit: WORKING-region stores are otherwise local dead stores.
            *units = vec![celox_sir::merge_sir_eu_refs(&phases).0];
        }
    }
    if let Some(ready) = startup_ready.filter(|_| gated_startup) {
        comb_blocks.push(startup_ready_path(
            &mut global_arena,
            &startup_statuses,
            ready,
        )?);
    }
    Ok((
        global_arena,
        eval_apply_ffs,
        eval_only_ffs,
        apply_ffs,
        ff_access_summaries,
        ff_runtime_relocations,
        comb_blocks,
        comb_observers,
        runtime_errors,
        runtime_event_sites,
        extern_functions,
        next_runtime_error_code,
        {
            // Instances are numbered in elaboration order, so this is the
            // order processes start in at time zero.
            processes.sort_unstable_by_key(|(key, _)| *key);
            processes.into_iter().map(|(_, process)| process).collect()
        },
        (functions, function_frames),
    ))
}

/// A process kernel placed in one instance.
pub(super) struct RelocatedProcess {
    pub(super) kernel: ExecutionUnit<RegionedAbsoluteAddr>,
    pub(super) slots: ProcessSlots<AbsoluteAddr>,
    pub(super) frame: Option<celox_design::ProcessFrame<AbsoluteAddr>>,
    pub(super) origin: celox_design::ProcessOrigin,
    /// The calls at which the kernel can be suspended inside the callee.
    pub(super) call_resumes: Vec<celox_design::CallResume>,
    /// The instance and the module-local index of the process.
    pub(super) key: (InstanceId, usize),
}
