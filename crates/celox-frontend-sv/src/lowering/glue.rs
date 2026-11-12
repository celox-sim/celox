//! Port glue between a module and its child instances.

use super::*;

pub(crate) fn attach_instance_glue(
    module: &mut SimModule,
    lowered: &LoweredSvModule,
    current_key: &LoweredSvModuleKey,
    module_ids: &HashMap<LoweredSvModuleKey, ModuleId>,
    lowered_modules: &HashMap<ModuleId, LoweredSvModule>,
    four_state: bool,
    allow_inferred_latches: bool,
    foreign: bool,
    hierarchical_targets: &HashSet<String>,
    warnings: &mut Vec<crate::warning::PendingWarning>,
    calls: Option<&mut recursion::CallGraph>,
) -> Result<(), ParserError> {
    let mut signal_names = lowered.signal_names.clone();
    let mut parent_variables = lowered.variables.clone();
    let mut implicit_output_signals = HashSet::default();
    let mut resolved_instances = Vec::new();
    for instance in &lowered.instances {
        let child_key = LoweredSvModuleKey::instance_key(instance);
        let Some(child_id) = module_ids.get(&child_key).copied() else {
            return Err(unsupported_sv_instance(instance.module_name.clone()));
        };
        if &child_key == current_key {
            return Err(ParserError::unsupported(
                sv::SV_FRONTEND_TRACKING_ISSUE,
                LoweringPhase::SimulatorParser,
                "recursive systemverilog module instantiation",
                instance.module_name.clone(),
                None,
            ));
        }
        let Some(child) = lowered_modules.get(&child_id) else {
            return Err(unsupported_sv_instance(instance.module_name.clone()));
        };
        let mut authored_connections = expand_structure_connections(
            &instance.port_connections,
            &lowered.source,
            &child.source,
        )?
        .into_owned();
        bind_port_patterns(&mut authored_connections, child)?;
        let connections = match instance.array_element {
            Some((position, count)) => {
                module
                    .indexed_instance_names
                    .insert(instance.instance_name.clone());
                if instance.array_index_base != 0 {
                    module
                        .instance_index_bases
                        .insert(instance.instance_name.clone(), instance.array_index_base);
                }
                array_element_connections(
                    &authored_connections,
                    child,
                    position,
                    count,
                    &parent_variables,
                    &signal_names,
                    &lowered.constants,
                    &lowered.parameter_types,
                )?
            }
            None => authored_connections,
        };
        check_tagged_port_connections(&lowered.source, &child.source, &connections)?;
        ensure_parent_output_signals(
            module,
            &mut parent_variables,
            &mut signal_names,
            &mut implicit_output_signals,
            lowered.implicit_nets_allowed,
            &lowered.source,
            &lowered.constants,
            &lowered.parameter_types,
            child,
            &connections,
        )?;
        resolved_instances.push((instance, child_id, child, connections));
    }
    // Materialize state-reading input actuals as continuous assignments in
    // the parent, including reads through callees and omitted defaults.
    // Stateful lowering guards activation, so time alone (or an unrelated
    // input) cannot reevaluate a time-only actual.
    let mut port_assignments = Vec::new();
    let tables = procedural::ModuleTables::new(&lowered.source);
    {
        let mut pm = procedural::ProcModule::new(
            &lowered.source,
            &tables,
            &mut parent_variables,
            &mut signal_names,
            &lowered.constants,
            &lowered.parameter_types,
            four_state,
            warnings,
        );
        for (_, _, child, connections) in &mut resolved_instances {
            for connection in connections {
                let Some(actual) = &connection.actual_expr else {
                    continue;
                };
                if !pm.reads_simulation_state(&[sv::ir::Stmt::Eval(actual.clone())]) {
                    continue;
                }
                let Some(port) =
                    child
                        .port_order
                        .iter()
                        .map(|id| &child.variables[id])
                        .find(|var| {
                            var.path.join(".") == connection.formal
                                && var.kind == VariableKind::Input
                        })
                else {
                    continue;
                };
                let (id, name) = pm.temp(
                    "port_time",
                    port.width,
                    port.signed,
                    four_state && port.is_4state,
                );
                port_assignments.push((
                    id,
                    vec![sv::ir::Stmt::Assign {
                        lhs: sv::ir::LValue::Ident(name.clone()),
                        rhs: actual.clone(),
                        nonblocking: false,
                    }],
                ));
                connection.actual_expr = Some(sv::ir::Expr::Ident(name));
            }
        }
        for id in &pm.created {
            module
                .variables
                .insert(*id, pm.variables[id].to_symbolic_variable());
        }
    }
    // An event expression is one more continuous assignment to a hidden
    // buffer the processes watch.
    port_assignments.extend(lowered.event_assignments.iter().cloned());
    // The repeated writes of flip-flops and combinational processes are
    // counted only in a design with a wait, so a design without one, such
    // as pure RTL, lowers exactly as before.
    let waits = lowered_modules
        .values()
        .any(|module| !module.sim_module.wait_sites.is_empty());
    // An `always_comb` alternative is kept only if a wait or a flip-flop
    // domain counts one of its targets. Within the module, they name its
    // wait terms, its triggers and whole-variable copies of them; beyond
    // it, ports, instance connections, package variables and the signals
    // hierarchical names reach, in either module. Every counter
    // a design declares also keeps nonblocking writes queued, so a design
    // with a nonblocking switch lowers every alternative, and so does a
    // module of a design with foreign modules, which this module does not see.
    let watch_seeds = (waits
        && !foreign
        && !lowered_modules
            .values()
            .any(|module| !module.sim_module.nba_switches.is_empty()))
    .then(|| {
        // A gate is read at each write of its term's signal too.
        let mut seeds: HashSet<SourceVarId> = module
            .wait_sites
            .iter()
            .flat_map(|site| {
                site.terms
                    .iter()
                    .map(|&(signal, _)| signal)
                    .chain(site.gates.iter().flatten().map(|gate| gate.signal))
            })
            .collect();
        let triggers = module
            .eval_only_ff_blocks
            .keys()
            .chain(module.apply_ff_blocks.keys())
            .chain(module.eval_apply_ff_blocks.keys())
            .chain(module.split_commit_ff_blocks.keys())
            .chain(module.parallel_ff_parts.keys());
        for trigger in triggers {
            seeds.insert(trigger.clock);
            seeds.extend(trigger.resets.iter().copied());
        }
        for &(copy, signal) in &lowered.event_copies {
            seeds.extend([copy, signal]);
        }
        seeds.extend(lowered.package_variables.iter().map(|(id, _, _)| *id));
        let instantiated = lowered_modules.values().any(|other| {
            other
                .instances
                .iter()
                .any(|instance| &LoweredSvModuleKey::instance_key(instance) == current_key)
        });
        if instantiated {
            seeds.extend(
                parent_variables
                    .iter()
                    .filter(|(_, variable)| variable.kind.is_port())
                    .map(|(id, _)| *id),
            );
        }
        let mut names = HashSet::default();
        for (_, _, _, connections) in &resolved_instances {
            for actual in connections.iter().filter_map(|c| c.actual_expr.as_ref()) {
                procedural::expr_idents(actual, &mut names);
            }
        }
        seeds.extend(
            names
                .iter()
                .filter_map(|name| signal_names.get(name).copied()),
        );
        seeds.extend(port_assignments.iter().map(|(target, _)| *target));
        seeds.extend(module.hierarchical_bindings.keys().copied());
        seeds.extend(
            hierarchical_targets
                .iter()
                .filter_map(|name| signal_names.get(name).copied()),
        );
        seeds
    });
    let gated_waits = lowered_modules.values().any(|module| {
        module
            .sim_module
            .wait_sites
            .iter()
            .any(|site| site.gates.iter().any(Option::is_some))
    });
    let (comb_blocks, arena, created, comb_observers, comb_sites, comb_alternatives) =
        lower_comb_processes(
            &lowered.source,
            &tables,
            &port_assignments,
            &mut parent_variables,
            &mut signal_names,
            &lowered.constants,
            &lowered.parameter_types,
            four_state,
            allow_inferred_latches,
            &mut module.runtime_errors,
            &mut module.extern_functions,
            warnings,
            waits,
            watch_seeds.as_ref(),
            gated_waits,
            calls,
        )
        .map_err(|error| match error {
            sv::AnalyzerError::MemoryFile(detail) => ParserError::MemoryFile {
                detail,
                source_location: None,
            },
            error => ParserError::unsupported(
                error.tracking_issue(),
                LoweringPhase::SimulatorParser,
                "systemverilog combinational process lowering",
                error.to_string(),
                None,
            ),
        })?;
    let (mut comb_blocks, mut arena) = (comb_blocks, arena);
    push_event_copies(&lowered.event_copies, &mut comb_blocks, &mut arena)?;
    module.comb_boundaries = comb_boundaries(&comb_blocks);
    module.comb_blocks = comb_blocks;
    // Signals a kernel or an imperative combinational unit may store twice
    // between two runtime checks get counters; assembly uses them only for
    // the ones a flip-flop domain or wait watches.
    let mut candidates = Vec::new();
    for process in &module.processes {
        candidates.extend(celox_frontend_core::watch::multi_store_candidates(
            &process.kernel,
            |address| (address.region == STABLE_REGION).then_some(address.var_id),
        ));
    }
    for path in &module.comb_blocks {
        if let LogicPathTarget::Process { unit, .. } = &path.target {
            candidates.extend(celox_frontend_core::watch::multi_store_candidates(
                unit,
                |address| Some(*address),
            ));
        }
    }
    // A flip-flop evaluation stores each update in the working state; two
    // of one execution are two update events (IEEE 1800-2023 4.3, 10.4.2).
    for unit in module.eval_only_ff_blocks.values() {
        candidates.extend(celox_frontend_core::watch::multi_store_only_candidates(
            unit,
            |address| (address.region == WORKING_REGION).then_some(address.var_id),
        ));
    }
    module.arena = arena;
    // Combinational event sites follow the flip-flop ones, including the
    // events run-time loops emit.
    let site_base = module.runtime_event_sites.len() as u32;
    if site_base != 0 {
        let len = module.arena.len();
        module
            .arena
            .remap_for_fold_effect_sites(0..len, |site_id, fatal_error_code| {
                site_id
                    .checked_add(site_base)
                    .map(|site_id| Some((site_id, fatal_error_code.map(|_| i64::from(site_id)))))
                    .ok_or(celox_slt::SLTNodeArenaEditError::SiteIdOverflow {
                        site_id,
                        offset: site_base,
                    })
            })
            .map_err(|error| {
                ParserError::illegal_context(
                    "systemverilog loop runtime-event remap",
                    error.to_string(),
                    None,
                )
            })?;
    }
    for path in &mut module.comb_blocks {
        if let LogicPathTarget::Process { unit, .. } = &mut path.target {
            for block in unit.blocks.values_mut() {
                for inst in &mut block.instructions {
                    if let SIRInstruction::RuntimeEvent { site_id, .. } = inst {
                        *site_id += site_base;
                    }
                }
            }
        }
    }
    for mut observer in comb_observers {
        observer.site_id += site_base;
        observer.activation_group += site_base;
        module.comb_observers.push(observer);
    }
    module
        .runtime_event_sites
        .extend(comb_sites.into_iter().map(|mut site| {
            if let Some(celox_design::DeferredSite::Action { queue, .. }) = &mut site.deferred {
                *queue += site_base;
            }
            site
        }));
    for id in created {
        module
            .variables
            .insert(id, parent_variables[&id].to_symbolic_variable());
    }
    // Combinational processes may read the time too.
    module.time_slot = signal_names.get(TIME_SLOT).copied();
    let mut sample_shadows: Vec<_> = parent_variables
        .iter()
        .filter_map(|(&copy, variable)| variable.sampled_from.map(|variable| (variable, copy)))
        .collect();
    sample_shadows.sort_by_key(|&(variable, copy)| (variable.0, copy.0));
    module.sample_shadows = sample_shadows;
    let mut initial_copies: Vec<_> = parent_variables
        .iter()
        .filter_map(|(&copy, variable)| variable.history_of.map(|variable| (variable, copy)))
        .collect();
    initial_copies.sort_by_key(|&(variable, copy)| (variable.0, copy.0));
    module.initial_copies = initial_copies;
    for (instance, child_id, child, connections) in resolved_instances {
        let glue = build_instance_glue(
            &parent_variables,
            &signal_names,
            &lowered.constants,
            &lowered.parameter_types,
            child,
            &connections,
            four_state,
        )?;
        module
            .glue_blocks
            .entry(instance.instance_name.clone())
            .or_default()
            .push(GlueBlock {
                module_id: child_id,
                input_ports: glue.0,
                output_ports: glue.1,
                arena: glue.2,
            });
    }
    if waits {
        candidates.extend(lowered.repeated_ff_writes.iter().map(|&(signal, _)| signal));
        for alternative in &comb_alternatives {
            candidates.extend(alternative.candidates.iter().copied());
        }
    }
    module.comb_alternatives = comb_alternatives;
    // Probe results are activation-local values, never global occurrences.
    let probe_results: HashSet<_> = module
        .processes
        .iter()
        .filter_map(|process| process.frame.as_ref())
        .flat_map(|frame| {
            frame
                .event_probes
                .iter()
                .flat_map(|probe| probe.scratch.iter().copied())
        })
        .collect();
    candidates.retain(|signal| !probe_results.contains(signal));
    // Monitors observe the entire time slot, not just the interval between
    // process suspensions. Even single writes therefore require counters.
    candidates.extend(
        module
            .host_request_sites
            .iter()
            .filter(|site| {
                site.kind == celox_design::HostRequestKind::FileIo
                    && matches!(site.operation, 17 | 18)
            })
            .flat_map(|site| {
                site.args
                    .iter()
                    .skip(usize::from(site.operation == 18))
                    .map(|arg| arg.id)
            }),
    );
    module.counter_slots =
        celox_frontend_core::watch::declare_counter_slots(&mut module.variables, candidates);
    if waits {
        module.pending_slots = celox_frontend_core::watch::declare_pending_slots(
            &mut module.variables,
            lowered.repeated_ff_writes.iter().copied(),
        );
    }
    Ok(())
}

