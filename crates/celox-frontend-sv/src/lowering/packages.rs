//! Package variables: the state modules of packages and their bindings.

use super::*;

/// Report the warnings of analyzing the packages the modules share, before
/// lowering a module can fail.
pub(super) fn package_warnings(
    analyzed: &HashMap<String, AnalyzedSvModule>,
    warnings: &mut Vec<crate::warning::PendingWarning>,
) {
    if let Some(module) = analyzed.values().next() {
        warnings.extend(crate::warning::analyzer_warnings(
            module.packages.warnings(),
        ));
    }
}

/// The packages that declare variables, lowered as the modules of their one
/// instance each, numbered from `first`.
/// The state modules of the packages, with what lowering them reads of the
/// analysis of the packages, so that the analysis can be released before
/// they are lowered (see [`lower_package_states`]).
pub(super) struct PackageStates {
    /// Each state module in lowering order, with its time scale.
    states: Vec<(
        sv::ir::Module,
        Result<celox_design::TimeScale, sv::AnalyzerError>,
    )>,
    global_precision: i32,
}

pub(super) fn package_states(
    packages: &sv::Packages,
    design: &sv::Design,
    properties: &BTreeSet<u32>,
    time_format: bool,
    virtual_ids: &HashMap<String, Vec<u32>>,
) -> PackageStates {
    let mut states = packages.specialized_state_modules(design);
    // Their initializers may reach interface instances through virtual
    // interfaces, as modules do.
    for state in &mut states {
        state.expand_virtual_interface_instances(virtual_ids);
    }
    // The change counts of the watched class properties (see
    // `ff::property_changes`), shared by every module and package, whose
    // initializers may store the properties too.
    if !properties.is_empty() {
        let names = property_change_names(properties);
        states = states
            .into_iter()
            .map(|state| {
                state.with_shared_variables(
                    ff::PROPERTY_CHANGES,
                    &names,
                    ff::PROPERTY_CHANGES_WIDTH,
                )
            })
            .collect();
        states.push(sv::ir::Module::shared_variables(
            ff::PROPERTY_CHANGES,
            &names,
            ff::PROPERTY_CHANGES_WIDTH,
        ));
    }
    // The format of `%t` of a design that calls `$timeformat` (see
    // `ff::TIME_FORMAT`), which the packages' sites read too.
    if time_format {
        let names = [ff::TIME_FORMAT.to_string()];
        let width = celox_design::time_format::WIDTH;
        states = states
            .into_iter()
            .map(|state| state.with_shared_variables(ff::TIME_FORMAT, &names, width))
            .collect();
        states.push(sv::ir::Module::shared_variables(
            ff::TIME_FORMAT,
            &names,
            width,
        ));
    }
    states.sort_by(|left, right| left.name().cmp(right.name()));
    PackageStates {
        states: states
            .into_iter()
            .map(|module| {
                let time_scale = scope_time_scale(packages, module.name());
                (module, time_scale)
            })
            .collect(),
        global_precision: packages.global_time_precision(),
    }
}

