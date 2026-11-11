//! Scheduling the combinational logic of a design.

use super::*;

/// Finish the symbolic logic before it is scheduled: inline constants, order
/// `always_comb` processes, build the capture paths of the observers, and
/// verify the result. Returns the width of every variable.
pub(super) fn prepare_comb_logic(
    design: &Design,
    logic: &mut CombLogic,
    units: &mut Units,
    trace_opts: &FrontendTraceOptions,
    trace: &mut Option<&mut FrontendTrace>,
) -> Result<HashMap<AbsoluteAddr, usize>, ParserError> {
    let Design {
        instance_modules,
        modules,
        bindings,
        unpacked_element_widths,
        ..
    } = design;
    let CombLogic {
        global_arena,
        comb_blocks,
        comb_observers,
    } = logic;
    let Units {
        runtime_errors,
        runtime_event_sites,
        ..
    } = units;

    if let Some(t) = trace.as_deref_mut()
        && trace_opts.flattened_comb_blocks
    {
        t.flattened_comb_blocks = Some((comb_blocks.clone(), global_arena.clone()));
    }

    // Constant variable inlining: detect variables whose every LogicPath
    // is a constant, then replace all Input references with Constant nodes.
    // This eliminates Store→Load roundtrips for compile-time constants
    // (e.g. genvar-expanded parity-check matrices).
    celox_slt::const_inline::inline_constant_variables(comb_blocks, global_arena)?;
    apply_always_comb_previous_source_ordering(comb_blocks);

    let var_widths: HashMap<AbsoluteAddr, usize> = instance_modules
        .iter()
        .flat_map(|(&inst_id, &mod_id)| {
            modules[&mod_id]
                .variables
                .iter()
                .map(move |(var_id, var)| (bindings.locate(inst_id, *var_id), var.metadata.width))
        })
        .collect();
    let var_signedness: HashMap<AbsoluteAddr, bool> = instance_modules
        .iter()
        .flat_map(|(&inst_id, &mod_id)| {
            modules[&mod_id]
                .variables
                .iter()
                .map(move |(var_id, var)| (bindings.locate(inst_id, *var_id), var.signed))
        })
        .collect();

    build_comb_observer_capture_paths(
        comb_blocks,
        comb_observers,
        runtime_event_sites,
        global_arena,
        unpacked_element_widths,
    )?;
    for (site_id, site) in runtime_event_sites.iter().enumerate() {
        if !site.kind.is_fatal() {
            continue;
        }
        runtime_errors
            .entry(site_id as i64)
            .or_insert_with(|| RuntimeErrorInfo {
                message: site
                    .template
                    .clone()
                    .unwrap_or_else(|| "assertion failed".to_string()),
                signals: Vec::new(),
            });
    }

    celox_slt::verify_symbolic_roots(
        global_arena,
        comb_blocks,
        comb_observers,
        &var_widths,
        &var_signedness,
    )
    .map_err(|error| ParserError::SltVerify {
        phase: "after flattening symbolic logic",
        error,
    })?;
    Ok(var_widths)
}

/// What a fused flip-flop factory schedules: the actions of every event and
/// the combinational paths, widths and loop options over stable addresses.
pub(super) type FusedInputs = (
    HashMap<AbsoluteAddr, Vec<FusedFfAction>>,
    SLTNodeArena<RegionedAbsoluteAddr>,
    Vec<LogicPath<RegionedAbsoluteAddr>>,
    HashMap<RegionedAbsoluteAddr, usize>,
    HashMap<RegionedAbsoluteAddr, usize>,
    HashSet<(RegionedAbsoluteAddr, RegionedAbsoluteAddr)>,
    HashMap<(RegionedAbsoluteAddr, RegionedAbsoluteAddr), usize>,
);

