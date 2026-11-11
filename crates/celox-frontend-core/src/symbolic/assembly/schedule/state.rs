//! The state of a scheduled design: hidden slots, initial values, and the
//! checks and choices made once its units are final.

use super::*;

/// Size the declared scheduler slots for the counted signals and the waits
/// of the design.
pub(super) fn size_scheduler_slots(
    design: &mut Design,
    counted: &BTreeMap<AbsoluteAddr, CounterSlots<AbsoluteAddr>>,
    timing: &TimingMetadata,
    four_state: bool,
) {
    let root_id = design.root_id;
    let scheduler_slots = &design.scheduler_slots;
    let instance_modules = &design.instance_modules;
    let modules = &mut design.modules;

    // Size the declared private variables before building either metadata
    // projection, so reflection and storage agree on the raw snapshot widths.
    if let Some(scheduler) = scheduler_slots {
        // The low plane holds event levels. Above it, preserve the reference
        // loop's three occurrence-counter baselines per counted signal.
        modules
            .get_mut(&root_id)
            .unwrap()
            .variables
            .get_mut(&scheduler.levels)
            .expect("declared scheduler levels")
            .metadata
            .width += counted.len() * 3 * 32;
        let mut widths = vec![celox_design::SCHEDULER_FLAG_WIDTH; scheduler.baseline.len()];
        for site in &timing.wait_sites {
            let terms = site
                .terms
                .iter()
                .enumerate()
                .map(|(index, (signal, _))| {
                    let metadata = &modules[&instance_modules[&signal.instance_id]].variables
                        [&signal.var_id]
                        .metadata;
                    // Aliased arrays and arrays with whole-value accesses
                    // can use packed storage. Reserve their logical width
                    // too; the laid-out scheduler decides whether a strided
                    // representation can actually use this snapshot.
                    (
                        metadata.width,
                        four_state && metadata.is_4state,
                        counted.contains_key(signal),
                        site.gate(index).is_some_and(|gate| gate.counters.is_some()),
                    )
                })
                .collect::<Vec<_>>();
            if let Some(layout) =
                celox_design::schedule::CompiledWaitLayout::with_gate_counters(&terms)
            {
                widths[site.process] = widths[site.process].max(layout.bits);
            }
        }
        for (baseline, width) in scheduler.baseline.iter().zip(widths) {
            modules
                .get_mut(&root_id)
                .unwrap()
                .variables
                .get_mut(baseline)
                .expect("declared scheduler baseline")
                .metadata
                .width = width;
        }
    }
}

/// The initial values of the state, the state objects, and the declaration
/// startup of a design with initializer processes.
pub(super) fn initial_state(
    design: &Design,
    instance_values: Vec<crate::symbolic::artifact::InstanceValue>,
    settle_flags: &Option<scheduler::SettlePartitioning<AbsoluteAddr>>,
    settle_partitions: &[scheduler::SettlePartition<AbsoluteAddr>],
    processes: &[RelocatedProcess],
    comb_startups: &BTreeMap<AbsoluteAddr, celox_slt::CombStartup<AbsoluteAddr>>,
    declaration_probes: Vec<(VarAtomBase<AbsoluteAddr>, AbsoluteAddr)>,
    four_state: bool,
) -> Result<
    (
        Vec<InitialStateValue<AbsoluteAddr>>,
        HashMap<AbsoluteAddr, VariableMetadata>,
        Option<celox_design::DeclarationStartup<AbsoluteAddr>>,
    ),
    ParserError,
