#![allow(clippy::disallowed_methods)] // This binary is the process environment boundary.

use std::{env, io::Write, process::ExitCode};

use celox::NativeProgramInstance;

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
    // Safety: this runtime executes the image attached to the same trusted
    // compiler-produced executable artifact.
    let instance = unsafe { NativeProgramInstance::from_current_executable() }
        .map_err(|error| format!("failed to load attached design: {error}"))?;
    let path = plugin_path()?;
    celox_vpi::driver::run_cocotb(instance, std::path::Path::new(&path))
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
