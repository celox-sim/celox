use std::{env, error::Error, fs, path::PathBuf, time::Instant};

use veryl_analyzer::ir as air;
use veryl_analyzer::{Analyzer, AnalyzerError, Context};
use veryl_metadata::Metadata;
use veryl_parser::{Parser, resource_table};
use veryl_simulator::Simulator as VerylSimulator;
use veryl_simulator::ir::{Config, Event, ProtoModuleCache, build_ir_cached};
use veryl_simulator::testbench::{
    TestResult, build_clock_periods, build_event_map, convert_initial_to_testbench, run_testbench,
};

struct Options {
    project: PathBuf,
    test: String,
    source_files: Vec<PathBuf>,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let options = parse_args().map_err(|error| format!("{error}\n\n{}", usage()))?;
    let metadata_path = Metadata::search_from(&options.project)?;
    let mut metadata = Metadata::load(&metadata_path)?;
    let paths = metadata.paths(&options.source_files, false, true)?;
    let sources = paths
        .into_iter()
        .map(|path| {
            let input = fs::read_to_string(&path.src)?;
            Ok::<_, std::io::Error>((path, input))
        })
        .collect::<Result<Vec<_>, _>>()?;

    println!(
        "VERYL_TEST_CONFIG test={} backend=cc aot_c_async=false",
        options.test
    );

    let total_start = Instant::now();
    let compile_start = Instant::now();
    let mut contexts = Vec::with_capacity(sources.len());
    for (path, input) in &sources {
        let parser = Parser::parse(input, &path.src)?;
        let analyzer = Analyzer::new(&metadata);
        ensure_no_errors(
            "analyze_pass1",
            analyzer.analyze_pass1(&path.prj, &parser.veryl),
        )?;
        contexts.push((path, parser, analyzer));
    }
    ensure_no_errors("analyze_post_pass1", Analyzer::analyze_post_pass1())?;

    let mut context = Context::default();
    let mut analyzer_ir = air::Ir::default();
    for (path, parser, analyzer) in &contexts {
        ensure_no_errors(
            "analyze_pass2",
            analyzer.analyze_pass2(
                &path.prj,
                &parser.veryl,
                &mut context,
                Some(&mut analyzer_ir),
            ),
        )?;
    }
    ensure_no_errors(
        "analyze_post_pass2",
        Analyzer::analyze_post_pass2(&analyzer_ir),
    )?;

    let top = resource_table::get_str_id(options.test.clone())
        .ok_or_else(|| format!("top module not found: {}", options.test))?;
    let config = Config {
        use_jit: true,
        aot_c: true,
        aot_c_event: true,
        aot_c_async: false,
        aot_c_validate: false,
        aot_c_min_stmts: 0,
        ..Config::default()
    };
    let mut cache = ProtoModuleCache::default();
    let sim_ir = build_ir_cached(&analyzer_ir, top, &config, &mut cache)?;
    let module_name = sim_ir.name.to_string();
    let mut sim = VerylSimulator::new(sim_ir, None);
    let event_map = build_event_map(&sim.ir.event_statements, &sim.ir.module_variables);
    let clock_periods = build_clock_periods(&sim.ir.event_statements);
    let initial_stmts = sim
        .ir
        .event_statements
        .get(&Event::Initial)
        .ok_or_else(|| format!("no initial block found: {module_name}"))?;
    let testbench = convert_initial_to_testbench(initial_stmts, &event_map, &clock_periods, 3);
    let compile_elapsed = compile_start.elapsed();

    let execute_start = Instant::now();
    let result = run_testbench(&mut sim, &testbench);
    let execute_elapsed = execute_start.elapsed();
    let elapsed = total_start.elapsed();
    println!(
        "VERYL_TEST_TIMING test={} compile_ns={} execute_ns={}",
        options.test,
        compile_elapsed.as_nanos(),
        execute_elapsed.as_nanos()
    );

    match result {
        TestResult::Pass => {
            println!(
                "VERYL_TEST_RESULT test={} status=pass elapsed_ns={}",
                options.test,
                elapsed.as_nanos()
            );
            Ok(())
        }
        TestResult::Fail(message) => {
            println!(
                "VERYL_TEST_RESULT test={} status=fail elapsed_ns={}",
                options.test,
                elapsed.as_nanos()
            );
            Err(message.into())
        }
    }
}

fn ensure_no_errors(stage: &str, diagnostics: Vec<AnalyzerError>) -> Result<(), Box<dyn Error>> {
    let errors = diagnostics
        .into_iter()
        .filter(AnalyzerError::is_error)
        .collect::<Vec<_>>();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!("{stage}: {errors:?}").into())
    }
}

fn parse_args() -> Result<Options, String> {
    let mut project = None;
    let mut test = None;
    let mut source_files = Vec::new();
    let mut args = env::args().skip(1);
    while let Some(argument) = args.next() {
        match argument.as_str() {
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
            "--source-file" => {
                source_files.push(PathBuf::from(
                    args.next()
                        .ok_or_else(|| "--source-file requires a path".to_string())?,
                ));
            }
            other if project.is_none() => project = Some(PathBuf::from(other)),
            other if test.is_none() => test = Some(other.to_string()),
            other => return Err(format!("unexpected argument: {other}")),
        }
    }
    Ok(Options {
        project: project.ok_or_else(|| "missing project path".to_string())?,
        test: test.ok_or_else(|| "missing test module".to_string())?,
        source_files,
    })
}

fn usage() -> &'static str {
    "usage: cargo run -p celox --example run_veryl_project_test_timed -- --project <dir> --test <module> [--source-file <path> ...]"
}
