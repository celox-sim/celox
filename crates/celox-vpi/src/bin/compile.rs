#![allow(clippy::disallowed_methods)] // This binary is the command-line boundary.

use std::{env, io::Write, path::PathBuf, process::ExitCode};

struct Arguments {
    sources: Vec<PathBuf>,
    top: String,
    runtime: PathBuf,
    output: PathBuf,
}

fn arguments() -> Result<Arguments, String> {
    let mut sources = Vec::new();
    let mut top = None;
    let mut runtime = None;
    let mut output = None;
    let mut args = env::args().skip(1);
    while let Some(argument) = args.next() {
        let value = match argument.as_str() {
            "--top" | "--runtime" | "--output" => args
                .next()
                .ok_or_else(|| format!("{argument} requires a value"))?,
            _ if argument.starts_with('-') => return Err(format!("unknown option `{argument}`")),
            _ => {
                sources.push(PathBuf::from(argument));
                continue;
            }
        };
        match argument.as_str() {
            "--top" => top = Some(value),
            "--runtime" => runtime = Some(PathBuf::from(value)),
            "--output" => output = Some(PathBuf::from(value)),
            _ => unreachable!(),
        }
    }
    if sources.is_empty() {
        return Err("missing Veryl or SystemVerilog source path".to_string());
    }
    Ok(Arguments {
        sources,
        top: top.ok_or_else(|| "missing --top".to_string())?,
        runtime: runtime.ok_or_else(|| "missing --runtime".to_string())?,
        output: output.ok_or_else(|| "missing --output".to_string())?,
    })
}

fn run() -> Result<(), String> {
    let arguments = arguments()?;
    let sources = arguments
        .sources
        .iter()
        .map(PathBuf::as_path)
        .collect::<Vec<_>>();
    celox_vpi::driver::compile_native_sources(&sources, &arguments.top)?
        .write_attached_runtime(&arguments.runtime, &arguments.output)
        .map_err(|error| format!("failed to write {}: {error}", arguments.output.display()))
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(
                std::io::stderr(),
                "celox-vpi-compile: {error}\nusage: celox-vpi-compile SOURCE... --top TOP --runtime RUNTIME --output OUTPUT"
            );
            ExitCode::FAILURE
        }
    }
}
