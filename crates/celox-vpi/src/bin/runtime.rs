#![allow(clippy::disallowed_methods)] // This binary is the process environment boundary.

use std::{env, io::Write, process::ExitCode};

use celox::NativeProgramInstance;
use libloading::{Library, Symbol};

fn plugin_path() -> Result<String, String> {
    let mut arguments = env::args().skip(1);
    while let Some(argument) = arguments.next() {
        if argument == "--vpi" {
            return arguments
                .next()
                .ok_or_else(|| "--vpi requires a shared-library path".to_string());
        }
    }
    env::var_os("CELOX_COCOTB_VPI")
        .map(|path| path.to_string_lossy().into_owned())
        .ok_or_else(|| "pass --vpi PATH or set CELOX_COCOTB_VPI".to_string())
}

fn run() -> Result<(), String> {
    let instance = NativeProgramInstance::from_current_executable()
        .map_err(|error| format!("failed to load attached design: {error}"))?;
    celox_vpi::install_runtime(instance);

    let path = plugin_path()?;
    // Safety: cocotb's VPI library is kept loaded throughout all callbacks and
    // its documented bootstrap takes no arguments.
    let library = unsafe { Library::new(&path) }
        .map_err(|error| format!("failed to load cocotb VPI library `{path}`: {error}"))?;
    // Safety: every cocotb VPI implementation exports this stable bootstrap.
    let bootstrap: Symbol<unsafe extern "C" fn()> =
        unsafe { library.get(b"vlog_startup_routines_bootstrap\0") }
            .map_err(|error| format!("cocotb VPI bootstrap is missing: {error}"))?;
    // Safety: the symbol type and lifetime are established above.
    unsafe { bootstrap() };

    if !celox_vpi::run_callbacks() {
        return Err("simulation stopped with no scheduled cocotb activity".to_string());
    }
    celox_vpi::clear_runtime();
    drop(library);
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "celox-vpi-runtime: {error}");
            ExitCode::FAILURE
        }
    }
}
