//! Parameter specializations of a module and their lowering.

use super::*;

/// Bound parameter-changing recursion along one instantiation path. Independent
/// siblings may have any number of distinct parameter specializations. Repeated
/// identical keys are handled by the hierarchy cycle checks.
pub(super) fn register_specialization_parent(
    child: &LoweredSvModuleKey,
    parent: &LoweredSvModuleKey,
    parents: &mut HashMap<LoweredSvModuleKey, LoweredSvModuleKey>,
) -> Result<(), ParserError> {
    let mut ancestor = Some(parent);
    let mut occurrences = 1usize;
    while let Some(key) = ancestor {
        if key.name == child.name {
            occurrences += 1;
            if occurrences > MAX_SV_RECURSIVE_SPECIALIZATIONS {
                return Err(sv_specialization_limit_error(child.name.clone()));
            }
        }
        ancestor = parents.get(key);
    }
    parents.insert(child.clone(), parent.clone());
    Ok(())
}

fn sv_specialization_limit_error(name: String) -> ParserError {
    ParserError::unsupported(
        sv::SV_FRONTEND_TRACKING_ISSUE,
        LoweringPhase::SimulatorParser,
        "systemverilog module specialization limit exceeded (possible recursive instantiation)",
        name,
        None,
    )
}

pub(super) fn validate_sv_module_graph(
    key: &LoweredSvModuleKey,
    module_ids: &HashMap<LoweredSvModuleKey, ModuleId>,
    lowered_modules: &HashMap<ModuleId, LoweredSvModule>,
    active: &mut HashSet<LoweredSvModuleKey>,
    complete: &mut HashSet<LoweredSvModuleKey>,
) -> Result<(), ParserError> {
    if complete.contains(key) {
        return Ok(());
    }
    if !active.insert(key.clone()) {
        return Err(ParserError::unsupported(
            sv::SV_FRONTEND_TRACKING_ISSUE,
            LoweringPhase::SimulatorParser,
            "recursive systemverilog module instantiation",
            key.name.clone(),
            None,
        ));
    }
    let module_id = module_ids
        .get(key)
        .copied()
        .ok_or_else(|| unsupported_sv_instance(key.name.clone()))?;
    let module = lowered_modules
        .get(&module_id)
        .ok_or_else(|| unsupported_sv_instance(key.name.clone()))?;
    for instance in &module.instances {
        validate_sv_module_graph(
            &LoweredSvModuleKey::instance_key(instance),
            module_ids,
            lowered_modules,
            active,
            complete,
        )?;
    }
    active.remove(key);
    complete.insert(key.clone());
    Ok(())
}

/// The value, type and pattern parameter bindings of specialization `key`.
/// Type bindings are already resolved in the caller scope.
#[allow(clippy::type_complexity)]
fn specialization_overrides(
    key: &LoweredSvModuleKey,
) -> Result<
    (
        HashMap<String, sv::ir::ConstExpr>,
        Vec<(String, sv::ast::Type)>,
        Vec<(String, sv::ast::BoundPattern)>,
    ),
    sv::AnalyzerError,
> {
    let overrides = evaluated_parameter_overrides(&key.parameter_overrides)?;
    let types = key
        .parameter_overrides
        .iter()
        .filter_map(|parameter| Some((parameter.name.clone(), parameter.bound_type.clone()?)))
        .collect();
    let patterns = key
        .parameter_overrides
        .iter()
        .filter_map(|parameter| Some((parameter.name.clone(), parameter.bound_pattern.clone()?)))
        .collect();
    Ok((overrides, types, patterns))
}

