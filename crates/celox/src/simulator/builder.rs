use std::path::{Path, PathBuf};

use veryl_analyzer::conv::utils::get_component;
use veryl_analyzer::ir::{Comptime, Expression, Signature, VarPath};
use veryl_analyzer::value::Value;
use veryl_analyzer::{Analyzer, AnalyzerError, Context, attribute_table, ir::Ir, symbol_table};
use veryl_metadata::{ClockType, Component, ComponentBackendKind, Metadata, ResetType};
use veryl_parser::Parser;
use veryl_parser::resource_table;

use crate::parser::BuildConfig;
use crate::{
    CompilationWarning, FrontendDiagnostic, HashMap, ParserError, SimulatorError,
    SimulatorErrorKind, ir::OptimizedSir, parser,
};

fn component_library_path(
    component: &Component,
    root: &Path,
    target_dir: &Path,
    backend: Option<ComponentBackendKind>,
) -> Option<std::path::PathBuf> {
    let wasm = component
        .wasm
        .as_ref()
        .map(|path| root.join(path))
        .filter(|path| path.is_file());
    let crate_dir = root.join(&component.path);
    let native = component_library_target_name(&crate_dir).and_then(|name| {
        let name = name.replace('-', "_");
        let path = target_dir.join("release").join(format!(
            "{}{}{}",
            std::env::consts::DLL_PREFIX,
            name,
            std::env::consts::DLL_SUFFIX
        ));
        path.is_file().then_some(path)
    });
    match backend {
        Some(ComponentBackendKind::Native) => native,
        Some(ComponentBackendKind::Wasm) => wasm,
        None => native.or(wasm),
    }
}

fn component_library_target_name(crate_dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(crate_dir.join("Cargo.toml")).ok()?;
    let value: toml::Value = toml::from_str(&text).ok()?;
    value
        .get("lib")
        .and_then(|lib| lib.get("name"))
        .and_then(toml::Value::as_str)
        .or_else(|| {
            value
                .get("package")
                .and_then(|package| package.get("name"))
                .and_then(toml::Value::as_str)
        })
        .map(str::to_owned)
}

fn component_runtime_config(
    metadata: &Metadata,
) -> (
    Vec<celox_testbench::ComponentLibrary>,
    Option<std::path::PathBuf>,
) {
    if metadata.metadata_path.as_os_str().is_empty() {
        return (Vec::new(), None);
    }
    let mut libraries = Vec::new();
    let mut collect =
        |components: &[Component], root: &Path, target_dir: &Path, project: Option<&str>| {
            for component in components {
                let Some(path) = component_library_path(
                    component,
                    root,
                    target_dir,
                    metadata.test.component_backend,
                ) else {
                    continue;
                };
                for (type_name, _) in component.collect_manifests(root, target_dir) {
                    let export = project
                        .map(|project| format!("{project}::{type_name}"))
                        .unwrap_or_else(|| type_name.clone());
                    if libraries
                        .iter()
                        .any(|library: &celox_testbench::ComponentLibrary| library.export == export)
                    {
                        continue;
                    }
                    libraries.push(celox_testbench::ComponentLibrary {
                        export,
                        type_name,
                        path: path.clone(),
                    });
                }
            }
        };
    let root = metadata.project_path();
    let target_dir = root.join("target/veryl-components");
    collect(&metadata.components, &root, &target_dir, None);
    if let Ok(dependencies) = metadata.collect_dependency_components() {
        for dependency in dependencies {
            collect(
                &dependency.components,
                &dependency.root,
                &dependency.target_dir,
                Some(&dependency.project),
            );
        }
    }
    (libraries, Some(root))
}