/// The inputs of the fused schedules, if a factory lowers flip-flops.
pub(super) fn fused_ff_inputs(
    fused_ff_factory: Option<&dyn FusedFfLoweringFactory>,
    design: &Design,
    logic: &CombLogic,
    ff_runtime_relocations: &HashMap<InstanceId, FfRuntimeRelocation>,
    var_widths: &HashMap<AbsoluteAddr, usize>,
    ignored_loops: &HashSet<(AbsoluteAddr, AbsoluteAddr)>,
    true_loops: &HashMap<(AbsoluteAddr, AbsoluteAddr), usize>,
) -> Result<Option<FusedInputs>, ParserError> {
    let Design {
        bindings,
        modules,
        instance_modules,
        clock_domains,
        unpacked_element_widths,
        ..
    } = design;
    let CombLogic {
        global_arena,
        comb_blocks,
        ..
    } = logic;

    let fused_inputs = if fused_ff_factory.is_some() {
        let actions = build_fused_ff_actions(
            bindings,
            modules,
            instance_modules,
            clock_domains,
            ff_runtime_relocations,
        );
        let mut clock_arena = SLTNodeArena::<RegionedAbsoluteAddr>::new();
        let mut clock_node_cache = HashMap::default();
        let clock_comb_blocks = comb_blocks
            .iter()
            .map(|path| {
                path.map_addr(
                    global_arena,
                    &mut clock_arena,
                    &mut clock_node_cache,
                    &|addr| RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, *addr),
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let clock_var_widths = var_widths
            .iter()
            .map(|(&addr, &width)| {
                (
                    RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, addr),
                    width,
                )
            })
            .collect::<HashMap<_, _>>();
        let clock_unpacked_element_widths = unpacked_element_widths
            .iter()
            .map(|(&addr, &element_width)| {
                (
                    RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, addr),
                    element_width,
                )
            })
            .collect::<HashMap<_, _>>();
        let clock_ignored_loops = ignored_loops
            .iter()
            .map(|&(from, to)| {
                (
                    RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, from),
                    RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, to),
                )
            })
            .collect::<HashSet<_>>();
        let clock_true_loops = true_loops
            .iter()
            .map(|(&(from, to), &limit)| {
                (
                    (
                        RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, from),
                        RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, to),
                    ),
                    limit,
                )
            })
            .collect::<HashMap<_, _>>();
        Some((
            actions,
            clock_arena,
            clock_comb_blocks,
            clock_var_widths,
            clock_unpacked_element_widths,
            clock_ignored_loops,
            clock_true_loops,
        ))
    } else {
        None
    };
    Ok(fused_inputs)
}

/// The event signals and their aliases.
pub(super) fn event_nets(
    topological_clocks: &[AbsoluteAddr],
    clock_domains: &HashMap<AbsoluteAddr, AbsoluteAddr>,
) -> HashSet<AbsoluteAddr> {
    let events = topological_clocks.iter().copied().collect::<HashSet<_>>();
    events
        .iter()
        .copied()
        .chain(
            clock_domains
                .iter()
                .filter(|(_, canonical)| events.contains(canonical))
                .map(|(alias, _)| *alias),
        )
        .collect()
}

/// The state combinational processes keep from one settle to the next, and
/// the startup of every wildcard process by the object that marks it
/// initialized.
pub(super) fn comb_process_state(
    logic: &CombLogic,
    variables: &HashMap<AbsoluteAddr, (usize, bool)>,
    four_state: bool,
) -> (
    HashSet<AbsoluteAddr>,
    BTreeMap<AbsoluteAddr, celox_slt::CombStartup<AbsoluteAddr>>,
) {
    let CombLogic {
        global_arena,
        comb_blocks,
        ..
    } = logic;

    let mut persistent_comb_state = HashSet::default();
    let mut comb_startups = BTreeMap::new();
    for path in comb_blocks {
        if let LogicPathTarget::Process { unit, startup, .. } = &path.target {
            if let Some(startup) = startup {
                comb_startups.entry(startup.initialized).or_insert_with(|| {
                    let mut startup = startup.clone();
                    // Transparent ports/wires already have their upstream
                    // declaration value before wildcard processes begin
                    // waiting. Their unconnected net default (Z) is not a
                    // startup event. Resolve only direct equal-type copies;
                    // constant/expression continuous drivers still run at
                    // time zero and can wake the process.
                    for (source, _) in &mut startup.inputs {
                        let mut seen = HashSet::default();
                        while seen.insert(*source) {
                            let upstream = comb_blocks.iter().find_map(|path| {
                                if !path.transparent_copy {
                                    return None;
                                }
                                let LogicPathTarget::Var(target) = &path.target else {
                                    return None;
                                };
                                if target.id != source.id
                                    || target.access.lsb > source.access.lsb
                                    || target.access.msb < source.access.msb
                                {
                                    return None;
                                }
                                let celox_slt::SLTNode::Input {
                                    variable,
                                    access,
                                    index,
                                    ..
                                } = global_arena.get(path.expr)
                                else {
                                    return None;
                                };
                                if !index.is_empty()
                                    || access.msb - access.lsb
                                        != target.access.msb - target.access.lsb
                                    || (four_state
                                        && variables[variable].1 != variables[&target.id].1)
                                {
                                    return None;
                                }
                                Some(VarAtomBase::new(
                                    *variable,
                                    access.lsb + source.access.lsb - target.access.lsb,
                                    access.lsb + source.access.msb - target.access.lsb,
                                ))
                            });
                            let Some(upstream) = upstream else {
                                break;
                            };
                            *source = upstream;
                        }
                    }
                    startup
                });
            }
            for block in unit.blocks.values() {
                for inst in &block.instructions {
                    // The stores of the process, and what its calls write.
                    if matches!(
                        inst,
                        SIRInstruction::Store(..) | SIRInstruction::Call { .. }
                    ) {
                        persistent_comb_state
                            .extend(super::super::super::written_objects(inst).copied());
                    }
                }
            }
        }
    }
    (persistent_comb_state, comb_startups)
}

