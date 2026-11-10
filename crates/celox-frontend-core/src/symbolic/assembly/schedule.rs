//! Scheduling the symbolic RTL of a design.

use super::*;

pub fn schedule_symbolic_rtl_reporting_loops(
    symbolic: SymbolicRtl,
    fused_ff_factory: Option<&dyn FusedFfLoweringFactory>,
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
    parallel: &ParallelScheduleOptions,
    trace_opts: &FrontendTraceOptions,
    mut trace: Option<&mut FrontendTrace>,
    loop_members: &mut Vec<LoopMember>,
) -> Result<ScheduledRtlOutput, ParserError> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_SOURCE_PROGRAM: AtomicU64 = AtomicU64::new(1);
    let _diagnostic = tracing::trace_span!(target: "celox::parallel", "source_program",
        source_program = NEXT_SOURCE_PROGRAM
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .expect("source program identity exhausted").to_string())
    .entered();
    let SymbolicRtl {
        mut modules,
        module_names,
        root_id,
        packages,
        instance_values,
    } = symbolic;
    let time_precision = modules
        .values()
        .filter_map(|module| module.time_precision)
        .min()
        .unwrap_or_default();
    let flatten_timing = trace_opts.phase_timing;
    macro_rules! timed_sub {
        ($label:expr, $body:expr) => {{
            if flatten_timing {
                let start = std::time::Instant::now();
                let result = $body;
                tracing::debug!("[flatten] {}: {:?}", $label, start.elapsed());
                result
            } else {
                $body
            }
        }};
    }

    if let Some(t) = trace.as_deref_mut()
        && trace_opts.sim_modules
    {
        t.sim_modules = Some(modules.clone());
    }

    let (mut expanded, mut instance_modules, indexed_instances) =
        timed_sub!("expand_hierarchy", expand_hierarchy(&root_id, &modules));
    let process_count = instance_modules
        .values()
        .map(|module| modules[module].processes.len())
        .sum();
    // At most one event per trigger signal of each instance's domains;
    // instances connected to one clock share its event. A design with more
    // events than its slots hold runs without the compiled scheduler.
    let expected_events = instance_modules
        .values()
        .map(|module| {
            let module = &modules[module];
            module
                .eval_apply_ff_blocks
                .keys()
                .chain(module.eval_only_ff_blocks.keys())
                .chain(module.apply_ff_blocks.keys())
                .flat_map(|triggers| std::iter::once(&triggers.clock).chain(&triggers.resets))
                .collect::<HashSet<_>>()
                .len()
        })
        .sum::<usize>();
    let scheduler_slots =
        declare_scheduler_slots(&mut modules, root_id, process_count, expected_events);
    // Each package with variables is one instance outside the hierarchy,
    // named by a path segment no instance name can take.
    let mut package_instances = HashMap::default();
    for (name, module_id) in &packages {
        let id = InstanceId(instance_modules.len());
        expanded.insert(InstancePath(vec![(format!("{name}::"), 0)]), id);
        instance_modules.insert(id, *module_id);
        package_instances.insert(*module_id, id);
    }
    let instance_time_scales = instance_modules
        .iter()
        .map(|(&instance, module)| (instance, modules[module].time_scale))
        .collect::<HashMap<_, _>>();
    let bindings =
        &PackageBindings::new(&expanded, &instance_modules, &modules, &package_instances)?;
    let global_boundaries = timed_sub!(
        "propagate_boundaries",
        propagate_boundaries(bindings, &expanded, &instance_modules, &modules)
    );
    let unpacked_element_widths = instance_modules
        .iter()
        .flat_map(|(&instance_id, &module_id)| {
            modules[&module_id]
                .variables
                .iter()
                .filter_map(move |(&var_id, variable)| {
                    let element_count = variable
                        .metadata
                        .array_dims
                        .iter()
                        .try_fold(1usize, |total, &dim| total.checked_mul(dim))?;
                    let element_width = variable.metadata.width.checked_div(element_count)?;
                    (element_count > 1 && element_width > 0)
                        .then_some((bindings.locate(instance_id, var_id), element_width))
                })
        })
        .collect::<HashMap<_, _>>();

    let clock_domains = timed_sub!(
        "unify_clock_domains",
        unify_clock_domains(bindings, &expanded, &instance_modules, &modules)
    );
    // Whether every automatically started process has reached its first
    // suspension: one combinational path computes it from their statuses,
    // and the gated `always_comb` units read only it.
    let gates_startup = instance_modules.values().any(|module| {
        modules[module].comb_blocks.iter().any(|path| {
            matches!(
                path.target,
                celox_slt::LogicPathTarget::Process {
                    after_initials: true,
                    ..
                }
            )
        })
    }) && instance_modules.values().any(|module| {
        // The same automatically started procedures the gate waits for.
        modules[module].processes.iter().any(|process| {
            matches!(
                process.slots.kind,
                celox_design::ProcessKind::Initializer | celox_design::ProcessKind::Regular
            )
        })
    });
    let startup_ready = gates_startup.then(|| {
        let var_id = SourceVarId(
            modules[&root_id]
                .variables
                .keys()
                .map(|id| id.0 + 1)
                .max()
                .unwrap_or(0),
        );
        declare_startup_ready(&mut modules, root_id, var_id);
        AbsoluteAddr {
            instance_id: expanded[&InstancePath(Vec::new())],
            var_id,
        }
    });
    let mut parallel_ff_units = HashMap::default();
    let mut split_commits = SplitCommits::default();
    let (
        mut global_arena,
        mut eval_apply_ffs,
        mut eval_only_ffs,
        mut apply_ffs,
        _ff_access_summaries,
        ff_runtime_relocations,
        mut comb_blocks,
        mut comb_observers,
        mut runtime_errors,
        runtime_event_sites,
        extern_functions,
        next_runtime_error_code,
        mut processes,
        (mut functions, function_frames),
    ) = timed_sub!(
        "relocate_units",
        relocate_units(
            bindings,
            &expanded,
            &instance_modules,
            &modules,
            &module_names[&root_id],
            &indexed_instances,
            &global_boundaries,
            &unpacked_element_widths,
            &clock_domains,
            parallel.enabled().then_some(&mut parallel_ff_units),
            &mut split_commits,
            trace_opts,
            &mut trace,
            startup_ready,
        )
    )?;
    let mut timing = relocate_timing(bindings, &instance_modules, &modules, &processes);
    let variables: HashMap<AbsoluteAddr, (usize, bool)> = instance_modules
        .iter()
        .flat_map(|(&instance, module)| {
            modules[module].variables.iter().map(move |(&var_id, var)| {
                (
                    bindings.locate(instance, var_id),
                    (var.metadata.width, var.metadata.is_4state),
                )
            })
        })
        .collect();
    let declaration_probes;
    let occurrence_probes: Vec<(VarAtomBase<AbsoluteAddr>, AbsoluteAddr)>;
    let counted = {
        // Flip-flop event domains and the aliases that resolve to them.
        let event_addresses: HashSet<AbsoluteAddr> = eval_apply_ffs
            .keys()
            .copied()
            .chain(
                clock_domains
                    .iter()
                    .filter(|(_, canonical)| eval_apply_ffs.contains_key(canonical))
                    .map(|(alias, _)| *alias),
            )
            .collect();
        let counted = counted_signals(&timing, &event_addresses);
        // A combinational process writing the signal or the gate of a gated
        // wait keeps its procedural order (IEEE 1800-2023 9.4.2.3).
        let gated: HashSet<AbsoluteAddr> = timing
            .wait_sites
            .iter()
            .flat_map(|site| {
                site.terms
                    .iter()
                    .zip(&site.gates)
                    .filter_map(|(&(signal, _), gate)| {
                        gate.as_ref()
                            .filter(|gate| gate.at_write)
                            .map(|gate| [signal, gate.signal])
                    })
            })
            .flatten()
            .collect();
        choose_comb_alternatives(
            bindings,
            &instance_modules,
            &modules,
            &counted,
            &gated,
            &mut comb_blocks,
        );
        // Count sensitivity occurrences before any optimizer can coalesce
        // consecutive writes whose final value is unchanged.
        let probes = sensitivity_probes(
            &comb_blocks,
            &comb_observers,
            &global_arena,
            |source, target| !four_state || variables[source].1 == variables[target].1,
        );
        declaration_probes = probes.clone();
        occurrence_probes = probes.clone();
        // Signals a flip-flop evaluation may store twice in its working
        // state. Each store is an update event (IEEE 1800-2023 4.3, 10.4.2)
        // that the commit of the final value alone would not show; the
        // working state is seeded with the stable value first, so a store
        // compares with the value its update replaces.
        let ff_multi_stored = ff_multi_stored(&eval_apply_ffs, &eval_only_ffs, &parallel_ff_units);
        // Each unit is instrumented once for all probes. Probe `p` is pass
        // `2p` over the stable state and pass `2p + 1` over a flip-flop
        // evaluation's working state, the order of instrumenting probe by
        // probe.
        let probe_codes: Vec<_> = probes
            .iter()
            .map(|&(source, counter)| {
                let width = source.access.msb - source.access.lsb + 1;
                let four_state = four_state && variables[&source.id].1;
                let offset = match unpacked_element_widths.get(&source.id) {
                    Some(&element_width)
                        if width > element_width
                            && source.access.lsb.is_multiple_of(element_width)
                            && width.is_multiple_of(element_width) =>
                    {
                        celox_sir::SIROffset::PackedElements {
                            bit_offset: source.access.lsb,
                            element_width,
                        }
                    }
                    _ => celox_sir::SIROffset::Static(source.access.lsb),
                };
                (counter, width, four_state, offset)
            })
            .collect();
        let mut probes_of: HashMap<AbsoluteAddr, Vec<usize>> = HashMap::default();
        for (index, (source, _)) in probes.iter().enumerate() {
            probes_of.entry(source.id).or_default().push(index);
        }
        // The first probe of `signal` whose `pass` is `after` or later.
        let first_probe = |signal: AbsoluteAddr, pass: fn(usize) -> usize, after: usize| {
            let indices = probes_of.get(&signal)?;
            indices
                .get(indices.partition_point(|&index| pass(index) < after))
                .copied()
        };
        fn probe_code<A>(
            probe: &(AbsoluteAddr, usize, bool, celox_sir::SIROffset),
            signal: A,
            counter: A,
        ) -> crate::watch::WatchCode<A> {
            let &(_, width, four_state, ref offset) = probe;
            crate::watch::WatchCode {
                signal,
                width,
                offset: offset.clone(),
                four_state,
                counters: vec![(CounterKind::Changes, counter)],
                gated: Vec::new(),
            }
        }
        let stable = |address| RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, address);
        let working = |address| RegionedAbsoluteAddr::from_absolute_addr(WORKING_REGION, address);
        let stable_code = |instruction: &SIRInstruction<RegionedAbsoluteAddr>, after: usize| {
            let address = crate::watch::write_destination(instruction)?;
            if address.region != STABLE_REGION {
                return None;
            }
            let index = first_probe(address.absolute_addr(), |index| 2 * index, after)?;
            Some((
                2 * index,
                probe_code(
                    &probe_codes[index],
                    stable(address.absolute_addr()),
                    stable(probe_codes[index].0),
                ),
            ))
        };
        // A flip-flop evaluation's stores to a signal of `ff_multi_stored`;
        // the commits that seed its working state are not writes of the
        // process.
        let evaluation_code = |instruction: &SIRInstruction<RegionedAbsoluteAddr>, after: usize| {
            if let SIRInstruction::Store(address, ..) = instruction
                && address.region == WORKING_REGION
                && ff_multi_stored.contains(&address.absolute_addr())
                && let Some(index) =
                    first_probe(address.absolute_addr(), |index| 2 * index + 1, after)
            {
                return Some((
                    2 * index + 1,
                    probe_code(
                        &probe_codes[index],
                        working(address.absolute_addr()),
                        stable(probe_codes[index].0),
                    ),
                ));
            }
            stable_code(instruction, after)
        };
        for process in &mut processes {
            crate::watch::instrument_writes_in_passes(&mut process.kernel, stable_code);
        }
        // A function unit's stores are a process's, whichever calls it.
        for function in &mut functions {
            crate::watch::instrument_writes_in_passes(&mut function.unit, stable_code);
        }
        for units in eval_apply_ffs
            .values_mut()
            .chain(eval_only_ffs.values_mut())
        {
            for unit in units {
                crate::watch::instrument_writes_in_passes(unit, evaluation_code);
            }
        }
        for units in apply_ffs.values_mut() {
            for unit in units {
                crate::watch::instrument_writes_in_passes(unit, stable_code);
            }
        }
        for parts in parallel_ff_units.values_mut() {
            for (_, part) in parts {
                crate::watch::instrument_writes_in_passes(&mut part.evaluate, evaluation_code);
                crate::watch::instrument_writes_in_passes(&mut part.apply, stable_code);
            }
        }
        for path in &mut comb_blocks {
            if let LogicPathTarget::Process { unit, .. } = &mut path.target {
                crate::watch::instrument_writes_in_passes(unit, |instruction, after| {
                    let &address = crate::watch::write_destination(instruction)?;
                    let index = first_probe(address, |index| index, after)?;
                    Some((
                        index,
                        probe_code(&probe_codes[index], address, probe_codes[index].0),
                    ))
                });
            }
        }
        let ff_written: HashSet<AbsoluteAddr> = eval_apply_ffs
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
            .flat_map(|unit| unit.blocks.values())
            .flat_map(|block| &block.instructions)
            .flat_map(super::super::written_objects)
            .map(|destination| destination.absolute_addr())
            .collect();
        let gated = gated_terms(&mut timing, &comb_blocks, &ff_written);
        let ff_units = eval_apply_ffs
            .values_mut()
            .chain(eval_only_ffs.values_mut())
            .chain(apply_ffs.values_mut())
            .flatten()
            .chain(
                parallel_ff_units
                    .values_mut()
                    .flatten()
                    .flat_map(|(_, part)| [&mut part.evaluate, &mut part.apply]),
            )
            .chain(
                split_commits
                    .values_mut()
                    .flatten()
                    .flatten()
                    .flat_map(|split| [&mut split.active, &mut split.nba]),
            );
        let counted = instrument_counted_signals(
            &timing,
            counted,
            &gated,
            &variables,
            four_state,
            &mut processes,
            &mut functions,
            &mut comb_blocks,
            ff_units,
        );
        // A counted signal without pending slots (a design without waits)
        // is counted at its commit, which sees only the final value. Count
        // each store of an evaluation that may store it twice as well.
        let pending: HashSet<AbsoluteAddr> = timing
            .pending_slots
            .iter()
            .map(|slots| slots.signal)
            .collect();
        // One pass per signal, in signal order; each pass watches only its
        // own signal.
        let regioned = RegionedAbsoluteAddr::from_absolute_addr;
        let multi_stored: HashMap<AbsoluteAddr, (usize, crate::watch::WatchCode<_>)> = counted
            .iter()
            .filter(|(signal, _)| ff_multi_stored.contains(signal) && !pending.contains(signal))
            .enumerate()
            .map(|(pass, (&signal, slots))| {
                let (width, is_4state) = variables[&signal];
                let code = crate::watch::WatchCode {
                    signal: regioned(WORKING_REGION, signal),
                    offset: celox_sir::SIROffset::Static(0),
                    width,
                    four_state: four_state && is_4state,
                    counters: CounterKind::ALL
                        .into_iter()
                        .map(|kind| (kind, regioned(STABLE_REGION, slots.counter(kind))))
                        .collect(),
                    gated: Vec::new(),
                };
                (signal, (pass, code))
            })
            .collect();
        if !multi_stored.is_empty() {
            for_each_ff_evaluation(
                &mut eval_apply_ffs,
                &mut eval_only_ffs,
                &mut parallel_ff_units,
                |unit| {
                    crate::watch::instrument_writes_in_passes(unit, |instruction, after| {
                        let SIRInstruction::Store(address, ..) = instruction else {
                            return None;
                        };
                        if address.region != WORKING_REGION {
                            return None;
                        }
                        let (pass, code) = multi_stored.get(&address.absolute_addr())?;
                        (*pass >= after).then(|| (*pass, code.clone()))
                    })
                },
            );
        }
        counted
    };
    // Combinational values and the variables they are computed from.
    let comb_sources = comb_sources(&comb_blocks, &global_arena);
    check_package_variable_drivers(
        bindings,
        &instance_modules,
        &modules,
        &package_instances,
        &comb_blocks,
    )?;
    let ignored_loops = parse_ignored_loops(ignored_loops, &instance_modules, &modules, &expanded);
    let true_loops = parse_true_loops(true_loops, &instance_modules, &modules, &expanded);

    flattening::refine_cyclic_logic_paths(
        &mut comb_blocks,
        &global_boundaries,
        &unpacked_element_widths,
        &mut global_arena,
    )?;

    // Build reset -> clock mapping with AbsoluteAddr
    let mut reset_clock_map: HashMap<AbsoluteAddr, AbsoluteAddr> = HashMap::default();
    for id in expanded.values() {
        let module_id = &instance_modules[id];
        let sim_module = &modules[module_id];
        for (reset_var_id, clock_var_id) in &sim_module.reset_clock_map {
            let reset_addr = bindings.locate(*id, *reset_var_id);
            let clock_addr = bindings.locate(*id, *clock_var_id);
            // Use canonical clock domain if available
            let canonical_clock = clock_domains
                .get(&clock_addr)
                .copied()
                .unwrap_or(clock_addr);
            let canonical_reset = clock_domains
                .get(&reset_addr)
                .copied()
                .unwrap_or(reset_addr);
            reset_clock_map.insert(canonical_reset, canonical_clock);
        }
    }

    let ClockAnalysis {
        topological_clocks,
        cascaded_clocks,
        derived_events,
        combinational_derived,
        independent_derived,
        combinationally_read,
        level_unread_by_domain,
    } = timed_sub!(
        "analyze_clock_dependencies",
        analyze_clock_dependencies(
            bindings,
            &mut eval_apply_ffs,
            &mut eval_only_ffs,
            &mut apply_ffs,
            &comb_blocks,
            &global_arena,
            &clock_domains,
            &expanded,
            &instance_modules,
            &modules,
        )
    );

    if let Some(t) = trace.as_deref_mut()
        && trace_opts.flattened_comb_blocks
    {
        t.flattened_comb_blocks = Some((comb_blocks.clone(), global_arena.clone()));
    }

    // Constant variable inlining: detect variables whose every LogicPath
    // is a constant, then replace all Input references with Constant nodes.
    // This eliminates Store→Load roundtrips for compile-time constants
    // (e.g. genvar-expanded parity-check matrices).
    celox_slt::const_inline::inline_constant_variables(&mut comb_blocks, &mut global_arena)?;
    apply_always_comb_previous_source_ordering(&mut comb_blocks);

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
        &mut comb_blocks,
        &mut comb_observers,
        &runtime_event_sites,
        &mut global_arena,
        &unpacked_element_widths,
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
        &global_arena,
        &comb_blocks,
        &comb_observers,
        &var_widths,
        &var_signedness,
    )
    .map_err(|error| ParserError::SltVerify {
        phase: "after flattening symbolic logic",
        error,
    })?;

    let fused_inputs = if fused_ff_factory.is_some() {
        let actions = build_fused_ff_actions(
            bindings,
            &modules,
            &instance_modules,
            &clock_domains,
            &ff_runtime_relocations,
        );
        let mut clock_arena = SLTNodeArena::<RegionedAbsoluteAddr>::new();
        let mut clock_node_cache = HashMap::default();
        let clock_comb_blocks = comb_blocks
            .iter()
            .map(|path| {
                path.map_addr(
                    &global_arena,
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

    // The event signals and their aliases: a partitioned settle keeps their
    // paths apart from the rest.
    let clock_nets: HashSet<AbsoluteAddr> = {
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
    };
    // Stores to event signals update shared trigger bytes; their writers stay
    // in one lane of a partitioned schedule.
    let lane0_objects = parallel.enabled().then(|| clock_nets.clone());
    let mut persistent_comb_state = HashSet::default();
    let mut comb_startups = BTreeMap::new();
    for path in &comb_blocks {
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
                        persistent_comb_state.extend(super::super::written_objects(inst).copied());
                    }
                }
            }
        }
    }
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
            &instance_modules,
            &modules,
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
                    .chain(&topological_clocks)
                    .chain(&clock_nets)
                    .copied()
                    .collect(),
            },
            &clock_domains,
            &timing,
            counted.keys().copied(),
            occurrence_probes
                .iter()
                .chain(&declaration_probes)
                .map(|(source, _)| source.id),
            &comb_observers,
            &comb_blocks,
            &unpacked_element_widths,
        );
        let report =
            celox_slt::net_inline::inline_nets(&mut comb_blocks, &mut global_arena, &pinned)?;
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
    let parallel_comb_blocks = parallel.enabled().then(|| comb_blocks.clone());
    // The flag object of a partitioned settle is declared once the
    // partitions are known; reserve its address now.
    let root_instance = expanded[&InstancePath(Vec::new())];
    let settle_flags =
        settle_partitioning(parallel, &comb_blocks, &global_arena, eval_apply_ffs.len()).map(
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
        );
    let sched_start = flatten_timing.then(std::time::Instant::now);
    let schedule = match scheduler::sort_with_unpacked_element_widths(
        comb_blocks,
        &global_arena,
        &ignored_loops,
        &true_loops,
        four_state,
        &var_widths,
        &unpacked_element_widths,
        next_runtime_error_code,
        settle_flags.clone(),
    ) {
        Ok(schedule) => schedule,
        Err(error) => {
            let (err_vars, err_path_idx) = module_variables(&modules);
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
            let source_locations = scheduler_source_locations(&error, &modules, &instance_modules);
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
            let error = error.map_addr(&global_arena, &mut target_arena, &|addr| {
                frontend_lookup.get_path(addr)
            })?;
            return Err(if source_locations.is_empty() {
                ParserError::Scheduler(error)
            } else {
                ParserError::SchedulerWithLocation {
                    error,
                    source_locations,
                }
            });
        }
    };
    let settle_partitions = schedule.partitions.clone();
    let settle_copies = schedule.copies.clone();
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
        t.settle_plan = Some(super::super::settle_plan::format(
            &settle_partitions,
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

    let settle_flags = settle_flags.filter(|_| !settle_partitions.is_empty());
    if let Some(settle) = &settle_flags {
        tracing::debug!(
            partitions = settle_partitions.len(),
            target_cost = settle.target_cost,
            "partitioned combinational settle"
        );
        // The flags carry what changed from one settle to the next.
        persistent_comb_state.insert(settle.flags);
        declare_settle_flags(
            &mut modules,
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
    let object_sizes = super::super::parallel::ObjectSizes {
        widths: &var_widths,
        element_widths: &unpacked_element_widths,
    };
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
                    let parents = instance_parents(&expanded);
                    let extra_costs = super::super::parallel::instance_ff_costs(
                        &parallel_ff_units,
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
                &global_arena,
                &ignored_loops,
                &true_loops,
                four_state,
                &var_widths,
                &unpacked_element_widths,
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
    // The comb schedule no longer refers to SLT nodes. Release its arena before
    // constructing the clock-fused schedules, whose arena already owns its
    // remapped nodes.
    drop(global_arena);
    if let Some(s) = sched_start {
        tracing::debug!("[flatten] scheduler::sort: {:?}", s.elapsed());
    }
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
    // Every event's combined comb-then-FF tick gets its own program, with
    // the combinational program in it, only while there are a handful of
    // events; that copy lets the FF part use comb values in registers.
    // Beyond that the copies would multiply the design by its event count,
    // so the ticks share the one `eval_comb` program instead.
    // A merged program drops comb stores the next full settle would redo;
    // a partitioned settle keeps what its skipped partitions stored.
    let merge_comb_apply =
        eval_apply_ffs.len() <= MERGED_COMB_APPLY_EVENTS && settle_flags.is_none();
    let mut eval_comb_apply_ffs = HashMap::default();
    let mut fused_direct_ff_writes = HashMap::default();
    if let (
        Some(factory),
        Some((
            actions,
            clock_arena,
            clock_comb_blocks,
            clock_var_widths,
            clock_unpacked_element_widths,
            clock_ignored_loops,
            clock_true_loops,
        )),
    ) = (fused_ff_factory, fused_inputs)
    {
        let mut fused_schedule_cache = HashMap::<
            Vec<usize>,
            (
                Option<Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
                Option<(
                    Vec<ExecutionUnit<RegionedAbsoluteAddr>>,
                    Vec<VarAtomBase<RegionedAbsoluteAddr>>,
                )>,
            ),
        >::default();
        let schedule = |paths, actions| {
            let mut ff_lowering = factory.create(actions)?;
            match scheduler::sort_clock(
                paths,
                &clock_arena,
                &clock_ignored_loops,
                &clock_true_loops,
                four_state,
                &clock_var_widths,
                &clock_unpacked_element_widths,
                next_runtime_error_code,
                ff_lowering.as_mut(),
            ) {
                Ok(schedule) => Ok(schedule),
                Err(scheduler::ClockSortError::Lowering(error)) => Err(error),
                Err(scheduler::ClockSortError::Scheduler(error)) => {
                    let mut error_arena = SLTNodeArena::new();
                    let error =
                        error.map_addr(&clock_arena, &mut error_arena, &|addr| addr.to_string())?;
                    Err(ParserError::Scheduler(error))
                }
            }
        };
        for (trigger, actions) in actions {
            let action_ids = actions.iter().map(|action| action.id).collect::<Vec<_>>();
            if let Some((ff_only, fused)) = fused_schedule_cache.get(&action_ids) {
                if let Some(ff_only) = ff_only {
                    eval_apply_ffs.insert(trigger, ff_only.clone());
                }
                if let Some((units, direct_ff_writes)) = fused {
                    eval_comb_apply_ffs.insert(trigger, units.clone());
                    fused_direct_ff_writes.insert(trigger, direct_ff_writes.clone());
                }
                continue;
            }
            let fused_start = flatten_timing.then(std::time::Instant::now);
            // The ordinary tick already settled comb. Reuse the existing
            // old-state dependency planner with no comb paths: acyclic readers
            // run before writers, and only cyclic/aliased ranges need staging.
            // Keep the merged eval-then-apply fallback for adapters without
            // source-aware lowering and the existing single-action fast path.
            let ff_only = if actions.len() > 1 {
                Some(schedule(Vec::new(), actions.clone())?.execution_units)
            } else {
                None
            };
            let fused = if merge_comb_apply {
                let fused = schedule(clock_comb_blocks.clone(), actions)?;
                Some((fused.execution_units, fused.direct_ff_writes))
            } else {
                None
            };
            if let Some(start) = fused_start {
                tracing::debug!("[flatten] scheduler::sort_clock: {:?}", start.elapsed());
            }
            fused_schedule_cache.insert(action_ids, (ff_only.clone(), fused.clone()));
            if let Some(ff_only) = ff_only {
                eval_apply_ffs.insert(trigger, ff_only);
            }
            if let Some((units, direct_ff_writes)) = fused {
                eval_comb_apply_ffs.insert(trigger, units);
                fused_direct_ff_writes.insert(trigger, direct_ff_writes);
            }
        }
    }

    if let Some(t) = trace
        && trace_opts.scheduled_units
    {
        t.scheduled_units = Some(eval_comb.clone());
    }

    // The unified function is the normal fast path.  Split evaluator/apply
    // functions are needed only when scheduling can evaluate several active
    // domains or must cascade through a derived clock.
    let active_ff_domains = eval_apply_ffs
        .values()
        .filter(|units| !units.is_empty())
        .count();
    // With process kernels, a round in which a zero delay is pending after
    // domains triggered evaluates them before the inactive region and
    // publishes them in the NBA region (IEEE 1800-2023 4.5). Only those
    // rounds use the split units; the fused units stay the default.
    let needs_split_path =
        active_ff_domains > 1 || !cascaded_clocks.is_empty() || !processes.is_empty();
    let parallel_sir = parallel.enabled().then(|| {
        let mut events = eval_apply_ffs
            .iter()
            .filter(|(_, units)| !units.is_empty())
            .map(|(event, _)| *event)
            .collect::<Vec<_>>();
        events.sort_unstable();
        let eval_apply_ffs = super::super::parallel::plan_parallel_ff_kernels(
            &events,
            parallel_ff_units,
            lane0_objects
                .as_ref()
                .expect("parallel scheduling computes lane-0 objects"),
            &parallel_eval_comb,
            object_sizes,
            parallel,
        );
        for (event, kernel) in &eval_apply_ffs {
            let lanes = kernel
                .units()
                .map(|unit| unit.lane)
                .collect::<BTreeSet<_>>();
            tracing::debug!(
                "[parallel] event={event} evaluations={} applications={} lanes={lanes:?}",
                kernel.evaluations.len(),
                kernel.applications.len(),
            );
        }
        ParallelSirProgram {
            lanes: parallel.lanes,
            eval_comb: parallel_eval_comb,
            eval_apply_ffs,
        }
    });
    // With process kernels, a domain whose blocking results another unit
    // observes commits them with its evaluation, in the active region, and
    // only its nonblocking results in the NBA region (IEEE 1800-2023 4.5,
    // 10.4). Every other domain commits both together, as before.
    let mut active_commits = Vec::new();
    if !processes.is_empty() {
        let events: HashSet<AbsoluteAddr> = eval_apply_ffs
            .keys()
            .copied()
            .chain(clock_domains.keys().copied())
            .collect();
        active_commits = split_observed_commits(
            &mut eval_only_ffs,
            &mut apply_ffs,
            split_commits,
            &comb_sources,
            processes.iter().map(|process| &process.kernel),
            timing.wait_sites.iter().flat_map(WaitSite::reads),
            &events,
            &clock_domains,
        );
    }
    let (eval_only_ffs, apply_ffs) = if needs_split_path {
        (eval_only_ffs, apply_ffs)
    } else {
        (HashMap::default(), HashMap::default())
    };

    // Size the declared private variables before building either metadata
    // projection, so reflection and storage agree on the raw snapshot widths.
    if let Some(scheduler) = &scheduler_slots {
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
    let (mod_vars, mod_path_idx) = module_variables(&modules);
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
    if let Some(settle) = &settle_flags {
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
    initial_memory_values.extend(super::super::comb_startup::initial_values(
        comb_startups.values(),
        &declaration_values,
        &state_objects,
        four_state,
    ));
    let runtime_comb_observers: Vec<RuntimeCombObserver<AbsoluteAddr>> = comb_observers
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
        .collect();
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
                &eval_apply_ffs,
                &eval_comb_apply_ffs,
                &eval_only_ffs,
                &apply_ffs,
            ] {
                observed.extend(ffs.keys().copied());
            }
            through_comb(observed, &comb_sources)
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
            &timing,
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
        select_nba_paths(&mut timing, &staged, &mut processes);
    }
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
    let events = EventTopology {
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
    };
    let event_aliases = events.aliases.clone();
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

    let process_frames: Vec<_> = process_frames
        .into_iter()
        .map(|frame| frame.map(|frame| frame.map(project)))
        .collect();
    let process_slots = process_slots
        .into_iter()
        .map(|slots| slots.map(project))
        .collect();
    let watches: Vec<EventWatch<StateAddr>> = {
        let event_kinds = ElaboratedDesign {
            time_precision,
            instance_time_scales: HashMap::default(),
            state_objects: state_objects.clone(),
            events: events.clone(),
            initial_state: Vec::new(),
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
    };
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
    let settle_partitions = settle_flags.map(|settle| {
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
        for site in &wait_sites {
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
    });
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
    // The runtime reads counters, the signals waits watch and staged
    // nonblocking values directly; no unit loads a wait's signal.
    let mut timing_read_roots: HashSet<StateAddr> = watches
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
        .collect();
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
    if let Some(startup) = &declaration_startup {
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
            .chain(&initial_copies)
            .flat_map(|&(variable, copy)| [variable, copy]),
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
