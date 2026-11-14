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
    let analyzed = analyze_sources(sources, &[top], analysis_options.clone())?;
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
        force_signals: force_signals
            .iter()
            .filter_map(|path| path.strip_prefix(&format!("{top}.")))
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
    // The virtual interfaces of the design, and the analyses of the modules
    // that may refer to their instances (see `virtual_interfaces`).
    let virtual_interfaces = analyzed[&top].packages.virtual_interfaces().cloned();
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
            Err(error) if hierarchical_reference_path(&error).is_some() => {
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
    let virtual_instances = virtual_interfaces.as_ref().map(|declarations| {
        (
            virtual_interfaces::Instances::elaborate(
                root_id,
                &module_ids,
                &elaborated,
                declarations,
            ),
            declarations.clone(),
        )
    });
    let virtual_ids = virtual_instances
        .as_ref()
        .map(|(instances, _)| instances.ids())
        .unwrap_or_default();
    if virtual_instances.is_some() {
        let names: HashMap<ModuleId, &str> = module_ids
            .iter()
            .map(|(key, &id)| (id, key.name.as_str()))
            .collect();
        for (module_id, ir) in &mut discovery_analyses {
            ir.expand_virtual_interface_instances(&virtual_ids);
            if let Some(source) = ir
                .modules()
                .iter()
                .find(|module| Some(module.name()) == names.get(module_id).copied())
            {
                virtual_sources.insert(*module_id, source.clone());
            }
        }
    }
    let keys: HashMap<ModuleId, &LoweredSvModuleKey> =
        module_ids.iter().map(|(key, &id)| (id, key)).collect();
    let hierarchy = hierarchy::Hierarchy::new(
        (&root_key, root_id),
        &keys,
        &module_ids,
        &elaborated,
        &analyzed,
    );
    // The signals that hierarchical names write gain writers outside their
    // own modules. Collect them from the analyzed modules, independently of
    // the order the modules are lowered in.
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
                    Err(error) => match hierarchical_reference_path(&error) {
                        Some(path) if waiting.iter().any(|(written, _)| *written == path) => {
                            if waiting
                                .iter()
                                .any(|&(written, id)| written == path && id == module_id)
                            {
                                return Err(sv::AnalyzerError::Unsupported(format!(
                                    "hierarchical call `{path}` of a subroutine of the calling \
                                     module itself"
                                ))
                                .into());
                            }
                            // The callee's own analysis is still to come.
                            break None;
                        }
                        Some(path) => {
                            let components =
                                path.split('.').map(str::to_string).collect::<Vec<_>>();
                            if !hierarchy::resolve_name(
                                &hierarchy,
                                module_id,
                                &components,
                                &mut resolved,
                            )? && !hierarchy::resolve_call(
                                &hierarchy,
                                module_id,
                                &components,
                                &mut calls,
                            )? {
                                return Err(error.into());
                            }
                        }
                        None => return Err(error.into()),
                    },
                }
            };
            let Some(mut ir) = analyzed_ir else {
                deferred.push(module_id);
                continue;
            };
            ir.expand_virtual_interface_instances(&virtual_ids);
            if let Some(module) = ir.modules().iter().find(|module| module.name() == key.name) {
                hierarchy::collect_foreign_writes(module, &resolved, &mut foreign_writes);
            }
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
    // The elaboration only served to resolve the hierarchical names.
    drop(hierarchy);
    drop(elaborated);
    // Signals that denote members of interface instances for virtual
    // interfaces, which other modules then write and watch.
    let mut virtual_references: HashMap<ModuleId, Vec<hierarchy::HierarchicalReference>> =
        HashMap::default();
    if let Some((instances, declarations)) = &virtual_instances {
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
            let references = instances.references(ir, declarations);
            if references.is_empty() {
                continue;
            }
            hierarchy::collect_virtual_writes(ir, &references, &mut foreign_writes);
            virtual_references.insert(module_id, references);
        }
    }
    drop(virtual_sources);
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
    let no_demotions = HashSet::default();
    let mut demoted: HashMap<ModuleId, HashSet<usize>> = HashMap::default();
    let mut trace = trace;
    let no_foreign_writes = HashMap::default();
    // Specialization keys, resolved hierarchical names and foreign writers are
    // fixed above. A retry changes only the demoted level lists of the modules
    // reported by `demote_level_lists`; retain every other lowered module.
    let mut lowered_modules = HashMap::default();
    let no_foreign_references = HashSet::default();
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
                    analyzed: analyses.get(&module_id),
                    recursion_report: analysis_options.recursion_report.as_deref(),
                    force_out_of_line: analysis_options.force_out_of_line_calls,
                    properties: &properties,
                    time_format,
                    virtual_ids: Some(&virtual_ids),
                    discovery: false,
                    shared: SharedAlways {
                        temporaries: analysis_options.shared_always_temporaries,
                        foreign_references: foreign_references
                            .get(&module_id)
                            .unwrap_or(&no_foreign_references),
                    },
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
        checkpoint!("specialize_modules");
        resolve_inout_drivers(&module_ids, &mut lowered_modules);
        validate_specialized_instance_net_drivers(&module_ids, &lowered_modules, four_state)?;
        let root = &lowered_modules[&root_id];
        if let Some(port) = root
            .port_order
            .iter()
            .map(|port_id| &root.variables[port_id])
            .find(|port| port.kind == VariableKind::Inout)
        {
            return Err(unsupported_sv_inout(port.path.join(".")).into());
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
        let no_targets = HashSet::default();
        let mut instance_values = Vec::new();
        let mut modules = HashMap::default();
        let mut module_names = HashMap::default();
        for (key, &module_id) in &module_ids {
            let lowered = &lowered_modules[&module_id];
            let mut sim_module = lowered.sim_module.clone();
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
            if let Some((instances, _)) = &virtual_instances {
                instance_values.extend(instances.values(module_id, lowered));
            }
            let mut calls = lowered.calls.clone();
            let glued = attach_instance_glue(
                &mut sim_module,
                lowered,
                key,
                &module_ids,
                &lowered_modules,
                four_state,
                allow_inferred_latches,
                false,
                hierarchical_targets.get(&module_id).unwrap_or(&no_targets),
                warnings,
                calls.as_mut(),
            );
            if let (Some(graph), Some(path)) =
                (&calls, analysis_options.recursion_report.as_deref())
            {
                recursion::append(&lowered.source, graph, path);
            }
            glued?;
            // The functions of every lowering of the module, its glue
            // included: the glue lowered with the graph the module's
            // processes left.
            if let Some(functions) = calls.as_ref().and_then(|graph| graph.out_of_line.as_ref()) {
                out_of_line::attach_functions(
                    &mut sim_module,
                    functions.units(),
                    functions.frames(),
                );
            }
            module_names.insert(module_id, key.name.clone());
            modules.insert(module_id, sim_module);
        }

        checkpoint!("instance_glue");
        let package_states = lower_package_states(
            &analyzed[&top].packages,
            &design,
            module_ids.len(),
            four_state,
            parallel.enabled(),
            &properties,
            time_format,
            warnings,
            analysis_options.force_out_of_line_calls,
            analysis_options.recursion_report.as_deref(),
            &virtual_ids,
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
        for (key, id) in &module_ids {
            restore_interface_member_paths(modules.get_mut(id).unwrap(), &analyzed[&key.name]);
        }
        let symbolic = SymbolicRtl {
            modules,
            module_names,
            root_id,
            packages,
            instance_values,
        };
        checkpoint!("packages");
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