> {
    let Design {
        instance_modules,
        package_instances,
        modules,
        bindings,
        expanded,
        ..
    } = design;
    let process_count = design.process_count;

    // A variable bound to a package variable has no state object of its own:
    // its initial writes, such as a constant `initial` assignment, go to the
    // package's object. Declaration initializers of packages come first
    // (IEEE 1800-2023 10.5).
    let mut initial_instances = instance_modules.iter().collect::<Vec<_>>();
    initial_instances.sort_unstable_by_key(|(instance, module)| {
        (!package_instances.contains_key(*module), instance.0)
    });
    let declaration_values: Vec<InitialStateValue<AbsoluteAddr>> = initial_instances
        .iter()
        .flat_map(|&(&instance_id, module_id)| {
            modules[module_id]
                .declaration_initial_memory_values
                .iter()
                .filter(move |init| {
                    !bindings.is_bound(&AbsoluteAddr {
                        instance_id,
                        var_id: init.address,
                    })
                })
                .map(move |init| InitialStateValue {
                    address: bindings.locate(instance_id, init.address),
                    data: init.data.clone(),
                })
        })
        .collect();
    let folded_initials: Vec<InitialStateValue<AbsoluteAddr>> = initial_instances
        .iter()
        .flat_map(|&(&instance_id, module_id)| {
            modules[module_id]
                .folded_initial_memory_values
                .iter()
                .map(move |init| InitialStateValue {
                    address: bindings.locate(instance_id, init.address),
                    data: init.data.clone(),
                })
        })
        .collect();
    // A write through a binding, such as a folded `initial` assignment
    // through a hierarchical name, follows the declaration values of every
    // instance, its target's among them.
    let (mut initial_memory_values, bound_initials): (Vec<_>, Vec<_>) = initial_instances
        .into_iter()
        .flat_map(|(&instance_id, module_id)| {
            modules[module_id]
                .initial_memory_values
                .iter()
                .map(move |init| {
                    (
                        bindings.is_bound(&AbsoluteAddr {
                            instance_id,
                            var_id: init.address,
                        }),
                        InitialStateValue {
                            address: bindings.locate(instance_id, init.address),
                            data: init.data.clone(),
                        },
                    )
                })
        })
        .partition(|(bound, _)| !bound);
    let mut initial_memory_values: Vec<InitialStateValue<AbsoluteAddr>> = initial_memory_values
        .drain(..)
        .chain(bound_initials)
        .map(|(_, init)| init)
        .collect();
    for value in instance_values {
        let instance_id = *expanded.get(&InstancePath(value.path)).ok_or_else(|| {
            ParserError::illegal_context(
                "instance value",
                "a value of an instance that does not exist".to_string(),
                None,
            )
        })?;
        let width = modules[&instance_modules[&instance_id]].variables[&value.variable]
            .metadata
            .width;
        initial_memory_values.push(InitialStateValue {
            address: bindings.locate(instance_id, value.variable),
            data: InitialStateData::Packed {
                value: value.value,
                mask: BigUint::default(),
                written_mask: (BigUint::from(1u8) << width) - 1u8,
            },
        });
    }
    if let Some(settle) = settle_flags {
        // Every partition runs at the first settle, and every wait is
        // checked at the first check.
        let partitions = settle_partitions.len();
        let flags = (0..partitions).chain(
            (0..process_count).map(|process| celox_design::settle_wait_flag(partitions, process)),
        );
        let value = flags.fold(BigUint::default(), |value, flag| {
            value | (BigUint::from(1u8) << (flag * 8))
        });
        initial_memory_values.push(InitialStateValue {
            address: settle.flags,
            data: InitialStateData::Packed {
                value,
                mask: BigUint::default(),
                written_mask: (BigUint::from(1u8)
                    << (celox_design::settle_flag_count(partitions, process_count) * 8))
                    - 1u8,
            },
        });
    }
    let state_objects: HashMap<AbsoluteAddr, VariableMetadata> = instance_modules
        .iter()
        .flat_map(|(&instance_id, module_id)| {
            modules[module_id]
                .variables
                .iter()
                .map(move |(&var_id, variable)| {
                    (
                        AbsoluteAddr {
                            instance_id,
                            var_id,
                        },
                        variable.metadata.clone(),
                    )
                })
        })
        .filter(|(address, _)| !bindings.is_bound(address))
        .collect();
    let declaration_startup = processes
        .iter()
        .any(|process| process.slots.kind == celox_design::ProcessKind::Initializer)
        .then(|| {
            let targets: BTreeSet<_> = folded_initials.iter().map(|init| init.address).collect();
            let mut reset = targets
                .iter()
                .map(|&address| InitialStateValue {
                    address,
                    data: InitialStateData::Packed {
                        value: BigUint::default(),
                        mask: BigUint::default(),
                        written_mask: (BigUint::from(1u8) << state_objects[&address].width) - 1u8,
                    },
                })
                .collect::<Vec<_>>();
            reset.extend(
                declaration_values
                    .iter()
                    .filter(|init| targets.contains(&init.address))
                    .cloned(),
            );
            celox_design::DeclarationStartup {
                reset,
                ordinary: folded_initials,
                snapshots: comb_startups
                    .values()
                    .flat_map(|startup| startup.inputs.iter().copied())
                    .collect(),
                counter_snapshots: comb_startups
                    .values()
                    .flat_map(|startup| startup.counters.iter().copied())
                    .collect(),
                probes: declaration_probes,
            }
        });
    // Arm wildcard processes against declaration-time state, before continuous
    // drivers, folded initial statements, or the first host write can change
    // their inputs. The resulting constants use the ordinary initialization
    // path on every backend, including native images.
    initial_memory_values.extend(super::super::super::comb_startup::initial_values(
        comb_startups.values(),
        &declaration_values,
        &state_objects,
        four_state,
    ));
    Ok((initial_memory_values, state_objects, declaration_startup))
}