/// Analyze specialization `key` of `module`, whose hierarchical names denote
/// `hierarchical`.
pub(super) fn analyze_specialization(
    module: &AnalyzedSvModule,
    design: &mut sv::Design,
    key: &LoweredSvModuleKey,
    hierarchical: &[sv::ast::HierarchicalSignal],
    warnings: &mut Vec<crate::warning::PendingWarning>,
) -> Result<sv::Ir, sv::AnalyzerError> {
    #[cfg(test)]
    tests::record_analysis(key);
    let (overrides, type_overrides, pattern_overrides) = specialization_overrides(key)?;
    let analyzed = module.source.analyze_module_with_hierarchy(
        &module.name,
        &overrides,
        &type_overrides,
        &pattern_overrides,
        &module.interfaces,
        &module.packages,
        design,
        hierarchical,
    );
    if let Err(error) = &analyzed {
        warnings.extend(crate::warning::analyzer_warnings(error.warnings()));
    }
    let ir = analyzed?;
    warnings.extend(crate::warning::analyzer_warnings(ir.warnings()));
    Ok(ir)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn specialize_module(
    module: &AnalyzedSvModule,
    design: &mut sv::Design,
    key: &LoweredSvModuleKey,
    four_state: bool,
    ff_parts: bool,
    warnings: &mut Vec<crate::warning::PendingWarning>,
    level_lists: LevelLists<'_>,
    hierarchical: HierarchicalNames<'_>,
    owned_analysis: Option<sv::Ir>,
) -> Result<LoweredSvModule, sv::AnalyzerError> {
    #[cfg(test)]
    tests::record_specialization(key, level_lists);
    let unsupported = || sv::AnalyzerError::Unsupported(format!("module `{}`", module.name));
    fn find<'i>(ir: &'i sv::Ir, name: &str) -> Option<&'i sv::ir::Module> {
        ir.modules()
            .iter()
            .find(|candidate| candidate.name() == name)
    }
    // The module lowered, and the same module when the recursion report
    // needs it after a failed lowering (it keeps the shared analysis then).
    let owned;
    let (specialized, reported): (
        std::borrow::Cow<'_, sv::ir::Module>,
        Option<&sv::ir::Module>,
    ) = match (hierarchical.analyzed, owned_analysis) {
        (Some(ir), _) => {
            warnings.extend(crate::warning::analyzer_warnings(ir.warnings()));
            let specialized = find(ir, &module.name).ok_or_else(unsupported)?;
            (std::borrow::Cow::Borrowed(specialized), Some(specialized))
        }
        (None, Some(ir)) if hierarchical.recursion_report.is_some() => {
            owned = ir;
            warnings.extend(crate::warning::analyzer_warnings(owned.warnings()));
            let specialized = find(&owned, &module.name).ok_or_else(unsupported)?;
            (std::borrow::Cow::Borrowed(specialized), Some(specialized))
        }
        (None, owned_analysis) => {
            let ir = match owned_analysis {
                Some(ir) => ir,
                None => {
                    let mut ir = analyze_specialization(
                        module,
                        design,
                        key,
                        hierarchical.signals,
                        warnings,
                    )?;
                    ir.expand_virtual_interface_instances(
                        hierarchical.virtual_ids.unwrap_or(&HashMap::default()),
                    );
                    ir
                }
            };
            warnings.extend(crate::warning::analyzer_warnings(ir.warnings()));
            let specialized = ir
                .into_modules()
                .into_iter()
                .find(|candidate| candidate.name() == module.name)
                .ok_or_else(unsupported)?;
            (std::borrow::Cow::Owned(specialized), None)
        }
    };
    // Each module may store or wait on a watched property.
    let own;
    let properties = if hierarchical.discovery {
        let mut keys = hierarchical.properties.clone();
        ff::watched_properties(&specialized, &mut keys);
        own = keys;
        &own
    } else {
        hierarchical.properties
    };
    let specialized = if properties.is_empty() {
        specialized
    } else {
        std::borrow::Cow::Owned(specialized.into_owned().with_shared_variables(
            ff::PROPERTY_CHANGES,
            &property_change_names(properties),
            ff::PROPERTY_CHANGES_WIDTH,
        ))
    };
    // Every module shares the format of `%t` of a design that calls
    // `$timeformat`; discovery sees whether this module does.
    let specialized = if hierarchical.time_format
        || (hierarchical.discovery && ff::calls_time_format(&specialized))
    {
        std::borrow::Cow::Owned(specialized.into_owned().with_shared_variables(
            ff::TIME_FORMAT,
            &[ff::TIME_FORMAT.to_string()],
            celox_design::time_format::WIDTH,
        ))
    } else {
        specialized
    };
    let local_forces = key
        .force_signals
        .iter()
        .map(|path| {
            module
                .interface_member_paths
                .iter()
                .find(|(_, segments)| segments.segments.join(".") == *path)
                .map_or_else(|| path.clone(), |(flat, _)| flat.clone())
        })
        .collect::<Vec<_>>();
    // The out-of-line functions are kept with the call graph, which every
    // lowering of the module's processes already receives. A function unit
    // must not store into storage whose stores assembly instruments
    // (`SimModule::instrumented_storage`), which the module's source shows
    // before it is lowered; waits and domains of other modules may watch
    // what hierarchical names reach.
    let mut calls = Some(recursion::CallGraph::new(
        hierarchical.force_out_of_line,
        hierarchical.recursion_report.is_some(),
    ));
    if let Some(functions) = calls.as_mut().and_then(|graph| graph.out_of_line.as_mut()) {
        functions.foreign = hierarchical.shared.foreign_references.clone();
    }
    let lowered = lower_module(
        specialized,
        four_state,
        module.implicit_nets_allowed,
        ff_parts,
        warnings,
        &local_forces,
        level_lists,
        hierarchical.foreign_writes,
        &module.interface_member_paths,
        hierarchical.shared,
        calls.as_mut(),
    );
    let mut lowered = match (lowered, &calls, hierarchical.recursion_report, reported) {
        (Ok(lowered), _, _, _) => lowered,
        // Report the graph recorded up to the error.
        (Err(error), Some(graph), Some(path), Some(specialized)) => {
            recursion::append(specialized, graph, path);
            return Err(error);
        }
        (Err(error), ..) => return Err(error),
    };
    lowered.calls = calls;
    propagate_force_signals(&mut lowered.instances, &key.force_signals);
    lowered.sim_module.time_precision = Some(
        celox_design::TimePrecision::try_from(module.packages.global_time_precision())
            .map_err(|reason| sv::AnalyzerError::Unsupported(reason.to_string()))?,
    );
    lowered.sim_module.time_scale = scope_time_scale(&module.packages, &module.name)?;
    Ok(lowered)
}

