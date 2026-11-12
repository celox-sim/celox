//! Projecting the scheduled design from source addresses onto state objects.

use super::*;

/// Build the program of the scheduled units, give every state object its
/// address, and project the program and everything the runtime reads of the
/// design onto those addresses.
pub(super) fn project_design(
    design: Design,
    clocks: ClockAnalysis,
    units: Units,
    reset_clock_map: HashMap<AbsoluteAddr, AbsoluteAddr>,
    eval_comb: Vec<ExecutionUnit<RegionedAbsoluteAddr>>,
    eval_comb_stateful: Option<Vec<RegionedAbsoluteAddr>>,
    eval_comb_apply_ffs: FfUnits,
    merge_comb_apply: bool,
    parallel_sir: Option<ParallelSirProgram<AbsoluteAddr, RegionedAbsoluteAddr>>,
    state_objects: HashMap<AbsoluteAddr, VariableMetadata>,
    initial_memory_values: Vec<InitialStateValue<AbsoluteAddr>>,
    runtime_comb_observers: Vec<RuntimeCombObserver<AbsoluteAddr>>,
    fused_direct_ff_writes: HashMap<AbsoluteAddr, Vec<VarAtomBase<RegionedAbsoluteAddr>>>,
    counted: BTreeMap<AbsoluteAddr, CounterSlots<AbsoluteAddr>>,
    active_commits: Vec<AbsoluteAddr>,
    settle_flags: Option<scheduler::SettlePartitioning<AbsoluteAddr>>,
    settle_partitions: Vec<scheduler::SettlePartition<AbsoluteAddr>>,
    settle_copies: Vec<(AbsoluteAddr, AbsoluteAddr)>,
    declaration_startup: Option<celox_design::DeclarationStartup<AbsoluteAddr>>,
    occurrence_probes: Vec<(VarAtomBase<AbsoluteAddr>, AbsoluteAddr)>,
    persistent_comb_state: HashSet<AbsoluteAddr>,
    inlined_nets: Vec<AbsoluteAddr>,
    mod_vars: HashMap<ModuleId, HashMap<SourceVarId, VariableInfo>>,
    mod_path_idx: HashMap<ModuleId, HashMap<Vec<String>, Option<SourceVarId>>>,
    four_state: bool,
) -> Result<ScheduledRtlOutput, ParserError> {
    let Design {
        modules,
        module_names,
        time_precision,
        expanded,
        instance_modules,
        indexed_instances,
        process_count,
        scheduler_slots,
        package_instances,
        instance_time_scales,
        bindings,
        clock_domains,
        ..
    } = design;
    let Units {
        eval_apply_ffs,
        eval_only_ffs,
        apply_ffs,
        processes,
        functions,
        function_frames,
        runtime_errors,
        runtime_event_sites,
        extern_functions,
        timing,
        ..
    } = units;

    let mut process_kernels = Vec::with_capacity(processes.len());
    let mut process_slots = Vec::with_capacity(processes.len());
    let mut process_frames = Vec::with_capacity(processes.len());
    let mut process_origins = Vec::with_capacity(processes.len());
    let mut process_call_resumes = Vec::with_capacity(processes.len());
    for process in processes {
        process_kernels.push(process.kernel);
        process_slots.push(process.slots);
        process_frames.push(process.frame);
        process_origins.push(process.origin);
        process_call_resumes.push(process.call_resumes);
    }
    let TimingMetadata {
        wait_sites,
        nba_sites,
        host_request_sites,
        time_slots,
        sample_shadows,
        initial_copies,
        ..
    } = timing;
    let mut source_sir = SirProgram {
        eval_apply_ffs,
        eval_comb_apply_ffs,
        merge_comb_apply,
        eval_comb_stateful,
        eval_only_ffs,
        apply_ffs,
        eval_comb,
        parallel: parallel_sir,
        processes: process_kernels,
        functions,
    };
    // Calls carry their callees' effects from the start (see
    // `SirProgram::calls_summarized`).
    source_sir.summarize_calls();
    let (source_to_state, frame_slots) = state_addresses(&state_objects, &bindings, &source_sir)?;
    let project = |source: AbsoluteAddr| source_to_state[&source];
    let project_regioned_state = |source: RegionedAbsoluteAddr| RegionedStateAddr {
        region: source.region,
        instance_id: source.instance_id,
        var_id: source_to_state[&source.absolute_addr()].var_id,
    };
    let project_regioned = |source: RegionedAbsoluteAddr| match frame_slots.get(&source) {
        Some(&var_id) => RegionedStateAddr {
            region: source.region,
            instance_id: source.instance_id,
            var_id,
        },
        None => project_regioned_state(source),
    };

    let mut sir = source_sir.into_map_addr(project, project_regioned);
    if !four_state {
        celox_sir::two_state::canonicalize_program(&mut sir);
    }
    let state_objects: HashMap<StateAddr, VariableMetadata> = state_objects
        .into_iter()
        .map(|(address, metadata)| (project(address), metadata))
        .collect();
    let initial_state = initial_memory_values
        .into_iter()
        .map(|initial| InitialStateValue {
            address: project(initial.address),
            data: initial.data,
        })
        .collect();
    let events = project_events(
        clocks,
        clock_domains,
        reset_clock_map,
        &source_to_state,
        project,
    );
    let event_aliases = events.aliases.clone();
    let (runtime_errors, comb_observers, direct_ff_writes) = project_runtime_tables(
        runtime_errors,
        runtime_comb_observers,
        fused_direct_ff_writes,
        project,
        project_regioned,
    );

    let rtl_writes = rtl_writes(&sir, &state_objects);

    let process_frames: Vec<_> = process_frames
        .into_iter()
        .map(|frame| frame.map(|frame| frame.map(project)))
        .collect();
    let process_slots = process_slots
        .into_iter()
        .map(|slots| slots.map(project))
        .collect();
    let watches = event_watches(&counted, time_precision, &state_objects, &events, project);
    let wait_sites: Vec<WaitSite<StateAddr>> =
        wait_sites.iter().map(|site| site.map(project)).collect();
    let nba_sites: Vec<NbaSite<StateAddr>> =
        nba_sites.iter().map(|site| site.map(project)).collect();
    let host_request_sites: Vec<HostRequestSite<StateAddr>> = host_request_sites
        .iter()
        .map(|site| site.map(project))
        .collect();
    let active_commits: Vec<StateAddr> = active_commits.into_iter().map(project).collect();
    let time_slots: Vec<StateAddr> = time_slots.into_iter().map(project).collect();
    let sample_shadows: Vec<(StateAddr, StateAddr)> = sample_shadows
        .into_iter()
        .map(|(variable, copy)| (project(variable), project(copy)))
        .collect();
    let initial_copies: Vec<(StateAddr, StateAddr)> = initial_copies
        .into_iter()
        .map(|(variable, copy)| (project(variable), project(copy)))
        .collect();
    let root_instance = expanded[&InstancePath(Vec::new())];
    let settle_partitions = project_settle_partitions(
        settle_flags,
        &settle_partitions,
        &settle_copies,
        &source_to_state,
        &wait_sites,
        &watches,
        process_count,
        project,
    );
    let scheduler = scheduler_slots.map(|slots| celox_design::SchedulerSchema {
        slots: slots.map(|var_id| {
            project(AbsoluteAddr {
                instance_id: root_instance,
                var_id,
            })
        }),
        unit: None,
        fallback: Vec::new(),
        events: Vec::new(),
        clocks: Vec::new(),
        kernel_events: Vec::new(),
        domain_waits: Vec::new(),
        update_points: false,
        value_change_kernels: Vec::new(),
        refreshes_sample_shadows: false,
    });
    let mut timing_read_roots =
        timing_read_roots(&watches, &wait_sites, &nba_sites, &host_request_sites);
    let declaration_startup = declaration_startup.map(|startup| startup.map(project));
    let occurrence_probes: Vec<(VarAtomBase<StateAddr>, StateAddr)> = occurrence_probes
        .into_iter()
        .map(|(source, counter)| {
            (
                VarAtomBase {
                    id: project(source.id),
                    access: source.access,
                },
                project(counter),
            )
        })
        .collect();
    extend_runtime_read_roots(
        &mut timing_read_roots,
        &process_frames,
        &occurrence_probes,
        &declaration_startup,
        &sample_shadows,
        &initial_copies,
    );
    // A package variable's state object maps back to the package's own
    // variable, not to one of its aliases.
    let state_to_source = source_to_state
        .iter()
        .filter(|(source, _)| !bindings.is_bound(source))
        .map(|(source, state)| (*state, *source))
        .collect();

    let persistent_comb_state = persistent_comb_state.into_iter().map(project).collect();
    // Nothing stores an inlined net any more, but it is still a value
    // combinational logic derives, not state of its own.
    let inlined_nets = inlined_nets.into_iter().map(project).collect();
    let scheduled = ScheduledRtl {
        sir,
        design: ElaboratedDesign {
            time_precision,
            instance_time_scales,
            state_objects,
            events,
            initial_state,
            memory_file_bases: {
                let mut bases: Vec<_> = modules
                    .values()
                    .flat_map(|module| module.memory_file_bases.iter().cloned())
                    .collect();
                bases.sort_by(|a, b| (a.key, &a.path).cmp(&(b.key, &b.path)));
                bases.dedup();
                bases
            },
        },
        frontend_lookup: FrontendLookup {
            instance_ids: expanded,
            instance_module: instance_modules,
            indexed_instances,
            module_variables: mod_vars,
            module_var_path_index: mod_path_idx,
            module_names,
            source_to_state,
            state_to_source,
            event_aliases,
            package_instances: package_instances.values().copied().collect(),
        },
        runtime_schema: RuntimeSchema {
            runtime_errors,
            runtime_event_sites,
            extern_functions,
            comb_observers,
            testbench_read_roots: timing_read_roots,
            rtl_writes,
            comb_writes: inlined_nets,
            persistent_comb_state,
            declaration_startup,
            dead_store_policy: Default::default(),
            live_signals: Default::default(),
            process_frames,
            process_call_resumes,
            function_frames,
            processes: process_slots,
            process_origins,
            watches,
            occurrence_probes,
            wait_sites,
            nba_sites,
            host_request_sites,
            active_commits,
            time_slots,
            sample_shadows,
            initial_copies,
            scheduler,
            derived_edges: Vec::new(),
            settle_partitions,
        },
    };

    if tracing::enabled!(target: "celox::parallel", tracing::Level::TRACE) {
        for source in scheduled.frontend_lookup.source_to_state.keys() {
            tracing::trace!(target: "celox::parallel", event = "source_signal",
                source_address = %source, name = scheduled.frontend_lookup.get_path(source),
                instance = source.instance_id.0, source_object = source.var_id.0);
        }
    }

    Ok(ScheduledRtlOutput {
        scheduled,
        fused_optimization_hints: FusedSirOptimizationHints { direct_ff_writes },
    })
}