pub(super) fn lower_package_states(
    states: PackageStates,
    first: usize,
    four_state: bool,
    ff_parts: bool,
    warnings: &mut Vec<crate::warning::PendingWarning>,
    force_out_of_line: bool,
    recursion_report: Option<&Path>,
) -> Result<Vec<(String, ModuleId, LoweredSvModule)>, sv::AnalyzerError> {
    let PackageStates {
        states,
        global_precision,
    } = states;
    states
        .into_iter()
        .enumerate()
        .map(|(index, (module, time_scale))| {
            let name = module.name().to_string();
            // The subroutines that the package's initializers call go out of
            // line as a module's do, recursive ones included.
            let mut calls = Some(recursion::CallGraph::new(
                force_out_of_line,
                recursion_report.is_some(),
            ));
            let report_source = recursion_report.map(|_| module.clone());
            let lowered = lower_module(
                std::borrow::Cow::Owned(module),
                four_state,
                true,
                ff_parts,
                warnings,
                &[],
                LevelLists::Processes,
                &HashMap::default(),
                &HashMap::default(),
                SharedAlways::default(),
                calls.as_mut(),
            );
            if let (Some(graph), Some(path), Some(module)) =
                (&calls, recursion_report, &report_source)
            {
                recursion::append(module, graph, path);
            }
            let mut lowered = lowered?;
            if let Some(functions) = calls.as_ref().and_then(|graph| graph.out_of_line.as_ref()) {
                out_of_line::attach_functions(
                    &mut lowered.sim_module,
                    functions.units(),
                    functions.frames(),
                );
            }
            lowered.sim_module.time_precision = Some(
                celox_design::TimePrecision::try_from(global_precision)
                    .map_err(|reason| sv::AnalyzerError::Unsupported(reason.to_string()))?,
            );
            lowered.sim_module.time_scale = time_scale?;
            // Declaration initializers run before every `initial` block
            // (IEEE 1800-2023 6.8, 10.5): one that is not folded into an
            // initial value runs as a declaration initializer, and any other
            // process would run among the `initial` blocks.
            if lowered
                .sim_module
                .processes
                .iter()
                .any(|process| process.slots.kind != celox_design::ProcessKind::Initializer)
            {
                return Err(sv::AnalyzerError::Unsupported(format!(
                    "package variable initializer that is not constant in package `{name}`"
                )));
            }
            Ok((name, ModuleId(first + index), lowered))
        })
        .collect()
}

/// Add the keys of the class properties that the level waits of the state
/// modules of `packages` read to `keys` (see `ff::watched_properties`).
pub(super) fn package_watched_properties(
    packages: &sv::Packages,
    design: &sv::Design,
    keys: &mut BTreeSet<u32>,
) {
    for state in packages.specialized_state_modules(design) {
        ff::watched_properties(&state, keys);
    }
}

/// Whether a state module of `packages` calls `$timeformat` (see
/// `ff::TIME_FORMAT`).
pub(super) fn packages_call_time_format(packages: &sv::Packages, design: &sv::Design) -> bool {
    packages
        .specialized_state_modules(design)
        .iter()
        .any(ff::calls_time_format)
}

/// The names of the change counts of the class properties with keys
/// `properties` (see `ff::property_changes`).
pub(super) fn property_change_names(properties: &BTreeSet<u32>) -> Vec<String> {
    properties
        .iter()
        .map(|&key| ff::property_changes(key))
        .collect()
}

/// Bind each variable that denotes a package variable to the variable of
/// the package's module, in the modules and in the package modules.
pub(super) fn bind_package_variables<'a>(
    modules: &mut HashMap<ModuleId, SimModule>,
    lowered: impl Iterator<Item = (ModuleId, &'a LoweredSvModule)>,
    packages: &'a [(String, ModuleId, LoweredSvModule)],
) -> Result<(), sv::AnalyzerError> {
    let target = |package: &str, variable: &str| {
        let (_, module_id, state) = packages
            .iter()
            .find(|(name, ..)| name == package)
            .ok_or_else(|| sv::AnalyzerError::UnknownPackage {
                name: package.to_string(),
            })?;
        let var_id = state.signal_names.get(variable).copied().ok_or_else(|| {
            sv::AnalyzerError::UnknownPackageItem {
                package: package.to_string(),
                name: variable.to_string(),
            }
        })?;
        Ok::<_, sv::AnalyzerError>(PackageBinding {
            package: *module_id,
            var_id,
            simulator_state: package == sv::ASSERTION_CONTROL_PACKAGE,
        })
    };
    let mut bindings = Vec::new();
    for (module_id, module) in lowered.chain(
        packages
            .iter()
            .map(|(_, module_id, state)| (*module_id, state)),
    ) {
        for (var_id, package, variable) in &module.package_variables {
            if module.variables[var_id].domain_kind != DomainKind::Other {
                return Err(sv::AnalyzerError::Unsupported(format!(
                    "package variable `{package}::{variable}` used as a clock or reset"
                )));
            }
            bindings.push((module_id, *var_id, target(package, variable)?));
        }
    }
    for (module_id, var_id, binding) in bindings {
        if let Some(module) = modules.get_mut(&module_id) {
            module.package_bindings.insert(var_id, binding);
        }
    }
    Ok(())
}