/// What the runtime keeps of every combinational observer.
pub(super) fn runtime_comb_observers(
    comb_observers: &[CombObserver<AbsoluteAddr>],
) -> Vec<RuntimeCombObserver<AbsoluteAddr>> {
    comb_observers
        .iter()
        .map(|observer| RuntimeCombObserver {
            site_id: observer.site_id,
            activation_group: observer.activation_group,
            sensitivity: observer.sensitivity.clone(),
            occurrence_counters: observer
                .sensitivity_counters
                .iter()
                .map(|&(_, counter)| counter)
                .collect(),
            written_inputs: observer.written_inputs.clone(),
        })
        .collect()
}

/// Keep the staged or the queued path of every switched nonblocking site.
pub(super) fn stage_nba_sites(
    design: &Design,
    clocks: &ClockAnalysis,
    units: &mut Units,
    eval_comb: &[ExecutionUnit<RegionedAbsoluteAddr>],
    eval_comb_apply_ffs: &FfUnits,
    runtime_comb_observers: &[RuntimeCombObserver<AbsoluteAddr>],
    comb_sources: &HashMap<AbsoluteAddr, HashSet<AbsoluteAddr>>,
) {
    let Design {
        clock_domains,
        expanded,
        modules,
        instance_modules,
        bindings,
        ..
    } = design;
    let ClockAnalysis {
        topological_clocks, ..
    } = clocks;
    let eval_apply_ffs = &units.eval_apply_ffs;
    let eval_only_ffs = &units.eval_only_ffs;
    let apply_ffs = &units.apply_ffs;
    let processes = &mut units.processes;
    let timing = &mut units.timing;

    // Nonblocking sites that can stage their value instead of queueing it
    // (`NbaSite::pending`). Staged values are published after the queue,
    // so a staged target must be one whose update order nothing observes:
    // no wait, counter, event domain or comb observer reads it, directly or
    // through combinational logic.
    if !timing.nba_switches.is_empty() {
        let observed = {
            let mut observed: HashSet<AbsoluteAddr> = timing
                .wait_sites
                .iter()
                .flat_map(WaitSite::reads)
                .chain(timing.counter_slots.iter().map(|slots| slots.signal))
                .chain(
                    clock_domains
                        .iter()
                        .flat_map(|(&alias, &canonical)| [alias, canonical]),
                )
                .chain(topological_clocks.iter().copied())
                .chain(
                    runtime_comb_observers
                        .iter()
                        .flat_map(|observer| observer.sensitivity.iter().map(|atom| atom.id)),
                )
                .collect();
            for ffs in [
                eval_apply_ffs,
                eval_comb_apply_ffs,
                eval_only_ffs,
                apply_ffs,
            ] {
                observed.extend(ffs.keys().copied());
            }
            through_comb(observed, comb_sources)
        };
        // The ports of the top module, which the host drives.
        let top_ports: HashSet<AbsoluteAddr> = expanded
            .get(&InstancePath(Vec::new()))
            .map(|&root| {
                modules[&instance_modules[&root]]
                    .variables
                    .iter()
                    .filter(|(_, variable)| {
                        matches!(
                            variable.kind,
                            crate::VariableKind::Input
                                | crate::VariableKind::Output
                                | crate::VariableKind::Inout
                        )
                    })
                    .map(|(&var_id, _)| bindings.locate(root, var_id))
                    .collect()
            })
            .unwrap_or_default();
        let staged = staged_nba_sites(
            timing,
            eval_comb
                .iter()
                .chain(eval_apply_ffs.values().flatten())
                .chain(eval_comb_apply_ffs.values().flatten())
                .chain(eval_only_ffs.values().flatten())
                .chain(apply_ffs.values().flatten())
                .chain(processes.iter().map(|process| &process.kernel)),
            &top_ports,
            &observed,
        );
        select_nba_paths(timing, &staged, processes);
    }
}