/// The state object of every source address, bound variables sharing their
/// target's, and the frame slot of every function's local.
fn state_addresses(
    state_objects: &HashMap<AbsoluteAddr, VariableMetadata>,
    bindings: &PackageBindings,
    source_sir: &SirProgram<AbsoluteAddr, RegionedAbsoluteAddr>,
) -> Result<
    (
        HashMap<AbsoluteAddr, StateAddr>,
        HashMap<RegionedAbsoluteAddr, StateObjectId>,
    ),
    ParserError,
> {
    let mut source_addresses = state_objects.keys().copied().collect::<Vec<_>>();
    source_addresses.sort_unstable();
    let mut source_to_state = HashMap::default();
    for (index, source) in source_addresses.into_iter().enumerate() {
        let object = StateObjectId(u32::try_from(index).map_err(|_| {
            ParserError::illegal_context(
                "design state projection",
                "flattened state object count exceeds u32",
                None,
            )
        })?);
        let state = StateAddr {
            instance_id: source.instance_id,
            var_id: object,
        };
        source_to_state.insert(source, state);
    }
    for (alias, target) in &bindings.targets {
        source_to_state.insert(*alias, source_to_state[target]);
    }
    // Frame slots are no state objects: each gets an address of its own in
    // the FRAME region, numbered in the order of the functions.
    let frame_slots: HashMap<RegionedAbsoluteAddr, StateObjectId> = source_sir
        .functions
        .iter()
        .flat_map(|function| &function.frame.slots)
        .enumerate()
        .map(|(index, slot)| {
            u32::try_from(index)
                .map(|index| (slot.addr, StateObjectId(index)))
                .map_err(|_| {
                    ParserError::illegal_context(
                        "frame slot projection",
                        "frame slot count exceeds u32",
                        None,
                    )
                })
        })
        .collect::<Result<_, _>>()?;
    Ok((source_to_state, frame_slots))
}

