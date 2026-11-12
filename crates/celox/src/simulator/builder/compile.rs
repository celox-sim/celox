//! Compiling sources of each frontend to SIR.

use super::*;

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
    compile_to_sir_with_latch_policy(
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
        false,
    )
}

/// Compile Veryl with an explicit inferred-latch policy.
///
/// The default compilation APIs reject inferred latches. Pass `true` here to
/// allow them, or use `SimulatorBuilder::allow_inferred_latches` on host targets.
pub fn compile_to_sir_with_latch_policy(
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
    allow_inferred_latches: bool,
) -> Result<(OptimizedSir, Vec<CompilationWarning>), SimulatorError> {
    compile_to_sir_with_diagnostic_policy(
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
        &crate::DiagnosticPolicy::default().with_inferred_latches(allow_inferred_latches),
    )
}

/// Compile Veryl with the same per-code policy used by host builders and celox.toml.
pub fn compile_to_sir_with_diagnostic_policy(
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
    diagnostic_policy: &crate::DiagnosticPolicy,
) -> Result<(OptimizedSir, Vec<CompilationWarning>), SimulatorError> {
    let result = compile_to_sir_with_layout_mode(
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
        diagnostic_policy,
        None,
        false,
        &[],
        crate::backend::memory_layout::MemoryLayoutMode::ElementStrided,
        true,
        false,
        true,
    );
    let (program, mut warnings) =
        result.map_err(|error| error.apply_diagnostic_policy(diagnostic_policy))?;
    diagnostic_policy.apply(&mut warnings)?;
    Ok((program, warnings))
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
        crate::backend::memory_layout::MemoryLayoutMode::ElementStrided,
    )
}