/// Reject a function body that stores into state only the units of
/// processes and flip-flops are instrumented for.
pub(super) fn check_function_stores(
    design: &Design,
    clocks: &ClockAnalysis,
    units: &Units,
    eval_comb: &[ExecutionUnit<RegionedAbsoluteAddr>],
    eval_comb_apply_ffs: &FfUnits,
) -> Result<(), ParserError> {
    let Design {
        instance_modules,
        modules,
        bindings,
        clock_domains,
        ..
    } = design;
    let ClockAnalysis {
        topological_clocks, ..
    } = clocks;
    let Units {
        eval_apply_ffs,
        eval_only_ffs,
        apply_ffs,
        functions,
        ..
    } = units;

    // The per-store instrumentation above rewrites the stores of function
    // bodies for the watch and occurrence counters, as those of process
    // kernels. The rest (flip-flop pending counts, nonblocking staging,
    // the triggers of events, comb-observer captures) is for the units of
    // processes and flip-flops, so a frontend keeps function bodies clear of
    // that storage (`SimModule::function_excluded_storage`). A body that
    // stores there anyway is rejected rather than miscompiled.
    if !functions.is_empty() {
        let instrumented: HashSet<AbsoluteAddr> = instance_modules
            .iter()
            .flat_map(|(&instance, module)| {
                modules[module]
                    .function_excluded_storage()
                    .into_iter()
                    .map(move |var_id| bindings.locate(instance, var_id))
            })
            .collect();
        // The events: those of flip-flop domains, the aliases that resolve
        // to them, and every event the design orders. A process's store there is counted as the domain's
        // occurrence, as a function unit's store is, by the per-store
        // instrumentation above; a combinational or flip-flop unit's store
        // triggers the domain (`ScheduledRtl::inject_triggers`), which no
        // function body has. So only a function such a unit calls must not
        // store there. A variable that is only wired to another, as through
        // a continuous assignment to a port, is no event: `clock_domains`
        // names every end of such wiring.
        let domains: HashSet<AbsoluteAddr> = eval_apply_ffs
            .keys()
            .chain(eval_comb_apply_ffs.keys())
            .chain(eval_only_ffs.keys())
            .chain(apply_ffs.keys())
            .copied()
            .collect();
        let events: HashSet<AbsoluteAddr> = clock_domains
            .iter()
            .filter(|(_, canonical)| domains.contains(canonical))
            .flat_map(|(&alias, &canonical)| [alias, canonical])
            .chain(domains.iter().copied())
            .chain(topological_clocks.iter().copied())
            .collect();
        let calls = |unit: &ExecutionUnit<RegionedAbsoluteAddr>| -> Vec<usize> {
            unit.blocks
                .values()
                .flat_map(|block| &block.instructions)
                .filter_map(|instruction| match instruction {
                    SIRInstruction::Call { function, .. } => Some(function.0 as usize),
                    _ => None,
                })
                .collect()
        };
        let mut pending: Vec<usize> = eval_comb
            .iter()
            .chain(eval_apply_ffs.values().flatten())
            .chain(eval_comb_apply_ffs.values().flatten())
            .chain(eval_only_ffs.values().flatten())
            .chain(apply_ffs.values().flatten())
            .flat_map(calls)
            .collect();
        let mut called_by_units = HashSet::default();
        while let Some(function) = pending.pop() {
            if called_by_units.insert(function)
                && let Some(callee) = functions.get(function)
            {
                pending.extend(calls(&callee.unit));
            }
        }
        let instrumented_store = functions.iter().enumerate().any(|(index, function)| {
            let triggered = called_by_units.contains(&index);
            function
                .unit
                .blocks
                .values()
                .flat_map(|block| &block.instructions)
                .any(|instruction| match instruction {
                    SIRInstruction::Store(_, _, _, _, triggers, captures)
                        if !triggers.is_empty() || !captures.is_empty() =>
                    {
                        true
                    }
                    SIRInstruction::Commit(..) => true,
                    instruction => instruction.memory_write().is_some_and(|access| {
                        let address = access.addr.absolute_addr();
                        instrumented.contains(&address) || (triggered && events.contains(&address))
                    }),
                })
        });
        if instrumented_store {
            return Err(ParserError::illegal_context(
                "function unit",
                "a function body stores into instrumented state",
                None,
            ));
        }
    }
    Ok(())
}