fn ensure_parent_output_signals(
    parent: &mut SimModule,
    parent_variables: &mut HashMap<SourceVarId, SvVariable>,
    parent_signal_names: &mut HashMap<String, SourceVarId>,
    implicit_output_signals: &mut HashSet<String>,
    implicit_nets_allowed: bool,
    parent_source: &sv::ir::Module,
    parent_constants: &HashMap<String, i128>,
    parent_parameter_types: &HashMap<String, (usize, bool)>,
    child: &LoweredSvModule,
    connections: &[LoweredSvPortConnection],
) -> Result<(), ParserError> {
    for child_port_id in &child.port_order {
        let child_var = &child.variables[child_port_id];
        let formal = child_var.path.join(".");
        let Some(connection) = connections
            .iter()
            .find(|connection| connection.formal == formal)
        else {
            continue;
        };
        if child_var.kind == VariableKind::Inout {
            check_inout_port_actual(
                connection,
                parent_variables,
                parent_signal_names,
                implicit_output_signals,
                parent_source,
                parent_constants,
                parent_parameter_types,
            )?;
        }
        if child_port_direction(child, child_var) != Some(VariableKind::Output) {
            continue;
        }
        let Some(actual) = connection
            .actual_expr
            .as_ref()
            .and_then(simple_output_lvalue_ident)
        else {
            continue;
        };
        if parent_signal_names.contains_key(actual) {
            if implicit_output_signals.contains(actual) {
                return Err(ParserError::illegal_context(
                    "systemverilog output port connection",
                    format!("multiple child outputs drive implicit net `{actual}`"),
                    None,
                ));
            }
            continue;
        }
        if parent_constants.contains_key(actual) {
            return Err(ParserError::illegal_context(
                "systemverilog output port connection",
                format!("cannot drive parameter `{actual}`"),
                None,
            ));
        }
        if !implicit_nets_allowed {
            return Err(ParserError::illegal_context(
                "systemverilog output port connection",
                format!("implicit net `{actual}` disabled by `default_nettype none"),
                None,
            ));
        }
        if !ModuleDrivers::new(parent_source, parent_constants, parent_parameter_types)
            .local(actual)
            .is_empty()
        {
            return Err(ParserError::illegal_context(
                "systemverilog output port connection",
                format!("multiple net drivers for `{actual}`"),
                None,
            ));
        }
        let mut next_id = SourceVarId::default();
        while parent.variables.contains_key(&next_id) {
            next_id.0 += 1;
        }
        parent_signal_names.insert(actual.to_string(), next_id);
        implicit_output_signals.insert(actual.to_string());
        let variable = SvVariable {
            float: None,

            value_kind: celox_design::ValueKind::Bits,
            path: vec![actual.to_string()],
            width: 1,
            signed: false,
            is_4state: true,
            is_unpacked_struct: false,
            packed_ranges: Vec::new(),
            array_dims: Vec::new(),
            unpacked_ranges: Vec::new(),
            domain_kind: DomainKind::Other,
            kind: VariableKind::Variable,
            type_kind: PortTypeKind::Logic,
            source: None,
            hidden: false,
            force_control: None,
            sampled_from: None,
            history_of: None,
            activation_scratch: false,
        };
        parent
            .variables
            .insert(next_id, variable.to_symbolic_variable());
        parent_variables.insert(next_id, variable);
    }
    Ok(())
}

