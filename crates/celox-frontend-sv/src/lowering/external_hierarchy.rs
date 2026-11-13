//! Lowering of requested modules into a hierarchy another frontend embeds.

use super::*;

/// Lower the requested SystemVerilog roots and their reachable children into
/// an embeddable hierarchy. The module IDs in the returned graph are local and
/// are remapped during mixed-language symbolic hierarchy assembly.
pub fn prepare_external_hierarchy(
    sources: &[(&str, &Path)],
    root_names: &HashSet<String>,
    four_state: bool,
    warnings: &mut Vec<crate::FrontendWarning>,
) -> Result<ExternalHierarchy, FrontendError> {
    prepare_external_hierarchy_with_foreign(
        sources,
        root_names,
        &ExternalHierarchy::default(),
        &[],
        four_state,
        warnings,
    )
}

/// Link SV bodies against foreign signatures. Foreign bodies are placeholders
/// here; their owning frontend replaces them before symbolic scheduling.
pub fn prepare_external_hierarchy_with_foreign(
    sources: &[(&str, &Path)],
    root_names: &HashSet<String>,
    foreign: &ExternalHierarchy,
    declarations: &[(&str, &Path)],
    four_state: bool,
    warnings: &mut Vec<crate::FrontendWarning>,
) -> Result<ExternalHierarchy, FrontendError> {
    prepare_external_hierarchy_with_foreign_resolver(
        sources,
        root_names,
        foreign,
        declarations,
        four_state,
        None,
        None,
        &[],
        warnings,
    )
}

pub type ForeignModuleResolver<'a> =
    dyn FnMut(&str, &[(String, SvParameterValue)]) -> Result<ExternalModule, ParserError> + 'a;

/// Appends the warnings of the lowered modules to `warnings`, also when
/// lowering fails after them.
#[allow(clippy::too_many_arguments)]
pub fn prepare_external_hierarchy_with_foreign_resolver(
    sources: &[(&str, &Path)],
    root_names: &HashSet<String>,
    foreign: &ExternalHierarchy,
    declarations: &[(&str, &Path)],
    four_state: bool,
    resolver: Option<&mut ForeignModuleResolver<'_>>,
    top_parameters: Option<(&str, &[(String, u64)])>,
    requests: &[SvModuleRequest],
    warnings: &mut Vec<crate::FrontendWarning>,
) -> Result<ExternalHierarchy, FrontendError> {
    prepare_external_hierarchy_with_foreign_resolver_with_options(
        sources,
        root_names,
        foreign,
        declarations,
        four_state,
        resolver,
        top_parameters,
        requests,
        warnings,
        sv::AnalysisOptions::default(),
    )
}