/// The event topology of the design over state addresses.
fn project_events(
    clocks: ClockAnalysis,
    clock_domains: HashMap<AbsoluteAddr, AbsoluteAddr>,
    reset_clock_map: HashMap<AbsoluteAddr, AbsoluteAddr>,
    source_to_state: &HashMap<AbsoluteAddr, StateAddr>,
    project: impl Fn(AbsoluteAddr) -> StateAddr + Copy,
) -> EventTopology<StateAddr> {
    let ClockAnalysis {
        topological_clocks,
        cascaded_clocks,
        derived_events,
        combinational_derived,
        independent_derived,
        combinationally_read,
        level_unread_by_domain,
    } = clocks;
    EventTopology {
        aliases: clock_domains
            .into_iter()
            .map(|(alias, canonical)| (project(alias), project(canonical)))
            .collect(),
        ordered_events: topological_clocks.into_iter().map(project).collect(),
        cascaded_events: cascaded_clocks.into_iter().map(project).collect(),
        derived_events: derived_events
            .into_iter()
            .map(|(event, targets)| (project(event), targets.into_iter().map(project).collect()))
            .collect(),
        independent_derived: independent_derived.into_iter().map(project).collect(),
        combinationally_read: combinationally_read.into_iter().map(project).collect(),
        level_unread_by_domain: level_unread_by_domain.into_iter().map(project).collect(),
        combinational_derived: combinational_derived
            .into_iter()
            .map(|(event, clocks)| {
                let clocks = clocks
                    .into_iter()
                    .map(|derived| CombDerivedClock {
                        clock: project(derived.clock),
                        inputs: derived.inputs.and_then(|inputs| {
                            inputs
                                .into_iter()
                                .map(|address| source_to_state.get(&address).copied())
                                .collect()
                        }),
                        // A gate reading an address without a state object
                        // falls back to settling the design.
                        gate: derived.gate.and_then(|gate| {
                            gate.into_iter()
                                .map(|op| match op {
                                    GateOp::Load {
                                        address,
                                        lsb,
                                        width,
                                    } => source_to_state.get(&address).map(|state| GateOp::Load {
                                        address: *state,
                                        lsb,
                                        width,
                                    }),
                                    op => {
                                        Some(op.map_addr(|_| {
                                            unreachable!("only loads have addresses")
                                        }))
                                    }
                                })
                                .collect()
                        }),
                    })
                    .collect();
                (project(event), clocks)
            })
            .collect(),
        reset_clocks: reset_clock_map
            .into_iter()
            .map(|(reset, clock)| (project(reset), project(clock)))
            .collect(),
    }
}