/// Rejects a variable or an expression connected to an `inout` port: only
/// nets may be on either side of one (IEEE 1800-2023 23.3.3.2). An
/// undeclared identifier is an implicit net.
fn check_inout_port_actual(
    connection: &LoweredSvPortConnection,
    parent_variables: &HashMap<SourceVarId, SvVariable>,
    parent_signal_names: &HashMap<String, SourceVarId>,
    implicit_nets: &HashSet<String>,
    parent_source: &sv::ir::Module,
    parent_constants: &HashMap<String, i128>,
    parent_parameter_types: &HashMap<String, (usize, bool)>,
) -> Result<(), ParserError> {
    let Some(actual) = connection.actual_expr.as_ref() else {
        return Ok(());
    };
    let is_net = |name: &str| {
        implicit_nets.contains(name)
            || parent_source
                .signals()
                .iter()
                .any(|signal| signal.name() == name && signal.is_net())
            || parent_source
                .ports()
                .iter()
                .any(|port| port.name() == name && port.is_net())
    };
    let error = |detail: String| {
        Err(ParserError::illegal_context(
            "systemverilog inout port connection",
            detail,
            None,
        ))
    };
    let Some(accesses) = output_lvalue_accesses(
        actual,
        parent_variables,
        parent_signal_names,
        parent_constants,
        parent_parameter_types,
    ) else {
        if simple_output_lvalue_ident(actual)
            .is_some_and(|name| !parent_signal_names.contains_key(name))
        {
            return Ok(());
        }
        return error(format!(
            "inout port `{}` must connect to a net, not `{}`",
            connection.formal, connection.actual
        ));
    };
    for OutputLvalueAccess { signal, .. } in accesses {
        let name = parent_variables[&signal].path.join(".");
        if !is_net(&name) {
            return error(format!(
                "inout port `{}` must connect to a net, not variable `{name}`",
                connection.formal
            ));
        }
    }
    Ok(())
}

