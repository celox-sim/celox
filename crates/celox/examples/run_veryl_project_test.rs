use std::{
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

use celox::{OptLevel, Simulator, TestResult};
use veryl_metadata::Metadata;

struct Options {
    project: PathBuf,
    test: String,
    opt_level: OptLevel,
    four_state: bool,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let opts = parse_args().map_err(|e| format!("{e}\n\n{}", usage()))?;
    let (sources, metadata) = load_project_sources(&opts.project)?;
    let source_refs: Vec<(&str, &Path)> = sources
        .iter()
        .map(|(source, path)| (source.as_str(), path.as_path()))
        .collect();

    let start = Instant::now();
    let builder = Simulator::from_sources(source_refs, &opts.test)
        .with_metadata(metadata)
        .opt_level(opts.opt_level)
        .four_state(opts.four_state);
    let result = builder.run_test()?;
    let elapsed = start.elapsed();

    match result {
        TestResult::Pass => {
            println!(
                "CELOX_TEST_RESULT test={} status=pass elapsed_ns={}",
                opts.test,
                elapsed.as_nanos()
            );
            Ok(())
        }
        TestResult::Fail(message) => {
            println!(
                "CELOX_TEST_RESULT test={} status=fail elapsed_ns={}",
                opts.test,
                elapsed.as_nanos()
            );
            Err(message.into())
        }
    }
}

fn parse_args() -> Result<Options, String> {
    let mut project = None;
    let mut test = None;
    let mut opt_level = OptLevel::O1;
    let mut four_state = false;
    let mut args = env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Err(String::new()),
            "--project" => {
                project = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| "--project requires a path".to_string())?,
                ));
            }
            "--test" => {
                test = Some(
                    args.next()
                        .ok_or_else(|| "--test requires a module name".to_string())?,
                );
            }
            "--opt-level" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--opt-level requires O0, O1, or O2".to_string())?;
                opt_level = parse_opt_level(&value)?;
            }
            "--four-state" => four_state = true,
            other if project.is_none() => project = Some(PathBuf::from(other)),
            other if test.is_none() => test = Some(other.to_string()),
            other => return Err(format!("unexpected argument: {other}")),
        }
    }

    Ok(Options {
        project: project.ok_or_else(|| "missing project path".to_string())?,
        test: test.ok_or_else(|| "missing test module".to_string())?,
        opt_level,
        four_state,
    })
}

fn parse_opt_level(value: &str) -> Result<OptLevel, String> {
    match value {
        "O0" | "o0" | "0" => Ok(OptLevel::O0),
        "O1" | "o1" | "1" => Ok(OptLevel::O1),
        "O2" | "o2" | "2" => Ok(OptLevel::O2),
        _ => Err(format!("invalid opt level: {value}")),
    }
}

fn usage() -> &'static str {
    "usage: cargo run -p celox --example run_veryl_project_test -- --project <dir> --test <module> [--opt-level O1] [--four-state]"
}

fn load_project_sources(
    project_path: &Path,
) -> Result<(Vec<(String, PathBuf)>, Metadata), Box<dyn Error>> {
    let toml_path = Metadata::search_from(project_path)?;
    let mut metadata = Metadata::load(&toml_path)?;
    let paths = metadata.paths::<&str>(&[], false, false)?;
    let mut sources = Vec::with_capacity(paths.len());
    for path in paths {
        let content = fs::read_to_string(&path.src)?;
        sources.push((content, path.src));
    }
    Ok((sources, metadata))
}
