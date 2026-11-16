//! Compiler and cocotb runtime entry points of the `celox` command.

use std::{collections::BTreeMap, path::Path};

use crate::{
    CompilationWarning, DiagnosticPolicy, NativeProgramImage, NativeProgramInstance, Simulator,
};
use libloading::{Library, Symbol};
use veryl_metadata::Metadata;

/// Compile numeric parameters with an explicit four-state simulation mode.
pub fn compile_native_image_with_state(
    source: &Path,
    top: &str,
    policy: DiagnosticPolicy,
    level: crate::OptLevel,
    access: crate::SignalAccessOptions,
    parameters: &BTreeMap<String, u64>,
    four_state: bool,
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
            .four_state(four_state)
            .native_force_support(false)
            .observable_updates(true)
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
            .four_state(four_state)
            .native_force_support(false)
            .observable_updates(true)
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

/// Run cocotb's VPI bootstrap against the design of a native image: a
/// timed simulation when the design has processes, so that they run in step
/// with the VPI time callbacks, and an untimed program instance otherwise.
pub fn run_cocotb_image(image: crate::NativeProgramImage, vpi_path: &Path) -> Result<(), String> {
    let session = if image.has_processes() {
        let simulation = crate::Simulation::from_sv_sources(Vec::new(), "")
            .build_native_from_image(image.clone())
            .map_err(|error| format!("failed to load attached design: {error}"))?;
        super::install_timed_runtime(image, simulation)?
    } else {
        // Safety: the image is compiler-produced data attached to this
        // executable.
        let instance = unsafe { NativeProgramInstance::from_image(image) }
            .map_err(|error| format!("failed to load attached design: {error}"))?;
        super::install_runtime(instance)?
    };
    run_session(session, vpi_path)
}

fn run_session(_session: super::VpiSession, vpi_path: &Path) -> Result<(), String> {
    // Safety: cocotb's VPI library is kept loaded throughout all callbacks and
    // its documented bootstrap takes no arguments.
    let library = match unsafe { Library::new(vpi_path) } {
        Ok(library) => library,
        Err(error) => {
            super::clear_runtime();
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

        if !super::run_callbacks_result()? {
            return Err("simulation stopped with no scheduled cocotb activity".to_string());
        }
        Ok(())
    })();
    super::clear_runtime();
    drop(library);
    result
}