/// The hierarchical name at which the analysis of a module failed, which
/// needs the specialized hierarchy below it.
pub(super) fn hierarchical_reference_path(error: &sv::AnalyzerError) -> Option<&str> {
    match error {
        sv::AnalyzerError::HierarchicalReference { path } => Some(path),
        sv::AnalyzerError::WithWarnings { error, .. } => hierarchical_reference_path(error),
        _ => None,
    }
}

/// Hand each instance the force targets of `force_signals` below it.
pub(super) fn propagate_force_signals(
    instances: &mut [LoweredSvInstance],
    force_signals: &[String],
) {
    for instance in instances {
        let name = if let Some((index, _)) = instance.array_element {
            format!(
                "{}[{}]",
                instance.instance_name,
                instance.array_index_base + index
            )
        } else {
            instance.instance_name.clone()
        };
        let prefix = format!("{name}.");
        instance.force_signals = force_signals
            .iter()
            .filter_map(|path| path.strip_prefix(&prefix).map(str::to_string))
            .collect();
    }
}

/// The declarations and instances of specialization `key`.
pub(super) fn specialize_module_declarations(
    module: &AnalyzedSvModule,
    design: &mut sv::Design,
    key: &LoweredSvModuleKey,
) -> Result<hierarchy::ElaboratedModule, sv::AnalyzerError> {
    let (overrides, type_overrides, pattern_overrides) = specialization_overrides(key)?;
    let ir = module.source.analyze_module_hierarchy(
        &module.name,
        &overrides,
        &type_overrides,
        &pattern_overrides,
        &module.interfaces,
        &module.packages,
        design,
    )?;
    let specialized = ir
        .modules()
        .iter()
        .find(|candidate| candidate.name() == module.name)
        .ok_or_else(|| sv::AnalyzerError::Unsupported(format!("module `{}`", module.name)))?;
    hierarchy::ElaboratedModule::from_declarations(specialized, &key.force_signals)
}

pub(super) fn scope_time_scale(
    packages: &sv::Packages,
    name: &str,
) -> Result<celox_design::TimeScale, sv::AnalyzerError> {
    let Some((unit, precision)) = packages.scope_time_scale(name) else {
        return Ok(celox_design::TimeScale::default());
    };
    let convert = |exponent: i32| {
        celox_design::TimePrecision::try_from(exponent)
            .map_err(|reason| sv::AnalyzerError::Unsupported(reason.to_string()))
    };
    Ok(celox_design::TimeScale {
        unit: convert(unit)?,
        precision: convert(precision)?,
    })
}