type SvGlue = (
    Vec<(Vec<SourceVarId>, LogicPath<GlueAddr>)>,
    Vec<(Vec<SourceVarId>, LogicPath<GlueAddr>)>,
    SLTNodeArena<GlueAddr>,
);

/// The type of `variable` when it is an unpacked array.
fn unpacked_array_type(variable: &SvVariable) -> Option<sv::typecheck::UnpackedArrayType> {
    let element_width = unpacked_element_width(variable)?;
    Some(sv::typecheck::UnpackedArrayType {
        dims: variable.array_dims.clone(),
        element_width,
        signed: variable.signed,
        four_state: variable.is_4state,
        // Lowered variables keep only the bits of their elements, so a port
        // connection compares the integral shape of the element types.
        element: None,
    })
}

/// Rejects an unpacked array connected to an unpacked array input or output
/// port of an incompatible type: such a connection is an assignment-like
/// context (IEEE 1800-2023 10.8, 23.3.3), which requires equivalent element
/// types and equal element counts (7.6).
fn check_unpacked_port_connection(
    connection: &LoweredSvPortConnection,
    child: &LoweredSvModule,
    parent_variables: &HashMap<SourceVarId, SvVariable>,
    parent_signal_names: &HashMap<String, SourceVarId>,
) -> Result<(), ParserError> {
    let Some(sv::ir::Expr::Ident(actual)) = connection.actual_expr.as_ref() else {
        return Ok(());
    };
    let Some(port) = child
        .signal_names
        .get(&connection.formal)
        .and_then(|id| child.variables.get(id))
        .filter(|port| matches!(port.kind, VariableKind::Input | VariableKind::Output))
    else {
        return Ok(());
    };
    let (Some(target), Some(actual_type)) = (
        unpacked_array_type(port),
        parent_signal_names
            .get(actual)
            .and_then(|id| parent_variables.get(id))
            .and_then(unpacked_array_type),
    ) else {
        return Ok(());
    };
    if actual_type.is_assignment_compatible_with(&target) {
        return Ok(());
    }
    let error = sv::AnalyzerError::IncompatibleUnpackedArray {
        context: format!("connection of port `{}`", connection.formal),
        actual: actual_type.to_string(),
        target: target.to_string(),
    };
    Err(ParserError::illegal_context(
        "systemverilog port connection",
        error.to_string(),
        None,
    ))
}