/// The runtime errors, combinational observers and direct flip-flop writes
/// over state addresses.
fn project_runtime_tables(
    runtime_errors: HashMap<i64, RuntimeErrorInfo<AbsoluteAddr>>,
    runtime_comb_observers: Vec<RuntimeCombObserver<AbsoluteAddr>>,
    fused_direct_ff_writes: HashMap<AbsoluteAddr, Vec<VarAtomBase<RegionedAbsoluteAddr>>>,
    project: impl Fn(AbsoluteAddr) -> StateAddr + Copy,
    project_regioned: impl Fn(RegionedAbsoluteAddr) -> RegionedStateAddr + Copy,
) -> (
    HashMap<i64, RuntimeErrorInfo<StateAddr>>,
    Vec<RuntimeCombObserver<StateAddr>>,
    HashMap<StateAddr, Vec<VarAtomBase<RegionedStateAddr>>>,
) {
    let runtime_errors = runtime_errors
        .into_iter()
        .map(|(code, info)| {
            (
                code,
                RuntimeErrorInfo {
                    message: info.message,
                    signals: info.signals.into_iter().map(project).collect(),
                },
            )
        })
        .collect();
    let comb_observers = runtime_comb_observers
        .into_iter()
        .map(|observer| RuntimeCombObserver {
            site_id: observer.site_id,
            activation_group: observer.activation_group,
            sensitivity: observer
                .sensitivity
                .into_iter()
                .map(|atom| VarAtomBase {
                    id: project(atom.id),
                    access: atom.access,
                })
                .collect(),
            occurrence_counters: observer
                .occurrence_counters
                .into_iter()
                .map(project)
                .collect(),
            written_inputs: observer.written_inputs.into_iter().map(project).collect(),
        })
        .collect();
    let direct_ff_writes = fused_direct_ff_writes
        .into_iter()
        .map(|(source, writes)| {
            (
                project(source),
                writes
                    .into_iter()
                    .map(|write| VarAtomBase {
                        id: project_regioned(write.id),
                        access: write.access,
                    })
                    .collect(),
            )
        })
        .collect();
    (runtime_errors, comb_observers, direct_ff_writes)
}