/// Inline the nets nothing observes into their readers, where the options
/// and the design allow it. Returns the nets that left the program.
pub(super) fn inline_unobserved_nets(
    parallel: &ParallelScheduleOptions,
    fused_ff_factory: Option<&dyn FusedFfLoweringFactory>,
    four_state: bool,
    design: &Design,
    logic: &mut CombLogic,
    units: &Units,
    parallel_ff_units: &FfParts,
    split_commits: &SplitCommits,
    topological_clocks: &[AbsoluteAddr],
    clock_nets: &HashSet<AbsoluteAddr>,
    counted: &BTreeMap<AbsoluteAddr, CounterSlots<AbsoluteAddr>>,
    occurrence_probes: &[(VarAtomBase<AbsoluteAddr>, AbsoluteAddr)],
    declaration_probes: &[(VarAtomBase<AbsoluteAddr>, AbsoluteAddr)],
) -> Result<Vec<AbsoluteAddr>, ParserError> {
    let Design {
        expanded,
        instance_modules,
        modules,
        bindings,
        clock_domains,
        unpacked_element_widths,
        ..
    } = design;
    let CombLogic {
        global_arena,
        comb_blocks,
        comb_observers,
    } = logic;
    let Units {
        eval_apply_ffs,
        eval_only_ffs,
        apply_ffs,
        processes,
        functions,
        timing,
        ..
    } = units;

    // Nets nothing observes leave the program before it is scheduled. A
    // fused flip-flop factory lowers the paths from their source form, and
    // a `$dumpvars` may select any object at run time.
    let mut inlined_nets: Vec<AbsoluteAddr> = Vec::new();
    if parallel.unobserved_nets != celox_design::DeadStorePolicy::Off
        && fused_ff_factory.is_none()
        && !four_state
        && !parallel.enabled()
        && !timing
            .host_request_sites
            .iter()
            .any(|site| site.kind == celox_design::HostRequestKind::Vcd)
    {
        let pinned = pinned_nets(
            parallel.unobserved_nets,
            expanded[&InstancePath(Vec::new())],
            instance_modules,
            modules,
            bindings,
            PinnedUnits {
                flip_flops: eval_apply_ffs
                    .values()
                    .chain(eval_only_ffs.values())
                    .chain(apply_ffs.values())
                    .flatten()
                    .chain(
                        parallel_ff_units
                            .values()
                            .flatten()
                            .flat_map(|(_, part)| [&part.evaluate, &part.apply]),
                    )
                    .chain(
                        split_commits
                            .values()
                            .flatten()
                            .flatten()
                            .flat_map(|split| [&split.active, &split.nba]),
                    )
                    .chain(processes.iter().map(|process| &process.kernel))
                    .chain(functions.iter().map(|function| &function.unit))
                    .collect(),
                events: eval_apply_ffs
                    .keys()
                    .chain(eval_only_ffs.keys())
                    .chain(apply_ffs.keys())
                    .chain(topological_clocks)
                    .chain(clock_nets)
                    .copied()
                    .collect(),
            },
            clock_domains,
            timing,
            counted.keys().copied(),
            occurrence_probes
                .iter()
                .chain(declaration_probes)
                .map(|(source, _)| source.id),
            comb_observers,
            comb_blocks,
            unpacked_element_widths,
        );
        let report = celox_slt::net_inline::inline_nets(comb_blocks, global_arena, &pinned)?;
        tracing::debug!(
            "[flatten] net_inline: {} paths to {}, {} nets inlined, {} copied, {} unread",
            report.paths_before,
            report.paths_after,
            report.inlined,
            report.duplicated,
            report.dead
        );
        inlined_nets = report.removed;
    }
    Ok(inlined_nets)
}

