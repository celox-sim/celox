//! Lowering and scheduling of a complete SystemVerilog design.

use super::*;

/// Analyze SystemVerilog sources and lower the selected top through Celox's
/// shared symbolic scheduling pipeline.
pub fn schedule_sources(
    sources: &[(&str, &Path)],
    top: &str,
    parameter_overrides: &[(String, u64)],
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
    parallel: &celox_frontend_core::ParallelScheduleOptions,
    trace_options: &FrontendTraceOptions,
    trace: Option<&mut FrontendTrace>,
    warnings: &mut Vec<crate::FrontendWarning>,
) -> Result<ScheduledRtlOutput, FrontendError> {
    schedule_sources_with_options(
        sources,
        top,
        parameter_overrides,
        ignored_loops,
        true_loops,
        four_state,
        parallel,
        trace_options,
        trace,
        warnings,
        sv::AnalysisOptions::default(),
    )
}

/// Schedule a design with explicit language compatibility options.
#[allow(clippy::too_many_arguments)]
pub fn schedule_sources_with_options(
    sources: &[(&str, &Path)],
    top: &str,
    parameter_overrides: &[(String, u64)],
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
    parallel: &celox_frontend_core::ParallelScheduleOptions,
    trace_options: &FrontendTraceOptions,
    trace: Option<&mut FrontendTrace>,
    warnings: &mut Vec<crate::FrontendWarning>,
    analysis_options: sv::AnalysisOptions,
) -> Result<ScheduledRtlOutput, FrontendError> {
    schedule_sources_with_force_signals(
        sources,
        top,
        parameter_overrides,
        ignored_loops,
        true_loops,
        four_state,
        parallel,
        trace_options,
        trace,
        warnings,
        analysis_options,
        &[],
    )
}