/// The ranges the units of the program write.
fn rtl_writes(
    sir: &SirProgram<StateAddr, RegionedStateAddr>,
    state_objects: &HashMap<StateAddr, VariableMetadata>,
) -> HashSet<VarAtomBase<StateAddr>> {
    let mut rtl_writes = HashSet::default();
    for unit in sir
        .eval_comb
        .iter()
        .chain(sir.eval_apply_ffs.values().flatten())
        .chain(sir.eval_comb_apply_ffs.values().flatten())
        .chain(sir.eval_only_ffs.values().flatten())
        .chain(sir.apply_ffs.values().flatten())
        .chain(&sir.processes)
    {
        for block in unit.blocks.values() {
            for instruction in &block.instructions {
                for (address, access) in written_bits(instruction) {
                    let address = address.absolute_addr();
                    let access = access.or_else(|| {
                        state_objects
                            .get(&address)
                            .and_then(|object| object.width.checked_sub(1))
                            .map(|msb| BitAccess::new(0, msb))
                    });
                    if let Some(access) = access {
                        rtl_writes.insert(VarAtomBase {
                            id: address,
                            access,
                        });
                    }
                }
            }
        }
    }
    rtl_writes
}

/// The watch of every counted signal: its counters and the domain its
/// occurrences trigger.
fn event_watches(
    counted: &BTreeMap<AbsoluteAddr, CounterSlots<AbsoluteAddr>>,
    time_precision: celox_design::TimePrecision,
    state_objects: &HashMap<StateAddr, VariableMetadata>,
    events: &EventTopology<StateAddr>,
    project: impl Fn(AbsoluteAddr) -> StateAddr + Copy,
) -> Vec<EventWatch<StateAddr>> {
    let event_kinds = ElaboratedDesign {
        time_precision,
        instance_time_scales: HashMap::default(),
        state_objects: state_objects.clone(),
        events: events.clone(),
        initial_state: Vec::new(),
        memory_file_bases: Vec::new(),
    }
    .event_kinds();
    counted
        .values()
        .map(|slots| {
            let signal = project(slots.signal);
            let object = &state_objects[&signal];
            let canonical = events.canonical(signal);
            let domains = event_kinds
                .get(&canonical)
                .filter(|_| events.ordered_events.contains(&signal) || canonical != signal)
                .map(|&kind| (canonical, CounterKind::for_domain(kind)))
                .into_iter()
                .collect();
            EventWatch {
                signal,
                width: object.width,
                four_state: object.is_4state,
                changes: Some(project(slots.changes)),
                rises: Some(project(slots.rises)),
                falls: Some(project(slots.falls)),
                domains,
            }
        })
        .collect()
}

