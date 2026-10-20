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
            .build()
            .map(|simulator| simulator.shared_code().program_image().clone())
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
        true,
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
    compile_native_image_configured(source, top, policy, level, access, false, parameters)
}

fn compile_native_image_configured(
    source: &Path,
    top: &str,
    policy: DiagnosticPolicy,
    level: celox::OptLevel,
    access: celox::SignalAccessOptions,
    legacy_force: bool,
    parameters: &BTreeMap<String, u64>,
) -> Result<(NativeProgramImage, Vec<CompilationWarning>), String> {
    let simulator = if let Ok(metadata_path) = Metadata::search_from(source) {
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
            .four_state(true)
            .native_force_support(legacy_force)
            .signal_access(access.clone())
            .opt_level(level);
        parameters
            .iter()
            .fold(builder, |builder, (name, value)| {
                builder.param(name, *value)
            })
            .build()
    } else {
        let contents = std::fs::read_to_string(source)
            .map_err(|error| format!("failed to read {}: {error}", source.display()))?;
        let builder = Simulator::from_sources(vec![(contents.as_str(), source)], top)
            .diagnostic_policy(policy.clone())
            .four_state(true)
            .native_force_support(legacy_force)
            .signal_access(access.clone())
            .opt_level(level);
        parameters
            .iter()
            .fold(builder, |builder, (name, value)| {
                builder.param(name, *value)
            })
            .build()
    }
    .map_err(|error| format!("compilation failed: {error}"))?;
    let image = simulator.shared_code().program_image().clone();
    Ok((image, simulator.into_warnings()))
}

/// Run cocotb's VPI bootstrap against a compiled native program instance.
pub fn run_cocotb(instance: NativeProgramInstance, vpi_path: &Path) -> Result<(), String> {
    let _session = crate::install_runtime(instance)?;
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