pub(super) fn compile_frontend_to_sir_with_layout_mode(
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
        &crate::optimizer::parallel_schedule_options(optimize_options),
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
pub(super) fn compile_frontend_testbench_to_sir_with_layout_mode(
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
    diagnostic_policy: &crate::DiagnosticPolicy,
    sv_timescale: Option<crate::SvTimescale>,
    legacy_empty_reductions: bool,
    injected_manifests: &[(String, veryl_metadata::ComponentManifest)],
    layout_mode: crate::backend::memory_layout::MemoryLayoutMode,
    recover_comb_loops: bool,
    allow_always_ff_function_effects: bool,
    allow_inferred_latches: bool,
) -> Result<(OptimizedSir, Vec<CompilationWarning>), SimulatorError> {
    // The testbench drives time itself and never resumes process kernels.
    if !artifact.processes().is_empty() {
        return Err(
            celox_frontend_core::FrontendArtifactError::ProcessesInTestbench {
                module: artifact.module_name().to_string(),
            }
            .into(),
        );
    }
    let lowered = celox_frontend_core::lower_frontend_artifact(artifact)?;
    let (sir, errors, frontend_diagnostics, elaboration_warnings) = analyze(
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
        diagnostic_policy,
        sv_timescale,
        legacy_empty_reductions,
        injected_manifests,
        layout_mode == crate::backend::memory_layout::MemoryLayoutMode::ElementStrided,
        recover_comb_loops,
        allow_always_ff_function_effects,
        allow_inferred_latches,
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
        .chain(elaboration_warnings)
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
        Err(error) => Err(error.with_warnings(warnings)),
    }
}

pub(super) fn compile_to_sir_with_layout_mode(
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
    diagnostic_policy: &crate::DiagnosticPolicy,
    sv_timescale: Option<crate::SvTimescale>,
    legacy_empty_reductions: bool,
    injected_manifests: &[(String, veryl_metadata::ComponentManifest)],
    layout_mode: crate::backend::memory_layout::MemoryLayoutMode,
    recover_comb_loops: bool,
    allow_always_ff_function_effects: bool,
    allow_inferred_latches: bool,
) -> Result<(OptimizedSir, Vec<CompilationWarning>), SimulatorError> {
    let (sir, errors, frontend_diagnostics, elaboration_warnings) = analyze(
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
        diagnostic_policy,
        sv_timescale,
        legacy_empty_reductions,
        injected_manifests,
        layout_mode == crate::backend::memory_layout::MemoryLayoutMode::ElementStrided,
        recover_comb_loops,
        allow_always_ff_function_effects,
        allow_inferred_latches,
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
        .chain(elaboration_warnings)
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
        Err(e) => Err(e.with_warnings(warnings)),
    }
}

/// Compile SystemVerilog sources to optimized SIR.
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
    let result = compile_sv_to_sir_with_layout_mode(
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
        &crate::DiagnosticPolicy::default(),
        None,
        false,
        crate::backend::memory_layout::MemoryLayoutMode::ElementStrided,
        false,
        &[],
        &[],
    );
    let policy = crate::DiagnosticPolicy::default();
    let (program, mut warnings) = result.map_err(|error| error.apply_diagnostic_policy(&policy))?;
    policy.apply(&mut warnings)?;
    Ok((program, warnings))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn compile_sv_to_sir_with_layout_mode(
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
    diagnostic_policy: &crate::DiagnosticPolicy,
    sv_timescale: Option<crate::SvTimescale>,
    legacy_empty_reductions: bool,
    layout_mode: crate::backend::memory_layout::MemoryLayoutMode,
    _allow_inferred_latches: bool,
    force_signals: &[String],
    sv_param_literals: &[(String, String)],
) -> Result<(OptimizedSir, Vec<CompilationWarning>), SimulatorError> {
    let mut parameters = param_overrides
        .iter()
        .map(|(name, value)| {
            (
                name.clone(),
                celox_frontend_sv::analyzer_ir::ConstExpr::Literal(value.to_string()),
            )
        })
        .collect::<Vec<_>>();
    for (name, literal) in sv_param_literals {
        let value = celox_frontend_sv::analyzer_typecheck::parse_parameter_literal(literal)
            .ok_or_else(|| {
                crate::CodegenError::message(format!("invalid SV parameter literal `{literal}`"))
            })?;
        parameters.push((name.clone(), value));
    }
    let metadata = metadata.unwrap_or_else(|| Metadata::create_default("prj").unwrap());
    let mut build_config = BuildConfig::from(&metadata.build);
    build_config.allow_inferred_latches = true;
    if let Some(clock_type) = clock_type {
        build_config.clock_type = clock_type;
    }
    if let Some(reset_type) = reset_type {
        build_config.reset_type = reset_type;
    }
    let (component_libraries, component_file_base) = component_runtime_config(&metadata);
    let mut sv_warnings = Vec::new();
    let sir = parser::parse_sv(
        sources,
        top,
        &parameters,
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
        layout_mode == crate::backend::memory_layout::MemoryLayoutMode::ElementStrided,
        metadata.test.seed,
        component_libraries,
        component_file_base,
        &mut sv_warnings,
        force_signals,
    );
    let warnings = sv_warnings
        .into_iter()
        .map(CompilationWarning::SystemVerilog)
        .collect();
    match sir {
        Ok(program) => Ok((program, warnings)),
        Err(error) => Err(SimulatorError::from(error).with_warnings(warnings)),
    }
}

/// Compile a mixed Veryl/SystemVerilog hierarchy to optimized SIR.
/// Either language may supply the top or instantiate the other language.
#[allow(clippy::too_many_arguments)]
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
    let result = compile_mixed_to_sir_with_layout_mode(
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
        &crate::DiagnosticPolicy::default(),
        None,
        false,
        &[],
        crate::backend::memory_layout::MemoryLayoutMode::ElementStrided,
        true,
        false,
        false,
    );
    let policy = crate::DiagnosticPolicy::default();
    let (program, mut warnings) = result.map_err(|error| error.apply_diagnostic_policy(&policy))?;
    policy.apply(&mut warnings)?;
    Ok((program, warnings))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn compile_mixed_to_sir_with_layout_mode(
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
    diagnostic_policy: &crate::DiagnosticPolicy,
    sv_timescale: Option<crate::SvTimescale>,
    legacy_empty_reductions: bool,
    injected_manifests: &[(String, veryl_metadata::ComponentManifest)],
    layout_mode: crate::backend::memory_layout::MemoryLayoutMode,
    recover_comb_loops: bool,
    allow_always_ff_function_effects: bool,
    allow_inferred_latches: bool,
) -> Result<(OptimizedSir, Vec<CompilationWarning>), SimulatorError> {
    let (sir, errors, frontend_diagnostics, elaboration_warnings) = analyze(
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
        diagnostic_policy,
        sv_timescale,
        legacy_empty_reductions,
        injected_manifests,
        layout_mode == crate::backend::memory_layout::MemoryLayoutMode::ElementStrided,
        recover_comb_loops,
        allow_always_ff_function_effects,
        allow_inferred_latches,
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
        .chain(elaboration_warnings)
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
        Err(error) => Err(error.with_warnings(warnings)),
    }
}

/// Write the settle plan a trace captured to `path`
/// ([`SimulatorBuilder::settle_plan_output`]); a build without a partitioned
/// settle writes nothing.
#[cfg(feature = "host-runtime")]
pub(super) fn write_settle_plan_output(
    path: Option<&std::path::Path>,
    trace: &crate::debug::CompilationTrace,
) {
    if let (Some(path), Some(plan)) = (path, &trace.settle_plan)
        && let Err(error) = std::fs::write(path, plan)
    {
        tracing::warn!(
            "cannot write the settle plan to {}: {error}",
            path.display()
        );
    }
}

#[allow(clippy::too_many_arguments)]
#[cfg(feature = "host-runtime")]
pub(super) fn compile_hdl_to_sir_with_layout_mode(
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
    diagnostic_policy: &crate::DiagnosticPolicy,
    sv_timescale: Option<crate::SvTimescale>,
    legacy_empty_reductions: bool,
    injected_manifests: &[(String, veryl_metadata::ComponentManifest)],
    layout_mode: crate::backend::memory_layout::MemoryLayoutMode,
    recover_comb_loops: bool,
    allow_always_ff_function_effects: bool,
    allow_inferred_latches: bool,
    force_signals: &[String],
    sv_param_literals: &[(String, String)],
) -> Result<(OptimizedSir, Vec<CompilationWarning>), SimulatorError> {
    if !sv_param_literals.is_empty() && (!sources.is_empty() || sv_sources.is_empty()) {
        return Err(crate::CodegenError::message(
            "parameter literals require pure SystemVerilog sources",
        )
        .into());
    }
    if !force_signals.is_empty() && !sources.is_empty() {
        return Err(crate::CodegenError::message(
            "force_signals currently require pure SystemVerilog sources",
        )
        .into());
    }
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
            diagnostic_policy,
            sv_timescale,
            legacy_empty_reductions,
            injected_manifests,
            layout_mode,
            recover_comb_loops,
            allow_always_ff_function_effects,
            allow_inferred_latches,
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
            diagnostic_policy,
            sv_timescale,
            legacy_empty_reductions,
            layout_mode,
            allow_inferred_latches,
            force_signals,
            sv_param_literals,
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
            diagnostic_policy,
            sv_timescale,
            legacy_empty_reductions,
            injected_manifests,
            layout_mode,
            recover_comb_loops,
            allow_always_ff_function_effects,
            allow_inferred_latches,
        ),
    }
}