fn build_instance_glue(
    parent_variables: &HashMap<SourceVarId, SvVariable>,
    parent_signal_names: &HashMap<String, SourceVarId>,
    parent_constants: &HashMap<String, i128>,
    parent_parameter_types: &HashMap<String, (usize, bool)>,
    child: &LoweredSvModule,
    connections: &[LoweredSvPortConnection],
    four_state: bool,
) -> Result<SvGlue, ParserError> {
    let mut input_ports = Vec::new();
    let mut output_ports = Vec::new();
    let mut arena = SLTNodeArena::<GlueAddr>::new();

    let mut connected_formals = HashSet::default();
    for connection in connections {
        let matches = child
            .port_order
            .iter()
            .filter(|port_id| child.variables[port_id].path.join(".") == connection.formal)
            .count();
        if matches != 1 || !connected_formals.insert(connection.formal.clone()) {
            return Err(ParserError::unsupported(
                sv::SV_FRONTEND_TRACKING_ISSUE,
                LoweringPhase::SimulatorParser,
                "unknown or duplicate systemverilog child port connection",
                connection.formal.clone(),
                None,
            ));
        }
        check_unpacked_port_connection(connection, child, parent_variables, parent_signal_names)?;
    }

    for child_port_id in &child.port_order {
        let child_var = &child.variables[child_port_id];
        let formal = child_var.path.join(".");
        let connection = connections
            .iter()
            .find(|connection| connection.formal == formal);
        let width = child_var.width;
        let Some(direction) = child_port_direction(child, child_var) else {
            return Err(unsupported_sv_inout(formal));
        };
        match direction {
            VariableKind::Input => {
                let collapse_unknown_literal = !four_state
                    && connection
                        .and_then(|item| item.actual_expr.as_ref())
                        .is_some_and(expr_is_unknown_literal);
                let (mut expr, sources, source_ids) = if let Some(actual_expr) =
                    connection.and_then(|item| item.actual_expr.as_ref())
                {
                    let actual_expr = expr_for_state_mode(actual_expr, four_state);
                    let actual = connection.map_or("", |item| item.actual.as_str());
                    let from =
                        floating::precision(&actual_expr, parent_variables, parent_signal_names);
                    let to = child_var.float;
                    let (expr, sources, source_ids) = lower_glue_parent_expr(
                        &actual_expr,
                        parent_variables,
                        parent_signal_names,
                        parent_constants,
                        parent_parameter_types,
                        &mut arena,
                        if from.is_some() || to.is_some() {
                            None
                        } else {
                            Some(width)
                        },
                        Some(sv_glue_expr_is_signed(
                            &actual_expr,
                            parent_variables,
                            parent_signal_names,
                            parent_parameter_types,
                        )),
                    )
                    .ok_or_else(|| {
                        ParserError::unsupported(
                            sv::SV_FRONTEND_TRACKING_ISSUE,
                            LoweringPhase::SimulatorParser,
                            "systemverilog input port connection",
                            format!("{formal} -> {actual}"),
                            None,
                        )
                    })?;
                    let source_signed = sv_glue_expr_is_signed(
                        &actual_expr,
                        parent_variables,
                        parent_signal_names,
                        parent_parameter_types,
                    );
                    let expr = floating::convert(
                        &mut arena,
                        expr,
                        from,
                        to,
                        width,
                        if from.is_none() {
                            source_signed
                        } else {
                            child_var.signed
                        },
                        true,
                    )
                    .ok_or_else(|| {
                        ParserError::unsupported(
                            sv::SV_FRONTEND_TRACKING_ISSUE,
                            LoweringPhase::SimulatorParser,
                            "real input port conversion",
                            formal.clone(),
                            None,
                        )
                    })?;
                    let expr = coerce_node_width(
                        &mut arena,
                        expr,
                        Some(width),
                        sv_glue_expr_is_signed(
                            &actual_expr,
                            parent_variables,
                            parent_signal_names,
                            parent_parameter_types,
                        ),
                    )?;
                    (expr, sources, source_ids)
                } else if let Some(default) = child
                    .source
                    .ports()
                    .iter()
                    .find(|port| port.name() == formal)
                    .and_then(|port| port.default())
                {
                    // IEEE 1800-2023 23.2.2.4: defaults belong to the child's
                    // parameter scope, and apply only to omitted connections.
                    let literal = sv::typecheck::context_size_const_integral_literal(
                        default,
                        &child.constants,
                        &child.parameter_types,
                        width,
                        child_var.signed,
                    )
                    .ok_or_else(|| {
                        ParserError::illegal_context(
                            "systemverilog input port default",
                            formal.clone(),
                            None,
                        )
                    })?;
                    (
                        arena.alloc(SLTNode::Constant(
                            literal.value,
                            literal.mask,
                            width,
                            child_var.signed,
                        ))?,
                        HashSet::default(),
                        Vec::new(),
                    )
                } else {
                    let unknown_mask = (BigUint::from(1u8) << width) - BigUint::from(1u8);
                    (
                        arena.alloc(SLTNode::Constant(
                            BigUint::default(),
                            unknown_mask,
                            width,
                            false,
                        ))?,
                        HashSet::default(),
                        Vec::new(),
                    )
                };
                if !child_var.is_4state || collapse_unknown_literal {
                    expr = arena.alloc(SLTNode::Unary(UnaryOp::ToTwoState, expr))?;
                }
                input_ports.push((
                    source_ids,
                    LogicPath {
                        transparent_copy: false,
                        target: LogicPathTarget::Var(VarAtomBase::new(
                            GlueAddr::Child(*child_port_id),
                            0,
                            width - 1,
                        )),
                        expr,
                        sources,
                        address_sources: HashSet::default(),
                        previous_sources: HashSet::default(),
                        local_inputs: Vec::new(),
                        order_before: HashSet::default(),
                        comb_capture_enable_sites: Vec::new(),
                        comb_capture_enable_always: false,
                        pre_lower_nodes: Vec::new(),
                    },
                ));
            }
            VariableKind::Output => {
                let Some(connection) = connection else {
                    continue;
                };
                let actual = connection.actual.as_str();
                let Some(actual_expr) = connection.actual_expr.as_ref() else {
                    continue;
                };
                let Some(accesses) = output_lvalue_accesses(
                    actual_expr,
                    parent_variables,
                    parent_signal_names,
                    parent_constants,
                    parent_parameter_types,
                ) else {
                    return Err(ParserError::unsupported(
                        sv::SV_FRONTEND_TRACKING_ISSUE,
                        LoweringPhase::SimulatorParser,
                        "systemverilog output port lvalue connection",
                        format!("{formal} -> {actual}: {actual_expr:?}"),
                        None,
                    ));
                };
                let target_width = accesses.iter().try_fold(0usize, |width, target| {
                    let access = &target.access;
                    width.checked_add(access.msb - access.lsb + 1)
                });
                let Some(target_width) = target_width.filter(|target_width| *target_width != 0)
                else {
                    return Err(ParserError::unsupported(
                        sv::SV_FRONTEND_TRACKING_ISSUE,
                        LoweringPhase::SimulatorParser,
                        "systemverilog output port lvalue connection",
                        format!("{formal} -> {actual}: {actual_expr:?}"),
                        None,
                    ));
                };
                let child_input = arena.alloc(SLTNode::Input {
                    variable: GlueAddr::Child(*child_port_id),
                    signed: child_var.signed,
                    index: Vec::new(),
                    access: BitAccess::new(0, width - 1),
                })?;
                let target_float = if let [target] = accesses.as_slice() {
                    parent_variables[&target.signal].float
                } else {
                    None
                };
                if child_var.float.is_some() && accesses.len() != 1 {
                    return Err(ParserError::unsupported(
                        sv::SV_FRONTEND_TRACKING_ISSUE,
                        LoweringPhase::SimulatorParser,
                        "real output port concatenation",
                        formal.clone(),
                        None,
                    ));
                }
                let target_signed = if let [target] = accesses.as_slice() {
                    parent_variables[&target.signal].signed
                } else {
                    child_var.signed
                };
                let child_input = floating::convert(
                    &mut arena,
                    child_input,
                    child_var.float,
                    target_float,
                    target_width,
                    if child_var.float.is_none() {
                        child_var.signed
                    } else {
                        target_signed
                    },
                    true,
                )
                .ok_or_else(|| {
                    ParserError::unsupported(
                        sv::SV_FRONTEND_TRACKING_ISSUE,
                        LoweringPhase::SimulatorParser,
                        "real output port conversion",
                        formal.clone(),
                        None,
                    )
                })?;
                let child_node = coerce_node_width(
                    &mut arena,
                    child_input,
                    Some(target_width),
                    child_var.signed,
                )?;
                let mut child_lsb = target_width;
                for OutputLvalueAccess {
                    signal: parent_signal_id,
                    access,
                    is_2state,
                } in accesses
                {
                    let parent_var = &parent_variables[&parent_signal_id];
                    let part_width = access.msb - access.lsb + 1;
                    child_lsb -= part_width;
                    let child_expr = if child_lsb == 0 && part_width == target_width {
                        child_node
                    } else {
                        arena.alloc(SLTNode::Slice {
                            expr: child_node,
                            access: BitAccess::new(child_lsb, child_lsb + part_width - 1),
                        })?
                    };
                    let mut expr = coerce_node_width(
                        &mut arena,
                        child_expr,
                        Some(part_width),
                        child_var.signed,
                    )?;
                    if is_2state || !parent_var.is_4state {
                        expr = arena.alloc(SLTNode::Unary(UnaryOp::ToTwoState, expr))?;
                    }
                    let mut sources = HashSet::default();
                    sources.insert(VarAtomBase::new(
                        GlueAddr::Child(*child_port_id),
                        0,
                        width - 1,
                    ));
                    output_ports.push((
                        vec![parent_signal_id],
                        LogicPath {
                            transparent_copy: false,
                            target: LogicPathTarget::Var(VarAtomBase::new(
                                GlueAddr::Parent(parent_signal_id),
                                access.lsb,
                                access.msb,
                            )),
                            expr,
                            sources,
                            address_sources: HashSet::default(),
                            previous_sources: HashSet::default(),
                            local_inputs: Vec::new(),
                            order_before: HashSet::default(),
                            comb_capture_enable_sites: Vec::new(),
                            comb_capture_enable_always: false,
                            pre_lower_nodes: Vec::new(),
                        },
                    ));
                }
            }
            _ => {}
        }
    }

    Ok((input_ports, output_ports, arena))
}

