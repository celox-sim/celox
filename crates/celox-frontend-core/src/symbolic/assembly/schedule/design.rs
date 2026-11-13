//! The design a schedule is made for: its hierarchy and its relocated units.

use super::*;

/// Expand the hierarchy of `symbolic`, declare the hidden variables scheduling
/// needs from the start, and derive what every later phase reads of the
/// hierarchy. Returns the design and the per-instance initial values.
pub(super) fn expand_design(
    symbolic: SymbolicRtl,
    flatten_timing: bool,
    trace_opts: &FrontendTraceOptions,
    trace: &mut Option<&mut FrontendTrace>,
) -> Result<(Design, Vec<crate::symbolic::artifact::InstanceValue>), ParserError> {
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

    if let Some(t) = trace.as_deref_mut()
        && trace_opts.sim_modules
    {
        t.sim_modules = Some(modules.clone());
    }

    let (mut expanded, mut instance_modules, indexed_instances) = timed_sub!(
        flatten_timing,
        "expand_hierarchy",
        expand_hierarchy(&root_id, &modules)
    );
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
    let package_bindings =
        PackageBindings::new(&expanded, &instance_modules, &modules, &package_instances)?;
    let bindings = &package_bindings;
    let global_boundaries = timed_sub!(
        flatten_timing,
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
        flatten_timing,
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
    Ok((
        Design {
            modules,
            module_names,
            root_id,
            time_precision,
            expanded,
            instance_modules,
            indexed_instances,
            process_count,
            scheduler_slots,
            package_instances,
            instance_time_scales,
            bindings: package_bindings,
            global_boundaries,
            unpacked_element_widths,
            clock_domains,
            startup_ready,
        },
        instance_values,
    ))
}

/// Relocate the units of every instance of `design` into one program and
/// place its timing metadata. Returns the symbolic combinational logic, the
/// units, and the flip-flop parts and split commits some schedules use.
pub(super) fn relocate_design(
    design: &Design,
    parallel: &ParallelScheduleOptions,
    flatten_timing: bool,
    trace_opts: &FrontendTraceOptions,
    trace: &mut Option<&mut FrontendTrace>,
) -> Result<(CombLogic, Units, FfParts, SplitCommits), ParserError> {
    let Design {
        bindings,
        expanded,
        instance_modules,
        modules,
        module_names,
        indexed_instances,
        global_boundaries,
        unpacked_element_widths,
        clock_domains,
        ..
    } = design;
    let root_id = design.root_id;
    let startup_ready = design.startup_ready;

    let mut parallel_ff_units = HashMap::default();
    let mut split_commits = SplitCommits::default();
    let (
        global_arena,
        eval_apply_ffs,
        eval_only_ffs,
        apply_ffs,
        _ff_access_summaries,
        ff_runtime_relocations,
        comb_blocks,
        comb_observers,
        runtime_errors,
        runtime_event_sites,
        extern_functions,
        next_runtime_error_code,
        processes,
        (functions, function_frames),
    ) = timed_sub!(
        flatten_timing,
        "relocate_units",
        relocate_units(
            bindings,
            expanded,
            instance_modules,
            modules,
            &module_names[&root_id],
            indexed_instances,
            global_boundaries,
            unpacked_element_widths,
            clock_domains,
            parallel.enabled().then_some(&mut parallel_ff_units),
            &mut split_commits,
            trace_opts,
            trace,
            startup_ready,
        )
    )?;
    let timing = relocate_timing(bindings, instance_modules, modules, &processes);
    Ok((
        CombLogic {
            global_arena,
            comb_blocks,
            comb_observers,
        },
        Units {
            eval_apply_ffs,
            eval_only_ffs,
            apply_ffs,
            processes,
            functions,
            function_frames,
            ff_runtime_relocations,
            runtime_errors,
            runtime_event_sites,
            extern_functions,
            next_runtime_error_code,
            timing,
        },
        parallel_ff_units,
        split_commits,
    ))
}

/// The width of every variable and whether it is four-state.
/// Release the units of every module once each instance holds its relocated
/// copy of them (see [`relocate_design`]) and the package-driver check has
/// read them: nothing after relocation runs a module's own units, and a
/// large design would otherwise keep both copies until its schedule is built.
pub(super) fn release_module_units(modules: &mut HashMap<ModuleId, SimModule>) {
    for module in modules.values_mut() {
        module.eval_only_ff_blocks = Default::default();
        module.apply_ff_blocks = Default::default();
        module.eval_apply_ff_blocks = Default::default();
        module.split_commit_ff_blocks = Default::default();
        module.parallel_ff_parts = Default::default();
        module.functions = Default::default();
        for process in &mut module.processes {
            process.kernel.blocks = Default::default();
            process.kernel.register_map = Default::default();
        }
    }
}

pub(super) fn variable_shapes(design: &Design) -> HashMap<AbsoluteAddr, (usize, bool)> {
    let Design {
        instance_modules,
        modules,
        bindings,
        ..
    } = design;

    instance_modules
        .iter()
        .flat_map(|(&instance, module)| {
            modules[module].variables.iter().map(move |(&var_id, var)| {
                (
                    bindings.locate(instance, var_id),
                    (var.metadata.width, var.metadata.is_4state),
                )
            })
        })
        .collect()
}

/// The clock of every reset, both as the canonical events of their domains.
pub(super) fn reset_clocks(design: &Design) -> HashMap<AbsoluteAddr, AbsoluteAddr> {
    let Design {
        expanded,
        instance_modules,
        modules,
        bindings,
        clock_domains,
        ..
    } = design;

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
    reset_clock_map
}
