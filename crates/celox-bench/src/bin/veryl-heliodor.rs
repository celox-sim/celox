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
use veryl_simulator::ir::{BuildSession, Config, ProtoModuleCache, build_ir_cached};
use veryl_simulator::testbench::{
    TestResult, build_clock_periods, build_event_map, convert_initial_to_testbench,
    run_testbench_blocks,
};

#[derive(ClapParser)]
#[command(about = "Run a Heliodor test with synchronous or tiered Veryl AOT-C")]
struct Options {
    #[arg(long)]
    project: PathBuf,
    #[arg(long)]
    test: String,
    #[arg(long = "source-file")]
    source_files: Vec<PathBuf>,
    /// Build the complete AOT-C simulator without running the testbench.
    #[arg(long)]
    compile_only: bool,
    /// Run on Cranelift while C compiles in the background, as in `veryl test --backend cc`.
    #[arg(long, conflicts_with = "compile_only")]
    aot_c_async: bool,
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
        .filter(|path| !options.source_files.is_empty() || !path.example)
        .map(|path| {
            let input = fs::read_to_string(&path.src)?;
            Ok::<_, std::io::Error>((path, input))
        })
        .collect::<Result<Vec<_>, _>>()?;

    println!(
        "VERYL_TEST_CONFIG test={} backend=cc aot_c_async={} compile_only={}",
        options.test, options.aot_c_async, options.compile_only
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
        aot_c_async: options.aot_c_async,
        aot_c_validate: false,
        aot_c_min_stmts: 0,
        ..Config::default()
    };
    let session = BuildSession::new(&analyzer_ir, &config, &[top]);
    let mut cache = ProtoModuleCache::new(&session);
    let sim_ir = build_ir_cached(top, &mut cache)?;
    let module_name = sim_ir.name.to_string();
    let mut sim = VerylSimulator::new(sim_ir, None);
    let event_map = build_event_map(&sim.ir.event_statements, &sim.ir.module_variables);
    let clock_periods = build_clock_periods(&sim.ir.event_statements);
    // Veryl keeps each initial block as a separate process, including blocks
    // in instantiated modules. Preserve declaration order and run them together.
    let mut initials: Vec<_> = sim
        .ir
        .event_statements
        .iter()
        .filter_map(|(event, stmts)| event.initial_index().map(|index| (index, stmts)))
        .collect();
    initials.sort_by_key(|(index, _)| *index);
    if initials.is_empty() {
        return Err(VerylHeliodorError::MissingInitialBlock {
            module: module_name,
        });
    }
    let testbenches: Vec<_> = initials
        .iter()
        .map(|(_, stmts)| convert_initial_to_testbench(stmts, &event_map, &clock_periods, 3))
        .collect();
    let blocks: Vec<_> = testbenches.iter().map(Vec::as_slice).collect();
    // In async mode this is startup until simulation can begin. C compilation
    // can continue during run_testbench, so it is not the full compile cost.
    let compile_elapsed = compile_start.elapsed();

    if options.compile_only {
        let elapsed = total_start.elapsed();
        println!(
            "VERYL_TEST_TIMING test={} compile_ns={} execute_ns=0",
            options.test,
            compile_elapsed.as_nanos()
        );
        println!(
            "VERYL_TEST_RESULT test={} status=compile-only elapsed_ns={}",
            options.test,
            elapsed.as_nanos()
        );
        return Ok(());
    }

    let execute_cpu_start = process_cpu_time();
    let execute_start = Instant::now();
    let result = run_testbench_blocks(&mut sim, &blocks);
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

    if options.aot_c_async {
        // Veryl publishes whole-module dispatch counts when the IR is dropped.
        // Drop and reporting stay outside both timed intervals.
        drop(sim);
        let (compiled, fallback) = veryl_simulator::residency::dispatch_counts()
            .into_iter()
            .fold(
                (0_u64, 0_u64),
                |(compiled, fallback), (_, ran, fell_back)| (compiled + ran, fallback + fell_back),
            );
        println!(
            "VERYL_TIERED_STATS test={} compiled_dispatches={} fallback_dispatches={}",
            options.test, compiled, fallback
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
