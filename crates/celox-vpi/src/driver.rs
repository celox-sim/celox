//! Shared compiler and cocotb runtime entry points used by the command-line tools.

use std::{collections::BTreeMap, path::Path};

use celox::{
    CompilationWarning, DiagnosticPolicy, NativeProgramImage, NativeProgramInstance, Simulator,
};
use libloading::{Library, Symbol};
use veryl_metadata::Metadata;

/// Compile a standalone SystemVerilog file or a Veryl file and its project dependencies.
pub fn compile_native_image(source: &Path, top: &str) -> Result<NativeProgramImage, String> {
    compile_native_sources(&[source], top)
}

/// Compile explicit SystemVerilog sources, or one Veryl project entry file.
pub fn compile_native_sources(sources: &[&Path], top: &str) -> Result<NativeProgramImage, String> {
    let source = *sources.first().ok_or("missing source path")?;
    let all_sv = sources
        .iter()
        .all(|path| path.extension().is_some_and(|extension| extension == "sv"));
    if sources.len() > 1 && !all_sv {
        return Err("multiple source paths require only .sv files; pass one Veryl project entry file for Veryl designs".to_string());
    }
    if all_sv {
        let contents = sources
            .iter()
            .map(|path| {
                std::fs::read_to_string(path)
                    .map_err(|error| format!("failed to read {}: {error}", path.display()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let source_refs = contents
            .iter()
            .zip(sources)
            .map(|(code, path)| (code.as_str(), *path))
            .collect();
        Simulator::from_sv_sources(source_refs, top)
            .four_state(true)
            .native_force_support(true)
            .opt_level(celox::OptLevel::O0)
            .compile_native()
            .map(|compiled| compiled.into_program_image())
            .map_err(|error| format!("compilation failed: {error}"))
    } else {
        compile_native_image_with_diagnostics(source, top, DiagnosticPolicy::default())
            .map(|(image, _)| image)
    }
}

/// Compile with a severity policy and retain warnings for the CLI boundary.
pub fn compile_native_image_with_diagnostics(
    source: &Path,
    top: &str,
    policy: DiagnosticPolicy,
) -> Result<(NativeProgramImage, Vec<CompilationWarning>), String> {
    compile_native_image_configured(
        source,
        top,
        policy,
        celox::OptLevel::O0,
        celox::SignalAccessOptions::default(),
        CompileMode {
            legacy_force: true,
            four_state: true,
        },
        &BTreeMap::new(),
    )
}

/// Compile with an explicit observation/force contract, retaining optimization.
pub fn compile_native_image_with_options(
    source: &Path,
    top: &str,
    policy: DiagnosticPolicy,
    level: celox::OptLevel,
    access: celox::SignalAccessOptions,
) -> Result<(NativeProgramImage, Vec<CompilationWarning>), String> {
    compile_native_image_with_parameters(source, top, policy, level, access, &BTreeMap::new())
}

/// Compile with numeric top parameter overrides, leaving project source files intact.
pub fn compile_native_image_with_parameters(
    source: &Path,
    top: &str,
    policy: DiagnosticPolicy,
    level: celox::OptLevel,
    access: celox::SignalAccessOptions,
    parameters: &BTreeMap<String, u64>,
) -> Result<(NativeProgramImage, Vec<CompilationWarning>), String> {
    compile_native_image_with_state(source, top, policy, level, access, parameters, true)
}

/// Compile numeric parameters with an explicit four-state simulation mode.
pub fn compile_native_image_with_state(
    source: &Path,
    top: &str,
    policy: DiagnosticPolicy,
    level: celox::OptLevel,
    access: celox::SignalAccessOptions,
    parameters: &BTreeMap<String, u64>,
    four_state: bool,
) -> Result<(NativeProgramImage, Vec<CompilationWarning>), String> {
    compile_native_image_configured(
        source,
        top,
        policy,
        level,
        access,
        CompileMode {
            legacy_force: false,
            four_state,
        },
        parameters,
    )
}

struct CompileMode {
    legacy_force: bool,
    four_state: bool,
}

fn compile_native_image_configured(
    source: &Path,
    top: &str,
    policy: DiagnosticPolicy,
    level: celox::OptLevel,
    access: celox::SignalAccessOptions,
    mode: CompileMode,
    parameters: &BTreeMap<String, u64>,
) -> Result<(NativeProgramImage, Vec<CompilationWarning>), String> {
    let compiled = if let Ok(metadata_path) = Metadata::search_from(source) {
        let mut metadata = Metadata::load(&metadata_path)
            .map_err(|error| format!("failed to load {}: {error}", metadata_path.display()))?;
        let paths = metadata
            .paths(&[source], false, true)
            .map_err(|error| format!("failed to discover project sources: {error}"))?;
        let sources = paths
            .into_iter()
            .map(|path| {
                let contents = std::fs::read_to_string(&path.src)
                    .map_err(|error| format!("failed to read {}: {error}", path.src.display()))?;
                Ok((contents, path.src))
            })
            .collect::<Result<Vec<_>, String>>()?;
        let source_refs = sources
            .iter()
            .map(|(contents, path)| (contents.as_str(), path.as_path()))
            .collect::<Vec<_>>();
        let builder = Simulator::from_sources(source_refs, top)
            .with_metadata(metadata)
            .diagnostic_policy(policy.clone())
            .four_state(mode.four_state)
            .native_force_support(mode.legacy_force)
            .signal_access(access.clone())
            .opt_level(level);
        parameters
            .iter()
            .fold(builder, |builder, (name, value)| {
                builder.param(name, *value)
            })
            .compile_native()
    } else {
        let contents = std::fs::read_to_string(source)
            .map_err(|error| format!("failed to read {}: {error}", source.display()))?;
        let builder = Simulator::from_sources(vec![(contents.as_str(), source)], top)
            .diagnostic_policy(policy.clone())
            .four_state(mode.four_state)
            .native_force_support(mode.legacy_force)
            .signal_access(access.clone())
            .opt_level(level);
        parameters
            .iter()
            .fold(builder, |builder, (name, value)| {
                builder.param(name, *value)
            })
            .compile_native()
    }
    .map_err(|error| format!("compilation failed: {error}"))?;
    Ok(compiled.into_program_image_and_warnings())
}

/// Run cocotb's VPI bootstrap against a compiled native program instance.
pub fn run_cocotb(instance: NativeProgramInstance, vpi_path: &Path) -> Result<(), String> {
    run_session(crate::install_runtime(instance)?, vpi_path)
}

/// Run cocotb's VPI bootstrap against the design of a native image: a
/// timed simulation when the design has processes, so that they run in step
/// with the VPI time callbacks, and an untimed program instance otherwise.
pub fn run_cocotb_image(image: celox::NativeProgramImage, vpi_path: &Path) -> Result<(), String> {
    let session = if image.has_processes() {
        let simulation = celox::Simulation::from_sv_sources(Vec::new(), "")
            .build_native_from_image(image.clone())
            .map_err(|error| format!("failed to load attached design: {error}"))?;
        crate::install_timed_runtime(image, simulation)?
    } else {
        // Safety: the image is compiler-produced data attached to this
        // executable.
        let instance = unsafe { NativeProgramInstance::from_image(image) }
            .map_err(|error| format!("failed to load attached design: {error}"))?;
        crate::install_runtime(instance)?
    };
    run_session(session, vpi_path)
}

fn run_session(_session: crate::VpiSession, vpi_path: &Path) -> Result<(), String> {
    // Safety: cocotb's VPI library is kept loaded throughout all callbacks and
    // its documented bootstrap takes no arguments.
    let library = match unsafe { Library::new(vpi_path) } {
        Ok(library) => library,
        Err(error) => {
            crate::clear_runtime();
            return Err(format!(
                "failed to load cocotb VPI library `{}`: {error}",
                vpi_path.display()
            ));
        }
    };
    let result = (|| {
        // Safety: every cocotb VPI implementation exports this stable bootstrap.
        let bootstrap: Symbol<unsafe extern "C" fn()> =
            unsafe { library.get(b"vlog_startup_routines_bootstrap\0") }
                .map_err(|error| format!("cocotb VPI bootstrap is missing: {error}"))?;
        // Safety: the symbol type and lifetime are established above.
        unsafe { bootstrap() };

        if !crate::run_callbacks_result()? {
            return Err("simulation stopped with no scheduled cocotb activity".to_string());
        }
        Ok(())
    })();
    crate::clear_runtime();
    drop(library);
    result
}