fn elaborate_parameterized_top(
    ir: &mut Ir,
    context: &mut Context,
    top: &str,
    param_overrides: &[(String, u64)],
) -> Result<(), ParserError> {
    let top_name = resource_table::insert_str(top);
    let Some(top_index) = ir.components.iter().rposition(
        |component| matches!(component, veryl_analyzer::ir::Component::Module(module) if module.name == top_name),
    ) else {
        // Preserve the existing TopNotFound diagnostic from the frontend.
        return Ok(());
    };
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

    let mut signature = Signature::new(symbol.found.id);
    let mut override_map = fxhash::FxHashMap::default();
    let token = veryl_parser::token_range::TokenRange::default();
    for (name, value) in param_overrides {
        let name_id = resource_table::insert_str(name);
        let path = VarPath::new(name_id);
        let value = Value::new(*value, 64, false);
        let comptime = Comptime::create_value(value.clone(), token);
        let expr = Expression::create_value(value, token);
        signature.add_parameter(name_id, comptime.value.clone());
        override_map.insert(path, (comptime, expr));
    }

    context.push_override(override_map);
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

fn analyze(
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
    injected_manifests: &[(String, veryl_metadata::ComponentManifest)],
    preserve_element_storage_layout: bool,
    recover_comb_loops: bool,
) -> (
    Result<OptimizedSir, ParserError>,
    Vec<AnalyzerError>,
    Vec<FrontendDiagnostic>,
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
                    )),
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
    let mut parsers = Vec::new();
    let mut errors = vec![];
    for (code, path) in sources {
        let parsed = Parser::parse(code, path).unwrap();
        let source_project = source_projects.get(*path).unwrap_or(&project_name);
        errors.append(&mut analyzer.analyze_pass1(source_project, &parsed.veryl));
        parsers.push(parsed);
    }
    let loop_sources =
        parser::loop_provenance::LoopSourceTable::collect(parsers.iter().map(|x| &x.veryl));

    // Global post-pass1
    errors.append(&mut Analyzer::analyze_post_pass1());

    // Shared context for pass2
    let mut context = Context::default();

    let mut ir = Ir::default();

    for parsed in &parsers {
        errors.append(&mut analyzer.analyze_pass2(&parsed.veryl, &mut context, Some(&mut ir)));
    }
    if !param_overrides.is_empty()
        && let Err(error) = elaborate_parameterized_top(&mut ir, &mut context, top, param_overrides)
    {
        errors.append(&mut context.drain_errors());
        return (Err(error), errors, Vec::new());
    }
    errors.append(&mut context.drain_errors());
    errors.append(&mut Analyzer::analyze_post_pass2(&ir));

    // Veryl reports combinational loops before Celox can apply its path-level
    // false-loop and true-loop authorizations. When the caller supplied such
    // an authorization, defer loop validation to the Celox scheduler: it will
    // still reject every cycle that is not covered by the supplied paths.
    if !ignored_loops.is_empty() || !true_loops.is_empty() {
        errors.retain(|error| !matches!(error, AnalyzerError::CombinationalLoop { .. }));
    }

    let mut frontend_diagnostics = if errors.iter().any(AnalyzerError::is_error) {
        Vec::new()
    } else {
        celox_frontend_veryl::check_dynamic_for_bounds(&ir)
    };
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
    if let Some(ct) = clock_type {
        build_config.clock_type = ct;
    }
    if let Some(rt) = reset_type {
        build_config.reset_type = rt;
    }
    let sir = if let Some(external) = external_frontend {
        parser::parse_with_external_hierarchy(
            &top,
            &ir,
            &loop_provenance,
            external,
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
    } else {
        #[cfg(feature = "systemverilog")]
        {
            if let Some(sv_sources) = sv_sources {
                parser::parse_mixed(
                    &top,
                    &ir,
                    &loop_provenance,
                    sv_sources,
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
            }
        }
        #[cfg(not(feature = "systemverilog"))]
        {
            debug_assert!(sv_sources.is_none());
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
        }
    };
    let sir = sir.map(|(sir, mut elaborated_diagnostics)| {
        frontend_diagnostics.append(&mut elaborated_diagnostics);
        sir
    });
    (sir, errors, frontend_diagnostics)
}

/// Compile Veryl source code to the SIR (Simulation IR) representation.
///
/// This is the shared compilation pipeline used by all backends.
/// Returns verified optimized SIR and any compilation warnings on success.
pub fn compile_to_sir(
    sources: &[(&str, &Path)],
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
) -> Result<(OptimizedSir, Vec<CompilationWarning>), SimulatorError> {
    compile_to_sir_with_layout_mode(
        sources,
        top,
        ignored_loops,
        true_loops,
        four_state,
        trace_opts,
        trace_out,
        metadata,
        clock_type,
        reset_type,
        param_overrides,
        optimize_options,
        &crate::RuntimeDiagnostics::default(),
        &[],
        crate::backend::memory_layout::MemoryLayoutMode::Packed,
        true,
    )
}

/// Compile an elaborated external-frontend artifact to optimized SIR.
///
/// The artifact carries source-independent signal reflection, so the resulting
/// program can be used by the same Rust and TypeScript testbench APIs as a
/// Veryl- or SystemVerilog-produced program.
pub fn compile_frontend_to_sir(
    artifact: &celox_frontend_sdk::FrontendArtifact,
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
    optimize_options: &crate::optimizer::OptimizeOptions,
) -> Result<(OptimizedSir, Vec<CompilationWarning>), SimulatorError> {
    compile_frontend_to_sir_with_layout_mode(
        artifact,
        ignored_loops,
        true_loops,
        four_state,
        trace_opts,
        trace_out,
        optimize_options,
        &crate::RuntimeDiagnostics::default(),
        crate::backend::memory_layout::MemoryLayoutMode::Packed,
    )
}

fn compile_frontend_to_sir_with_layout_mode(
    artifact: &celox_frontend_sdk::FrontendArtifact,
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
    mut trace_out: Option<&mut crate::debug::CompilationTrace>,
    optimize_options: &crate::optimizer::OptimizeOptions,
    diagnostics: &crate::RuntimeDiagnostics,
    layout_mode: crate::backend::memory_layout::MemoryLayoutMode,
) -> Result<(OptimizedSir, Vec<CompilationWarning>), SimulatorError> {
    let lowered = celox_frontend_core::lower_frontend_artifact(artifact)?;
    let frontend_trace_options = trace_opts.frontend(diagnostics);
    let mut frontend_trace = celox_frontend_core::FrontendTrace::default();
    let scheduled = celox_frontend_core::symbolic::assembly::schedule_symbolic_rtl(
        lowered.symbolic,
        None,
        ignored_loops,
        true_loops,
        four_state,
        &frontend_trace_options,
        trace_out.is_some().then_some(&mut frontend_trace),
    )
    .map_err(celox_frontend_veryl::ParserError::from)?;
    if let Some(trace) = trace_out.as_deref_mut() {
        trace.absorb_frontend(frontend_trace);
    }
    let program = parser::finalize_scheduled_rtl(
        scheduled,
        None,
        four_state,
        trace_opts,
        trace_out,
        optimize_options,
        diagnostics,
        layout_mode == crate::backend::memory_layout::MemoryLayoutMode::ElementStrided,
        None,
        Vec::new(),
        None,
    )?;
    Ok((program, Vec::new()))
}

#[cfg(feature = "host-runtime")]
fn compile_frontend_testbench_to_sir_with_layout_mode(
    artifact: &celox_frontend_sdk::FrontendArtifact,
    sources: &[(&str, &Path)],
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
    optimize_options: &crate::optimizer::OptimizeOptions,
    diagnostics: &crate::RuntimeDiagnostics,
    injected_manifests: &[(String, veryl_metadata::ComponentManifest)],
    layout_mode: crate::backend::memory_layout::MemoryLayoutMode,
    recover_comb_loops: bool,
) -> Result<(OptimizedSir, Vec<CompilationWarning>), SimulatorError> {
    let lowered = celox_frontend_core::lower_frontend_artifact(artifact)?;
    let (sir, errors, frontend_diagnostics) = analyze(
        sources,
        None,
        Some(&lowered.external),
        top,
        ignored_loops,
        true_loops,
        four_state,
        trace_opts,
        trace_out,
        metadata,
        clock_type,
        reset_type,
        &[],
        optimize_options,
        diagnostics,
        injected_manifests,
        layout_mode == crate::backend::memory_layout::MemoryLayoutMode::ElementStrided,
        recover_comb_loops,
    );
    let (real_errors, analyzer_warnings): (Vec<_>, Vec<_>) =
        errors.into_iter().partition(AnalyzerError::is_error);
    let (frontend_errors, frontend_warnings): (Vec<_>, Vec<_>) = frontend_diagnostics
        .into_iter()
        .partition(FrontendDiagnostic::is_error);
    let warnings = analyzer_warnings
        .into_iter()
        .map(CompilationWarning::Analyzer)
        .chain(
            frontend_warnings
                .into_iter()
                .map(CompilationWarning::Frontend),
        )
        .collect::<Vec<_>>();
    if !real_errors.is_empty() {
        return Err(
            SimulatorError::new(SimulatorErrorKind::Analyzer(real_errors)).with_warnings(warnings),
        );
    }
    if !frontend_errors.is_empty() {
        return Err(
            SimulatorError::new(SimulatorErrorKind::Frontend(frontend_errors))
                .with_warnings(warnings),
        );
    }
    match sir {
        Ok(program) => Ok((program, warnings)),
        Err(error) => Err(SimulatorError::from(error).with_warnings(warnings)),
    }
}

fn compile_to_sir_with_layout_mode(
    sources: &[(&str, &Path)],
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
    injected_manifests: &[(String, veryl_metadata::ComponentManifest)],
    layout_mode: crate::backend::memory_layout::MemoryLayoutMode,
    recover_comb_loops: bool,
) -> Result<(OptimizedSir, Vec<CompilationWarning>), SimulatorError> {
    let (sir, errors, frontend_diagnostics) = analyze(
        sources,
        None,
        None,
        top,
        ignored_loops,
        true_loops,
        four_state,
        trace_opts,
        trace_out,
        metadata,
        clock_type,
        reset_type,
        param_overrides,
        optimize_options,
        diagnostics,
        injected_manifests,
        layout_mode == crate::backend::memory_layout::MemoryLayoutMode::ElementStrided,
        recover_comb_loops,
    );
    let (real_errors, analyzer_warnings): (Vec<_>, Vec<_>) =
        errors.into_iter().partition(AnalyzerError::is_error);
    let (frontend_errors, frontend_warnings): (Vec<_>, Vec<_>) = frontend_diagnostics
        .into_iter()
        .partition(FrontendDiagnostic::is_error);
    let warnings = analyzer_warnings
        .into_iter()
        .map(CompilationWarning::Analyzer)
        .chain(
            frontend_warnings
                .into_iter()
                .map(CompilationWarning::Frontend),
        )
        .collect::<Vec<_>>();
    if !real_errors.is_empty() {
        return Err(
            SimulatorError::new(SimulatorErrorKind::Analyzer(real_errors)).with_warnings(warnings),
        );
    }
    if !frontend_errors.is_empty() {
        return Err(
            SimulatorError::new(SimulatorErrorKind::Frontend(frontend_errors))
                .with_warnings(warnings),
        );
    }
    match sir {
        Ok(p) => Ok((p, warnings)),
        Err(e) => Err(SimulatorError::from(e).with_warnings(warnings)),
    }
}

/// Compile SystemVerilog sources to optimized SIR.
#[cfg(feature = "systemverilog")]
pub fn compile_sv_to_sir(
    sources: &[(&str, &Path)],
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
) -> Result<(OptimizedSir, Vec<CompilationWarning>), SimulatorError> {
    compile_sv_to_sir_with_layout_mode(
        sources,
        top,
        ignored_loops,
        true_loops,
        four_state,
        trace_opts,
        trace_out,
        metadata,
        clock_type,
        reset_type,
        param_overrides,
        optimize_options,
        &crate::RuntimeDiagnostics::default(),
        crate::backend::memory_layout::MemoryLayoutMode::Packed,
    )
}

#[allow(clippy::too_many_arguments)]
#[cfg(feature = "systemverilog")]
fn compile_sv_to_sir_with_layout_mode(
    sources: &[(&str, &Path)],
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
    layout_mode: crate::backend::memory_layout::MemoryLayoutMode,
) -> Result<(OptimizedSir, Vec<CompilationWarning>), SimulatorError> {
    let metadata = metadata.unwrap_or_else(|| Metadata::create_default("prj").unwrap());
    let mut build_config = BuildConfig::from(&metadata.build);
    if let Some(clock_type) = clock_type {
        build_config.clock_type = clock_type;
    }
    if let Some(reset_type) = reset_type {
        build_config.reset_type = reset_type;
    }
    let (component_libraries, component_file_base) = component_runtime_config(&metadata);
    parser::parse_sv(
        sources,
        top,
        param_overrides,
        &build_config,
        ignored_loops,
        true_loops,
        four_state,
        trace_opts,
        trace_out,
        optimize_options,
        diagnostics,
        layout_mode == crate::backend::memory_layout::MemoryLayoutMode::ElementStrided,
        metadata.test.seed,
        component_libraries,
        component_file_base,
    )
    .map(|program| (program, Vec::new()))
    .map_err(SimulatorError::from)
}

/// Compile a Veryl hierarchy with SystemVerilog modules to optimized SIR.
#[allow(clippy::too_many_arguments)]
#[cfg(feature = "systemverilog")]
pub fn compile_mixed_to_sir(
    sources: &[(&str, &Path)],
    sv_sources: &[(&str, &Path)],
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
) -> Result<(OptimizedSir, Vec<CompilationWarning>), SimulatorError> {
    compile_mixed_to_sir_with_layout_mode(
        sources,
        sv_sources,
        top,
        ignored_loops,
        true_loops,
        four_state,
        trace_opts,
        trace_out,
        metadata,
        clock_type,
        reset_type,
        param_overrides,
        optimize_options,
        &crate::RuntimeDiagnostics::default(),
        &[],
        crate::backend::memory_layout::MemoryLayoutMode::Packed,
        true,
    )
}

#[allow(clippy::too_many_arguments)]
#[cfg(feature = "systemverilog")]
fn compile_mixed_to_sir_with_layout_mode(
    sources: &[(&str, &Path)],
    sv_sources: &[(&str, &Path)],
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
    injected_manifests: &[(String, veryl_metadata::ComponentManifest)],
    layout_mode: crate::backend::memory_layout::MemoryLayoutMode,
    recover_comb_loops: bool,
) -> Result<(OptimizedSir, Vec<CompilationWarning>), SimulatorError> {
    let (sir, errors, frontend_diagnostics) = analyze(
        sources,
        Some(sv_sources),
        None,
        top,
        ignored_loops,
        true_loops,
        four_state,
        trace_opts,
        trace_out,
        metadata,
        clock_type,
        reset_type,
        param_overrides,
        optimize_options,
        diagnostics,
        injected_manifests,
        layout_mode == crate::backend::memory_layout::MemoryLayoutMode::ElementStrided,
        recover_comb_loops,
    );
    let (real_errors, analyzer_warnings): (Vec<_>, Vec<_>) =
        errors.into_iter().partition(AnalyzerError::is_error);
    let (frontend_errors, frontend_warnings): (Vec<_>, Vec<_>) = frontend_diagnostics
        .into_iter()
        .partition(FrontendDiagnostic::is_error);
    let warnings = analyzer_warnings
        .into_iter()
        .map(CompilationWarning::Analyzer)
        .chain(
            frontend_warnings
                .into_iter()
                .map(CompilationWarning::Frontend),
        )
        .collect::<Vec<_>>();
    if !real_errors.is_empty() {
        return Err(
            SimulatorError::new(SimulatorErrorKind::Analyzer(real_errors)).with_warnings(warnings),
        );
    }
    if !frontend_errors.is_empty() {
        return Err(
            SimulatorError::new(SimulatorErrorKind::Frontend(frontend_errors))
                .with_warnings(warnings),
        );
    }
    match sir {
        Ok(program) => Ok((program, warnings)),
        Err(error) => Err(SimulatorError::from(error).with_warnings(warnings)),
    }
}

#[allow(clippy::too_many_arguments)]
#[cfg(feature = "host-runtime")]
fn compile_hdl_to_sir_with_layout_mode(
    sources: &[(&str, &Path)],
    sv_sources: &[(&str, &Path)],
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
    injected_manifests: &[(String, veryl_metadata::ComponentManifest)],
    layout_mode: crate::backend::memory_layout::MemoryLayoutMode,
    recover_comb_loops: bool,
) -> Result<(OptimizedSir, Vec<CompilationWarning>), SimulatorError> {
    #[cfg(not(feature = "systemverilog"))]
    {
        debug_assert!(sv_sources.is_empty());
        compile_to_sir_with_layout_mode(
            sources,
            top,
            ignored_loops,
            true_loops,
            four_state,
            trace_opts,
            trace_out,
            metadata,
            clock_type,
            reset_type,
            param_overrides,
            optimize_options,
            diagnostics,
            injected_manifests,
            layout_mode,
            recover_comb_loops,
        )
    }
    #[cfg(feature = "systemverilog")]
    match (sources.is_empty(), sv_sources.is_empty()) {
        (_, true) => compile_to_sir_with_layout_mode(
            sources,
            top,
            ignored_loops,
            true_loops,
            four_state,
            trace_opts,
            trace_out,
            metadata,
            clock_type,
            reset_type,
            param_overrides,
            optimize_options,
            diagnostics,
            injected_manifests,
            layout_mode,
            recover_comb_loops,
        ),
        (true, false) => compile_sv_to_sir_with_layout_mode(
            sv_sources,
            top,
            ignored_loops,
            true_loops,
            four_state,
            trace_opts,
            trace_out,
            metadata,
            clock_type,
            reset_type,
            param_overrides,
            optimize_options,
            diagnostics,
            layout_mode,
        ),
        (false, false) => compile_mixed_to_sir_with_layout_mode(
            sources,
            sv_sources,
            top,
            ignored_loops,
            true_loops,
            four_state,
            trace_opts,
            trace_out,
            metadata,
            clock_type,
            reset_type,
            param_overrides,
            optimize_options,
            diagnostics,
            injected_manifests,
            layout_mode,
            recover_comb_loops,
        ),
    }
}

// ── JIT-specific types and builders (native only) ────────────────────

#[cfg(feature = "host-runtime")]
mod host {
    use super::super::Simulator;
    use super::*;
    use crate::backend::JitBackend;
    use crate::ir::LaidOutProgram;

    /// Controls which stores the dead store elimination pass preserves.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub enum DeadStorePolicy {
        /// Keep all stores (no dead store elimination). Default for user-facing builds.
        #[default]
        Off,
        /// Eliminate stores except those explicitly marked live via `live_signal()`
        /// and those loaded by execution units.
        PreserveListedSignals,
        /// Eliminate stores except those to top-module ports and those loaded by EUs.
        PreserveTopPorts,
        /// Eliminate stores except those to ports of *all* instances and those loaded by EUs.
        PreserveAllPorts,
    }

    #[derive(Debug, Clone)]
    pub struct SimulatorOptions {
        pub four_state: bool,
        /// Per-pass SIRT optimizer flags.
        pub optimize_options: crate::optimizer::OptimizeOptions,
        /// Fine-grained Cranelift backend options.
        pub cranelift_options: crate::backend::CraneliftOptions,
        #[cfg(any(
            target_arch = "x86_64",
            all(target_arch = "aarch64", feature = "experimental-arm64-backend")
        ))]
        pub x86_options: crate::backend::X86BackendOptions,
        pub trace: crate::debug::TraceOptions,
        pub diagnostics: crate::RuntimeDiagnostics,
        /// When true, JIT-compiled functions emit trigger detection code for
        /// edge-based event discovery. Only needed by [`crate::Simulation`].
        pub emit_triggers: bool,
        /// Emit the additional combinational entries needed to reapply foreign
        /// force overrides between procedural store boundaries.
        pub native_force_support: bool,
        /// Dead store elimination policy.
        pub dead_store_policy: DeadStorePolicy,
    }

    /// A fully code-generated native simulator that has not run any simulator
    /// initialization yet.
    ///
    /// Keeping compilation separate from initialization lets compiler-only
    /// clients stop after native code generation without applying initial
    /// values or executing the first combinational settle.
    #[cfg(any(
        target_arch = "x86_64",
        all(target_arch = "aarch64", feature = "experimental-arm64-backend")
    ))]
    #[must_use]
    pub struct NativeCompilation {
        backend: crate::backend::native::NativeBackend,
        program: LaidOutProgram,
        warnings: Vec<CompilationWarning>,
        options: SimulatorOptions,
        vcd_path: Option<std::path::PathBuf>,
        injected_components: crate::InjectedComponents,
    }

    #[cfg(any(
        target_arch = "x86_64",
        all(target_arch = "aarch64", feature = "experimental-arm64-backend")
    ))]
    impl NativeCompilation {
        /// Warnings emitted while compiling this artifact.
        pub fn warnings(&self) -> &[CompilationWarning] {
            &self.warnings
        }

        /// Allocate and initialize the runtime state for this compiled artifact.
        pub fn initialize(
            self,
        ) -> Result<Simulator<crate::backend::native::NativeBackend>, SimulatorError> {
            let Self {
                backend,
                program,
                warnings,
                options,
                vcd_path,
                injected_components,
            } = self;
            let mut sim =
                Simulator::with_backend_and_program(backend, program.into_runtime(), warnings);
            sim.components.set_injected(injected_components);
            sim.diagnostics = options.diagnostics.clone();
            if let Some(path) = vcd_path {
                let descs = sim.build_vcd_descs(options.four_state);
                let vcd_writer = crate::VcdWriter::new(path, &descs)
                    .map_err(|_| SimulatorError::from(crate::RuntimeErrorCode::InternalError))?;
                sim.vcd_writer = Some(vcd_writer);
            }
            let apply_initial_start = options.diagnostics.phase_timing.then(crate::timing::now);
            sim.apply_initial_values();
            if let Some(start) = apply_initial_start {
                tracing::debug!("[phase-timing] apply_initial_values: {:?}", start.elapsed());
            }
            let settle_start = options.diagnostics.phase_timing.then(crate::timing::now);
            sim.modify(|_| {}).map_err(SimulatorError::from)?;
            if let Some(start) = settle_start {
                tracing::debug!("[phase-timing] initial_settle: {:?}", start.elapsed());
            }
            Ok(sim)
        }
    }

    impl Default for SimulatorOptions {
        fn default() -> Self {
            let opt = crate::optimizer::OptimizeOptions::default();
            Self {
                four_state: false,
                optimize_options: opt,
                cranelift_options: crate::backend::CraneliftOptions::default(),
                #[cfg(any(
                    target_arch = "x86_64",
                    all(target_arch = "aarch64", feature = "experimental-arm64-backend")
                ))]
                x86_options: crate::backend::X86BackendOptions::default(),
                trace: Default::default(),
                diagnostics: Default::default(),
                emit_triggers: false,
                native_force_support: false,
                dead_store_policy: DeadStorePolicy::Off,
            }
        }
    }

    /// A fluent builder for configuring and initializing a [`Simulator`] or
    /// [`Simulation`](crate::Simulation).
    ///
    /// Use [`Simulator::builder()`] or [`Simulation::builder()`](crate::Simulation::builder)
    /// to obtain the appropriate variant. Both share the same configuration methods;
    /// only `.build()` differs in return type.
    pub struct SimulatorBuilder<'a, Target = Simulator> {
        sources: Vec<(&'a str, &'a Path)>,
        sv_sources: Vec<(&'a str, &'a Path)>,
        top: &'a str,
        ignored_loops: Vec<(
            (Vec<(String, usize)>, Vec<String>),
            (Vec<(String, usize)>, Vec<String>),
        )>,
        true_loops: Vec<(
            (Vec<(String, usize)>, Vec<String>),
            (Vec<(String, usize)>, Vec<String>),
            usize,
        )>,
        options: SimulatorOptions,
        vcd_path: Option<std::path::PathBuf>,
        metadata: Option<Metadata>,
        clock_type: Option<ClockType>,
        reset_type: Option<ResetType>,
        param_overrides: Vec<(String, u64)>,
        live_signals: Vec<(Vec<(String, usize)>, Vec<String>)>,
        injected_components: crate::InjectedComponents,
        frontend_artifact: Option<celox_frontend_sdk::FrontendArtifact>,
        _marker: std::marker::PhantomData<Target>,
    }

    /// Configuration methods shared by all builder variants.
    impl<'a, Target> SimulatorBuilder<'a, Target> {
        /// Returns the source files passed to this builder.
        pub fn sources(&self) -> &[(&'a str, &'a Path)] {
            &self.sources
        }

        /// Returns the SystemVerilog source files passed to this builder.
        #[cfg(feature = "systemverilog")]
        pub fn sv_sources(&self) -> &[(&'a str, &'a Path)] {
            &self.sv_sources
        }

        /// Replace the builder's SystemVerilog source set.
        #[cfg(feature = "systemverilog")]
        pub fn with_sv_sources(mut self, sources: Vec<(&'a str, &'a Path)>) -> Self {
            self.sv_sources = sources;
            self
        }

        /// Returns the top module name.
        pub fn top(&self) -> &str {
            if !self.sources.is_empty() {
                self.top
            } else {
                self.frontend_artifact
                    .as_ref()
                    .map_or(self.top, celox_frontend_sdk::FrontendArtifact::module_name)
            }
        }

        /// Returns whether four-state simulation is enabled for this builder.
        pub fn four_state_enabled(&self) -> bool {
            self.options.four_state
        }

        /// Supply project metadata (clock/reset settings, etc.) instead of defaults.
        pub fn with_metadata(mut self, metadata: Metadata) -> Self {
            self.metadata = Some(metadata);
            self
        }

        /// Override the clock type (posedge/negedge) from metadata or defaults.
        pub fn clock_type(mut self, clock_type: ClockType) -> Self {
            self.clock_type = Some(clock_type);
            self
        }

        /// Override the reset type (async_high/async_low/sync_high/sync_low) from metadata or defaults.
        pub fn reset_type(mut self, reset_type: ResetType) -> Self {
            self.reset_type = Some(reset_type);
            self
        }

        /// Override a top-level module parameter value.
        pub fn param(mut self, name: &str, value: u64) -> Self {
            self.param_overrides.push((name.to_string(), value));
            self
        }

        /// Make in-process component implementations available as `$comp::<name>`.
        pub fn with_injected_components(mut self, components: crate::InjectedComponents) -> Self {
            self.injected_components = components;
            self
        }

        /// Enable VCD dumping to the specified file.
        pub fn vcd<P: AsRef<std::path::Path>>(mut self, path: P) -> Self {
            self.vcd_path = Some(path.as_ref().to_path_buf());
            self
        }

        /// Enable 4-state (0, 1, X, Z) simulation mode.
        pub fn four_state(mut self, enable: bool) -> Self {
            self.options.four_state = enable;
            self
        }

        /// Enable native-image support for foreign force/release operations.
        ///
        /// This emits extra combinational entry points, so ordinary simulator
        /// builds leave it disabled. Enabling it forces SIR O0 at build time so
        /// every store boundary needed to reapply a force remains intact.
        pub fn native_force_support(mut self, enable: bool) -> Self {
            self.options.native_force_support = enable;
            self.enforce_native_force_optimizer();
            self
        }

        fn enforce_native_force_optimizer(&mut self) {
            if !self.options.native_force_support {
                return;
            }
            let diagnostics = self.options.optimize_options.diagnostics.clone();
            self.options.optimize_options = crate::optimizer::OptimizeOptions::none();
            self.options.optimize_options.diagnostics = diagnostics;
            self.options.dead_store_policy = DeadStorePolicy::Off;
        }

        /// Set the overall optimization level. Sets defaults for SIR passes,
        /// Cranelift options, and DSE policy. Per-pass overrides can be applied after.
        pub fn opt_level(mut self, level: crate::optimizer::OptLevel) -> Self {
            self.options.optimize_options = crate::optimizer::OptimizeOptions::new(level);
            self.options.cranelift_options =
                crate::backend::CraneliftOptions::for_speed_optimization(
                    level != crate::optimizer::OptLevel::O0,
                );
            self.options.dead_store_policy = match level {
                crate::optimizer::OptLevel::O2 => DeadStorePolicy::PreserveTopPorts,
                _ => DeadStorePolicy::Off,
            };
            self
        }

        /// Enable a specific SIR pass, overriding the OptLevel default.
        pub fn enable_pass(mut self, pass: crate::optimizer::SirPass) -> Self {
            if pass == crate::optimizer::SirPass::TailCallSplit {
                self.options.cranelift_options.tail_call_split = true;
            }
            self.options.optimize_options = self.options.optimize_options.enable(pass);
            self
        }

        /// Disable a specific SIR pass, overriding the OptLevel default.
        pub fn disable_pass(mut self, pass: crate::optimizer::SirPass) -> Self {
            if pass == crate::optimizer::SirPass::TailCallSplit {
                self.options.cranelift_options.tail_call_split = false;
            }
            self.options.optimize_options = self.options.optimize_options.disable(pass);
            self
        }

        /// Enable or disable all SIRT optimization passes at once.
        /// Shorthand: `true` → `OptLevel::O1`, `false` → `OptLevel::O0`.
        pub fn optimize(mut self, enable: bool) -> Self {
            self.options.optimize_options = if enable {
                crate::optimizer::OptimizeOptions::all()
            } else {
                crate::optimizer::OptimizeOptions::none()
            };
            self
        }

        /// Set per-pass optimizer flags directly.
        pub fn optimize_options(mut self, options: crate::optimizer::OptimizeOptions) -> Self {
            self.options.cranelift_options.tail_call_split =
                options.is_enabled(crate::optimizer::SirPass::TailCallSplit);
            self.options.optimize_options = options;
            self
        }

        #[cfg(any(
            target_arch = "x86_64",
            all(target_arch = "aarch64", feature = "experimental-arm64-backend")
        ))]
        pub fn x86_slp(mut self, enable: bool) -> Self {
            self.options.x86_options.slp = enable;
            self
        }

        #[cfg(not(any(
            target_arch = "x86_64",
            all(target_arch = "aarch64", feature = "experimental-arm64-backend")
        )))]
        pub fn x86_slp(self, enable: bool) -> Self {
            let _ = enable;
            self
        }

        /// Set fine-grained Cranelift backend options.
        pub fn cranelift_options(mut self, options: crate::backend::CraneliftOptions) -> Self {
            self.options.cranelift_options = options;
            self
        }

        /// Set the register allocator algorithm.
        pub fn regalloc_algorithm(mut self, algo: crate::backend::RegallocAlgorithm) -> Self {
            self.options.cranelift_options.regalloc_algorithm = algo;
            self
        }

        /// Enable or disable alias analysis in the Cranelift egraph pass.
        pub fn enable_alias_analysis(mut self, enable: bool) -> Self {
            self.options.cranelift_options.enable_alias_analysis = enable;
            self
        }

        /// Enable or disable the Cranelift IR verifier.
        pub fn enable_verifier(mut self, enable: bool) -> Self {
            self.options.cranelift_options.enable_verifier = enable;
            self
        }

        /// Set the dead store elimination policy.
        pub fn dead_store_policy(mut self, policy: DeadStorePolicy) -> Self {
            self.options.dead_store_policy = policy;
            self
        }

        /// Mark a signal as externally observable (live) for dead store elimination.
        pub fn live_signal(
            mut self,
            instance_path: Vec<(String, usize)>,
            var_path: Vec<String>,
        ) -> Self {
            self.live_signals.push((instance_path, var_path));
            self
        }

        /// Configure compilation tracing options.
        pub fn trace(mut self, trace: crate::debug::TraceOptions) -> Self {
            self.options.trace = trace;
            self
        }

        /// Configure diagnostics explicitly for this build.
        pub fn diagnostics(mut self, diagnostics: crate::DiagnosticsOptions) -> Self {
            self.options.diagnostics = diagnostics.runtime;
            self.options.optimize_options.diagnostics = diagnostics.sir;
            self.options.cranelift_options.diagnostics = diagnostics.cranelift;
            #[cfg(any(
                target_arch = "x86_64",
                all(target_arch = "aarch64", feature = "experimental-arm64-backend")
            ))]
            {
                self.options.x86_options.diagnostics = diagnostics.native;
                if let Some(enabled) = diagnostics.native_tick_loop {
                    self.options.x86_options.native_tick_loop = enabled;
                }
            }
            self
        }

        /// Import legacy `CELOX_*` diagnostics switches once at the API boundary.
        pub fn diagnostics_from_env(self) -> Self {
            self.diagnostics(crate::DiagnosticsOptions::from_env())
        }

        pub fn trace_sim_modules(mut self) -> Self {
            self.options.trace.sim_modules = true;
            self
        }

        pub fn trace_pre_atomized_comb_blocks(mut self) -> Self {
            self.options.trace.pre_atomized_comb_blocks = true;
            self
        }

        pub fn trace_atomized_comb_blocks(mut self) -> Self {
            self.options.trace.atomized_comb_blocks = true;
            self
        }

        pub fn trace_flattened_comb_blocks(mut self) -> Self {
            self.options.trace.flattened_comb_blocks = true;
            self
        }

        pub fn trace_scheduled_units(mut self) -> Self {
            self.options.trace.scheduled_units = true;
            self
        }

        pub fn trace_pre_optimized_sir(mut self) -> Self {
            self.options.trace.pre_optimized_sir = true;
            self
        }

        pub fn trace_post_optimized_sir(mut self) -> Self {
            self.options.trace.post_optimized_sir = true;
            self
        }

        pub fn trace_analyzer_ir(mut self) -> Self {
            self.options.trace.analyzer_ir = true;
            self
        }

        pub fn trace_pre_optimized_clif(mut self) -> Self {
            self.options.trace.pre_optimized_clif = true;
            self
        }

        pub fn trace_post_optimized_clif(mut self) -> Self {
            self.options.trace.post_optimized_clif = true;
            self
        }

        pub fn trace_native(mut self) -> Self {
            self.options.trace.native = true;
            self
        }

        pub fn trace_mir(mut self) -> Self {
            self.options.trace.mir = true;
            self
        }

        /// Add one profile-selected native JIT block to state-layout feasibility
        /// analysis. The analysis is captured by [`Self::build_with_trace`] from
        /// the exact merged SIR passed to native instruction selection.
        pub fn trace_native_profile_block(
            mut self,
            function: impl Into<String>,
            block: u32,
            samples: u64,
        ) -> Self {
            self.options
                .trace
                .native_profile_blocks
                .push(crate::debug::NativeProfileBlock {
                    function: function.into(),
                    block,
                    samples,
                });
            self
        }

        pub fn trace_on_build(mut self) -> Self {
            self.options.trace.output_to_stdout = true;
            self
        }

        /// Explicitly ignore a dependency between two signals.
        pub fn false_loop(
            mut self,
            from: (Vec<(String, usize)>, Vec<String>),
            to: (Vec<(String, usize)>, Vec<String>),
        ) -> Self {
            self.ignored_loops.push((from, to));
            self
        }

        /// Mark a dependency as a "true loop" and specify its convergence limit.
        pub fn true_loop(
            mut self,
            from: (Vec<(String, usize)>, Vec<String>),
            to: (Vec<(String, usize)>, Vec<String>),
            max_iter: usize,
        ) -> Self {
            self.true_loops.push((from, to, max_iter));
            self
        }
    }

    impl<'a> SimulatorBuilder<'a, Simulator> {
        pub fn new(code: &'a str, top: &'a str) -> Self {
            Self {
                sources: vec![(code, Path::new(""))],
                sv_sources: Vec::new(),
                top,
                ignored_loops: Vec::new(),
                true_loops: Vec::new(),
                options: SimulatorOptions::default(),
                vcd_path: None,
                metadata: None,
                clock_type: None,
                reset_type: None,
                param_overrides: Vec::new(),
                live_signals: Vec::new(),
                injected_components: Default::default(),
                frontend_artifact: None,
                _marker: std::marker::PhantomData,
            }
        }

        pub fn from_sources(sources: Vec<(&'a str, &'a Path)>, top: &'a str) -> Self {
            Self {
                sources,
                sv_sources: Vec::new(),
                top,
                ignored_loops: Vec::new(),
                true_loops: Vec::new(),
                options: SimulatorOptions::default(),
                vcd_path: None,
                metadata: None,
                clock_type: None,
                reset_type: None,
                param_overrides: Vec::new(),
                live_signals: Vec::new(),
                injected_components: Default::default(),
                frontend_artifact: None,
                _marker: std::marker::PhantomData,
            }
        }

        #[cfg(feature = "systemverilog")]
        pub fn from_sv_sources(sources: Vec<(&'a str, &'a Path)>, top: &'a str) -> Self {
            Self {
                sources: Vec::new(),
                sv_sources: sources,
                top,
                ignored_loops: Vec::new(),
                true_loops: Vec::new(),
                options: SimulatorOptions::default(),
                vcd_path: None,
                metadata: None,
                clock_type: None,
                reset_type: None,
                param_overrides: Vec::new(),
                live_signals: Vec::new(),
                injected_components: Default::default(),
                frontend_artifact: None,
                _marker: std::marker::PhantomData,
            }
        }

        #[cfg(feature = "systemverilog")]
        pub fn from_mixed_sources(
            sources: Vec<(&'a str, &'a Path)>,
            sv_sources: Vec<(&'a str, &'a Path)>,
            top: &'a str,
        ) -> Self {
            Self {
                sources,
                sv_sources,
                top,
                ignored_loops: Vec::new(),
                true_loops: Vec::new(),
                options: SimulatorOptions::default(),
                vcd_path: None,
                metadata: None,
                clock_type: None,
                reset_type: None,
                param_overrides: Vec::new(),
                live_signals: Vec::new(),
                injected_components: Default::default(),
                frontend_artifact: None,
                _marker: std::marker::PhantomData,
            }
        }

        /// Build a simulator from a source-independent external frontend artifact.
        pub fn from_frontend(
            artifact: celox_frontend_sdk::FrontendArtifact,
        ) -> SimulatorBuilder<'static, Simulator> {
            SimulatorBuilder {
                sources: Vec::new(),
                sv_sources: Vec::new(),
                top: "",
                ignored_loops: Vec::new(),
                true_loops: Vec::new(),
                options: SimulatorOptions::default(),
                vcd_path: None,
                metadata: None,
                clock_type: None,
                reset_type: None,
                param_overrides: Vec::new(),
                live_signals: Vec::new(),
                injected_components: Default::default(),
                frontend_artifact: Some(artifact),
                _marker: std::marker::PhantomData,
            }
        }

        /// Build a Veryl native testbench whose `$sv::Module` instances are
        /// resolved from an external frontend artifact.
        pub fn from_frontend_with_testbench(
            artifact: celox_frontend_sdk::FrontendArtifact,
            sources: Vec<(&'a str, &'a Path)>,
            top: &'a str,
        ) -> Self {
            Self {
                sources,
                sv_sources: Vec::new(),
                top,
                ignored_loops: Vec::new(),
                true_loops: Vec::new(),
                options: SimulatorOptions::default(),
                vcd_path: None,
                metadata: None,
                clock_type: None,
                reset_type: None,
                param_overrides: Vec::new(),
                live_signals: Vec::new(),
                injected_components: Default::default(),
                frontend_artifact: Some(artifact),
                _marker: std::marker::PhantomData,
            }
        }

        /// Compile SIR, finalize its state layout, and return the typed artifact
        /// along with the remaining builder state.
        /// Consumes self.
        fn into_laid_out_program(
            mut self,
            layout_mode: crate::backend::memory_layout::MemoryLayoutMode,
        ) -> Result<
            (
                crate::ir::LaidOutProgram,
                Vec<CompilationWarning>,
                SimulatorOptions,
                Option<std::path::PathBuf>,
                crate::InjectedComponents,
            ),
            SimulatorError,
        > {
            self.enforce_native_force_optimizer();
            let phase_timing = self.options.diagnostics.phase_timing;
            let compile_start = phase_timing.then(crate::timing::now);
            let injected_manifests = self.injected_components.manifests();
            let (program, warnings) = if let Some(artifact) = &self.frontend_artifact {
                if self.sources.is_empty() {
                    compile_frontend_to_sir_with_layout_mode(
                        artifact,
                        &self.ignored_loops,
                        &self.true_loops,
                        self.options.four_state,
                        &self.options.trace,
                        None,
                        &self.options.optimize_options,
                        &self.options.diagnostics,
                        layout_mode,
                    )?
                } else {
                    compile_frontend_testbench_to_sir_with_layout_mode(
                        artifact,
                        &self.sources,
                        self.top,
                        &self.ignored_loops,
                        &self.true_loops,
                        self.options.four_state,
                        &self.options.trace,
                        None,
                        self.metadata,
                        self.clock_type,
                        self.reset_type,
                        &self.options.optimize_options,
                        &self.options.diagnostics,
                        &injected_manifests,
                        layout_mode,
                        !self.options.native_force_support,
                    )?
                }
            } else {
                compile_hdl_to_sir_with_layout_mode(
                    &self.sources,
                    &self.sv_sources,
                    self.top,
                    &self.ignored_loops,
                    &self.true_loops,
                    self.options.four_state,
                    &self.options.trace,
                    None,
                    self.metadata,
                    self.clock_type,
                    self.reset_type,
                    &self.param_overrides,
                    &self.options.optimize_options,
                    &self.options.diagnostics,
                    &injected_manifests,
                    layout_mode,
                    !self.options.native_force_support,
                )?
            };
            if let Some(start) = compile_start {
                tracing::debug!("[phase-timing] compile_to_sir: {:?}", start.elapsed());
            }

            // Build memory layout (consumes semantic layout requirements).
            let layout_start = phase_timing.then(crate::timing::now);
            let mut laid_out =
                program.into_laid_out_with_mode(self.options.four_state, layout_mode);
            if let Some(start) = layout_start {
                tracing::debug!("[phase-timing] build_layout: {:?}", start.elapsed());
            }

            if self.options.dead_store_policy != DeadStorePolicy::Off {
                let dse_start = phase_timing.then(crate::timing::now);
                run_dead_store_elimination(&mut laid_out, &self.live_signals, &self.options);
                if let Some(start) = dse_start {
                    tracing::debug!(
                        "[phase-timing] dead_store_elimination: {:?}",
                        start.elapsed()
                    );
                }
            }

            Ok((
                laid_out,
                warnings,
                self.options,
                self.vcd_path,
                self.injected_components,
            ))
        }

        /// Compiles the Veryl source and constructs the simulator.
        /// Uses a custom native backend on x86-64 and opt-in AArch64, Cranelift elsewhere.
        pub fn build(self) -> Result<Simulator<crate::DefaultBackend>, SimulatorError> {
            #[cfg(any(
                target_arch = "x86_64",
                all(target_arch = "aarch64", feature = "experimental-arm64-backend")
            ))]
            {
                self.build_native()
            }
            #[cfg(not(any(
                target_arch = "x86_64",
                all(target_arch = "aarch64", feature = "experimental-arm64-backend")
            )))]
            {
                self.build_cranelift()
            }
        }

        /// Compiles using the Cranelift JIT backend.
        pub fn build_cranelift(self) -> Result<Simulator<JitBackend>, SimulatorError> {
            let phase_timing = self.options.diagnostics.phase_timing;
            let phase_start = phase_timing.then(crate::timing::now);

            let (laid_out, warnings, options, vcd_path, injected_components) = self
                .into_laid_out_program(crate::backend::memory_layout::MemoryLayoutMode::Packed)?;

            if let Some(s) = phase_start {
                tracing::debug!(
                    "[phase-timing] compile_and_layout (total): {:?}",
                    s.elapsed()
                );
            }

            let jit_start = phase_timing.then(crate::timing::now);
            let mut trace = crate::debug::CompilationTrace::default();
            let wants_codegen_trace = options.trace.pre_optimized_clif
                || options.trace.post_optimized_clif
                || options.trace.native;
            let backend = JitBackend::new(
                &laid_out,
                &options,
                wants_codegen_trace.then_some(&mut trace),
            )?;
            if options.trace.output_to_stdout {
                trace.print();
            }
            if let Some(s) = jit_start {
                tracing::debug!("[phase-timing] jit_backend: {:?}", s.elapsed());
            }

            let mut sim =
                Simulator::with_backend_and_program(backend, laid_out.into_runtime(), warnings);
            sim.components.set_injected(injected_components);
            sim.diagnostics = options.diagnostics.clone();
            if let Some(path) = vcd_path {
                let descs = sim.build_vcd_descs(options.four_state);
                let vcd_writer = crate::VcdWriter::new(path, &descs)
                    .map_err(|_| SimulatorError::from(crate::RuntimeErrorCode::InternalError))?;
                sim.vcd_writer = Some(vcd_writer);
            }
            sim.apply_initial_values();
            sim.modify(|_| {}).map_err(SimulatorError::from)?;
            Ok(sim)
        }

        /// Compiles using the custom native backend for this host architecture.
        #[cfg(any(
            target_arch = "x86_64",
            all(target_arch = "aarch64", feature = "experimental-arm64-backend")
        ))]
        pub fn compile_native(self) -> Result<NativeCompilation, SimulatorError> {
            let phase_timing = self.options.diagnostics.phase_timing;
            let sir_start = phase_timing.then(crate::timing::now);
            let (laid_out, warnings, options, vcd_path, injected_components) = self
                .into_laid_out_program(
                    crate::backend::memory_layout::MemoryLayoutMode::ElementStrided,
                )?;
            if let Some(start) = sir_start {
                tracing::debug!(
                    "[phase-timing] into_laid_out_program total: {:?}",
                    start.elapsed()
                );
            }
            let backend_start = phase_timing.then(crate::timing::now);
            let backend = crate::backend::native::NativeBackend::new(&laid_out, &options)?;
            if let Some(start) = backend_start {
                tracing::debug!("[phase-timing] native_backend: {:?}", start.elapsed());
            }
            Ok(NativeCompilation {
                backend,
                program: laid_out,
                warnings,
                options,
                vcd_path,
                injected_components,
            })
        }

        /// Compiles using the custom native backend and initializes the simulator.
        #[cfg(any(
            target_arch = "x86_64",
            all(target_arch = "aarch64", feature = "experimental-arm64-backend")
        ))]
        pub fn build_native(
            self,
        ) -> Result<Simulator<crate::backend::native::NativeBackend>, SimulatorError> {
            self.compile_native()?.initialize()
        }

        /// Compiles using the Wasmtime WASM backend.
        pub fn build_wasm(
            self,
        ) -> Result<Simulator<crate::backend::wasm_runtime::WasmBackend>, SimulatorError> {
            let (laid_out, warnings, options, vcd_path, injected_components) = self
                .into_laid_out_program(crate::backend::memory_layout::MemoryLayoutMode::Packed)?;
            let backend = crate::backend::wasm_runtime::WasmBackend::new(&laid_out, &options)?;
            let mut sim =
                Simulator::with_backend_and_program(backend, laid_out.into_runtime(), warnings);
            sim.components.set_injected(injected_components);
            sim.diagnostics = options.diagnostics.clone();
            if let Some(path) = vcd_path {
                let descs = sim.build_vcd_descs(options.four_state);
                let vcd_writer = crate::VcdWriter::new(path, &descs)
                    .map_err(|_| SimulatorError::from(crate::RuntimeErrorCode::InternalError))?;
                sim.vcd_writer = Some(vcd_writer);
            }
            sim.apply_initial_values();
            sim.modify(|_| {}).map_err(SimulatorError::from)?;
            Ok(sim)
        }

        /// Compiles and runs a native testbench (`#[test]` module).
        pub fn run_test(self) -> Result<crate::testbench::TestResult, SimulatorError> {
            run_test_with_sim(self.build()?)
        }

        /// Compiles and runs a testbench using the Cranelift JIT backend.
        pub fn run_test_cranelift(self) -> Result<crate::testbench::TestResult, SimulatorError> {
            run_test_with_sim(self.build_cranelift()?)
        }

        /// Compiles and runs a testbench using the custom native backend.
        #[cfg(any(
            target_arch = "x86_64",
            all(target_arch = "aarch64", feature = "experimental-arm64-backend")
        ))]
        pub fn run_test_native(self) -> Result<crate::testbench::TestResult, SimulatorError> {
            run_test_with_sim(self.build_native()?)
        }

        /// Compiles and runs a native testbench, returning assertion results
        /// observed before the test finishes or stops on a fatal failure.
        pub fn run_test_detailed(
            self,
        ) -> Result<crate::testbench::TestResultDetailed, SimulatorError> {
            let mut sim = self.build()?;
            let testbench = crate::testbench::compile_initial_testbench(&sim).ok_or_else(|| {
                SimulatorError::new(SimulatorErrorKind::Codegen(crate::CodegenError::message(
                    "no initial block found — this module is not a native testbench",
                )))
            })?;
            Ok(crate::testbench::run_testbench_detailed(
                &mut sim, &testbench,
            ))
        }

        /// Compiles the Veryl source and constructs the core logic simulator,
        /// while capturing compilation trace data as configured by TraceOptions.
        pub fn build_with_trace(mut self) -> crate::debug::CompilationTraceResult {
            self.enforce_native_force_optimizer();
            let mut trace = crate::debug::CompilationTrace::default();
            #[cfg(any(
                target_arch = "x86_64",
                all(target_arch = "aarch64", feature = "experimental-arm64-backend")
            ))]
            let layout_mode = crate::backend::memory_layout::MemoryLayoutMode::ElementStrided;
            #[cfg(not(any(
                target_arch = "x86_64",
                all(target_arch = "aarch64", feature = "experimental-arm64-backend")
            )))]
            let layout_mode = crate::backend::memory_layout::MemoryLayoutMode::Packed;
            let program_res = compile_hdl_to_sir_with_layout_mode(
                &self.sources,
                &self.sv_sources,
                self.top,
                &self.ignored_loops,
                &self.true_loops,
                self.options.four_state,
                &self.options.trace,
                Some(&mut trace),
                self.metadata,
                self.clock_type,
                self.reset_type,
                &self.param_overrides,
                &self.options.optimize_options,
                &self.options.diagnostics,
                &self.injected_components.manifests(),
                layout_mode,
                !self.options.native_force_support,
            );

            let sim_res = program_res.and_then(|(program, warnings)| {
                let mut laid_out =
                    program.into_laid_out_with_mode(self.options.four_state, layout_mode);

                if self.options.dead_store_policy != DeadStorePolicy::Off {
                    run_dead_store_elimination(&mut laid_out, &self.live_signals, &self.options);
                }

                #[cfg(any(
                    target_arch = "x86_64",
                    all(target_arch = "aarch64", feature = "experimental-arm64-backend")
                ))]
                let backend = if self.options.trace.mir
                    || !self.options.trace.native_profile_blocks.is_empty()
                {
                    let (backend, native_trace) =
                        crate::backend::native::NativeBackend::new_with_codegen_trace(
                            &laid_out,
                            &self.options,
                        )?;
                    trace.native_optimized_sir = Some(native_trace.optimized_sir);
                    trace.mir = Some(native_trace.mir);
                    trace.reactive_event_graph = Some(native_trace.reactive_graph);
                    trace.native_state_layout = Some(native_trace.state_layout);
                    backend
                } else {
                    crate::backend::native::NativeBackend::new(&laid_out, &self.options)?
                };
                #[cfg(not(any(
                    target_arch = "x86_64",
                    all(target_arch = "aarch64", feature = "experimental-arm64-backend")
                )))]
                let backend = JitBackend::new(&laid_out, &self.options, None)?;

                let mut sim =
                    Simulator::with_backend_and_program(backend, laid_out.into_runtime(), warnings);
                sim.components
                    .set_injected(self.injected_components.clone());
                sim.diagnostics = self.options.diagnostics.clone();
                sim.apply_initial_values();
                sim.modify(|_| {}).map_err(SimulatorError::from)?;
                Ok(sim)
            });

            if self.options.trace.output_to_stdout {
                trace.print();
            }

            crate::debug::CompilationTraceResult {
                res: sim_res,
                trace,
            }
        }
    }

    fn run_test_with_sim<B: crate::backend::SimBackend>(
        mut sim: Simulator<B>,
    ) -> Result<crate::testbench::TestResult, SimulatorError> {
        let phase_timing = sim.diagnostics.phase_timing;
        let testbench_start = phase_timing.then(crate::timing::now);
        let testbench = crate::testbench::compile_initial_testbench(&sim).ok_or_else(|| {
            SimulatorError::new(SimulatorErrorKind::Codegen(crate::CodegenError::message(
                "no initial block found — this module is not a native testbench",
            )))
        })?;
        let result = crate::testbench::run_testbench(&mut sim, &testbench);
        if let Some(start) = testbench_start {
            tracing::debug!("[phase-timing] testbench: {:?}", start.elapsed());
        }
        Ok(result)
    }

    impl<'a> SimulatorBuilder<'a, crate::Simulation> {
        pub(crate) fn new(code: &'a str, top: &'a str) -> Self {
            Self {
                sources: vec![(code, Path::new(""))],
                sv_sources: Vec::new(),
                top,
                ignored_loops: Vec::new(),
                true_loops: Vec::new(),
                options: SimulatorOptions::default(),
                vcd_path: None,
                metadata: None,
                clock_type: None,
                reset_type: None,
                param_overrides: Vec::new(),
                live_signals: Vec::new(),
                injected_components: Default::default(),
                frontend_artifact: None,
                _marker: std::marker::PhantomData,
            }
        }

        pub(crate) fn from_sources(sources: Vec<(&'a str, &'a Path)>, top: &'a str) -> Self {
            Self {
                sources,
                sv_sources: Vec::new(),
                top,
                ignored_loops: Vec::new(),
                true_loops: Vec::new(),
                options: SimulatorOptions::default(),
                vcd_path: None,
                metadata: None,
                clock_type: None,
                reset_type: None,
                param_overrides: Vec::new(),
                live_signals: Vec::new(),
                injected_components: Default::default(),
                frontend_artifact: None,
                _marker: std::marker::PhantomData,
            }
        }

        pub(crate) fn from_frontend(
            artifact: celox_frontend_sdk::FrontendArtifact,
        ) -> SimulatorBuilder<'static, crate::Simulation> {
            SimulatorBuilder {
                sources: Vec::new(),
                sv_sources: Vec::new(),
                top: "",
                ignored_loops: Vec::new(),
                true_loops: Vec::new(),
                options: SimulatorOptions::default(),
                vcd_path: None,
                metadata: None,
                clock_type: None,
                reset_type: None,
                param_overrides: Vec::new(),
                live_signals: Vec::new(),
                injected_components: Default::default(),
                frontend_artifact: Some(artifact),
                _marker: std::marker::PhantomData,
            }
        }

        /// Compiles the Veryl source and constructs the timed simulation wrapper.
        pub fn build(mut self) -> Result<crate::Simulation, SimulatorError> {
            self.options.emit_triggers = true;
            self.enforce_native_force_optimizer();
            #[cfg(any(
                target_arch = "x86_64",
                all(target_arch = "aarch64", feature = "experimental-arm64-backend")
            ))]
            let layout_mode = crate::backend::memory_layout::MemoryLayoutMode::ElementStrided;
            #[cfg(not(any(
                target_arch = "x86_64",
                all(target_arch = "aarch64", feature = "experimental-arm64-backend")
            )))]
            let layout_mode = crate::backend::memory_layout::MemoryLayoutMode::Packed;
            let (program, warnings) = if let Some(artifact) = &self.frontend_artifact {
                compile_frontend_to_sir_with_layout_mode(
                    artifact,
                    &self.ignored_loops,
                    &self.true_loops,
                    self.options.four_state,
                    &self.options.trace,
                    None,
                    &self.options.optimize_options,
                    &self.options.diagnostics,
                    layout_mode,
                )?
            } else {
                compile_hdl_to_sir_with_layout_mode(
                    &self.sources,
                    &self.sv_sources,
                    self.top,
                    &self.ignored_loops,
                    &self.true_loops,
                    self.options.four_state,
                    &self.options.trace,
                    None,
                    self.metadata,
                    self.clock_type,
                    self.reset_type,
                    &self.param_overrides,
                    &self.options.optimize_options,
                    &self.options.diagnostics,
                    &self.injected_components.manifests(),
                    layout_mode,
                    !self.options.native_force_support,
                )?
            };
            let mut laid_out =
                program.into_laid_out_with_mode(self.options.four_state, layout_mode);

            if self.options.dead_store_policy != DeadStorePolicy::Off {
                run_dead_store_elimination(&mut laid_out, &self.live_signals, &self.options);
            }
            #[cfg(any(
                target_arch = "x86_64",
                all(target_arch = "aarch64", feature = "experimental-arm64-backend")
            ))]
            let backend = crate::backend::native::NativeBackend::new(&laid_out, &self.options)?;
            #[cfg(not(any(
                target_arch = "x86_64",
                all(target_arch = "aarch64", feature = "experimental-arm64-backend")
            )))]
            let backend = crate::backend::JitBackend::new(&laid_out, &self.options, None)?;

            let mut sim =
                Simulator::with_backend_and_program(backend, laid_out.into_runtime(), warnings);
            sim.components.set_injected(self.injected_components);
            sim.diagnostics = self.options.diagnostics.clone();
            if let Some(path) = self.vcd_path {
                let descs = sim.build_vcd_descs(self.options.four_state);
                let vcd_writer = crate::VcdWriter::new(path, &descs)
                    .map_err(|_| SimulatorError::from(crate::RuntimeErrorCode::InternalError))?;
                sim.vcd_writer = Some(vcd_writer);
            }
            sim.apply_initial_values();
            sim.modify(|_| {}).map_err(SimulatorError::from)?;
            Ok(crate::Simulation::new(sim))
        }
    }

    /// Resolve user-specified `(instance_path, var_path)` to `AbsoluteAddr` and run DSE.
    fn run_dead_store_elimination(
        program: &mut LaidOutProgram,
        live_signals: &[(Vec<(String, usize)>, Vec<String>)],
        options: &SimulatorOptions,
    ) {
        use crate::HashSet;
        use crate::ir::InstancePath;
        let mut externally_live = HashSet::default();

        // Native testbench expressions bypass SIR and read simulator memory
        // directly. Their inputs are therefore external DSE roots just like
        // signals named with `live_signal()`.
        externally_live.extend(program.runtime_schema.testbench_read_roots.iter().copied());

        // User-specified live signals
        for (inst_path, var_path) in live_signals {
            let inst_refs: Vec<(&str, usize)> =
                inst_path.iter().map(|(s, i)| (s.as_str(), *i)).collect();
            let var_refs: Vec<&str> = var_path.iter().map(|s| s.as_str()).collect();
            let addr = program.get_addr(&inst_refs, &var_refs).unwrap();
            externally_live.insert(addr);
        }

        // PreserveTopPorts: auto-collect top module port addresses
        if options.dead_store_policy == DeadStorePolicy::PreserveTopPorts {
            if let Some(&top_instance_id) = program.frontend.instance_ids.get(&InstancePath(vec![]))
            {
                if let Some(&top_module_id) = program.frontend.instance_module.get(&top_instance_id)
                {
                    if let Some(top_vars) = program.frontend.module_variables.get(&top_module_id) {
                        for info in top_vars.values() {
                            if info.var_kind.is_port() {
                                if let Some(address) =
                                    program.state_address_for_source(top_instance_id, info.id)
                                {
                                    externally_live.insert(address);
                                }
                            }
                        }
                    }
                }
            }
        }

        // PreserveAllPorts: collect port addresses from every instance
        if options.dead_store_policy == DeadStorePolicy::PreserveAllPorts {
            for (&instance_id, &module_id) in &program.frontend.instance_module {
                if let Some(vars) = program.frontend.module_variables.get(&module_id) {
                    for info in vars.values() {
                        if info.var_kind.is_port() {
                            if let Some(address) =
                                program.state_address_for_source(instance_id, info.id)
                            {
                                externally_live.insert(address);
                            }
                        }
                    }
                }
            }
        }

        crate::optimizer::sir::optimize_rooted_comb_memory(
            program,
            &externally_live,
            options.four_state,
        );
    }
}

#[cfg(feature = "host-runtime")]
pub use host::*;

#[cfg(test)]
mod component_library_tests {
    use super::component_library_target_name;

    #[test]
    fn cargo_lib_target_name_overrides_package_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            r#"
                [package]
                name = "package-name"
                version = "0.1.0"

                [lib]
                name = "actual_component"
            "#,
        )
        .unwrap();

        assert_eq!(
            component_library_target_name(dir.path()).as_deref(),
            Some("actual_component")
        );
    }
}
