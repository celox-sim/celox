//! Lowering of one specialized module: its variables, ports and processes.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn lower_module(
    module: std::borrow::Cow<'_, sv::ir::Module>,
    four_state: bool,
    implicit_nets_allowed: bool,
    ff_parts: bool,
    warnings: &mut Vec<crate::warning::PendingWarning>,
    force_signals: &[String],
    level_lists: LevelLists<'_>,
    foreign_writes: &HashMap<String, hierarchy::ForeignWrites>,
    reflected_paths: &HashMap<String, sv::InterfaceMemberPath>,
    shared: SharedAlways<'_>,
    calls: Option<&mut recursion::CallGraph>,
) -> Result<LoweredSvModule, sv::AnalyzerError> {
    lower_module_with_overrides(
        module,
        &[],
        four_state,
        implicit_nets_allowed,
        ff_parts,
        warnings,
        force_signals,
        level_lists,
        foreign_writes,
        reflected_paths,
        shared,
        calls,
    )
}

fn needs_host_request(
    module: &sv::ir::Module,
    body: &[sv::ir::Stmt],
    visited: &mut HashSet<String>,
) -> bool {
    let mut found = false;
    let mut calls = Vec::new();
    for stmt in body {
        stmt.walk(&mut |stmt| {
            if let sv::ir::Stmt::SystemTask { task, .. } = stmt {
                found |= task.is_host_request();
            }
            procedural::stmt_calls(stmt, &mut calls);
        });
    }
    if found {
        return true;
    }
    let mut seen_defaults = procedural::SeenDefaults::default();
    while let Some((callee, args)) = calls.pop() {
        match callee {
            sv::ir::Callee::System(function) if function.is_host_request() => return true,
            sv::ir::Callee::String(sv::ir::StringOp::Format { .. }) => return true,
            sv::ir::Callee::Subroutine(name) => {
                let Some(subroutine) = module.subroutines().iter().find(|sub| sub.name == name)
                else {
                    continue;
                };
                // Different calls can select different defaults, even when the
                // subroutine body has already been inspected.
                let omitted: Vec<_> = (0..subroutine.params.len())
                    .map(|position| matches!(args.get(position), None | Some(None)))
                    .collect();
                if visited.insert(format!("defaults:{name}:{omitted:?}")) {
                    procedural::default_calls(subroutine, &args, &mut calls, &mut seen_defaults);
                }
                if visited.insert(format!("body:{name}"))
                    && needs_host_request(module, &subroutine.body, visited)
                {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// An edge signal's role and the clock of the declaring process. Keep these
/// together when host requests move a process out of the retained FF list.
pub(super) struct FfEventRole {
    pub(super) signal: String,
    pub(super) kind: DomainKind,
    pub(super) clock: String,
    pub(super) clock_kind: DomainKind,
}

/// `ff_parts` additionally keeps every `always_ff` process as an
/// independently evaluated part, for lane-partitioned builds.
#[allow(clippy::too_many_arguments)]
fn lower_module_with_overrides(
    module: std::borrow::Cow<'_, sv::ir::Module>,
    parameter_overrides: &[LoweredSvParameterOverride],
    four_state: bool,
    implicit_nets_allowed: bool,
    ff_parts: bool,
    warnings: &mut Vec<crate::warning::PendingWarning>,
    force_signals: &[String],
    level_lists: LevelLists<'_>,
    foreign_writes: &HashMap<String, hierarchy::ForeignWrites>,
    reflected_paths: &HashMap<String, sv::InterfaceMemberPath>,
    shared: SharedAlways<'_>,
    mut calls: Option<&mut recursion::CallGraph>,
) -> Result<LoweredSvModule, sv::AnalyzerError> {
    let parameter_types = module_parameter_types(&module);
    let constants = module_constants_with_overrides(&module, parameter_overrides);
    // The module's processes are rewritten in place when the lowering owns
    // the module; a shared module is copied for the first rewrite.
    let mut module = module;
    if let LevelLists::Lowered { demoted } = level_lists
        && module.comb_processes().iter().any(|process| {
            process.kind() == sv::ir::CombProcessKind::AlwaysStar
                && demoted.contains(&process.creation_order())
        })
    {
        module = std::borrow::Cow::Owned(module.into_owned().run_comb_processes_as_processes(
            |_, process| demoted.contains(&process.creation_order()),
        ));
    }
    let sharing = share_always_variables(&module, &constants, &parameter_types, shared, warnings);
    let shared_process_variables = match sharing {
        Some((shared_module, names)) => {
            module = std::borrow::Cow::Owned(shared_module);
            Some(names)
        }
        None => None,
    };
    let shared_process_variables = shared_process_variables.as_ref();
    // The rewrite visits the edge processes in order, once each.
    let assertion_control = module
        .signals()
        .iter()
        .any(|signal| signal.name() == sv::ASSERTIONS_OFF);
    let fallbacks = module
        .ff_processes()
        .iter()
        .map(|process| {
            runs_as_process(&module, process, &constants, &parameter_types)
                || shares_foreign_variable(&module, process, foreign_writes)
                || (assertion_control && controls_assertions(&module, process))
        })
        .collect::<Vec<_>>();
    if fallbacks.contains(&true) {
        let mut fallbacks = fallbacks.into_iter();
        module = std::borrow::Cow::Owned(
            module
                .into_owned()
                .run_edge_processes_as_processes(|_| fallbacks.next().unwrap_or(false)),
        );
    }
    let module: &sv::ir::Module = &module;
    let mut ff_event_roles = Vec::new();
    for process in module.ff_processes() {
        let clock = clock_event_from_ff_process(process)
            .ok_or_else(|| sv::AnalyzerError::Unsupported("always_ff event control".into()))?;
        for event in process.events() {
            let kind = match (event.signal() == clock.signal(), event.edge()) {
                (true, sv::ir::FfEdge::Pos) => DomainKind::ClockPosedge,
                (true, sv::ir::FfEdge::Neg) => DomainKind::ClockNegedge,
                (false, sv::ir::FfEdge::Pos) => DomainKind::ResetAsyncHigh,
                (false, sv::ir::FfEdge::Neg) => DomainKind::ResetAsyncLow,
            };
            ff_event_roles.push(FfEventRole {
                signal: event.signal().to_string(),
                kind,
                clock: clock.signal().to_string(),
                clock_kind: match clock.edge() {
                    sv::ir::FfEdge::Pos => DomainKind::ClockPosedge,
                    sv::ir::FfEdge::Neg => DomainKind::ClockNegedge,
                },
            });
        }
    }
    let drivers = ModuleDrivers::new(module, &constants, &parameter_types);
    let shared_memories = shared_edge_memories(module, &drivers, foreign_writes, warnings);
    let shared_variables = shared_edge_variables(module, &drivers);
    let shared_all = shared_memories
        .union(&shared_variables)
        .chain(shared_process_variables.into_iter().flatten())
        .cloned()
        .collect();
    validate_variable_driver_ranges(module, &drivers, &shared_all)?;
    validate_foreign_drivers(module, &drivers, &shared_memories, foreign_writes)?;
    // The drivers above were scanned before the scheduled edges move, as
    // the roles were: the rare rewrite works on a copy.
    let scheduled = module
        .ff_processes()
        .iter()
        .any(|process| needs_host_request(module, process.body(), &mut HashSet::default()))
        .then(|| {
            module.clone().schedule_edge_processes(|process| {
                needs_host_request(module, process.body(), &mut HashSet::default())
            })
        });
    let module = scheduled.as_ref().unwrap_or(module);
    let mut package_variables = Vec::new();
    let name = module.name().to_string();
    let mut next_id = SourceVarId::default();
    let mut variables = HashMap::default();
    let mut name_to_id = HashMap::default();
    let mut port_order = Vec::new();
    let mut initial_memory_values = Vec::new();

    // Array-valued subroutine slots need to preserve their array shape in
    // captured arguments and results; keep their existing unsupported boundary.
    let string_array = |ty: &sv::ir::Type| {
        ty.kind() == sv::ir::TypeKind::String && !ty.unpacked_ranges().is_empty()
    };
    if module.subroutines().iter().any(|routine| {
        routine.return_type.as_ref().is_some_and(string_array)
            || routine
                .params
                .iter()
                .any(|param| string_array(&param.r#type))
    }) {
        return Err(sv::AnalyzerError::Unsupported(
            "fixed string arrays in subroutine interfaces".into(),
        ));
    }

    for port in module.ports() {
        if port.r#type().structure_identity().is_some() {
            continue;
        }
        if name_to_id.contains_key(port.name()) {
            return Err(sv::AnalyzerError::Unsupported(format!(
                "duplicate port name `{}`",
                port.name()
            )));
        }
        let id = next_var_id(&mut next_id);
        let type_info = signal_type_from_sv(port.r#type(), &constants, &parameter_types)?;
        let path = vec![port.name().to_string()];
        let kind = signal_kind_from_port_direction(port.direction())?;
        let variable = SvVariable {
            float: type_info.float,

            value_kind: type_info.value_kind,
            path,
            width: type_info.width,
            signed: type_info.signed,
            is_4state: type_info.is_4state,
            is_unpacked_struct: type_info.is_unpacked_struct,
            packed_ranges: type_info.packed_ranges,
            array_dims: type_info.array_dims,
            unpacked_ranges: type_info.unpacked_ranges,
            domain_kind: DomainKind::Other,
            kind,
            type_kind: type_info.type_kind,
            source: None,
            hidden: false,
            force_control: None,
            sampled_from: None,
            history_of: None,
            activation_scratch: false,
        };
        name_to_id.insert(port.name().to_string(), id);
        port_order.push(id);
        if let Some(default) = port.default() {
            if !variable.array_dims.is_empty() {
                warnings.push((
                    crate::FrontendWarningKind::UnpackedPortDefault {
                        module: name.clone(),
                        port: port.name().to_string(),
                    },
                    None,
                ));
            }
            let literal = sv::typecheck::context_size_const_integral_literal(
                default,
                &constants,
                &parameter_types,
                type_info.width,
                type_info.signed,
            )
            .ok_or_else(|| {
                sv::AnalyzerError::Unsupported(format!(
                    "nonconstant input port default `{}`",
                    port.name()
                ))
            })?;
            initial_memory_values.push(InitialStateValue {
                address: id,
                data: InitialStateData::Packed {
                    value: literal.value,
                    mask: literal.mask,
                    written_mask: (BigUint::from(1u8) << type_info.width) - BigUint::from(1u8),
                },
            });
        } else if port.is_net() || type_info.is_4state {
            let written_mask = (BigUint::from(1u8) << type_info.width) - BigUint::from(1u8);
            let value = if port.is_net() {
                BigUint::default()
            } else {
                written_mask.clone()
            };
            initial_memory_values.push(InitialStateValue {
                address: id,
                data: InitialStateData::Packed {
                    value,
                    mask: written_mask.clone(),
                    written_mask,
                },
            });
        }
        variables.insert(id, variable);
    }

    for signal in module.signals() {
        if signal.r#type().structure_identity().is_some()
            && signal.r#type().unpacked_ranges().is_empty()
        {
            continue;
        }
        if name_to_id.contains_key(signal.name()) {
            return Err(sv::AnalyzerError::Unsupported(format!(
                "duplicate port or signal name `{}`",
                signal.name()
            )));
        }
        let id = next_var_id(&mut next_id);
        let type_info = signal_type_from_sv(signal.r#type(), &constants, &parameter_types)?;
        let path = vec![signal.name().to_string()];
        // A package variable is the package's object, not the module's.
        if let Some((package, variable)) = signal.package_variable() {
            package_variables.push((id, package.to_string(), variable.to_string()));
        }
        let variable = SvVariable {
            float: type_info.float,

            value_kind: type_info.value_kind,
            path,
            width: type_info.width,
            signed: type_info.signed,
            is_4state: type_info.is_4state,
            is_unpacked_struct: type_info.is_unpacked_struct,
            packed_ranges: type_info.packed_ranges,
            array_dims: type_info.array_dims,
            unpacked_ranges: type_info.unpacked_ranges,
            domain_kind: DomainKind::Other,
            kind: VariableKind::Variable,
            type_kind: type_info.type_kind,
            source: None,
            hidden: false,
            force_control: None,
            sampled_from: None,
            history_of: None,
            activation_scratch: false,
        };
        name_to_id.insert(signal.name().to_string(), id);
        // The package's object, or the other instance's, has its own
        // default value.
        if (signal.is_net() || type_info.is_4state)
            && signal.package_variable().is_none()
            && signal.hierarchical().is_none()
        {
            let written_mask = (BigUint::from(1u8) << type_info.width) - BigUint::from(1u8);
            let value = if signal.is_net() {
                BigUint::default()
            } else {
                written_mask.clone()
            };
            initial_memory_values.push(InitialStateValue {
                address: id,
                data: InitialStateData::Packed {
                    value,
                    mask: written_mask.clone(),
                    written_mask,
                },
            });
        }
        variables.insert(id, variable);
    }

    // Retain integral parameters for external observation, without adding them
    // to name_to_id: HDL expressions must still fold parameter references as
    // constants rather than introduce runtime loads.
    // Interface declarations are flattened into imported symbols, rather
    // than authored module parameters. Retain their instance-local values as
    // well; package imports remain owned by their package (IEEE 37.10).
    let interface_parameters = module
        .imported_parameters()
        .iter()
        .filter(|parameter| restore_interface_path(parameter.name(), reflected_paths).is_some());
    for parameter in module.parameters().iter().chain(interface_parameters) {
        let Some(width) = parameter.resolved_width().filter(|width| *width > 0) else {
            continue;
        };
        let signed = parameter.resolved_signed().unwrap_or(false);
        let expr = parameter.value().cloned().or_else(|| {
            constants
                .get(parameter.name())
                .copied()
                .map(const_expr_from_i128)
        });
        let Some(literal) = expr.and_then(|expr| {
            sv::typecheck::context_size_const_integral_literal(
                &expr,
                &constants,
                &parameter_types,
                width,
                signed,
            )
        }) else {
            continue;
        };
        let id = next_var_id(&mut next_id);
        let written_mask = (BigUint::from(1u8) << width) - BigUint::from(1u8);
        initial_memory_values.push(InitialStateValue {
            address: id,
            data: InitialStateData::Packed {
                value: literal.value,
                mask: literal.mask,
                written_mask,
            },
        });
        variables.insert(
            id,
            SvVariable {
                float: None,

                value_kind: celox_design::ValueKind::Bits,
                path: vec![parameter.name().to_string()],
                width,
                signed,
                is_4state: true,
                is_unpacked_struct: false,
                packed_ranges: vec![(width as i128 - 1, 0)],
                array_dims: Vec::new(),
                unpacked_ranges: Vec::new(),
                domain_kind: DomainKind::Other,
                kind: VariableKind::Parameter,
                type_kind: PortTypeKind::Logic,
                source: None,
                hidden: false,
                force_control: None,
                sampled_from: None,
                history_of: None,
                activation_scratch: false,
            },
        );
    }

    let forceable = variables
        .iter()
        .filter_map(|(&id, var)| {
            if var.kind == VariableKind::Parameter {
                return None;
            }
            let name = &var.path[0];
            force_signals.iter().any(|path| name == path).then_some(id)
        })
        .collect::<Vec<_>>();
    for id in forceable {
        let target = variables[&id].clone();
        // Whole packed variables only. Array elements require a separately
        // specified addressing contract rather than a guessed flat offset.
        if !target.array_dims.is_empty() {
            continue;
        }
        for (prefix, width, is_4state) in [
            (FORCE_ENABLE, 1, false),
            (FORCE_VALUE, target.width, target.is_4state),
        ] {
            let control_name = format!("{prefix}{}", target.path[0]);
            if name_to_id.contains_key(&control_name) {
                return Err(sv::AnalyzerError::Unsupported(format!(
                    "signal `{control_name}` conflicts with a compiler force control"
                )));
            }
            let mut control = target.clone();
            control.force_control = Some((id, prefix == FORCE_ENABLE));
            control.path = vec![control_name.clone()];
            control.width = width;
            control.signed = false;
            control.is_4state = is_4state;
            control.kind = VariableKind::Variable;
            control.hidden = true;
            control.packed_ranges = vec![(width as i128 - 1, 0)];
            control.type_kind = if is_4state {
                PortTypeKind::Logic
            } else {
                PortTypeKind::Bit
            };
            let control_id = next_var_id(&mut next_id);
            variables.insert(control_id, control);
            name_to_id.insert(control_name, control_id);
        }
    }

    procedural::register_locals(
        module,
        &mut variables,
        &mut name_to_id,
        &mut next_id,
        &constants,
        &parameter_types,
    )?;
    // The level-sensitive lowering below changes processes only: one table
    // of the subroutines serves every lowering of the module.
    let tables = procedural::ModuleTables::new(module);
    // `always @(list)` naming exactly the reads of its statement is
    // `always @*` (IEEE 1800-2023 9.4.2.2), which needs no process.
    let level_sensitive = match level_lists {
        LevelLists::Processes => None,
        LevelLists::Lowered { demoted } => Some(demoted),
    }
    .filter(|_| {
        module
            .initial_processes()
            .iter()
            .any(|process| process.level_sensitive().is_some())
    })
    .map(|demoted| {
        let pm = procedural::ProcModule::new(
            module,
            &tables,
            &mut variables,
            &mut name_to_id,
            &constants,
            &parameter_types,
            four_state,
            warnings,
        );
        module.clone().lower_level_sensitive_processes(|process| {
            !demoted.contains(&process.creation_order())
                && process
                    .level_sensitive()
                    .is_some_and(|(items, body)| pm.is_implicit_event_list(items, body))
        })
    });
    let module = level_sensitive.as_ref().unwrap_or(module);
    let mut repeated_ff_writes = Vec::new();
    let (
        (
            eval_only_ff_blocks,
            apply_ff_blocks,
            eval_apply_ff_blocks,
            split_commit_ff_blocks,
            reset_clock_map,
            parallel_ff_parts,
            event_vars,
            delayed_sites,
        ),
        mut runtime_event_sites,
        mut runtime_errors,
        mut extern_functions,
    ) = {
        let mut pm = procedural::ProcModule::new(
            module,
            &tables,
            &mut variables,
            &mut name_to_id,
            &constants,
            &parameter_types,
            four_state,
            warnings,
        );
        pm.calls = calls.as_deref_mut();
        let foreign = pm
            .out_of_line()
            .map(|functions| std::mem::take(&mut functions.foreign))
            .unwrap_or_default();
        pm.mark_recursive(module);
        pm.observe_storage(module, &foreign);
        let blocks = lower_ff_processes(
            module,
            &mut pm,
            ff_parts,
            &ff_event_roles,
            &shared_memories,
            foreign_writes,
            &shared_variables,
            &mut repeated_ff_writes,
        )?;
        (
            blocks,
            std::mem::take(&mut pm.runtime.event_sites),
            std::mem::take(&mut pm.runtime.errors),
            std::mem::take(&mut pm.externs.functions),
        )
    };
    mark_ff_event_domains(&event_vars, &mut variables);
    let (
        initial_values,
        declaration_values,
        folded_initial_memory_values,
        mut processes,
        wait_sites,
        mut nba_sites,
        nba_switches,
        host_request_sites,
        event_assignments,
    ) = lower_initial_processes(
        module,
        &tables,
        &mut variables,
        &mut name_to_id,
        &constants,
        &parameter_types,
        four_state,
        &mut runtime_event_sites,
        &mut runtime_errors,
        &mut extern_functions,
        warnings,
        calls,
    )?;
    // The delayed flip-flop updates are published by their domains' commit
    // units through timed nonblocking sites; one synthetic process groups
    // the module's sites for publication. Its kernel never runs.
    if !delayed_sites.is_empty() {
        let index = processes.len();
        processes.push(delayed_publication_process(&mut variables, index));
        nba_sites.extend(delayed_sites.into_iter().map(|mut site| {
            site.process = index;
            site
        }));
    }
    let mut declaration_initial_memory_values = initial_memory_values.clone();
    declaration_initial_memory_values.extend(declaration_values);
    initial_memory_values.extend(initial_values);

    let shared_variables = variables
        .iter()
        .map(|(&id, variable)| (id, variable.to_symbolic_variable()))
        .collect();
    let instances = lower_instances(module, &constants, &parameter_types)?;

    Ok(LoweredSvModule {
        source: module.clone(),
        implicit_nets_allowed,
        sim_module: SimModule {
            time_precision: None,
            time_scale: celox_design::TimeScale::default(),
            name,
            variables: shared_variables,
            ff_access_summaries: HashMap::default(),
            eval_only_ff_blocks,
            apply_ff_blocks,
            eval_apply_ff_blocks,
            split_commit_ff_blocks,
            parallel_ff_parts,
            glue_blocks: HashMap::default(),
            indexed_instance_names: HashSet::default(),
            instance_index_bases: HashMap::default(),
            comb_blocks: Vec::new(),
            comb_observers: Vec::<CombObserver<SourceVarId>>::new(),
            runtime_errors,
            runtime_event_sites,
            memory_file_bases: Vec::new(),
            extern_functions,
            initial_memory_values,
            declaration_initial_memory_values,
            folded_initial_memory_values,
            comb_boundaries: HashMap::default(),
            arena: SLTNodeArena::new(),
            reset_clock_map,
            processes,
            functions: Vec::new(),
            function_frames: Vec::new(),
            package_bindings: HashMap::default(),
            hierarchical_bindings: HashMap::default(),
            counter_slots: Vec::new(),
            pending_slots: Vec::new(),
            comb_alternatives: Vec::new(),
            wait_sites,
            nba_sites,
            nba_switches,
            host_request_sites,
            time_slot: name_to_id.get(TIME_SLOT).copied(),
            sample_shadows: Vec::new(),
            initial_copies: Vec::new(),
        },
        variables,
        port_order,
        signal_names: name_to_id,
        constants: constants.clone(),
        parameter_types,
        instances,
        package_variables,
        event_copies: event_vars.copies,
        event_assignments,
        repeated_ff_writes,
        inout_driven_inside: HashSet::default(),
        calls: None,
    })
}

fn mark_ff_event_domains(
    event_vars: &FfEventVars,
    variables: &mut HashMap<SourceVarId, SvVariable>,
) {
    for (id, kind) in event_vars.kinds() {
        if let Some(variable) = variables.get_mut(&id) {
            variable.domain_kind = kind;
            variable.type_kind = match kind {
                DomainKind::ResetAsyncHigh => PortTypeKind::ResetAsyncHigh,
                DomainKind::ResetAsyncLow => PortTypeKind::ResetAsyncLow,
                _ => PortTypeKind::Clock,
            };
        }
    }
}

/// The widths and signedness of the parameters a module can name.
pub(super) fn module_parameter_types(module: &sv::ir::Module) -> HashMap<String, (usize, bool)> {
    module
        .imported_parameters()
        .iter()
        .chain(module.parameters())
        .filter_map(|parameter| {
            Some((
                parameter.name().to_string(),
                (
                    parameter.resolved_width()?,
                    parameter.resolved_signed().unwrap_or(false),
                ),
            ))
        })
        .collect()
}

pub(super) fn module_constants_with_overrides(
    module: &sv::ir::Module,
    parameter_overrides: &[LoweredSvParameterOverride],
) -> HashMap<String, i128> {
    let override_values: HashMap<&str, &sv::ir::ConstExpr> = parameter_overrides
        .iter()
        .filter_map(|parameter| {
            parameter
                .value
                .as_ref()
                .map(|value| (parameter.name.as_str(), value))
        })
        .collect();
    let mut constants = HashMap::default();
    // The package parameters a module uses come before its own.
    for parameter in module.imported_parameters() {
        if let Some(value) = parameter.resolved_value() {
            constants.insert(parameter.name().to_string(), value);
        }
    }
    for parameter in module.parameters() {
        let value = if let Some(override_value) = override_values.get(parameter.name()) {
            sv::typecheck::eval_const_expr(override_value, &constants)
        } else {
            parameter.resolved_value().or_else(|| {
                parameter
                    .value()
                    .and_then(|expr| sv::typecheck::eval_const_expr(expr, &constants))
            })
        };
        if let Some(value) = value {
            constants.insert(parameter.name().to_string(), value);
        }
    }

    constants
}