/// Prepare a mixed hierarchy with explicit language compatibility options.
#[allow(clippy::too_many_arguments)]
pub fn prepare_external_hierarchy_with_foreign_resolver_with_options(
    sources: &[(&str, &Path)],
    root_names: &HashSet<String>,
    foreign: &ExternalHierarchy,
    declarations: &[(&str, &Path)],
    four_state: bool,
    resolver: Option<&mut ForeignModuleResolver<'_>>,
    top_parameters: Option<(&str, &[(String, u64)])>,
    requests: &[SvModuleRequest],
    warnings: &mut Vec<crate::FrontendWarning>,
    analysis_options: sv::AnalysisOptions,
) -> Result<ExternalHierarchy, FrontendError> {
    prepare_external_hierarchy_with_foreign_resolver_with_options_and_latch_policy(
        sources,
        root_names,
        foreign,
        declarations,
        four_state,
        resolver,
        top_parameters,
        requests,
        false,
        warnings,
        analysis_options,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn prepare_external_hierarchy_with_foreign_resolver_and_latch_policy(
    sources: &[(&str, &Path)],
    root_names: &HashSet<String>,
    foreign: &ExternalHierarchy,
    declarations: &[(&str, &Path)],
    four_state: bool,
    resolver: Option<&mut ForeignModuleResolver<'_>>,
    top_parameters: Option<(&str, &[(String, u64)])>,
    requests: &[SvModuleRequest],
    allow_inferred_latches: bool,
    warnings: &mut Vec<crate::FrontendWarning>,
) -> Result<ExternalHierarchy, FrontendError> {
    prepare_external_hierarchy_with_foreign_resolver_with_options_and_latch_policy(
        sources,
        root_names,
        foreign,
        declarations,
        four_state,
        resolver,
        top_parameters,
        requests,
        allow_inferred_latches,
        warnings,
        sv::AnalysisOptions::default(),
    )
}

/// Prepare a mixed hierarchy with both compatibility and latch options.
#[allow(clippy::too_many_arguments)]
pub fn prepare_external_hierarchy_with_foreign_resolver_with_options_and_latch_policy(
    sources: &[(&str, &Path)],
    root_names: &HashSet<String>,
    foreign: &ExternalHierarchy,
    declarations: &[(&str, &Path)],
    four_state: bool,
    resolver: Option<&mut ForeignModuleResolver<'_>>,
    top_parameters: Option<(&str, &[(String, u64)])>,
    requests: &[SvModuleRequest],
    allow_inferred_latches: bool,
    warnings: &mut Vec<crate::FrontendWarning>,
    analysis_options: sv::AnalysisOptions,
) -> Result<ExternalHierarchy, FrontendError> {
    let mut found = Vec::new();
    let hierarchy = prepare_external_hierarchy_collecting(
        sources,
        root_names,
        foreign,
        declarations,
        four_state,
        resolver,
        top_parameters,
        requests,
        allow_inferred_latches,
        &mut found,
        analysis_options,
    );
    append_error_warnings(&hierarchy, &mut found);
    crate::warning::append_unique_warnings(sources, warnings, found);
    hierarchy
}

#[allow(clippy::too_many_arguments)]
fn prepare_external_hierarchy_collecting(
    sources: &[(&str, &Path)],
    root_names: &HashSet<String>,
    foreign: &ExternalHierarchy,
    declarations: &[(&str, &Path)],
    four_state: bool,
    mut resolver: Option<&mut ForeignModuleResolver<'_>>,
    top_parameters: Option<(&str, &[(String, u64)])>,
    requests: &[SvModuleRequest],
    allow_inferred_latches: bool,
    warnings: &mut Vec<crate::warning::PendingWarning>,
    analysis_options: sv::AnalysisOptions,
) -> Result<ExternalHierarchy, FrontendError> {
    let force_out_of_line = analysis_options.force_out_of_line_calls;
    let (analyzed, bound_names) = analyze_sources_with_requests(
        sources,
        foreign,
        declarations,
        requests,
        Some(&root_names.iter().map(String::as_str).collect::<Vec<_>>()),
        analysis_options,
    )?;
    package_warnings(&analyzed, warnings);
    // The specializations that the modules request of package class
    // templates, shared by all of their analyses.
    let mut design = analyzed
        .values()
        .next()
        .map_or_else(sv::Design::default, |module| module.packages.design());
    let dummy = sv::ParsedSource::parse(
        "module __celox_foreign(); endmodule",
        Path::new("<foreign-signature>"),
    )?;
    let dummy_ir = dummy.analyze_module(
        "__celox_foreign",
        &HashMap::default(),
        &[],
        &[],
        &sv::ModuleInterfaces::default(),
        &sv::Packages::default(),
        &mut sv::Design::default(),
    )?;
    let lower_foreign = |signature: &ExternalModule| {
        let variables: HashMap<_, _> = signature
            .sim_module
            .variables
            .iter()
            .map(|(&id, variable)| {
                (
                    id,
                    SvVariable {
                        float: None,

                        value_kind: celox_design::ValueKind::Bits,
                        path: vec![variable.path.join("$")],
                        width: variable.metadata.width,
                        signed: variable.signed,
                        is_4state: variable.metadata.is_4state,
                        is_unpacked_struct: false,
                        packed_ranges: variable
                            .packed_dims
                            .iter()
                            .map(|&width| ((width - 1) as i128, 0))
                            .collect(),
                        array_dims: variable.metadata.array_dims.clone(),
                        unpacked_ranges: variable.unpacked_ranges.clone(),
                        domain_kind: variable.metadata.kind,
                        kind: variable.kind,
                        type_kind: variable.metadata.type_kind,
                        source: variable.source.clone(),
                        hidden: !variable.module_affiliated,
                        force_control: None,
                        sampled_from: None,
                        history_of: None,
                        activation_scratch: false,
                    },
                )
            })
            .collect();
        LoweredSvModule {
            source: dummy_ir.modules()[0].clone(),
            implicit_nets_allowed: false,
            sim_module: signature.sim_module.clone(),
            signal_names: variables
                .iter()
                .map(|(&id, variable)| (variable.path.join("."), id))
                .collect(),
            variables,
            port_order: signature.port_order.clone(),
            constants: HashMap::default(),
            parameter_types: HashMap::default(),
            instances: Vec::new(),
            package_variables: Vec::new(),
            // The foreign module's own combinational blocks carry its copies.
            event_copies: Vec::new(),
            event_assignments: Vec::new(),
            repeated_ff_writes: Vec::new(),
            inout_driven_inside: HashSet::default(),
            calls: None,
        }
    };
    let foreign_modules: HashMap<_, _> = foreign
        .roots
        .iter()
        .map(|(name, id)| (name.clone(), lower_foreign(&foreign.modules[id])))
        .collect();
    let mut foreign_specializations = HashMap::default();
    let mut names = root_names
        .iter()
        .filter(|&name| {
            analyzed.contains_key(name)
                || foreign_modules.contains_key(name)
                || requests.iter().any(|request| {
                    &request.alias == name
                        && analyzed.contains_key(bound_names.get(name).unwrap_or(&request.module))
                })
        })
        .cloned()
        .collect::<Vec<_>>();
    names.sort();

    let mut module_ids = HashMap::default();
    let mut specialization_parents = HashMap::default();
    let mut queue = Vec::new();
    let mut requested_roots = HashMap::default();
    for name in names {
        let mut key = LoweredSvModuleKey::base(name.clone());
        if let Some(request) = requests.iter().find(|request| request.alias == name) {
            key.name = bound_names
                .get(&request.alias)
                .unwrap_or(&request.module)
                .clone();
            key.parameter_overrides = request
                .parameters
                .iter()
                .map(|(name, value)| LoweredSvParameterOverride {
                    name: name.clone(),
                    value: Some(value.expression()),
                    bound_type: None,
                    bound_pattern: None,
                })
                .collect();
            requested_roots.insert(name.clone(), key.clone());
        }
        if let Some((top, parameters)) = top_parameters
            && name == top
            && analyzed.contains_key(top)
        {
            key.parameter_overrides = parameters
                .iter()
                .map(|(name, value)| LoweredSvParameterOverride {
                    name: name.clone(),
                    value: Some(sv::ir::ConstExpr::Literal(value.to_string())),
                    bound_type: None,
                    bound_pattern: None,
                })
                .collect();
        }
        if module_ids.contains_key(&key) {
            continue;
        }
        let module_id = ModuleId(module_ids.len());
        module_ids.insert(key.clone(), module_id);
        queue.push(key);
    }

    // The keys of the class properties that level waits read, in any
    // module or package, whose stores every module counts (see
    // `ff::property_changes`), collected while discovering the hierarchy.
    let mut properties = BTreeSet::new();
    // Every module shares the format of `%t` if any calls `$timeformat`.
    let mut time_format = false;
    let mut index = 0;
    while index < queue.len() {
        let key = queue[index].clone();
        index += 1;
        if foreign_modules.contains_key(&key.name) {
            let specialized;
            let signature = if key.parameter_overrides.is_empty() {
                &foreign.modules[&foreign.roots[&key.name]]
            } else {
                let Some(resolver) = resolver.as_deref_mut() else {
                    return Err(sv::AnalyzerError::Unsupported(format!(
                        "foreign module parameter specialization `{}`",
                        key.name
                    ))
                    .into());
                };
                let parameters = key
                    .parameter_overrides
                    .iter()
                    .map(|parameter| {
                        let value = parameter
                            .value
                            .as_ref()
                            .and_then(SvParameterValue::from_expression)
                            .ok_or_else(|| {
                                sv::AnalyzerError::Unsupported(format!(
                                    "foreign parameter `{}` without a numeric value",
                                    parameter.name
                                ))
                            })?;
                        Ok((parameter.name.clone(), value))
                    })
                    .collect::<Result<Vec<_>, sv::AnalyzerError>>()?;
                specialized = resolver(&key.name, &parameters)?;
                foreign_specializations.insert(key.clone(), lower_foreign(&specialized));
                &specialized
            };
            for name in &signature.unresolved_instances {
                let mut dependency = LoweredSvModuleKey::base(name.clone());
                if let Some(request) = requests.iter().find(|request| &request.alias == name) {
                    dependency.name = bound_names.get(name).unwrap_or(&request.module).clone();
                    dependency.parameter_overrides = request
                        .parameters
                        .iter()
                        .map(|(name, value)| LoweredSvParameterOverride {
                            name: name.clone(),
                            value: Some(value.expression()),
                            bound_type: None,
                            bound_pattern: None,
                        })
                        .collect();
                    requested_roots.insert(name.clone(), dependency.clone());
                }
                if !analyzed.contains_key(&dependency.name)
                    && !foreign_modules.contains_key(&dependency.name)
                {
                    continue;
                }
                if !module_ids.contains_key(&dependency) {
                    let id = ModuleId(module_ids.len());
                    module_ids.insert(dependency.clone(), id);
                    queue.push(dependency);
                }
            }
            continue;
        }
        let base = analyzed
            .get(&key.name)
            .ok_or_else(|| unsupported_sv_instance(key.name.clone()))?;
        let lowered = specialize_module(
            base,
            &mut design,
            &key,
            four_state,
            false,
            warnings,
            LevelLists::Processes,
            HierarchicalNames {
                forced: &[],
                discovery: true,
                ..HierarchicalNames::default()
            },
            None,
        )?;
        ff::watched_properties(&lowered.source, &mut properties);
        time_format |= ff::calls_time_format(&lowered.source);
        for instance in &lowered.instances {
            let child_key = LoweredSvModuleKey::instance_key(instance);
            if !analyzed.contains_key(&child_key.name)
                && !foreign_modules.contains_key(&child_key.name)
            {
                continue;
            }
            if !module_ids.contains_key(&child_key) {
                register_specialization_parent(&child_key, &key, &mut specialization_parents)?;
                let child_id = ModuleId(module_ids.len());
                module_ids.insert(child_key.clone(), child_id);
                queue.push(child_key);
            }
        }
    }

    if let Some(module) = analyzed.values().next() {
        package_watched_properties(&module.packages, &design, &mut properties);
        time_format |= packages_call_time_format(&module.packages, &design);
    }
    let mut lowered_modules = module_ids
        .iter()
        .map(|(key, &module_id)| {
            if let Some(module) = foreign_specializations.get(key) {
                return Ok((module_id, module.clone()));
            }
            if let Some(module) = foreign_modules.get(&key.name) {
                return Ok((module_id, module.clone()));
            }
            let base = analyzed
                .get(&key.name)
                .ok_or_else(|| unsupported_sv_instance(key.name.clone()))?;
            Ok((
                module_id,
                specialize_module(
                    base,
                    &mut design,
                    key,
                    four_state,
                    false,
                    warnings,
                    LevelLists::Processes,
                    HierarchicalNames {
                        forced: &[],
                        properties: &properties,
                        time_format,
                        ..HierarchicalNames::default()
                    },
                    None,
                )?,
            ))
        })
        .collect::<Result<HashMap<_, _>, FrontendError>>()?;
    resolve_inout_drivers(&module_ids, &mut lowered_modules);
    validate_specialized_instance_net_drivers(&module_ids, &lowered_modules, four_state)?;
    let mut modules = HashMap::default();
    for (key, &module_id) in &module_ids {
        let lowered = &lowered_modules[&module_id];
        let mut sim_module = lowered.sim_module.clone();
        let unresolved_instances: Vec<String> = lowered
            .instances
            .iter()
            .filter_map(|instance| {
                (!module_ids.contains_key(&LoweredSvModuleKey::instance_key(instance)))
                    .then_some(instance.module_name.clone())
            })
            .collect();
        let mut resolved = lowered.clone();
        resolved.instances.retain(|instance| {
            module_ids.contains_key(&LoweredSvModuleKey::instance_key(instance))
        });
        attach_instance_glue(
            &mut sim_module,
            &resolved,
            key,
            &module_ids,
            &lowered_modules,
            four_state,
            allow_inferred_latches,
            true,
            &HashSet::default(),
            warnings,
            None,
        )?;
        modules.insert(
            module_id,
            ExternalModule {
                sim_module,
                port_order: lowered.port_order.clone(),
                unresolved_instances,
            },
        );
    }
    let mut roots: HashMap<_, _> = module_ids
        .iter()
        .filter(|(key, _)| key.parameter_overrides.is_empty())
        .map(|(key, &module_id)| (key.name.clone(), module_id))
        .collect();
    roots.extend(
        requested_roots
            .into_iter()
            .map(|(alias, key)| (alias, module_ids[&key])),
    );
    if let Some((top, _)) = top_parameters
        && let Some(key) = queue.iter().find(|key| key.name == top)
    {
        roots.insert(top.to_string(), module_ids[key]);
    }
    let Some(any_module) = analyzed.values().next() else {
        return Ok(ExternalHierarchy {
            modules,
            roots,
            packages: Vec::new(),
        });
    };
    let package_states = lower_package_states(
        &any_module.packages,
        &design,
        module_ids.len(),
        four_state,
        false,
        &properties,
        time_format,
        warnings,
        force_out_of_line,
        None,
        &HashMap::default(),
    )?;
    // How often the design instantiates these modules is not known here;
    // assembly checks the drivers of package variables across instances.
    let mut sim_modules: HashMap<ModuleId, SimModule> = modules
        .iter()
        .map(|(&module_id, module)| (module_id, module.sim_module.clone()))
        .collect();
    let mut packages = Vec::new();
    for (name, module_id, lowered) in &package_states {
        sim_modules.insert(*module_id, lowered.sim_module.clone());
        packages.push((name.clone(), *module_id));
    }
    bind_package_variables(
        &mut sim_modules,
        lowered_modules
            .iter()
            .map(|(&module_id, lowered)| (module_id, lowered)),
        &package_states,
    )?;
    for (module_id, sim_module) in sim_modules {
        match modules.get_mut(&module_id) {
            Some(module) => module.sim_module = sim_module,
            None => {
                modules.insert(
                    module_id,
                    ExternalModule {
                        sim_module,
                        port_order: Vec::new(),
                        unresolved_instances: Vec::new(),
                    },
                );
            }
        }
    }
    for (key, id) in &module_ids {
        if let Some(base) = analyzed.get(&key.name) {
            restore_interface_member_paths(&mut modules.get_mut(id).unwrap().sim_module, base);
        }
    }
    Ok(ExternalHierarchy {
        modules,
        roots,
        packages,
    })
}
