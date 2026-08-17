#![allow(clippy::disallowed_methods)] // This binary is the command-line boundary.

use std::{env, io::Write, path::PathBuf, process::ExitCode};

use celox::Simulator;
use veryl_metadata::Metadata;

struct Arguments {
    source: PathBuf,
    top: String,
    runtime: PathBuf,
    output: PathBuf,
}

fn arguments() -> Result<Arguments, String> {
    let mut source = None;
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
            _ if source.is_none() => {
                source = Some(PathBuf::from(argument));
                continue;
            }
            _ => return Err(format!("unexpected argument `{argument}`")),
        };
        match argument.as_str() {
            "--top" => top = Some(value),
            "--runtime" => runtime = Some(PathBuf::from(value)),
            "--output" => output = Some(PathBuf::from(value)),
            _ => unreachable!(),
        }
    }
    Ok(Arguments {
        source: source.ok_or_else(|| "missing Veryl source path".to_string())?,
        top: top.ok_or_else(|| "missing --top".to_string())?,
        runtime: runtime.ok_or_else(|| "missing --runtime".to_string())?,
        output: output.ok_or_else(|| "missing --output".to_string())?,
    })
}

fn run() -> Result<(), String> {
    let arguments = arguments()?;
    let simulator = if let Ok(metadata_path) = Metadata::search_from(&arguments.source) {
        let mut metadata = Metadata::load(&metadata_path)
            .map_err(|error| format!("failed to load {}: {error}", metadata_path.display()))?;
        let paths = metadata
            .paths(&[arguments.source.as_path()], false, true)
            .map_err(|error| format!("failed to discover project sources: {error}"))?;
        let sources = paths
            .into_iter()
            .map(|path| {
                let source = std::fs::read_to_string(&path.src)
                    .map_err(|error| format!("failed to read {}: {error}", path.src.display()))?;
                Ok((source, path.src))
            })
            .collect::<Result<Vec<_>, String>>()?;
        let source_refs = sources
            .iter()
            .map(|(source, path)| (source.as_str(), path.as_path()))
            .collect::<Vec<_>>();
        Simulator::from_sources(source_refs, &arguments.top)
            .with_metadata(metadata)
            .four_state(true)
            .native_force_support(true)
            .opt_level(celox::OptLevel::O0)
            .build()
    } else {
        let source = std::fs::read_to_string(&arguments.source)
            .map_err(|error| format!("failed to read {}: {error}", arguments.source.display()))?;
        Simulator::from_sources(
            vec![(source.as_str(), arguments.source.as_path())],
            &arguments.top,
        )
        .four_state(true)
        .native_force_support(true)
        .opt_level(celox::OptLevel::O0)
        .build()
    }
    .map_err(|error| format!("compilation failed: {error:?}"))?;
    simulator
        .shared_code()
        .program_image()
        .write_attached_runtime(&arguments.runtime, &arguments.output)
        .map_err(|error| format!("failed to write {}: {error}", arguments.output.display()))
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(
                std::io::stderr(),
                "celox-vpi-compile: {error}\nusage: celox-vpi-compile SOURCE --top TOP --runtime RUNTIME --output OUTPUT"
            );
            ExitCode::FAILURE
        }
    }
}
