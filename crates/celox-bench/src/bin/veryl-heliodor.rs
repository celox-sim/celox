#![allow(clippy::disallowed_macros)] // CLI errors intentionally use stderr

use std::{
    fs,
    path::PathBuf,
    time::{Duration, Instant},
};

use clap::Parser as ClapParser;
use veryl_analyzer::ir as air;
use veryl_analyzer::{Analyzer, AnalyzerError, Context};
use veryl_metadata::Metadata;
use veryl_parser::{Parser, resource_table};
use veryl_simulator::Simulator as VerylSimulator;
use veryl_simulator::ir::{Config, Event, ProtoModuleCache, build_ir_cached};
use veryl_simulator::testbench::{
    TestResult, build_clock_periods, build_event_map, convert_initial_to_testbench, run_testbench,
};

#[derive(ClapParser)]
#[command(about = "Run a Heliodor test with synchronous Veryl AOT-C")]
struct Options {
    #[arg(long)]
    project: PathBuf,
    #[arg(long)]
    test: String,
    #[arg(long = "source-file")]
    source_files: Vec<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
enum VerylHeliodorError {
    #[error(transparent)]
    Metadata(#[from] veryl_metadata::MetadataError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Parser(#[from] veryl_parser::ParserError),
    #[error(transparent)]
    Simulator(#[from] veryl_simulator::SimulatorError),
    #[error("{stage}: {errors:?}")]
    Analyzer {
        stage: &'static str,
        errors: Vec<AnalyzerError>,
    },
    #[error("top module not found: {module}")]
    TopModuleNotFound { module: String },
    #[error("no initial block found: {module}")]
    MissingInitialBlock { module: String },
    #[error("{message}")]
    TestFailed { message: String },
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), VerylHeliodorError> {
    let options = Options::parse();
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
    for (_path, parser, analyzer) in &contexts {
        ensure_no_errors(
            "analyze_pass2",
            analyzer.analyze_pass2(&parser.veryl, &mut context, Some(&mut analyzer_ir)),
        )?;
    }
    ensure_no_errors(
        "analyze_post_pass2",
        Analyzer::analyze_post_pass2(&analyzer_ir),
    )?;

    let top = resource_table::get_str_id(options.test.clone()).ok_or_else(|| {
        VerylHeliodorError::TopModuleNotFound {
            module: options.test.clone(),
        }
    })?;
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
        .ok_or_else(|| VerylHeliodorError::MissingInitialBlock {
            module: module_name.clone(),
        })?;
    let testbench = convert_initial_to_testbench(initial_stmts, &event_map, &clock_periods, 3);
    let compile_elapsed = compile_start.elapsed();

    let execute_cpu_start = process_cpu_time();
    let execute_start = Instant::now();
    let result = run_testbench(&mut sim, &testbench);
    let execute_elapsed = execute_start.elapsed();
    let execute_cpu_elapsed = process_cpu_time()
        .zip(execute_cpu_start)
        .map(|(end, start)| end.saturating_sub(start));
    let elapsed = total_start.elapsed();
    if let Some(execute_cpu_elapsed) = execute_cpu_elapsed {
        println!(
            "VERYL_TEST_TIMING test={} compile_ns={} execute_ns={} execute_cpu_ns={}",
            options.test,
            compile_elapsed.as_nanos(),
            execute_elapsed.as_nanos(),
            execute_cpu_elapsed.as_nanos()
        );
    } else {
        println!(
            "VERYL_TEST_TIMING test={} compile_ns={} execute_ns={}",
            options.test,
            compile_elapsed.as_nanos(),
            execute_elapsed.as_nanos()
        );
    }

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
            Err(VerylHeliodorError::TestFailed { message })
        }
    }
}

#[cfg(unix)]
fn process_cpu_time() -> Option<Duration> {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let result = unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut time) };
    (result == 0).then(|| {
        Duration::new(
            time.tv_sec.try_into().unwrap_or_default(),
            time.tv_nsec.try_into().unwrap_or_default(),
        )
    })
}

#[cfg(not(unix))]
fn process_cpu_time() -> Option<Duration> {
    None
}

fn ensure_no_errors(
    stage: &'static str,
    diagnostics: Vec<AnalyzerError>,
) -> Result<(), VerylHeliodorError> {
    let errors = diagnostics
        .into_iter()
        .filter(AnalyzerError::is_error)
        .collect::<Vec<_>>();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(VerylHeliodorError::Analyzer { stage, errors })
    }
}