/// The request for a partitioned settle, if the program is to have one. The
/// flag object is declared once the partitions are known; its address is
/// reserved here.
pub(super) fn plan_settle_flags(
    parallel: &ParallelScheduleOptions,
    design: &Design,
    logic: &CombLogic,
    units: &Units,
    counted: &BTreeMap<AbsoluteAddr, CounterSlots<AbsoluteAddr>>,
    clock_nets: &HashSet<AbsoluteAddr>,
) -> Option<scheduler::SettlePartitioning<AbsoluteAddr>> {
    let Design {
        expanded, modules, ..
    } = design;
    let root_id = design.root_id;
    let process_count = design.process_count;
    let CombLogic {
        global_arena,
        comb_blocks,
        ..
    } = logic;
    let Units {
        eval_apply_ffs,
        timing,
        ..
    } = units;

    let root_instance = expanded[&InstancePath(Vec::new())];
    settle_partitioning(parallel, comb_blocks, global_arena, eval_apply_ffs.len()).map(
        |target_cost| {
            let var_id = SourceVarId(
                modules[&root_id]
                    .variables
                    .keys()
                    .map(|id| id.0 + 1)
                    .max()
                    .unwrap_or(0),
            );
            // The wait flags follow the partition flags: a partition
            // changing a signal an edge wait watches, or one of the
            // signal's occurrence counters, sets its process's flag
            // (`SettlePartitions::waits`). A pulse that returns to the
            // starting value within one partition changes only the
            // counters.
            let mut external_readers: HashMap<AbsoluteAddr, Vec<usize>> = HashMap::default();
            for site in &timing.wait_sites {
                for &(signal, _) in &site.terms {
                    let counters = counted
                        .get(&signal)
                        .into_iter()
                        .flat_map(|slots| [slots.changes, slots.rises, slots.falls]);
                    for watched in std::iter::once(signal).chain(counters) {
                        let readers = external_readers.entry(watched).or_default();
                        if !readers.contains(&site.process) {
                            readers.push(site.process);
                        }
                    }
                }
            }
            scheduler::SettlePartitioning {
                flags: AbsoluteAddr {
                    instance_id: root_instance,
                    var_id,
                },
                target_cost,
                external_readers,
                clock_nets: clock_nets.clone(),
                profile: parallel.settle_profile.then_some(process_count),
            }
        },
    )
}

/// The error of a program the scheduler rejected, in the names of the
/// design, with the ranges the blocks of a combinational loop write added
/// to `loop_members`.
pub(super) fn scheduler_error(
    error: SchedulerError<AbsoluteAddr>,
    design: &Design,
    global_arena: &SLTNodeArena<AbsoluteAddr>,
    loop_members: &mut Vec<LoopMember>,
) -> Result<ParserError, ParserError> {
    let Design {
        modules,
        expanded,
        instance_modules,
        indexed_instances,
        module_names,
        package_instances,
        ..
    } = design;

    let (err_vars, err_path_idx) = module_variables(modules);
    let frontend_lookup = FrontendLookup {
        instance_ids: expanded.clone(),
        instance_module: instance_modules.clone(),
        indexed_instances: indexed_instances.clone(),
        module_variables: err_vars,
        module_var_path_index: err_path_idx,
        module_names: module_names.clone(),
        source_to_state: HashMap::default(),
        state_to_source: HashMap::default(),
        event_aliases: HashMap::default(),
        package_instances: package_instances.values().copied().collect(),
    };
    let source_locations = scheduler_source_locations(&error, modules, instance_modules);
    if let SchedulerError::CombinationalLoop { blocks } = &error {
        loop_members.extend(blocks.iter().filter_map(|block| {
            let var = block.target.var()?;
            Some((
                *instance_modules.get(&var.id.instance_id)?,
                VarAtomBase::new(var.id.var_id, var.access.lsb, var.access.msb),
            ))
        }));
    }
    let mut target_arena = SLTNodeArena::new();
    let error = error.map_addr(global_arena, &mut target_arena, &|addr| {
        frontend_lookup.get_path(addr)
    })?;
    Ok(if source_locations.is_empty() {
        ParserError::Scheduler(error)
    } else {
        ParserError::SchedulerWithLocation {
            error,
            source_locations,
        }
    })
}

