//! Shared compiler and cocotb runtime entry points used by the command-line tools.

use std::path::Path;

use celox::{NativeProgramImage, NativeProgramInstance, Simulator};
use libloading::{Library, Symbol};
use veryl_metadata::Metadata;

/// Compile a Veryl source file and its project dependencies into a native image.
pub fn compile_native_image(source: &Path, top: &str) -> Result<NativeProgramImage, String> {
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
        Simulator::from_sources(source_refs, top)
            .with_metadata(metadata)
            .four_state(true)
            .native_force_support(true)
            .opt_level(celox::OptLevel::O0)
            .build()
    } else {
        let contents = std::fs::read_to_string(source)
            .map_err(|error| format!("failed to read {}: {error}", source.display()))?;
        Simulator::from_sources(vec![(contents.as_str(), source)], top)
            .four_state(true)
            .native_force_support(true)
            .opt_level(celox::OptLevel::O0)
            .build()
    }
    .map_err(|error| format!("compilation failed: {error:?}"))?;
    Ok(simulator.shared_code().program_image().clone())
}

/// Run cocotb's VPI bootstrap against a compiled native program instance.
pub fn run_cocotb(instance: NativeProgramInstance, vpi_path: &Path) -> Result<(), String> {
    crate::install_runtime(instance);
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