/// Schedule with explicitly forceable signal names. Only these objects receive
/// external override inputs; unrelated logic retains its normal optimization.
#[allow(clippy::too_many_arguments)]
pub fn schedule_sources_with_force_signals(
    sources: &[(&str, &Path)],
    top: &str,
    parameter_overrides: &[(String, u64)],
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
    parallel: &celox_frontend_core::ParallelScheduleOptions,
    trace_options: &FrontendTraceOptions,
    trace: Option<&mut FrontendTrace>,
    warnings: &mut Vec<crate::FrontendWarning>,
    analysis_options: sv::AnalysisOptions,
    force_signals: &[String],
) -> Result<ScheduledRtlOutput, FrontendError> {
    schedule_sources_with_force_signals_and_latch_policy(
        sources,
        top,
        parameter_overrides,
        ignored_loops,
        true_loops,
        four_state,
        parallel,
        trace_options,
        trace,
        false,
        warnings,
        analysis_options,
        force_signals,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn schedule_sources_with_latch_policy(
    sources: &[(&str, &Path)],
    top: &str,
    parameter_overrides: &[(String, u64)],
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
    parallel: &celox_frontend_core::ParallelScheduleOptions,
    trace_options: &FrontendTraceOptions,
    trace: Option<&mut FrontendTrace>,
    allow_inferred_latches: bool,
    warnings: &mut Vec<crate::FrontendWarning>,
) -> Result<ScheduledRtlOutput, FrontendError> {
    schedule_sources_with_force_signals_and_latch_policy(
        sources,
        top,
        parameter_overrides,
        ignored_loops,
        true_loops,
        four_state,
        parallel,
        trace_options,
        trace,
        allow_inferred_latches,
        warnings,
        sv::AnalysisOptions::default(),
        &[],
    )
}

/// Schedule with compatibility options, signal overrides, and latch inference.
#[allow(clippy::too_many_arguments)]
pub fn schedule_sources_with_force_signals_and_latch_policy(
    sources: &[(&str, &Path)],
    top: &str,
    parameter_overrides: &[(String, u64)],
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
    parallel: &celox_frontend_core::ParallelScheduleOptions,
    trace_options: &FrontendTraceOptions,
    trace: Option<&mut FrontendTrace>,
    allow_inferred_latches: bool,
    warnings: &mut Vec<crate::FrontendWarning>,
    analysis_options: sv::AnalysisOptions,
    force_signals: &[String],
) -> Result<ScheduledRtlOutput, FrontendError> {
    let parameters = parameter_overrides
        .iter()
        .map(|(name, value)| (name.clone(), sv::ir::ConstExpr::Literal(value.to_string())))
        .collect::<Vec<_>>();
    schedule_sources_with_parameter_expressions_and_latch_policy(
        sources,
        top,
        &parameters,
        ignored_loops,
        true_loops,
        four_state,
        parallel,
        trace_options,
        trace,
        allow_inferred_latches,
        warnings,
        analysis_options,
        force_signals,
    )
}

/// Schedule with typed SV constant expressions and strict latch inference.
#[allow(clippy::too_many_arguments)]
pub fn schedule_sources_with_parameter_expressions(
    sources: &[(&str, &Path)],
    top: &str,
    parameter_overrides: &[(String, sv::ir::ConstExpr)],
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
    parallel: &celox_frontend_core::ParallelScheduleOptions,
    trace_options: &FrontendTraceOptions,
    trace: Option<&mut FrontendTrace>,
    warnings: &mut Vec<crate::FrontendWarning>,
    analysis_options: sv::AnalysisOptions,
    force_signals: &[String],
) -> Result<ScheduledRtlOutput, FrontendError> {
    schedule_sources_with_parameter_expressions_and_latch_policy(
        sources,
        top,
        parameter_overrides,
        ignored_loops,
        true_loops,
        four_state,
        parallel,
        trace_options,
        trace,
        false,
        warnings,
        analysis_options,
        force_signals,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn schedule_sources_with_parameter_expressions_and_latch_policy(
    sources: &[(&str, &Path)],
    top: &str,
    parameter_overrides: &[(String, sv::ir::ConstExpr)],
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
    parallel: &celox_frontend_core::ParallelScheduleOptions,
    trace_options: &FrontendTraceOptions,
    trace: Option<&mut FrontendTrace>,
    allow_inferred_latches: bool,
    warnings: &mut Vec<crate::FrontendWarning>,
    analysis_options: sv::AnalysisOptions,
    force_signals: &[String],
) -> Result<ScheduledRtlOutput, FrontendError> {
    let mut found = Vec::new();
    let scheduled = schedule_sources_collecting(
        sources,
        top,
        parameter_overrides,
        ignored_loops,
        true_loops,
        four_state,
        parallel,
        trace_options,
        trace,
        allow_inferred_latches,
        &mut found,
        analysis_options,
        force_signals,
    );
    append_error_warnings(&scheduled, &mut found);
    crate::warning::append_unique_warnings(sources, warnings, found);
    scheduled
}

#[allow(clippy::too_many_arguments)]
fn schedule_sources_collecting(
    sources: &[(&str, &Path)],
    top: &str,
    parameter_overrides: &[(String, sv::ir::ConstExpr)],
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
    parallel: &celox_frontend_core::ParallelScheduleOptions,
    trace_options: &FrontendTraceOptions,
    trace: Option<&mut FrontendTrace>,
    allow_inferred_latches: bool,
    warnings: &mut Vec<crate::warning::PendingWarning>,
    analysis_options: sv::AnalysisOptions,
    force_signals: &[String],
) -> Result<ScheduledRtlOutput, FrontendError> {
    let frontend_timing = trace_options.phase_timing;
    let mut checkpoint = std::time::Instant::now();
    macro_rules! checkpoint {
        ($label:expr) => {
            if frontend_timing {
                tracing::debug!("[sv-frontend] {}: {:?}", $label, checkpoint.elapsed());
                checkpoint = std::time::Instant::now();
            }
        };
    }
    // The root generated for several tops (23.3.1), which no name includes.
    let hidden_root = analysis_options.generated_root;
    // The tops below it are roots of the design too, which timescale
    // queries such as `$printtimescale(top.u)` name.
    let tops = if hidden_root {
        crate::generated_root_tops(sources, top)
    } else {
        Vec::new()
    };
    let roots: Vec<&str> = std::iter::once(top)
        .chain(tops.iter().map(String::as_str))
        .collect();
    let mut analyzed = analyze_sources(sources, &roots, analysis_options.clone())?;
    checkpoint!("analyze_sources");
    package_warnings(&analyzed, warnings);
    // The specializations that the modules request of package class
    // templates, shared by all of their analyses.
    let mut design = analyzed
        .values()
        .next()
        .map_or_else(sv::Design::default, |module| module.packages.design());
    let top = top.to_string();
    let root_key = LoweredSvModuleKey {
        name: top.clone(),
        // A path starts at the top, or below the root of several tops at
        // one of them.
        force_signals: force_signals
            .iter()
            .filter_map(|path| {
                if hidden_root {
                    Some(path.as_str())
                } else {
                    path.strip_prefix(&format!("{top}."))
                }
            })
            .map(str::to_string)
            .collect(),
        parameter_overrides: parameter_overrides
            .iter()
            .map(|(name, value)| LoweredSvParameterOverride {
                name: name.clone(),
                value: Some(value.clone()),
                bound_type: None,
                bound_pattern: None,
            })
            .collect(),
        interconnect_descriptor: Vec::new(),
    };
    if !analyzed.contains_key(&top) {
        return Err(sv_top_not_found(top).into());
    }

    let root_id = ModuleId(0);
    let mut module_ids = HashMap::default();
    module_ids.insert(root_key.clone(), root_id);
    let mut specialization_parents = HashMap::default();
    let mut queue = vec![root_key.clone()];
    let mut index = 0;
    let mut elaborated = HashMap::default();
    let mut hierarchical_modules = Vec::new();
    // The analyses of the modules that may refer to the instances of virtual
    // interfaces (see `virtual_interfaces`).
    let mut virtual_sources = HashMap::default();
    // These analyses have no unresolved hierarchical names. Retain them until
    // their first final lowering, whose scheduling and foreign-writer context
    // is established below. The cache belongs only to this elaboration attempt.
    let mut discovery_analyses = HashMap::default();
    while index < queue.len() {
        let key = queue[index].clone();
        index += 1;
        let base = analyzed
            .get(&key.name)
            .ok_or_else(|| unsupported_sv_instance(key.name.clone()))?;
        let module_id = module_ids[&key];
        // Discovery needs only instance bindings and reflected declarations.
        // Executable lowering is deferred until hierarchy and foreign writers
        // are known, rather than constructing and discarding a full SimModule.
        let discovery = analyze_specialization(
            base,
            &mut design,
            &key,
            sv::ast::HierarchicalObjects::default(),
            &[],
            warnings,
        )
        .and_then(|ir| {
            let source = ir
                .modules()
                .iter()
                .find(|module| module.name() == base.name)
                .ok_or_else(|| sv::AnalyzerError::Unsupported(format!("module `{}`", base.name)))?;
            let declarations =
                hierarchy::ElaboratedModule::from_declarations(source, &key.force_signals)?;
            Ok((declarations, ir))
        });
        let module = match discovery {
            Ok((declarations, ir)) => {
                discovery_analyses.insert(module_id, ir);
                declarations
            }
            // Elaborate the hierarchy below the module before resolving its
            // hierarchical names.
            Err(error) if super::specialize::waits_for_hierarchy(&error) => {
                hierarchical_modules.push(module_id);
                specialize_module_declarations(base, &mut design, &key)?
            }
            Err(error) => return Err(error.into()),
        };
        let module = elaborated.entry(module_id).or_insert(module);
        for instance in module.instances() {
            let child_key = LoweredSvModuleKey::instance_key(instance);
            if !analyzed.contains_key(&child_key.name) {
                return Err(unsupported_sv_instance(child_key.name.clone()).into());
            }
            if !module_ids.contains_key(&child_key) {
                register_specialization_parent(&child_key, &key, &mut specialization_parents)?;
                let child_id = ModuleId(module_ids.len());
                module_ids.insert(child_key.clone(), child_id);
                queue.push(child_key);
            }
        }
    }

    // The ids of interface instances depend on the elaborated hierarchy.
    // Analysis builds each virtual interface access once, with placeholders
    // for the instance it selects; repeat each for the instances now known.
    checkpoint!("discovery");
    let keys: HashMap<ModuleId, &LoweredSvModuleKey> =
        module_ids.iter().map(|(key, &id)| (id, key)).collect();
    let hierarchy = hierarchy::Hierarchy::new(
        (&root_key, root_id),
        hidden_root,
        &keys,
        &module_ids,
        &elaborated,
        &analyzed,
    );
    // The signals that hierarchical names write gain writers outside their
    // own modules. Collect them from the analyzed modules, independently of
    // the order the modules are lowered in.
    // The tasks and functions called through virtual interfaces whose
    // hierarchical names each instance finds from its parent (IEEE 1800-2023
    // 23.8) are built now, with the object of every instance, before the
    // analyses that call them.
    let mut outward = HashMap::default();
    for (key, names) in design.virtual_interface_outward() {
        let mut objects: Vec<(Vec<String>, sv::ast::Type, bool)> = Vec::new();
        let mut holders: Vec<_> = elaborated.keys().copied().collect();
        holders.sort_by_key(|module| module.0);
        for module_id in holders {
            for handle in elaborated[&module_id]
                .interface_handles
                .iter()
                .filter(|handle| handle.interface == key && !handle.port)
            {
                let scope = sv::InterfaceBodyScope {
                    handle: handle.handle.clone(),
                    blocks: handle.blocks.clone(),
                    interface: key.clone(),
                };
                let mut members = Vec::new();
                for components in &names {
                    let object =
                        hierarchy::outward_object(&hierarchy, module_id, &scope, components)?
                            .ok_or_else(|| {
                                sv::AnalyzerError::Unsupported(format!(
                                    "hierarchical name `{}` of a task or function of interface instance `{}` called through a virtual interface, which names no object",
                                    components.join("."),
                                    handle.handle
                                ))
                            })?;
                    match objects.iter().find(|(known, ..)| known == components) {
                        Some((_, r#type, is_net))
                            if *r#type != object.r#type || *is_net != object.is_net =>
                        {
                            return Err(sv::AnalyzerError::Unsupported(format!(
                                "hierarchical name `{}` of a task or function of interface `{key}` called through a virtual interface, which denotes objects of different types in different instances",
                                components.join(".")
                            ))
                            .into());
                        }
                        Some(_) => {}
                        None => {
                            objects.push((components.clone(), object.r#type.clone(), object.is_net))
                        }
                    }
                    members.push((
                        sv::ast::virtual_interfaces::outward_member(components),
                        object,
                    ));
                }
                outward.insert((module_id, handle.handle.clone()), members);
            }
        }
        design.resolve_virtual_interface_outward(&analyzed[&top].packages, &key, &objects)?;
    }
    let mut references = HashMap::default();
    let mut analyses = HashMap::default();
    let mut foreign_references: HashMap<ModuleId, HashSet<String>> = HashMap::default();
    let mut foreign_writes: HashMap<ModuleId, HashMap<String, hierarchy::ForeignWrites>> =
        HashMap::default();
    // A module that calls a subroutine of another instance through a
    // hierarchical name is analyzed after the callee, whose analyzed body it
    // imports (see `hierarchy::calls`).
    let mut pending = hierarchical_modules.clone();
    while !pending.is_empty() {
        let mut deferred = Vec::new();
        for &module_id in &pending {
            let key = keys[&module_id];
            let (mut resolved, mut calls) = hierarchy::resolve(&hierarchy, module_id);
            let analysis = |id: &ModuleId| {
                analyses
                    .get(id)
                    .or_else(|| discovery_analyses.get(id))
                    .and_then(|ir: &sv::Ir| {
                        ir.modules()
                            .iter()
                            .find(|module| module.name() == keys[id].name)
                    })
            };
            let analyzed_ir = loop {
                // The callees whose analyses are done. The scan of the
                // module's names also finds calls in inactive generate
                // branches: one whose callee is still to come waits only
                // once the analysis needs it.
                let mut callees = Vec::new();
                let mut waiting = Vec::new();
                for call in &calls {
                    let (id, _) = call.callee()?;
                    match analysis(&id) {
                        Some(module) => callees.push(hierarchy::calls::CallTarget {
                            call,
                            id,
                            module,
                            references: references.get(&id).map_or(&[][..], Vec::as_slice),
                        }),
                        None => waiting.push((call.written.as_str(), id)),
                    }
                }
                let signals: Vec<_> = resolved
                    .iter()
                    .map(|reference| reference.signal.clone())
                    .collect();
                let subroutines = callees
                    .iter()
                    .map(|callee| callee.signature(&hierarchy))
                    .collect::<Result<Vec<_>, _>>()?;
                match analyze_specialization(
                    &analyzed[&key.name],
                    &mut design,
                    key,
                    sv::ast::HierarchicalObjects {
                        signals: &signals,
                        subroutines: &subroutines,
                    },
                    &[],
                    &mut Vec::new(),
                ) {
                    Ok(mut ir) => {
                        let mut imported = hierarchy::calls::Imported::default();
                        if let Some(module) =
                            ir.modules().iter().find(|module| module.name() == key.name)
                        {
                            for callee in &callees {
                                hierarchy::calls::import(
                                    &hierarchy,
                                    module,
                                    callee,
                                    &resolved,
                                    &mut imported,
                                )?;
                            }
                        }
                        if let Some(module) = ir.module_mut(&key.name) {
                            module.import_subroutines(
                                imported.subroutines,
                                imported.signals,
                                imported.locals,
                                imported.dpi_imports,
                            );
                        }
                        resolved.extend(imported.references);
                        break Some(ir);
                    }
                    // A name with an instance index that is a constant
                    // expression reaches the analysis with the index
                    // evaluated.
                    Err(error) => match unresolved_name(&error) {
                        Some((components, None)) => {
                            let path = components.join(".");
                            if waiting.iter().any(|(written, _)| *written == path) {
                                if waiting
                                    .iter()
                                    .any(|&(written, id)| written == path && id == module_id)
                                {
                                    return Err(sv::AnalyzerError::Unsupported(format!(
                                        "hierarchical call `{path}` of a subroutine of the \
                                         calling module itself"
                                    ))
                                    .into());
                                }
                                // The callee's own analysis is still to come.
                                break None;
                            }
                            if !hierarchy::resolve_name(
                                &hierarchy,
                                module_id,
                                components,
                                &mut resolved,
                            )? && !hierarchy::resolve_call(
                                &hierarchy, module_id, components, &mut calls,
                            )? {
                                return Err(error.into());
                            }
                        }
                        // A name of the body of an interface instance of the
                        // module, which the module's scopes are searched for.
                        Some((components, Some(scope)))
                            if hierarchy::resolve_interface_name(
                                &hierarchy,
                                module_id,
                                scope,
                                components,
                                &mut resolved,
                            )? => {}
                        _ => return Err(error.into()),
                    },
                }
            };
            let Some(ir) = analyzed_ir else {
                deferred.push(module_id);
                continue;
            };
            for reference in &resolved {
                for (target_module, target) in &reference.targets {
                    foreign_references
                        .entry(*target_module)
                        .or_default()
                        .insert(target.clone());
                }
            }
            references.insert(module_id, resolved);
            analyses.insert(module_id, ir);
        }
        if deferred.len() == pending.len() {
            return Err(sv::AnalyzerError::Unsupported(format!(
                "hierarchical calls between the subroutines of modules `{}`",
                deferred
                    .iter()
                    .map(|id| keys[id].name.as_str())
                    .collect::<Vec<_>>()
                    .join("`, `")
            ))
            .into());
        }
        pending = deferred;
    }
    // The variables and nets that `force` statements force through
    // hierarchical names (IEEE 1800-2023 10.6.2), by specialization: the
    // forcing module's companion controls denote their controls, which
    // their own module declares, and whose writes of them depend on. Every
    // instance of a forced specialization carries the controls, each with
    // its own storage; an instance no force names never sets them.
    let mut forced: HashMap<ModuleId, Vec<String>> = HashMap::default();
    for (module_id, ir) in &analyses {
        let key = keys[module_id];
        let Some(module) = ir.modules().iter().find(|module| module.name() == key.name) else {
            continue;
        };
        for signal in module.signals() {
            let Some(control) = signal.force_control() else {
                continue;
            };
            if signal.hierarchical().is_none() || control.role != sv::ir::ForceRole::Enable {
                continue;
            }
            let Some(reference) = references[module_id].iter().find(
                |reference: &&hierarchy::HierarchicalReference| {
                    reference.signal.name == control.target
                },
            ) else {
                continue;
            };
            for (target_module, target) in &reference.targets {
                let names = forced.entry(*target_module).or_default();
                if !names.contains(target) {
                    names.push(target.clone());
                }
            }
        }
    }
    // A forced specialization is analyzed again with its forced names, also
    // when its own hierarchical names were analyzed above.
    let mut forced_discovery = HashMap::default();
    for (&module_id, names) in &mut forced {
        names.sort();
        if let Some(ir) = discovery_analyses.remove(&module_id) {
            forced_discovery.insert(module_id, ir);
        }
        if analyses.remove(&module_id).is_none() {
            continue;
        }
        let key = keys[&module_id];
        let signals: Vec<_> = references[&module_id]
            .iter()
            .map(|reference| reference.signal.clone())
            .collect();
        let ir = analyze_specialization(
            &analyzed[&key.name],
            &mut design,
            key,
            sv::ast::HierarchicalObjects {
                signals: &signals,
                subroutines: &[],
            },
            names,
            &mut Vec::new(),
        )?;
        analyses.insert(module_id, ir);
    }
    // The elaboration served to resolve the hierarchical names, and below
    // to number the instances.
    drop(hierarchy);

    // The ids of interface instances depend on the elaborated hierarchy.
    // Analysis builds each virtual interface access once, with placeholders
    // for the instance it selects; repeat each for the instances now known.
    // The overrides of module classes, of every analysis, join the
    // dispatchers of every module; they are repeated for the instances of
    // their modules, as virtual interface accesses are.
    let overrides = exported_overrides(discovery_analyses.values().chain(analyses.values()))?;
    // A module that hierarchical names reach declared its instances by its
    // declarations alone, before the analysis that exports its overrides.
    for (&module_id, ir) in &analyses {
        let Some(module) = ir
            .modules()
            .iter()
            .find(|module| module.name() == keys[&module_id].name)
        else {
            continue;
        };
        if let Some(elaborated) = elaborated.get_mut(&module_id) {
            for handle in module.interface_handles() {
                if handle.handle == sv::ir::MODULE_HANDLE
                    && !elaborated
                        .interface_handles
                        .iter()
                        .any(|known| known.handle == handle.handle)
                {
                    elaborated.interface_handles.push(handle.clone());
                }
            }
        }
    }
    let mut modules: HashMap<String, Vec<sv::ast::virtual_interfaces::VirtualInterfaceMember>> =
        HashMap::default();
    for export in &overrides {
        for module in export.modules() {
            modules.entry(module.to_owned()).or_default();
        }
        for (module, member) in &export.members {
            let known = modules.entry(module.clone()).or_default();
            if !known.iter().any(|known| known.name == member.name) {
                known.push(member.clone());
            }
        }
    }
    // The virtual interfaces of the design, with every specialization of a
    // parameterized interface that the analyses above named (25.9).
    let virtual_interfaces = design.virtual_interfaces().cloned();
    let definitions = virtual_interfaces::Definitions {
        interfaces: virtual_interfaces.as_deref(),
        modules,
        outward,
    };
    let virtual_instances = (definitions.interfaces.is_some() || !definitions.modules.is_empty())
        .then(|| {
            virtual_interfaces::Instances::elaborate(
                root_id,
                &module_ids,
                &elaborated,
                &definitions,
            )
        });
    let port_homes = virtual_interfaces::port_homes(root_id, &module_ids, &elaborated);
    let virtual_ids = virtual_instances
        .as_ref()
        .map(|instances| instances.ids())
        .unwrap_or_default();
    let site_homes = celox_design::SiteHomes {
        instances: virtual_instances
            .as_ref()
            .map(|instances| instances.site_homes())
            .unwrap_or_default(),
        ports: port_homes,
    };
    let expansion = sv::ir::InstanceExpansion::new(&virtual_ids, &overrides);
    if virtual_instances.is_some() || !overrides.is_empty() {
        let names: HashMap<ModuleId, &str> = module_ids
            .iter()
            .map(|(key, &id)| (id, key.name.as_str()))
            .collect();
        for (module_id, ir) in &mut discovery_analyses {
            ir.expand_instances(&expansion);
            if let Some(source) = ir
                .modules()
                .iter()
                .find(|module| Some(module.name()) == names.get(module_id).copied())
            {
                virtual_sources.insert(*module_id, source.clone());
            }
        }
    }
    for (module_id, ir) in &mut analyses {
        ir.expand_instances(&expansion);
        let key = keys[module_id];
        if let Some(module) = ir.modules().iter().find(|module| module.name() == key.name) {
            hierarchy::collect_foreign_writes(module, &references[module_id], &mut foreign_writes);
        }
    }
    // Signals that denote members of interface instances for virtual
    // interfaces, which other modules then write and watch.
    let mut virtual_references: HashMap<ModuleId, Vec<hierarchy::HierarchicalReference>> =
        HashMap::default();
    if let Some(instances) = &virtual_instances {
        for (&module_id, ir) in
            virtual_sources
                .iter()
                .chain(analyses.iter().filter_map(|(module_id, ir)| {
                    let key = keys[module_id];
                    Some((
                        module_id,
                        ir.modules()
                            .iter()
                            .find(|module| module.name() == key.name)?,
                    ))
                }))
        {
            let references = instances.references(ir, &definitions, &mut design);
            if references.is_empty() {
                continue;
            }
            hierarchy::collect_virtual_writes(ir, &references, &mut foreign_writes);
            virtual_references.insert(module_id, references);
        }
    }
    drop(virtual_sources);
    let ref_ports::RefPortAnalysis {
        targets: ref_port_targets,
        same_instance_aliased,
        same_instance_alias_groups,
        same_instance_alias_partitions,
    } = ref_ports::analyze(
        root_id,
        &keys,
        &module_ids,
        &elaborated,
        &analyses,
        &discovery_analyses,
        &forced_discovery,
        &references,
        &virtual_references,
        &design,
        &forced,
        &mut foreign_writes,
        &mut foreign_references,
    )?;
    drop(forced_discovery);
    drop(elaborated);
    checkpoint!("hierarchy");

    // A store of a class property that a level wait reads, in any module,
    // wakes the wait (see `ff::property_changes`).
    let mut properties = BTreeSet::new();
    // Every module shares the format of `%t` if any calls `$timeformat`.
    let mut time_format = false;
    for module_id in module_ids.values() {
        let key = keys[module_id];
        let ir = analyses
            .get(module_id)
            .or_else(|| discovery_analyses.get(module_id));
        if let Some(module) = ir
            .into_iter()
            .flat_map(|ir| ir.modules())
            .find(|module| module.name() == key.name)
        {
            ff::watched_properties(module, &mut properties);
            time_format |= ff::calls_time_format(module);
        }
    }
    package_watched_properties(&analyzed[&top].packages, &design, &mut properties);
    time_format |= packages_call_time_format(&analyzed[&top].packages, &design);

    // Level-sensitive always blocks in a static combinational loop may
    // converge as event processes. Demote the participating blocks and retry;
    // the runtime then evaluates convergence using their authored sensitivity.
    // Whether any module or package of the design can construct a
    // covergroup: read from the analyses, whose subroutine lists hold a
    // constructor wherever one can be called (see
    // `sv::ir::Module::constructs_covergroup`).
    let no_covergroups = !constructs_covergroups(
        discovery_analyses.values().chain(analyses.values()),
        &analyzed[&top].packages,
        &design,
    );
    let no_demotions = HashSet::default();
    let mut demoted: HashMap<ModuleId, HashSet<usize>> = HashMap::default();
    let mut trace = trace;
    let no_foreign_writes = HashMap::default();
    // Specialization keys, resolved hierarchical names and foreign writers are
    // fixed above. A retry changes only the demoted level lists of the modules
    // reported by `demote_level_lists`; retain every other lowered module.
    let mut lowered_modules = HashMap::default();
    let no_foreign_references = HashSet::default();
    let mut reported_forces = false;
    loop {
        let mut specialize = |key: &LoweredSvModuleKey,
                              module_id: ModuleId|
         -> Result<LoweredSvModule, FrontendError> {
            let base = analyzed
                .get(&key.name)
                .ok_or_else(|| unsupported_sv_instance(key.name.clone()))?;
            let level_lists = LevelLists::Lowered {
                demoted: demoted.get(&module_id).unwrap_or(&no_demotions),
            };
            let signals: Vec<_> = references
                .get(&module_id)
                .into_iter()
                .flatten()
                .map(|reference: &hierarchy::HierarchicalReference| reference.signal.clone())
                .collect();
            // Move out the discovery result: final lowering owns its source
            // module, so keeping another complete IR through retries is needless.
            let discovery_analysis = discovery_analyses.remove(&module_id);
            Ok(specialize_module(
                base,
                &mut design,
                key,
                four_state,
                parallel.enabled(),
                warnings,
                level_lists,
                HierarchicalNames {
                    signals: &signals,
                    foreign_writes: foreign_writes.get(&module_id).unwrap_or(&no_foreign_writes),
                    forced: forced.get(&module_id).map_or(&[], Vec::as_slice),
                    analyzed: analyses.get(&module_id),
                    recursion_report: analysis_options.recursion_report.as_deref(),
                    force_out_of_line: analysis_options.force_out_of_line_calls,
                    next_state: NextStateOptions {
                        report: analysis_options.ff_next_state_report.as_deref(),
                        forms: analysis_options.ff_next_state,
                    },
                    properties: &properties,
                    time_format,
                    instances: Instances::Known(&expansion),
                    shared: SharedAlways {
                        temporaries: analysis_options.shared_always_temporaries,
                        foreign_references: foreign_references
                            .get(&module_id)
                            .unwrap_or(&no_foreign_references),
                        no_covergroups,
                    },
                    same_instance_aliased: same_instance_aliased
                        .get(&module_id)
                        .unwrap_or(&no_foreign_references),
                    same_instance_alias_groups: same_instance_alias_groups
                        .get(&module_id)
                        .map_or(&[], Vec::as_slice),
                },
                discovery_analysis,
            )?)
        };
        for (key, &module_id) in &module_ids {
            if let std::collections::hash_map::Entry::Vacant(entry) =
                lowered_modules.entry(module_id)
            {
                entry.insert(specialize(key, module_id)?);
            }
        }
        // Every instance of a forced specialization carries its force
        // controls; report the specializations that more than one instance
        // shares, once.
        if !reported_forces && !forced.is_empty() {
            reported_forces = true;
            let mut counts: HashMap<ModuleId, usize> = HashMap::default();
            // A path deeper than the number of specializations goes round
            // a cycle of instances, which the validation of the module
            // graph below reports; nothing is counted then.
            let mut pending = vec![(root_id, 1usize, 0usize)];
            while let Some((module_id, count, depth)) = pending.pop() {
                if depth > lowered_modules.len() {
                    counts.clear();
                    break;
                }
                *counts.entry(module_id).or_default() += count;
                for instance in &lowered_modules[&module_id].instances {
                    if let Some(&child) =
                        module_ids.get(&LoweredSvModuleKey::instance_key(instance))
                    {
                        pending.push((child, count, depth + 1));
                    }
                }
            }
            let name_of = |module_id: ModuleId| {
                module_ids
                    .iter()
                    .find(|(_, id)| **id == module_id)
                    .map(|(key, _)| key.name.clone())
                    .unwrap_or_default()
            };
            let mut forced_modules: Vec<_> = forced
                .iter()
                .map(|(module_id, names)| (name_of(*module_id), module_id, names))
                .collect();
            forced_modules.sort_by(|left, right| left.0.cmp(&right.0));
            for (module, module_id, names) in forced_modules {
                let instances = counts.get(module_id).copied().unwrap_or(0);
                if instances > 1 {
                    warnings.push((
                        crate::FrontendWarningKind::ForcedSpecialization {
                            module,
                            forced: names
                                .iter()
                                .map(|name| format!("`{name}`"))
                                .collect::<Vec<_>>()
                                .join(", "),
                            instances,
                        },
                        None,
                    ));
                }
            }
        }
        checkpoint!("specialize_modules");
        resolve_inout_drivers(&module_ids, &mut lowered_modules);
        resolve_interconnects(&mut module_ids, &mut lowered_modules)?;
        validate_specialized_instance_net_drivers(&module_ids, &lowered_modules, four_state)?;
        validate_forced_interface_members(&module_ids, &lowered_modules, &forced)?;
        let root = &lowered_modules[&root_id];
        if let Some(port) = root
            .port_order
            .iter()
            .map(|port_id| &root.variables[port_id])
            .find(|port| port.kind == VariableKind::Inout)
        {
            return Err(unsupported_sv_inout(port.path.join(".")).into());
        }
        // The host writes an input of the top module directly, past the
        // controls of a procedural force of it (IEEE 1800-2023 10.6.2).
        if let Some(port) = root.port_order.iter().find(|&&id| {
            root.variables[&id].kind == VariableKind::Input
                && root.variables.values().any(|variable| {
                    variable
                        .force_control
                        .is_some_and(|slot| slot.procedural && slot.target == id)
                })
        }) {
            return Err(sv::AnalyzerError::Unsupported(format!(
                "force or release of `{}`, an input port of the top module, which the host drives",
                root.variables[port].path.join(".")
            ))
            .into());
        }
        validate_sv_module_graph(
            &root_key,
            &module_ids,
            &lowered_modules,
            &mut HashSet::default(),
            &mut HashSet::default(),
        )?;

        // Waits and flip-flop domains of other modules may watch the
        // signals that hierarchical names reach.
        checkpoint!("validate_graph");
        let mut hierarchical_targets: HashMap<ModuleId, HashSet<String>> = HashMap::default();
        for reference in references
            .values()
            .flatten()
            .chain(virtual_references.values().flatten())
        {
            for (module, target) in &reference.targets {
                hierarchical_targets
                    .entry(*module)
                    .or_default()
                    .insert(target.clone());
            }
        }
        for (module, targets) in &ref_port_targets {
            hierarchical_targets
                .entry(*module)
                .or_default()
                .extend(targets.iter().cloned());
        }
        let no_targets = HashSet::default();
        let mut instance_values = Vec::new();
        let mut modules = HashMap::default();
        let mut module_names = HashMap::default();
        // A combinational loop through level lists is scheduled again with
        // them demoted, which lowers their modules again and elaborates every
        // module anew. Without a list left to demote, a loop is final and
        // nothing below runs again: the elaborated modules take the lowered
        // modules' symbolic modules and calls instead of copies, and the
        // lowering is released before scheduling builds the program, the
        // frontend's largest.
        let last_attempt = !can_demote_level_lists(&lowered_modules, &demoted);
        let design_waits = DesignWaits::of(&lowered_modules);
        let mut home = None;
        for (key, &module_id) in &module_ids {
            let (mut sim_module, mut calls) = if last_attempt {
                let lowered = lowered_modules.get_mut(&module_id).unwrap();
                (
                    std::mem::take(&mut lowered.sim_module),
                    lowered.calls.take(),
                )
            } else {
                let lowered = &lowered_modules[&module_id];
                (lowered.sim_module.clone(), lowered.calls.clone())
            };
            let lowered = &lowered_modules[&module_id];
            if let Some(references) = references.get(&module_id) {
                sim_module.hierarchical_bindings =
                    hierarchy::bindings(references, lowered, &lowered_modules)?;
            }
            if let Some(references) = virtual_references.get(&module_id) {
                sim_module.hierarchical_bindings.extend(hierarchy::bindings(
                    references,
                    lowered,
                    &lowered_modules,
                )?);
            }
            if let Some(instances) = &virtual_instances {
                instance_values.extend(instances.values(module_id, lowered));
            }
            let elaborated = elaborate_instances(
                &mut sim_module,
                lowered,
                key,
                &module_ids,
                &lowered_modules,
                &design_waits,
                four_state,
                allow_inferred_latches,
                false,
                matches!(
                    parallel.unobserved_nets,
                    celox_design::DeadStorePolicy::PreserveTopPorts
                        | celox_design::DeadStorePolicy::PreserveAllPorts
                ),
                hierarchical_targets.get(&module_id).unwrap_or(&no_targets),
                same_instance_aliased.get(&module_id).unwrap_or(&no_targets),
                same_instance_alias_groups
                    .get(&module_id)
                    .map_or(&[][..], Vec::as_slice),
                same_instance_alias_partitions
                    .get(&module_id)
                    .map_or(&[][..], Vec::as_slice),
                warnings,
                calls.as_mut(),
                &mut design,
            );
            if let (Some(graph), Some(path)) =
                (&calls, analysis_options.recursion_report.as_deref())
            {
                recursion::append(&lowered.source, graph, path);
            }
            elaborated?;
            // Port expressions and processes share the module's call graph.
            if let Some(functions) = calls.as_ref().and_then(|graph| graph.out_of_line.as_ref()) {
                let kept = out_of_line::attach_functions(&mut sim_module, functions);
                // The state modules of the packages may call the root's
                // functions instead of lowering their own.
                if module_id == root_id && !hidden_root {
                    home = out_of_line::Home::new(
                        module_id,
                        &key.name,
                        lowered,
                        &sim_module,
                        functions,
                        &kept,
                    )
                    .map(std::sync::Arc::new);
                }
            }
            module_names.insert(module_id, key.name.clone());
            modules.insert(module_id, sim_module);
        }

        checkpoint!("elaborate_instances");
        for (key, id) in &module_ids {
            restore_interface_member_paths(modules.get_mut(id).unwrap(), &analyzed[&key.name]);
        }
        let package_states = package_states(
            &analyzed[&top].packages,
            &design,
            &properties,
            time_format,
            &expansion,
        );
        // Nothing reads the analyses past this point unless a retry lowers
        // modules again: release them before the packages are lowered.
        if last_attempt {
            drop(std::mem::take(&mut analyses));
            drop(std::mem::take(&mut discovery_analyses));
            drop(std::mem::take(&mut design));
            drop(std::mem::take(&mut analyzed));
        }
        let package_states = lower_package_states(
            package_states,
            module_ids.len(),
            four_state,
            parallel.enabled(),
            warnings,
            analysis_options.force_out_of_line_calls,
            analysis_options.recursion_report.as_deref(),
            home,
        )?;
        validate_package_variable_drivers([&root_key], &module_ids, &lowered_modules)?;
        let mut packages = Vec::new();
        for (name, module_id, lowered) in &package_states {
            module_names.insert(*module_id, name.clone());
            modules.insert(*module_id, lowered.sim_module.clone());
            packages.push((name.clone(), *module_id));
        }
        bind_package_variables(
            &mut modules,
            lowered_modules
                .iter()
                .map(|(&module_id, lowered)| (module_id, lowered)),
            &package_states,
        )?;
        let symbolic = SymbolicRtl {
            modules,
            module_names,
            root_id,
            hidden_root,
            packages,
            instance_values,
            site_homes: site_homes.clone(),
        };
        checkpoint!("packages");
        drop(package_states);
        if last_attempt {
            drop(std::mem::take(&mut lowered_modules));
        }
        let mut loop_members = Vec::new();
        let scheduled =
            celox_frontend_core::symbolic::assembly::schedule_symbolic_rtl_reporting_loops(
                symbolic,
                None,
                ignored_loops,
                true_loops,
                four_state,
                parallel,
                trace_options,
                trace.as_deref_mut(),
                &mut loop_members,
            );
        checkpoint!("schedule_symbolic_rtl");
        let error = match scheduled {
            Ok(scheduled) => return Ok(scheduled),
            Err(error) => error,
        };
        let changed = demote_level_lists(&loop_members, &lowered_modules, &mut demoted);
        if changed.is_empty() {
            return Err(error.into());
        }
        for module_id in changed {
            lowered_modules.remove(&module_id);
        }
    }
}

/// Whether a module has an `always @*` or equivalent explicit-list process
/// that [`demote_level_lists`] could still demote.
fn can_demote_level_lists(
    lowered_modules: &HashMap<ModuleId, LoweredSvModule>,
    demoted: &HashMap<ModuleId, HashSet<usize>>,
) -> bool {
    lowered_modules.iter().any(|(module_id, lowered)| {
        lowered.source.comb_processes().iter().any(|process| {
            process.kind() == sv::ir::CombProcessKind::AlwaysStar
                && !demoted
                    .get(module_id)
                    .is_some_and(|demoted| demoted.contains(&process.creation_order()))
        })
    })
}

/// Demote `always @*` and equivalent explicit-list processes which drive
/// a range of a combinational loop. Return the changed modules so only
/// their lowering is invalidated before scheduling again.
fn demote_level_lists(
    loop_members: &[celox_frontend_core::symbolic::assembly::LoopMember],
    lowered_modules: &HashMap<ModuleId, LoweredSvModule>,
    demoted: &mut HashMap<ModuleId, HashSet<usize>>,
) -> HashSet<ModuleId> {
    let mut changed = HashSet::default();
    for (module_id, member) in loop_members {
        let Some(lowered) = lowered_modules.get(module_id) else {
            continue;
        };
        let Some(signal_name) = lowered
            .signal_names
            .iter()
            .find_map(|(name, &id)| (id == member.id).then_some(name.as_str()))
        else {
            continue;
        };
        let scan = DriverScan::of(&lowered.source);
        for process in lowered.source.comb_processes() {
            let creation_order = process.creation_order();
            if process.kind() != sv::ir::CombProcessKind::AlwaysStar
                || demoted
                    .get(module_id)
                    .is_some_and(|demoted| demoted.contains(&creation_order))
            {
                continue;
            }
            let mut drives = false;
            scan.body(
                &mut |lvalue| drives |= lvalue.name() == signal_name,
                process.body(),
            );
            if drives {
                demoted
                    .entry(*module_id)
                    .or_default()
                    .insert(creation_order);
                changed.insert(*module_id);
            }
        }
    }
    changed
}

/// Whether a module of `analyses` or the state of a package of `packages`
/// can construct a covergroup (IEEE 1800-2023 19.3). A design in which none
/// can has no covergroup instance.
pub(super) fn constructs_covergroups<'a>(
    mut analyses: impl Iterator<Item = &'a sv::ir::Ir>,
    packages: &sv::Packages,
    design: &sv::Design,
) -> bool {
    analyses.any(|ir| {
        ir.modules()
            .iter()
            .any(sv::ir::Module::constructs_covergroup)
    }) || packages.states_construct_covergroups(design)
}

/// The overrides that the module classes of `analyses` export (see
/// `sv::ir::ExportedOverride`), those of one override merged.
pub(super) fn exported_overrides<'a>(
    analyses: impl Iterator<Item = &'a sv::ir::Ir> + Clone,
) -> Result<Vec<sv::ir::ExportedOverride>, sv::AnalyzerError> {
    let modules = analyses.flat_map(|ir| ir.modules());
    let taken: HashSet<String> = modules
        .clone()
        .flat_map(|module| module.subroutines())
        .map(|subroutine| subroutine.name.clone())
        .collect();
    sv::ir::merge_overrides(
        modules.flat_map(|module| module.exported_overrides().iter().cloned()),
        &taken,
    )
}