/// Record the plan of a partitioned settle in the trace, if asked for.
pub(super) fn trace_settle_plan(
    trace: &mut Option<&mut FrontendTrace>,
    trace_opts: &FrontendTraceOptions,
    settle_partitions: &[scheduler::SettlePartition<AbsoluteAddr>],
    design: &Design,
) {
    let Design {
        expanded,
        instance_modules,
        modules,
        ..
    } = design;

    if let Some(t) = trace.as_deref_mut()
        && trace_opts.settle_plan
        && !settle_partitions.is_empty()
    {
        let instance_paths: HashMap<InstanceId, String> = expanded
            .iter()
            .map(|(path, &id)| {
                let text = path
                    .0
                    .iter()
                    .map(|(name, index)| {
                        if *index == 0 {
                            name.clone()
                        } else {
                            format!("{name}[{index}]")
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(".");
                (id, text)
            })
            .collect();
        t.settle_plan = Some(super::super::super::settle_plan::format(
            settle_partitions,
            |address| {
                let variable = instance_modules
                    .get(&address.instance_id)
                    .and_then(|module| modules.get(module))
                    .and_then(|module| module.variables.get(&address.var_id));
                match variable {
                    Some(variable) => format!(
                        "{}.{}",
                        instance_paths
                            .get(&address.instance_id)
                            .map_or("", String::as_str),
                        variable.path.join(".")
                    ),
                    None => address.to_string(),
                }
            },
        ));
    }
}

/// Declare the flags of a partitioned settle, which keep what changed from
/// one settle to the next.
pub(super) fn declare_partitioned_settle(
    design: &mut Design,
    settle_flags: &Option<scheduler::SettlePartitioning<AbsoluteAddr>>,
    settle_partitions: &[scheduler::SettlePartition<AbsoluteAddr>],
    persistent_comb_state: &mut HashSet<AbsoluteAddr>,
) {
    let root_id = design.root_id;
    let process_count = design.process_count;
    let modules = &mut design.modules;

    if let Some(settle) = settle_flags {
        tracing::debug!(
            partitions = settle_partitions.len(),
            target_cost = settle.target_cost,
            "partitioned combinational settle"
        );
        // The flags carry what changed from one settle to the next.
        persistent_comb_state.insert(settle.flags);
        declare_settle_flags(
            modules,
            root_id,
            settle.flags.var_id,
            if settle.profile.is_some() {
                celox_design::settle_profile_offset(settle_partitions.len(), process_count)
                    + celox_design::settle_profile_bytes(settle_partitions.len())
            } else {
                celox_design::settle_flag_count(settle_partitions.len(), process_count)
            },
        );
    }
}

/// The lane-partitioned schedule of the combinational paths kept for it, if
/// partitioning them pays.
pub(super) fn sort_comb_lanes(
    parallel_comb_blocks: Option<Vec<LogicPath<AbsoluteAddr>>>,
    parallel: &ParallelScheduleOptions,
    lane0_objects: &Option<HashSet<AbsoluteAddr>>,
    design: &Design,
    units: &Units,
    parallel_ff_units: &FfParts,
    object_sizes: super::super::super::parallel::ObjectSizes<'_>,
    global_arena: &SLTNodeArena<AbsoluteAddr>,
    schedule: &scheduler::ScheduleResult<AbsoluteAddr>,
    ignored_loops: &HashSet<(AbsoluteAddr, AbsoluteAddr)>,
    true_loops: &HashMap<(AbsoluteAddr, AbsoluteAddr), usize>,
    var_widths: &HashMap<AbsoluteAddr, usize>,
    four_state: bool,
) -> Result<Option<scheduler::LaneScheduleResult<AbsoluteAddr>>, ParserError> {
    let Design {
        expanded,
        unpacked_element_widths,
        ..
    } = design;
    let Units { runtime_errors, .. } = units;
    let next_runtime_error_code = units.next_runtime_error_code;

    let parallel_comb = match parallel_comb_blocks {
        Some(paths) => {
            let first_code = runtime_errors
                .keys()
                .chain(schedule.runtime_errors.keys())
                .copied()
                .max()
                .map_or(next_runtime_error_code, |code| code + 1)
                .max(next_runtime_error_code);
            let options = scheduler::LanePartitionOptions {
                lanes: parallel.lanes,
                synchronization_cost: parallel.synchronization_cost,
                lane0_targets: lane0_objects.clone().unwrap_or_default(),
                minimum_speedup_percent: parallel.minimum_speedup_percent,
                hierarchy: Some({
                    let parents = instance_parents(expanded);
                    let extra_costs = super::super::super::parallel::instance_ff_costs(
                        parallel_ff_units,
                        parents.len(),
                        object_sizes,
                    );
                    scheduler::InstanceHierarchy {
                        instance_of: |address: &AbsoluteAddr| address.instance_id.0,
                        parents,
                        extra_costs,
                    }
                }),
            };
            let partitioned = scheduler::sort_lanes(
                paths,
                global_arena,
                ignored_loops,
                true_loops,
                four_state,
                var_widths,
                unpacked_element_widths,
                first_code,
                &options,
            )
            .map_err(|error| {
                ParserError::illegal_context(
                    "lane-partitioned combinational schedule",
                    format!("{error}"),
                    None,
                )
            })?;
            if let Some(partitioned) = &partitioned {
                tracing::debug!(
                    "[parallel] comb lanes={} units={} estimated_makespan={} total_cost={}",
                    parallel.lanes,
                    partitioned.units.len(),
                    partitioned.estimated_makespan,
                    partitioned.total_cost
                );
            }
            partitioned
        }
        None => None,
    };
    Ok(parallel_comb)
}

/// The scheduled combinational units over stable addresses: the targets
/// that keep state between settles, the units, and the lane units. The
/// runtime errors of the schedules join `runtime_errors`.
pub(super) fn stable_comb_units(
    schedule: scheduler::ScheduleResult<AbsoluteAddr>,
    parallel_comb: Option<scheduler::LaneScheduleResult<AbsoluteAddr>>,
    runtime_errors: &mut HashMap<i64, RuntimeErrorInfo<AbsoluteAddr>>,
) -> (
    Option<Vec<RegionedAbsoluteAddr>>,
    Vec<ExecutionUnit<RegionedAbsoluteAddr>>,
    Vec<LaneUnit<RegionedAbsoluteAddr>>,
) {
    runtime_errors.extend(schedule.runtime_errors);
    let eval_comb_stateful = schedule.stateful_targets.as_ref().map(|targets| {
        targets
            .iter()
            .map(|target| RegionedAbsoluteAddr {
                region: STABLE_REGION,
                instance_id: target.id.instance_id,
                var_id: target.id.var_id,
            })
            .collect::<Vec<_>>()
    });
    let stable_unit = |eu: ExecutionUnit<AbsoluteAddr>| ExecutionUnit {
        entry_block_id: eu.entry_block_id,
        blocks: eu
            .blocks
            .into_iter()
            .map(|(id, bb)| {
                (
                    id,
                    BasicBlock {
                        id: bb.id,
                        params: bb.params,
                        instructions: bb
                            .instructions
                            .into_iter()
                            .map(|inst| {
                                inst.into_map_addr(|addr| RegionedAbsoluteAddr {
                                    region: STABLE_REGION,
                                    instance_id: addr.instance_id,
                                    var_id: addr.var_id,
                                })
                            })
                            .collect(),
                        terminator: bb.terminator,
                    },
                )
            })
            .collect(),
        register_map: eu.register_map,
    };
    let eval_comb: Vec<ExecutionUnit<RegionedAbsoluteAddr>> = schedule
        .execution_units
        .into_iter()
        .map(stable_unit)
        .collect();
    let parallel_eval_comb = parallel_comb
        .map(|partitioned| {
            runtime_errors.extend(partitioned.runtime_errors);
            partitioned
                .units
                .into_iter()
                .map(|unit| LaneUnit::new(unit.lane, stable_unit(unit.unit)))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    (eval_comb_stateful, eval_comb, parallel_eval_comb)
}