/// The partitioned settle over state addresses: the flags each state object
/// marks when it changes.
fn project_settle_partitions(
    settle_flags: Option<scheduler::SettlePartitioning<AbsoluteAddr>>,
    settle_partitions: &[scheduler::SettlePartition<AbsoluteAddr>],
    settle_copies: &[(AbsoluteAddr, AbsoluteAddr)],
    source_to_state: &HashMap<AbsoluteAddr, StateAddr>,
    wait_sites: &[WaitSite<StateAddr>],
    watches: &[EventWatch<StateAddr>],
    process_count: usize,
    project: impl Fn(AbsoluteAddr) -> StateAddr + Copy,
) -> Option<celox_design::SettlePartitions<StateAddr>> {
    settle_flags.map(|settle| {
        let mut marks: HashMap<StateAddr, BTreeSet<u32>> = HashMap::default();
        for (index, partition) in settle_partitions.iter().enumerate() {
            let index = index as u32;
            for addr in &partition.reads {
                if let Some(&state) = source_to_state.get(addr) {
                    marks.entry(state).or_default().insert(index);
                }
            }
            for addr in &partition.writes {
                if let Some(&state) = source_to_state.get(addr) {
                    marks.entry(state).or_default().insert(index);
                }
            }
        }
        // A process's wait flag, for the signals its waits watch and the
        // counters of their occurrences.
        for site in wait_sites {
            let flag = celox_design::settle_wait_flag(settle_partitions.len(), site.process) as u32;
            for &(signal, _) in &site.terms {
                marks.entry(signal).or_default().insert(flag);
                for watch in watches.iter().filter(|watch| watch.signal == signal) {
                    for (_, counter) in watch.counters() {
                        marks.entry(counter).or_default().insert(flag);
                    }
                }
            }
        }
        let mut marks = marks
            .into_iter()
            .map(|(state, partitions)| (state, partitions.into_iter().collect()))
            .collect::<Vec<_>>();
        marks.sort_unstable();
        celox_design::SettlePartitions {
            flags: project(settle.flags),
            partitions: settle_partitions.len() as u32,
            marks,
            waits: process_count as u32,
            profile: settle.profile.is_some(),
            copies: settle_copies
                .iter()
                .filter_map(|(target, source)| {
                    Some((*source_to_state.get(target)?, *source_to_state.get(source)?))
                })
                .collect(),
        }
    })
}

/// What the runtime reads directly of the timing state.
fn timing_read_roots(
    watches: &[EventWatch<StateAddr>],
    wait_sites: &[WaitSite<StateAddr>],
    nba_sites: &[NbaSite<StateAddr>],
    host_request_sites: &[HostRequestSite<StateAddr>],
) -> HashSet<StateAddr> {
    // The runtime reads counters, the signals waits watch and staged
    // nonblocking values directly; no unit loads a wait's signal.
    watches
        .iter()
        .flat_map(|watch| {
            watch
                .counters()
                .map(|(_, counter)| counter)
                .collect::<Vec<_>>()
        })
        .chain(wait_sites.iter().flat_map(WaitSite::reads))
        .chain(
            nba_sites
                .iter()
                .flat_map(|site| {
                    [
                        Some(site.value),
                        site.position.map(|position| position.offset),
                        site.position.map(|position| position.width),
                        site.pending,
                    ]
                })
                .flatten(),
        )
        .chain(host_request_sites.iter().flat_map(|site| site.slots()))
        .collect()
}

/// Add what the runtime reads for its own writes, the declaration startup
/// and the sampled variables.
fn extend_runtime_read_roots(
    timing_read_roots: &mut HashSet<StateAddr>,
    process_frames: &[Option<celox_design::ProcessFrame<StateAddr>>],
    occurrence_probes: &[(VarAtomBase<StateAddr>, StateAddr)],
    declaration_startup: &Option<celox_design::DeclarationStartup<StateAddr>>,
    sample_shadows: &[(StateAddr, StateAddr)],
    initial_copies: &[(StateAddr, StateAddr)],
) {
    // The runtime counts the occurrences its own writes cause.
    timing_read_roots.extend(
        process_frames
            .iter()
            .flatten()
            .flat_map(|frame| frame.private.iter().chain(&frame.locals))
            .copied(),
    );
    timing_read_roots.extend(
        occurrence_probes
            .iter()
            .flat_map(|(source, counter)| [source.id, *counter]),
    );
    if let Some(startup) = declaration_startup {
        timing_read_roots.extend(
            startup
                .reset
                .iter()
                .chain(&startup.ordinary)
                .map(|value| value.address),
        );
        timing_read_roots.extend(
            startup
                .snapshots
                .iter()
                .flat_map(|(source, saved)| [source.id, *saved]),
        );
        timing_read_roots.extend(
            startup
                .counter_snapshots
                .iter()
                .flat_map(|&(counter, saved)| [counter, saved]),
        );
        timing_read_roots.extend(
            startup
                .probes
                .iter()
                .flat_map(|(source, counter)| [source.id, *counter]),
        );
    }
    // The host copies each sampled variable into its copy.
    timing_read_roots.extend(
        sample_shadows
            .iter()
            .chain(initial_copies)
            .flat_map(|&(variable, copy)| [variable, copy]),
    );
}
