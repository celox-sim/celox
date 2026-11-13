//! Analysis of the SystemVerilog sources into the modules the lowering specializes.

use super::*;

pub(super) fn analyze_sources(
    sources: &[(&str, &Path)],
    roots: &[&str],
    analysis_options: sv::AnalysisOptions,
) -> Result<HashMap<String, AnalyzedSvModule>, sv::AnalyzerError> {
    analyze_sources_with_requests(
        sources,
        &ExternalHierarchy::default(),
        &[],
        &[],
        Some(roots),
        analysis_options,
    )
    .map(|(modules, _)| modules)
}

pub(super) fn analyze_sources_with_requests(
    sources: &[(&str, &Path)],
    foreign: &ExternalHierarchy,
    declarations: &[(&str, &Path)],
    requests: &[SvModuleRequest],
    roots: Option<&[&str]>,
    analysis_options: sv::AnalysisOptions,
) -> Result<(HashMap<String, AnalyzedSvModule>, HashMap<String, String>), sv::AnalyzerError> {
    if !declarations.is_empty() {
        let mut declared_modules = HashSet::default();
        for (code, path) in sources {
            let parsed = if analysis_options.first_duplicate_module {
                sv::ParsedSource::parse_with_first_modules(code, path, &mut declared_modules)?
            } else {
                sv::ParsedSource::parse(code, path)?
            };
            if let Some(name) = parsed
                .module_names()
                .iter()
                .find(|name| foreign.roots.contains_key(*name))
            {
                return Err(sv::AnalyzerError::DuplicateModule { name: name.clone() });
            }
        }
    }
    let selected_roots = roots.map(|roots| {
        roots
            .iter()
            .map(|root| {
                requests
                    .iter()
                    .find(|request| request.alias == *root)
                    .map(|request| request.module.as_str())
                    .unwrap_or(root)
            })
            .collect::<Vec<_>>()
    });
    let time_declarations = if declarations.iter().any(|(code, _)| !code.is_empty()) {
        sv::generated_time_declarations_with_options(
            sources,
            selected_roots.as_deref(),
            analysis_options.clone(),
        )?
    } else {
        String::new()
    };
    let declarations = declarations
        .iter()
        .map(|(code, path)| (format!("{time_declarations}{code}"), *path))
        .collect::<Vec<_>>();
    let mut joint_sources = sources.to_vec();
    joint_sources.extend(
        declarations
            .iter()
            .map(|(code, path)| (code.as_str(), *path)),
    );
    let sources = joint_sources.as_slice();
    let bindings = requests
        .iter()
        .map(|request| {
            (
                request.alias.clone(),
                request.module.clone(),
                request.interfaces.clone(),
            )
        })
        .collect::<Vec<_>>();
    // Each step reports its time as a `[analysis]` event, which applications
    // print when they ask for phase timing.
    let mut checkpoint = std::time::Instant::now();
    let mut step = |label: &str| {
        tracing::debug!("[analysis] {label}: {:?}", checkpoint.elapsed());
        checkpoint = std::time::Instant::now();
    };
    let (elaboration, interface_member_paths) = sv::plan_interfaces_of_first_modules(
        sources,
        &bindings,
        analysis_options.first_duplicate_module,
    )?;
    step("plan_interfaces");
    let root_names = elaboration.root_names.clone();
    let mut modules = HashMap::default();
    let mut declared_modules = HashSet::default();
    let sources: Vec<_> = sources
        .iter()
        .map(|(code, path)| {
            let parsed = if analysis_options.first_duplicate_module {
                sv::ParsedSource::parse_with_first_modules(code, path, &mut declared_modules)?
            } else {
                sv::ParsedSource::parse(code, path)?
            };
            Ok((std::rc::Rc::new(parsed), *code, *path))
        })
        .collect::<Result<_, sv::AnalyzerError>>()?;
    let mut interfaces = sv::ModuleInterfaces::default();
    for (source, _, _) in &sources {
        interfaces.extend(source.module_interfaces().clone());
    }
    for (name, id) in &foreign.roots {
        if interfaces.contains_key(name) && declarations.is_empty() {
            return Err(sv::AnalyzerError::DuplicateModule { name: name.clone() });
        }
        if interfaces.contains_key(name) {
            continue;
        }
        let module = &foreign.modules[id];
        interfaces.insert(
            name.clone(),
            sv::ModuleInterface {
                ports: module
                    .port_order
                    .iter()
                    .map(|id| module.sim_module.variables[id].path.join("."))
                    .collect(),
                parameters: Vec::new(),
            },
        );
    }
    let interfaces = std::sync::Arc::new(interfaces);
    step("parse");
    let parsed: Vec<&sv::ParsedSource> = sources.iter().map(|(source, ..)| &**source).collect();
    // The interfaces that virtual interface types name, before their
    // instances are known (see `virtual_interfaces`).
    let first_duplicate_module = analysis_options.first_duplicate_module;
    let analysis_options = if analysis_options.virtual_interfaces.is_none() {
        let texts = sources
            .iter()
            .map(|(_, code, path)| (*code, *path))
            .collect::<Vec<_>>();
        sv::AnalysisOptions {
            virtual_interfaces: sv::VirtualInterfaces::declarations_of(
                &texts,
                &parsed,
                &analysis_options,
            )?,
            ..analysis_options
        }
    } else {
        analysis_options
    };
    step("virtual_interface_declarations");
    let mut packages = match roots {
        Some(roots) => {
            let roots = roots
                .iter()
                .map(|root| {
                    root_names
                        .get(*root)
                        .map(|name| {
                            elaboration
                                .modules
                                .get(name)
                                .map_or(name.as_str(), |plan| plan.original.as_str())
                        })
                        .or_else(|| {
                            requests
                                .iter()
                                .find(|request| request.alias == *root)
                                .map(|request| request.module.as_str())
                        })
                        .unwrap_or(root)
                })
                .collect::<Vec<_>>();
            sv::ParsedSource::analyze_packages_with_options(
                &parsed,
                Some(&roots),
                analysis_options,
            )?
        }
        None => sv::ParsedSource::analyze_packages_with_options(&parsed, None, analysis_options)?,
    };
    step("analyze_packages");
    if let Some(error) = packages.compilation_unit_illegality() {
        return Err(error.clone());
    }
    packages.set_interface_plan(elaboration.clone());
    let packages = std::rc::Rc::new(packages);
    let mut net_declared_modules = HashSet::default();
    for (source, code, path) in &sources {
        // The first declaration of a module name is the one analyzed.
        let permissions = if first_duplicate_module {
            sv::source_first_module_implicit_net_permissions(code, path, &mut net_declared_modules)?
        } else {
            sv::source_module_implicit_net_permissions(code, path)?
        };
        let mut implicit_net_permissions = HashMap::default();
        for (name, allowed) in permissions {
            implicit_net_permissions.entry(name).or_insert(allowed);
        }
        let mut module_names = source.module_names().to_vec();
        module_names.extend(
            elaboration
                .modules
                .iter()
                .filter(|(name, plan)| {
                    *name != &plan.original && source.module_names().contains(&plan.original)
                })
                .map(|(name, _)| name.clone()),
        );
        for module_name in module_names {
            if !declarations.is_empty() && foreign.roots.contains_key(&module_name) {
                continue;
            }
            let name = module_name.clone();
            if modules.contains_key(&name) {
                return Err(sv::AnalyzerError::DuplicateModule { name: module_name });
            }
            modules.insert(
                name,
                AnalyzedSvModule {
                    interface_member_paths: interface_member_paths
                        .get(&module_name)
                        .cloned()
                        .unwrap_or_default(),
                    implicit_nets_allowed: implicit_net_permissions
                        .get(
                            elaboration
                                .modules
                                .get(&module_name)
                                .map_or(module_name.as_str(), |plan| plan.original.as_str()),
                        )
                        .copied()
                        .unwrap_or(true),
                    name: module_name,
                    source: source.clone(),
                    interfaces: interfaces.clone(),
                    packages: packages.clone(),
                },
            );
        }
    }
    Ok((modules, root_names))
}