/// The direction in which the connection of `port` carries its value. An
/// `inout` port carries the value of its net out of the child when the child
/// drives the net, and into it otherwise. `None` is an inout port that the
/// child's frontend does not describe.
fn child_port_direction(child: &LoweredSvModule, port: &SvVariable) -> Option<VariableKind> {
    if port.kind != VariableKind::Inout {
        return Some(port.kind);
    }
    let name = port.path.join(".");
    child
        .source
        .ports()
        .iter()
        .any(|source| source.name() == name && source.direction() == sv::ir::PortDirection::Inout)
        .then(|| {
            if child.inout_driven_inside.contains(&name) {
                VariableKind::Output
            } else {
                VariableKind::Input
            }
        })
}

fn simple_output_lvalue_ident(expr: &sv::ir::Expr) -> Option<&str> {
    match expr {
        sv::ir::Expr::Ident(name) => Some(name),
        sv::ir::Expr::Resize { expr, .. } => simple_output_lvalue_ident(expr),
        _ => None,
    }
}

fn output_lvalue_access(
    expr: &sv::ir::Expr,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Option<(SourceVarId, BitAccess)> {
    match expr {
        sv::ir::Expr::Ident(name) => {
            let id = *name_to_id.get(name)?;
            let variable = variables.get(&id)?;
            Some((id, BitAccess::new(0, variable.width.checked_sub(1)?)))
        }
        sv::ir::Expr::Resize { expr, .. } => {
            output_lvalue_access(expr, variables, name_to_id, constants, parameter_types)
        }
        sv::ir::Expr::Select { expr, msb, lsb, .. } => {
            let sv::ir::Expr::Ident(name) = &**expr else {
                return None;
            };
            let id = *name_to_id.get(name)?;
            let variable = variables.get(&id)?;
            let msb = sv::typecheck::eval_const_expr_with_types(msb, constants, parameter_types)?;
            let lsb = sv::typecheck::eval_const_expr_with_types(lsb, constants, parameter_types)?;
            let (msb, lsb) = packed_expr_select_offsets(expr, msb, lsb, variables, name_to_id)?;
            let access = BitAccess::new(msb.min(lsb), msb.max(lsb));
            (access.msb < variable.width).then_some((id, access))
        }
        _ => None,
    }
}

pub(super) struct OutputLvalueAccess {
    pub(super) signal: SourceVarId,
    pub(super) access: BitAccess,
    is_2state: bool,
}

pub(super) fn output_lvalue_accesses(
    expr: &sv::ir::Expr,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Option<Vec<OutputLvalueAccess>> {
    match expr {
        sv::ir::Expr::Concat(parts) if !parts.is_empty() => {
            let mut accesses = Vec::new();
            for part in parts {
                accesses.extend(output_lvalue_accesses(
                    part,
                    variables,
                    name_to_id,
                    constants,
                    parameter_types,
                )?);
            }
            Some(accesses)
        }
        sv::ir::Expr::Resize { expr, .. } => {
            output_lvalue_accesses(expr, variables, name_to_id, constants, parameter_types)
        }
        sv::ir::Expr::Unary {
            op: sv::ir::UnaryOp::ToTwoState,
            expr,
        } => {
            // A bit member carries a read conversion in the analyzed expression.
            // For an output connection, retain its target and apply that conversion
            // to the incoming value, even when the enclosing struct is four-state.
            let mut accesses =
                output_lvalue_accesses(expr, variables, name_to_id, constants, parameter_types)?;
            for access in &mut accesses {
                access.is_2state = true;
            }
            Some(accesses)
        }
        _ => output_lvalue_access(expr, variables, name_to_id, constants, parameter_types).map(
            |(signal, access)| {
                vec![OutputLvalueAccess {
                    signal,
                    access,
                    is_2state: false,
                }]
            },
        ),
    }
}
