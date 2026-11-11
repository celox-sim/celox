//! Analyzing Veryl sources and elaborating the top module.

use super::*;

pub(in crate::simulator) fn elaborate_parameterized_top(
    ir: &mut Ir,
    context: &mut Context,
    top: &str,
    param_overrides: &[(String, u64)],
) -> Result<(), ParserError> {
    let values = param_overrides
        .iter()
        .map(|(name, value)| (name.clone(), Value::new(*value, 64, false)))
        .collect::<Vec<_>>();
    let top_name = resource_table::insert_str(top);
    let Some(top_index) = ir.components.iter().rposition(
        |component| matches!(component, veryl_analyzer::ir::Component::Module(module) if module.name == top_name),
    ) else {
        // Preserve the existing TopNotFound diagnostic from the frontend.
        return Ok(());
    };
    elaborate_parameterized_module(ir, context, top_index, top, &values)
}

/// Re-elaborate the module at `top_index` of `ir.components` with parameter
/// overrides. `top` names it in diagnostics.
pub(in crate::simulator) fn elaborate_parameterized_module(
    ir: &mut Ir,
    context: &mut Context,
    top_index: usize,
    top: &str,
    param_overrides: &[(String, Value)],
) -> Result<(), ParserError> {
    let top_token = match &ir.components[top_index] {
        veryl_analyzer::ir::Component::Module(module) => module.token,
        _ => unreachable!(),
    };
    let symbol = symbol_table::resolve(&top_token.beg).map_err(|error| {
        ParserError::illegal_context(
            "top-level parameter override",
            format!("unable to resolve top module `{top}`: {error:?}"),
            Some(&top_token),
        )
    })?;

    let SymbolKind::Module(property) = &symbol.found.kind else {
        unreachable!();
    };
    let values: fxhash::FxHashMap<_, _> = param_overrides
        .iter()
        .map(|(name, value)| (resource_table::insert_str(name), value.clone()))
        .collect();

    for name in values.keys() {
        if !property
            .parameters
            .iter()
            .any(|parameter| parameter.name == *name)
        {
            return Err(ParserError::illegal_context(
                "top-level parameter override",
                format!("unknown top-level parameter override `{name}`"),
                Some(&top_token),
            ));
        }
    }

    // push_override expects expressions that have already been evaluated in
    // the formal parameter type (as get_overridden_params does for HDL insts).
    // Resolve the header in declaration order so dependent widths/defaults see
    // earlier overrides, independently of the order of builder.param() calls.
    let mut header = Context::default();
    header.inherit(context);
    header.push_affiliation(Affiliation::Module);
    header.push_namespace(symbol.found.inner_namespace());
    let overrides = (|| {
        let mut signature = Signature::new(symbol.found.id);
        let mut override_map = fxhash::FxHashMap::default();
        for parameter in &property.parameters {
            let property = parameter.property();
            let token = property.token.into();
            let invalid = || {
                ParserError::illegal_context(
                    "top-level parameter override",
                    format!(
                        "unable to evaluate parameter `{}` of top module `{top}`",
                        parameter.name
                    ),
                    Some(&token),
                )
            };
            let r#type = property
                .r#type
                .to_ir_type(&mut header, TypePosition::Variable)
                .map_err(|_| invalid())?;
            let path = VarPath::new(parameter.name);
            let mut expr = if let Some(value) = values.get(&parameter.name) {
                let width = r#type.total_width().ok_or_else(invalid)?;
                let mut value = value.expand(width, value.signed()).into_owned();
                value.trunc(width);
                value.set_signed(r#type.signed);
                let mut comptime = Comptime::create_value(value.clone(), token);
                comptime.r#type = r#type.clone();
                let expr = Expression::create_value(value, token);
                signature.add_parameter(parameter.name, comptime.value.clone());
                override_map.insert(path.clone(), (comptime.clone(), expr.clone()));
                (comptime, expr)
            } else {
                eval_expr(
                    &mut header,
                    Some(r#type.clone()),
                    property.value.as_ref().ok_or_else(invalid)?,
                    false,
                )
                .map_err(|_| invalid())?
            };
            let dst = AssignDestination {
                id: VarId::default(),
                path,
                index: VarIndex::default(),
                select: VarSelect::default(),
                comptime: Comptime::from_type(r#type, ClockDomain::None, token),
                token,
            };
            eval_const_assign(&mut header, (&property.kind).into(), &dst, &mut expr)
                .map_err(|_| invalid())?;
        }
        Ok::<_, ParserError>((signature, override_map))
    })();
    header.pop_namespace();
    context.inherit(&mut header);
    let (signature, override_map) = overrides?;

    context.push_override(symbol.found.inner_namespace(), override_map);
    let component = get_component(context, &signature, top_token).map_err(|_| {
        ParserError::illegal_context(
            "top-level parameter override",
            format!("unable to elaborate top module `{top}` with parameter overrides"),
            Some(&top_token),
        )
    });
    let mut module = component.and_then(|component| match component.as_ref() {
        veryl_analyzer::ir::Component::Module(module) => Ok(module.clone()),
        _ => Err(ParserError::illegal_context(
            "top-level parameter override",
            format!("top `{top}` did not elaborate to a module"),
            Some(&top_token),
        )),
    });
    if let Ok(module) = &mut module {
        module.eval_assign(context);
    }
    context.pop_override();

    ir.components[top_index] = veryl_analyzer::ir::Component::Module(module?);
    Ok(())
}

pub(super) fn analyze(
    sources: &[(&str, &Path)],
    sv_sources: Option<&[(&str, &Path)]>,
    external_frontend: Option<&celox_frontend_core::symbolic::artifact::ExternalHierarchy>,
    top: &str,
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
    trace_opts: &crate::debug::TraceOptions,
    trace_out: Option<&mut crate::debug::CompilationTrace>,
    metadata: Option<Metadata>,
    clock_type: Option<ClockType>,
    reset_type: Option<ResetType>,
    param_overrides: &[(String, u64)],
    optimize_options: &crate::optimizer::OptimizeOptions,
    diagnostics: &crate::RuntimeDiagnostics,
    diagnostic_policy: &crate::DiagnosticPolicy,
    sv_timescale: Option<crate::SvTimescale>,
    legacy_empty_reductions: bool,
    injected_manifests: &[(String, veryl_metadata::ComponentManifest)],
    preserve_element_storage_layout: bool,
    recover_comb_loops: bool,
    allow_always_ff_function_effects: bool,
    _allow_inferred_latches: bool,
    file_dependencies: &celox_frontend_veryl::FileDependencyCollector,
) -> (
    Result<OptimizedSir, SimulatorError>,
    Vec<AnalyzerError>,
    Vec<FrontendDiagnostic>,
    Vec<CompilationWarning>,
) {
    symbol_table::clear();
    attribute_table::clear();

    let mut metadata = metadata.unwrap_or_else(|| Metadata::create_default("prj").unwrap());
    // Pass 1 uses this name as the source namespace. Preserve the namespaces
    // assigned by Veryl for dependency and standard-library sources instead
    // of registering every supplied source under the root project.
    let source_projects: HashMap<PathBuf, String> = if metadata.metadata_path.as_os_str().is_empty()
    {
        HashMap::default()
    } else {
        match metadata.paths::<&Path>(&[], false, true) {
            Ok(paths) => paths.into_iter().map(|path| (path.src, path.prj)).collect(),
            Err(error) => {
                return (
                    Err(ParserError::illegal_context(
                        "Veryl project source discovery",
                        error.to_string(),
                        None,
                    )
                    .into()),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                );
            }
        }
    };
    // Preserve an explicitly configured seed, but defer generating an
    // implicit seed until testbench execution. This keeps compilation
    // deterministic and avoids host-only time APIs in the browser compiler.
    let testbench_random_seed = metadata.test.seed;
    let (component_libraries, component_file_base) = component_runtime_config(&metadata);
    let analyzer = Analyzer::new(&metadata);
    if !injected_manifests.is_empty() {
        let names: Vec<_> = injected_manifests
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        veryl_analyzer::tb_component::insert_external_components(&names);
        for (name, manifest) in injected_manifests {
            veryl_analyzer::component_manifest_table::insert(
                resource_table::insert_str(name),
                manifest.clone(),
            );
        }
    }
    let project_name = metadata.project.name.clone();

    // Per-file: parse + pass1
    let phase = |name: &str, start: Option<crate::timing::Instant>| {
        if let Some(start) = start {
            tracing::debug!("[phase-timing] {name}: {:?}", start.elapsed());
        }
    };
    let parse_start = diagnostics.phase_timing.then(crate::timing::now);
    let mut parsers = Vec::new();
    let mut errors = vec![];
    let mut authored_parsers = Vec::new();
    for (code, path) in sources {
        match Parser::parse(code, path) {
            Ok(parsed) => authored_parsers.push(parsed),
            Err(error) => {
                return (
                    Err(SimulatorError::new(SimulatorErrorKind::Syntax(error))),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                );
            }
        }
    }
    let mut sv_interface_signatures = Vec::new();
    // Behaviorless interfaces may never be lowered again. Retain their probe
    // warnings too, deduplicating calls also encountered during design lowering.
    let mut sv_probe_warnings = Vec::new();
    if let Some(sv_sources) = sv_sources {
        let references = super::super::mixed::referenced_sv_components(
            authored_parsers.iter().map(|parser| &parser.veryl),
        );
        let contracts = celox_frontend_sv::interface_signatures_for_with_options(
            sv_sources,
            Some(&references),
            &mut sv_probe_warnings,
            diagnostic_policy.sv_options(sv_timescale, legacy_empty_reductions),
        )
        .map_err(|error| match error {
            celox_frontend_sv::FrontendError::Analyzer(error) => parser::sv_analysis_error(error),
            celox_frontend_sv::FrontendError::Lowering(error) => error.into(),
        })
        .and_then(|mut signatures| {
            signatures.retain(|signature| references.contains(&signature.name));
            let source = super::super::mixed::veryl_interface_contracts(&signatures)?;
            sv_interface_signatures = signatures;
            Ok(source)
        });
        let contracts = match contracts {
            Ok(contracts) => contracts,
            Err(error) => {
                return (
                    Err(error.into()),
                    errors,
                    Vec::new(),
                    sv_compilation_warnings(sv_probe_warnings),
                );
            }
        };
        if !contracts.is_empty() {
            let parsed = match Parser::parse(&contracts, &Path::new("<sv-interface-contracts>")) {
                Ok(parsed) => parsed,
                Err(error) => {
                    return (
                        Err(SimulatorError::new(SimulatorErrorKind::Syntax(error))),
                        errors,
                        Vec::new(),
                        sv_compilation_warnings(sv_probe_warnings),
                    );
                }
            };
            errors.append(&mut analyzer.analyze_pass1("$sv", &parsed.veryl));
            parsers.push(parsed);
        }
    }
    phase("veryl_parse", parse_start);
    let pass1_start = diagnostics.phase_timing.then(crate::timing::now);
    for ((_, path), parsed) in sources.iter().zip(authored_parsers) {
        let source_project = source_projects.get(*path).unwrap_or(&project_name);
        errors.append(&mut analyzer.analyze_pass1(source_project, &parsed.veryl));
        parsers.push(parsed);
    }
    let loop_sources =
        parser::loop_provenance::LoopSourceTable::collect(parsers.iter().map(|x| &x.veryl));

    // Global post-pass1
    errors.append(&mut Analyzer::analyze_post_pass1());
    phase("veryl_analyze_pass1", pass1_start);
    let pass2_start = diagnostics.phase_timing.then(crate::timing::now);

    // Shared context for pass2
    let mut context = Context::default();

    let mut ir = Ir::default();

    for parsed in &parsers {
        errors.append(&mut analyzer.analyze_pass2(&parsed.veryl, &mut context, Some(&mut ir)));
    }
    let native_top = ir.components.iter().any(|component| matches!(component, veryl_analyzer::ir::Component::Module(module) if module.name.to_string() == top));
    if !param_overrides.is_empty()
        && (sv_sources.is_none() || native_top)
        && let Err(error) = elaborate_parameterized_top(&mut ir, &mut context, top, param_overrides)
    {
        errors.append(&mut context.drain_errors());
        let warnings = sv_compilation_warnings(sv_probe_warnings);
        return (Err(error.into()), errors, Vec::new(), warnings);
    }
    errors.append(&mut context.drain_errors());
    errors.append(&mut Analyzer::analyze_post_pass2(&ir));
    phase("veryl_analyze_pass2", pass2_start);

    // Veryl reports combinational loops before Celox can apply its path-level
    // false-loop and true-loop authorizations. When the caller supplied such
    // an authorization, defer loop validation to the Celox scheduler: it will
    // still reject every cycle that is not covered by the supplied paths.
    if !ignored_loops.is_empty() || !true_loops.is_empty() {
        errors.retain(|error| !matches!(error, AnalyzerError::CombinationalLoop { .. }));
    }

    // Celox's FF lowerer can model function copy-out and non-local writes that
    // Veryl rejects for SystemVerilog always_ff compatibility. Keep the
    // language diagnostics by default, but let semantic regression tests opt
    // into lowering the analyzer IR for those constructs.
    if allow_always_ff_function_effects {
        errors.retain(|error| {
            !matches!(
                error,
                AnalyzerError::SideEffectFunctionCallInAlwaysFf { .. }
                    | AnalyzerError::FunctionOutputInAlwaysFf { .. }
            )
        });
    }

    let mut frontend_diagnostics = if errors.iter().any(AnalyzerError::is_error) {
        Vec::new()
    } else {
        let mut diagnostics = celox_frontend_veryl::check_dynamic_for_bounds(&ir);
        diagnostics.extend(celox_frontend_veryl::check_function_output_aliases(&ir));
        diagnostics.extend(celox_frontend_veryl::check_array_literal_side_effects(
            &ir,
            parsers.iter().map(|x| &x.veryl),
            &context.config.defines,
        ));
        diagnostics
    };
    celox_frontend_veryl::lower_interface_captures(&mut ir);
    // Force-capable native images reapply an override after each static store.
    // Keep analyzer-unrolled loops expanded for that mode so one compiled
    // entry cannot execute the same store across multiple iterations.
    let loop_provenance = if recover_comb_loops {
        loop_sources.match_unrolled(&ir)
    } else {
        Default::default()
    };

    let top = veryl_parser::resource_table::insert_str(top);
    let mut build_config = BuildConfig::from(&metadata.build);
    build_config.allow_inferred_latches = true;
    build_config.four_state = four_state;
    build_config.file_dependencies = file_dependencies.clone();
    if let Some(ct) = clock_type {
        build_config.clock_type = ct;
    }
    if let Some(rt) = reset_type {
        build_config.reset_type = rt;
    }
    build_config.parallel_lanes = optimize_options.parallel_lanes();
    let mut sv_warnings = Vec::new();
    let sir = if let Some(external) = external_frontend {
        parser::parse_with_external_hierarchy(
            &top,
            &ir,
            &loop_provenance,
            external,
            None,
            &build_config,
            ignored_loops,
            true_loops,
            four_state,
            trace_opts,
            trace_out,
            optimize_options,
            diagnostics,
            preserve_element_storage_layout,
            testbench_random_seed,
            component_libraries,
            component_file_base,
        )
    } else if let Some(sv_sources) = sv_sources {
        (|| {
            let names = super::super::mixed::EmittedNames::new(&metadata);
            // The checks before the SystemVerilog analysis see the modules it
            // analyzes, the first of each name where `duplicate_module` allows
            // later ones.
            let first_modules = diagnostic_policy
                .sv_options(sv_timescale, legacy_empty_reductions)
                .first_duplicate_module;
            let mut used_names =
                super::super::mixed::component_names(sv_sources, names, first_modules)?;
            let behavior_sources = super::super::mixed::attach_sv_interface_behavior(
                &mut ir,
                &sv_interface_signatures,
                &mut context,
                &mut used_names,
            )?;
            let mut joint_sources = sv_sources.to_vec();
            joint_sources.extend(
                behavior_sources
                    .iter()
                    .map(|source| (source.as_str(), Path::new("<sv-interface-behavior>"))),
            );
            let requests = super::super::mixed::specialize_sv_instances(
                &mut ir,
                &joint_sources,
                &mut context,
                &mut used_names,
                names,
                first_modules,
            )?;
            let declarations = super::super::mixed::sv_declaration_contracts(
                &ir,
                &mut context,
                &joint_sources,
                &requests,
                names,
                first_modules,
            )?;
            parser::parse_mixed(
                &top,
                &mut ir,
                &mut context,
                &loop_provenance,
                &joint_sources,
                &[(
                    declarations.as_str(),
                    Path::new("<veryl-declaration-contracts>"),
                )],
                param_overrides,
                &requests,
                &sv_interface_signatures,
                &mut used_names,
                names,
                &build_config,
                ignored_loops,
                true_loops,
                four_state,
                trace_opts,
                trace_out,
                optimize_options,
                diagnostics,
                diagnostic_policy,
                sv_timescale,
                legacy_empty_reductions,
                preserve_element_storage_layout,
                testbench_random_seed,
                component_libraries,
                component_file_base,
                &mut sv_warnings,
            )
        })()
    } else {
        parser::parse(
            &top,
            &ir,
            &loop_provenance,
            &build_config,
            ignored_loops,
            true_loops,
            four_state,
            trace_opts,
            trace_out,
            optimize_options,
            diagnostics,
            preserve_element_storage_layout,
            testbench_random_seed,
            component_libraries,
            component_file_base,
        )
    };
    let sir = sir
        .map(|(sir, mut elaborated_diagnostics)| {
            frontend_diagnostics.append(&mut elaborated_diagnostics);
            sir
        })
        .map_err(SimulatorError::from);
    let elaboration_warnings = {
        for warning in sv_probe_warnings {
            if !sv_warnings.contains(&warning) {
                sv_warnings.push(warning);
            }
        }
        sv_warnings.sort_by(|left, right| {
            left.location
                .as_ref()
                .map(|location| (&location.path, location.line, location.column))
                .cmp(
                    &right
                        .location
                        .as_ref()
                        .map(|location| (&location.path, location.line, location.column)),
                )
                .then(left.kind.cmp(&right.kind))
        });
        sv_compilation_warnings(sv_warnings)
    };
    errors.retain(|error| {
        !frontend_diagnostics
            .iter()
            .any(|diagnostic| diagnostic.covers_uncovered_branch(error))
    });
    frontend_diagnostics.retain(|diagnostic| !diagnostic.proven_binary_combinational());
    (sir, errors, frontend_diagnostics, elaboration_warnings)
}
