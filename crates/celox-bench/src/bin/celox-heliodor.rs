#![allow(clippy::disallowed_macros)] // CLI errors and progress intentionally use stderr

use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use celox::testbench::{
    compile_initial_testbench, run_compiled_testbench, run_compiled_testbench_with_tick_limit,
};
use celox::{
    OptLevel, OptimizeOptions, Simulator, SirPass, TestResult, TieredExecutionStats,
    TieredExecutionTier, TieredPromotionStatus,
};
use clap::{Parser, ValueEnum};
use veryl_metadata::Metadata;

#[derive(Parser)]
#[command(about = "Run a Heliodor test with Celox and report split benchmark timing")]
struct Cli {
    #[arg(long)]
    project: PathBuf,
    #[arg(long)]
    test: String,
    #[arg(long = "source-file")]
    source_files: Vec<PathBuf>,
    #[arg(long, value_enum, ignore_case = true, default_value = "o2")]
    opt_level: OptimizationLevel,
    #[arg(long, value_enum, default_value = "native")]
    backend: Backend,
    #[arg(long)]
    four_state: bool,
    #[arg(long)]
    compile_only: bool,
    /// Write a pointer-free native image during compile-only mode.
    #[arg(long, value_name = "PATH")]
    native_image_output: Option<PathBuf>,
    /// Load a pointer-free native image instead of generating machine code.
    #[arg(long, value_name = "PATH")]
    native_image_input: Option<PathBuf>,
    #[arg(long, value_parser = parse_positive_u64)]
    tick_limit: Option<u64>,
    #[arg(long)]
    dump_ir_dir: Option<PathBuf>,
    #[arg(long)]
    dump_ir_and_run: bool,
    #[arg(long = "native-profile-block", value_parser = parse_native_profile_block)]
    native_profile_blocks: Vec<celox::NativeProfileBlock>,
    #[arg(long = "sir-pass", value_parser = parse_pass_override)]
    pass_overrides: Vec<(bool, SirPass)>,
    #[arg(long, value_parser = parse_native_memory_width)]
    native_memory_width: Option<usize>,
    #[arg(long, value_enum)]
    x86_slp: Option<OnOff>,
}

struct Options {
    project: PathBuf,
    test: String,
    source_files: Vec<PathBuf>,
    opt_level: OptLevel,
    backend: Backend,
    four_state: bool,
    compile_only: bool,
    native_image_output: Option<PathBuf>,
    native_image_input: Option<PathBuf>,
    tick_limit: Option<u64>,
    dump_ir_dir: Option<PathBuf>,
    dump_ir_and_run: bool,
    native_profile_blocks: Vec<celox::NativeProfileBlock>,
    pass_overrides: Vec<(bool, SirPass)>,
    native_memory_width: usize,
    x86_slp: bool,
}

#[derive(Debug, thiserror::Error)]
enum CeloxHeliodorError {
    #[error(transparent)]
    Metadata(#[from] veryl_metadata::MetadataError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("Celox build failed: {source:?}")]
    Build {
        #[from]
        #[source]
        source: celox::SimulatorError,
    },
    #[cfg(any(
        target_arch = "x86_64",
        feature = "arm64-codegen",
        target_arch = "aarch64"
    ))]
    #[error("native image container failed: {0}")]
    NativeImage(#[from] celox::NativeImageContainerError),
    #[error("{message}")]
    InvalidConfiguration { message: &'static str },
    #[error("{artifact} trace was not captured")]
    MissingTrace { artifact: &'static str },
    #[error("no initial block found — this module is not a native testbench")]
    MissingInitialBlock,
    #[error("{message}")]
    TestFailed { message: String },
}

#[derive(Clone, Copy, ValueEnum)]
enum Backend {
    Native,
    Cranelift,
    Interpreter,
    Tiered,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum OptimizationLevel {
    O0,
    O1,
    O2,
}

impl From<OptimizationLevel> for OptLevel {
    fn from(value: OptimizationLevel) -> Self {
        match value {
            OptimizationLevel::O0 => Self::O0,
            OptimizationLevel::O1 => Self::O1,
            OptimizationLevel::O2 => Self::O2,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum OnOff {
    On,
    Off,
}

impl Backend {
    fn as_str(self) -> &'static str {
        match self {
            Backend::Native => "native",
            Backend::Cranelift => "cranelift",
            Backend::Interpreter => "interpreter",
            Backend::Tiered => "tiered",
        }
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), CeloxHeliodorError> {
    let cli = Cli::parse();
    let opts = Options {
        project: cli.project,
        test: cli.test,
        source_files: cli.source_files,
        opt_level: cli.opt_level.into(),
        backend: cli.backend,
        four_state: cli.four_state,
        compile_only: cli.compile_only,
        native_image_output: cli.native_image_output,
        native_image_input: cli.native_image_input,
        tick_limit: cli.tick_limit,
        dump_ir_dir: cli.dump_ir_dir,
        dump_ir_and_run: cli.dump_ir_and_run,
        native_profile_blocks: cli.native_profile_blocks,
        pass_overrides: cli.pass_overrides,
        native_memory_width: cli.native_memory_width.unwrap_or({
            if cfg!(any(
                feature = "x86_64-codegen",
                all(target_arch = "x86_64", not(feature = "arm64-codegen"))
            )) {
                128
            } else {
                64
            }
        }),
        x86_slp: cli
            .x86_slp
            .map(|value| matches!(value, OnOff::On))
            .unwrap_or(cfg!(any(
                feature = "x86_64-codegen",
                all(target_arch = "x86_64", not(feature = "arm64-codegen"))
            ))),
    };
    if opts.native_image_output.is_some() && !opts.compile_only {
        return Err(CeloxHeliodorError::InvalidConfiguration {
            message: "--native-image-output requires --compile-only",
        });
    }
    if opts.compile_only && matches!(opts.backend, Backend::Interpreter | Backend::Tiered) {
        return Err(CeloxHeliodorError::InvalidConfiguration {
            message: "--compile-only requires a compiled backend",
        });
    }
    if opts.native_image_input.is_some() && opts.compile_only {
        return Err(CeloxHeliodorError::InvalidConfiguration {
            message: "--native-image-input cannot be used with --compile-only",
        });
    }
    if opts.native_image_input.is_some() && opts.native_image_output.is_some() {
        return Err(CeloxHeliodorError::InvalidConfiguration {
            message: "--native-image-input and --native-image-output are mutually exclusive",
        });
    }
    if (opts.native_image_input.is_some() || opts.native_image_output.is_some())
        && !matches!(opts.backend, Backend::Native)
    {
        return Err(CeloxHeliodorError::InvalidConfiguration {
            message: "native image options require --backend native",
        });
    }
    if opts.native_image_input.is_some() && opts.dump_ir_dir.is_some() {
        return Err(CeloxHeliodorError::InvalidConfiguration {
            message: "--native-image-input cannot be combined with --dump-ir-dir",
        });
    }
    if !opts.native_profile_blocks.is_empty() && opts.dump_ir_dir.is_none() {
        return Err(CeloxHeliodorError::InvalidConfiguration {
            message: "--native-profile-block requires --dump-ir-dir",
        });
    }
    if opts.dump_ir_and_run && opts.dump_ir_dir.is_none() {
        return Err(CeloxHeliodorError::InvalidConfiguration {
            message: "--dump-ir-and-run requires --dump-ir-dir",
        });
    }
    #[cfg(any(
        all(target_arch = "x86_64", feature = "arm64-codegen"),
        all(target_arch = "aarch64", feature = "x86_64-codegen")
    ))]
    if opts.dump_ir_and_run {
        return Err(CeloxHeliodorError::InvalidConfiguration {
            message: "--dump-ir-and-run is unavailable during cross-codegen",
        });
    }
    #[cfg(all(target_arch = "x86_64", feature = "arm64-codegen"))]
    if matches!(opts.backend, Backend::Native) && !opts.compile_only {
        return Err(CeloxHeliodorError::InvalidConfiguration {
            message: "AArch64 cross-codegen requires --compile-only on an x86-64 host",
        });
    }
    #[cfg(all(target_arch = "aarch64", feature = "x86_64-codegen"))]
    if matches!(opts.backend, Backend::Native) && !opts.compile_only {
        return Err(CeloxHeliodorError::InvalidConfiguration {
            message: "x86-64 cross-codegen requires --compile-only on an AArch64 host",
        });
    }
    #[cfg(not(any(
        target_arch = "x86_64",
        feature = "arm64-codegen",
        target_arch = "aarch64"
    )))]
    if matches!(opts.backend, Backend::Native) {
        return Err(CeloxHeliodorError::InvalidConfiguration {
            message: "the custom native backend is unavailable on this target",
        });
    }
    #[cfg(any(
        target_arch = "x86_64",
        feature = "arm64-codegen",
        target_arch = "aarch64"
    ))]
    #[cfg_attr(
        any(
            all(target_arch = "x86_64", feature = "arm64-codegen"),
            all(target_arch = "aarch64", feature = "x86_64-codegen")
        ),
        allow(unused_mut)
    )]
    let mut native_image = match &opts.native_image_input {
        Some(path) => Some(celox::NativeProgramImage::from_container_bytes(&fs::read(
            path,
        )?)?),
        None => None,
    };
    #[cfg(not(any(
        target_arch = "x86_64",
        feature = "arm64-codegen",
        target_arch = "aarch64"
    )))]
    let native_image: Option<()> = None;
    let (sources, metadata): (Vec<(String, PathBuf)>, Option<Metadata>) = if native_image.is_some()
    {
        // The image contains the target-specific machine code and all
        // source-independent runtime/testbench metadata needed to execute
        // it. The execution side therefore does not need the project
        // sources at all.
        (Vec::new(), None)
    } else {
        let (sources, metadata) = load_sources(&opts.project, &opts.source_files)?;
        (sources, Some(metadata))
    };
    let source_refs: Vec<(&str, &Path)> = sources
        .iter()
        .map(|(source, path)| (source.as_str(), path.as_path()))
        .collect();
    println!(
        "CELOX_TEST_CONFIG test={} backend={} opt_level={} four_state={} compile_only={}",
        opts.test,
        opts.backend.as_str(),
        opts.opt_level.as_str(),
        opts.four_state,
        opts.compile_only
    );

    let total_start = Instant::now();
    let optimize_options =
        OptimizeOptions::new(opts.opt_level).with_max_native_memory_width(opts.native_memory_width);
    let mut builder = Simulator::from_sources(source_refs, &opts.test);
    if let Some(metadata) = metadata {
        builder = builder.with_metadata(metadata);
    }
    let mut builder = builder
        .opt_level(opts.opt_level)
        .optimize_options(optimize_options)
        .x86_slp(opts.x86_slp)
        .four_state(opts.four_state)
        .diagnostics_from_env();
    for block in &opts.native_profile_blocks {
        builder = builder.trace_native_profile_block(&block.function, block.block, block.samples);
    }
    for (enable, pass) in opts.pass_overrides {
        builder = if enable {
            builder.enable_pass(pass)
        } else {
            builder.disable_pass(pass)
        };
    }
    if let Some(output_dir) = opts.dump_ir_dir {
        if !matches!(opts.backend, Backend::Native) {
            return Err(CeloxHeliodorError::InvalidConfiguration {
                message: "--dump-ir-dir is only supported with --backend native",
            });
        }
        #[cfg(not(any(
            target_arch = "x86_64",
            feature = "arm64-codegen",
            target_arch = "aarch64"
        )))]
        unreachable!("native backend availability checked above");
        #[cfg(any(
            target_arch = "x86_64",
            feature = "arm64-codegen",
            target_arch = "aarch64"
        ))]
        {
            fs::create_dir_all(&output_dir)?;
            let (compiled, mut trace) = builder
                .trace_pre_optimized_sir()
                .trace_post_optimized_sir()
                .trace_mir()
                .compile_native_with_trace()?;
            if let Some(output_path) = &opts.native_image_output {
                if let Some(parent) = output_path
                    .parent()
                    .filter(|path| !path.as_os_str().is_empty())
                {
                    fs::create_dir_all(parent)?;
                }
                compiled.write_image(output_path)?;
                println!(
                    "CELOX_NATIVE_IMAGE test={} mode=generated path={}",
                    opts.test,
                    output_path.display()
                );
            }
            let compile_elapsed = total_start.elapsed();
            let pre_sir_path = output_dir.join("pre_optimized.sir");
            let sir_path = output_dir.join("post_optimized.sir");
            let pre_optimized_sir = trace.format_pre_optimized_sir();
            let sir = trace.format_post_optimized_sir();
            if let Some(pre_optimized_sir) = &pre_optimized_sir {
                fs::write(&pre_sir_path, pre_optimized_sir)?;
                eprintln!(
                    "wrote pre-optimized SIR ({} bytes) to {}",
                    pre_optimized_sir.len(),
                    pre_sir_path.display()
                );
            }
            if let Some(sir) = &sir {
                fs::write(&sir_path, sir)?;
                eprintln!(
                    "wrote post-optimized SIR ({} bytes) to {}",
                    sir.len(),
                    sir_path.display()
                );
            }
            let _pre_optimized_sir = pre_optimized_sir.ok_or(CeloxHeliodorError::MissingTrace {
                artifact: "pre-optimized SIR",
            })?;
            let _sir = sir.ok_or(CeloxHeliodorError::MissingTrace {
                artifact: "post-optimized SIR",
            })?;
            let native_sir =
                trace
                    .native_optimized_sir
                    .take()
                    .ok_or(CeloxHeliodorError::MissingTrace {
                        artifact: "native optimized SIR",
                    })?;
            let mir = trace
                .mir
                .take()
                .ok_or(CeloxHeliodorError::MissingTrace { artifact: "MIR" })?;
            let reactive_graph =
                trace
                    .reactive_event_graph
                    .take()
                    .ok_or(CeloxHeliodorError::MissingTrace {
                        artifact: "reactive event graph",
                    })?;
            let state_layout =
                trace
                    .native_state_layout
                    .take()
                    .ok_or(CeloxHeliodorError::MissingTrace {
                        artifact: "native state-layout analysis",
                    })?;
            let native_sir_path = output_dir.join("native_optimized.sir");
            let mir_path = output_dir.join("mir.txt");
            let reactive_graph_path = output_dir.join("reactive_event_graph.txt");
            let state_layout_path = output_dir.join("native_state_layout.txt");
            fs::write(&native_sir_path, &native_sir)?;
            fs::write(&mir_path, &mir)?;
            fs::write(&reactive_graph_path, &reactive_graph)?;
            fs::write(&state_layout_path, &state_layout)?;
            eprintln!(
                "wrote native optimized SIR ({} bytes) to {}",
                native_sir.len(),
                native_sir_path.display()
            );
            eprintln!(
                "wrote full native MIR ({} bytes) to {}",
                mir.len(),
                mir_path.display()
            );
            eprintln!(
                "wrote reactive event graph ({} bytes) to {}",
                reactive_graph.len(),
                reactive_graph_path.display()
            );
            eprintln!(
                "wrote native state-layout analysis ({} bytes) to {}",
                state_layout.len(),
                state_layout_path.display()
            );
            // The trace owns complete SIR programs and the formatted dumps are
            // hundreds of MiB on Heliodor. They are not part of the generated JIT
            // state; release them before timing that exact Simulator instance.
            drop((
                _pre_optimized_sir,
                _sir,
                native_sir,
                mir,
                reactive_graph,
                state_layout,
                trace,
            ));
            if opts.dump_ir_and_run {
                let mut sim = compiled.initialize()?;
                let testbench = compile_initial_testbench(&sim)
                    .ok_or(CeloxHeliodorError::MissingInitialBlock)?;
                #[cfg(any(
                    all(target_arch = "x86_64", not(feature = "arm64-codegen")),
                    all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
                ))]
                sim.start_native_execution_timing();
                let execute_cpu_start = process_cpu_time();
                let execute_start = Instant::now();
                let (result, ticks, tick_limit_reached) = if let Some(limit) = opts.tick_limit {
                    let limited =
                        run_compiled_testbench_with_tick_limit(&mut sim, &testbench, limit);
                    (limited.result, limited.ticks, limited.tick_limit_reached)
                } else {
                    (run_compiled_testbench(&mut sim, &testbench), 0, false)
                };
                #[cfg(any(
                    all(target_arch = "x86_64", not(feature = "arm64-codegen")),
                    all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
                ))]
                let jit_execute_elapsed = sim
                    .finish_native_execution_timing()
                    .expect("native execution timing was started")
                    .elapsed();
                #[cfg(not(any(
                    all(target_arch = "x86_64", not(feature = "arm64-codegen")),
                    all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
                )))]
                let jit_execute_elapsed = Duration::ZERO;
                let execute_elapsed = execute_start.elapsed();
                let execute_cpu_elapsed = process_cpu_time()
                    .zip(execute_cpu_start)
                    .map(|(end, start)| end.saturating_sub(start));
                print_celox_timing(
                    &opts.test,
                    compile_elapsed,
                    execute_elapsed,
                    Some(jit_execute_elapsed),
                    execute_cpu_elapsed,
                );
                if opts.tick_limit.is_some() {
                    println!(
                        "CELOX_TEST_TICK_LIMIT test={} ticks={} reached={}",
                        opts.test, ticks, tick_limit_reached
                    );
                }
                let elapsed = total_start.elapsed();
                return match result {
                    TestResult::Pass if tick_limit_reached => {
                        println!(
                            "CELOX_TEST_RESULT test={} status=tick-limit elapsed_ns={}",
                            opts.test,
                            elapsed.as_nanos()
                        );
                        Ok(())
                    }
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
                        Err(CeloxHeliodorError::TestFailed { message })
                    }
                };
            }
            println!(
                "CELOX_TEST_RESULT test={} status=trace-only elapsed_ns={}",
                opts.test,
                total_start.elapsed().as_nanos()
            );
            return Ok(());
        }
    }
    if opts.compile_only {
        let compile_start = Instant::now();
        match opts.backend {
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Backend::Native => {
                let compiled = builder.compile_native()?;
                if let Some(output_path) = &opts.native_image_output {
                    if let Some(parent) = output_path
                        .parent()
                        .filter(|path| !path.as_os_str().is_empty())
                    {
                        fs::create_dir_all(parent)?;
                    }
                    compiled.write_image(output_path)?;
                    println!(
                        "CELOX_NATIVE_IMAGE test={} mode=generated path={}",
                        opts.test,
                        output_path.display()
                    );
                }
            }
            #[cfg(not(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            )))]
            Backend::Native => unreachable!("native backend availability checked above"),
            Backend::Cranelift => {
                let _sim = builder.build_cranelift()?;
            }
            Backend::Interpreter | Backend::Tiered => {
                unreachable!("non-compiled backends were rejected above")
            }
        }
        let compile_elapsed = compile_start.elapsed();
        let elapsed = total_start.elapsed();
        print_celox_timing(
            &opts.test,
            compile_elapsed,
            Duration::ZERO,
            matches!(opts.backend, Backend::Native).then_some(Duration::ZERO),
            None,
        );
        println!(
            "CELOX_TEST_RESULT test={} status=compile-only elapsed_ns={}",
            opts.test,
            elapsed.as_nanos()
        );
        return Ok(());
    }

    let compile_start = Instant::now();
    let (
        result,
        ticks,
        tick_limit_reached,
        compile_elapsed,
        execute_elapsed,
        jit_execute_elapsed,
        execute_cpu_elapsed,
        tiered_stats,
        tiered_promotion_error,
        tiered_promotion_elapsed,
    ) = match opts.backend {
        #[cfg(any(
            all(target_arch = "x86_64", not(feature = "arm64-codegen")),
            all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
        ))]
        Backend::Native => {
            let mut sim = if let Some(image_path) = &opts.native_image_input {
                let image = native_image
                    .take()
                    .expect("native image was loaded before builder construction");
                println!(
                    "CELOX_NATIVE_IMAGE test={} mode=loaded path={}",
                    opts.test,
                    image_path.display()
                );
                builder.build_native_from_image(image)?
            } else {
                builder.build_native()?
            };
            let testbench =
                compile_initial_testbench(&sim).ok_or(CeloxHeliodorError::MissingInitialBlock)?;
            let compile_elapsed = compile_start.elapsed();
            sim.start_native_execution_timing();
            let execute_cpu_start = process_cpu_time();
            let execute_start = Instant::now();
            let (result, ticks, tick_limit_reached) = if let Some(limit) = opts.tick_limit {
                let limited = run_compiled_testbench_with_tick_limit(&mut sim, &testbench, limit);
                (limited.result, limited.ticks, limited.tick_limit_reached)
            } else {
                (run_compiled_testbench(&mut sim, &testbench), 0, false)
            };
            let jit_execute_elapsed = sim
                .finish_native_execution_timing()
                .expect("native execution timing was started")
                .elapsed();
            (
                result,
                ticks,
                tick_limit_reached,
                compile_elapsed,
                execute_start.elapsed(),
                Some(jit_execute_elapsed),
                process_cpu_time()
                    .zip(execute_cpu_start)
                    .map(|(end, start)| end.saturating_sub(start)),
                None,
                None,
                None,
            )
        }
        #[cfg(not(any(
            all(target_arch = "x86_64", not(feature = "arm64-codegen")),
            all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
        )))]
        Backend::Native => unreachable!("native backend availability checked above"),
        Backend::Cranelift => {
            let mut sim = builder.build_cranelift()?;
            let testbench =
                compile_initial_testbench(&sim).ok_or(CeloxHeliodorError::MissingInitialBlock)?;
            let compile_elapsed = compile_start.elapsed();
            let execute_cpu_start = process_cpu_time();
            let execute_start = Instant::now();
            let (result, ticks, tick_limit_reached) = if let Some(limit) = opts.tick_limit {
                let limited = run_compiled_testbench_with_tick_limit(&mut sim, &testbench, limit);
                (limited.result, limited.ticks, limited.tick_limit_reached)
            } else {
                (run_compiled_testbench(&mut sim, &testbench), 0, false)
            };
            (
                result,
                ticks,
                tick_limit_reached,
                compile_elapsed,
                execute_start.elapsed(),
                None,
                process_cpu_time()
                    .zip(execute_cpu_start)
                    .map(|(end, start)| end.saturating_sub(start)),
                None,
                None,
                None,
            )
        }
        Backend::Interpreter => {
            let mut sim = builder.build_interpreter()?;
            let testbench =
                compile_initial_testbench(&sim).ok_or(CeloxHeliodorError::MissingInitialBlock)?;
            let compile_elapsed = compile_start.elapsed();
            let execute_cpu_start = process_cpu_time();
            let execute_start = Instant::now();
            let (result, ticks, tick_limit_reached) = if let Some(limit) = opts.tick_limit {
                let limited = run_compiled_testbench_with_tick_limit(&mut sim, &testbench, limit);
                (limited.result, limited.ticks, limited.tick_limit_reached)
            } else {
                (run_compiled_testbench(&mut sim, &testbench), 0, false)
            };
            (
                result,
                ticks,
                tick_limit_reached,
                compile_elapsed,
                execute_start.elapsed(),
                None,
                process_cpu_time()
                    .zip(execute_cpu_start)
                    .map(|(end, start)| end.saturating_sub(start)),
                None,
                None,
                None,
            )
        }
        Backend::Tiered => {
            let mut sim = builder.build_tiered()?;
            let testbench =
                compile_initial_testbench(&sim).ok_or(CeloxHeliodorError::MissingInitialBlock)?;
            // `build_tiered` returns after constructing the interpreter and
            // starting code generation. The execution interval therefore
            // includes the background compile and promotion, which is the
            // end-to-end latency tiered execution is intended to improve.
            let compile_elapsed = compile_start.elapsed();
            sim.start_tiered_execution_timing();
            let execute_cpu_start = process_cpu_time();
            let execute_start = Instant::now();
            let (result, ticks, tick_limit_reached) = if let Some(limit) = opts.tick_limit {
                let limited = run_compiled_testbench_with_tick_limit(&mut sim, &testbench, limit);
                (limited.result, limited.ticks, limited.tick_limit_reached)
            } else {
                (run_compiled_testbench(&mut sim, &testbench), 0, false)
            };
            let execute_elapsed = execute_start.elapsed();
            let promotion_elapsed = sim
                .finish_tiered_execution_timing()
                .and_then(|timing| timing.promotion_elapsed());
            let stats = sim.tiered_execution_stats();
            let promotion_error = sim.promotion_error().map(ToString::to_string);
            (
                result,
                ticks,
                tick_limit_reached,
                compile_elapsed,
                execute_elapsed,
                None,
                process_cpu_time()
                    .zip(execute_cpu_start)
                    .map(|(end, start)| end.saturating_sub(start)),
                Some(stats),
                promotion_error,
                promotion_elapsed,
            )
        }
    };
    let elapsed = total_start.elapsed();
    print_celox_timing(
        &opts.test,
        compile_elapsed,
        execute_elapsed,
        jit_execute_elapsed,
        execute_cpu_elapsed,
    );
    if let Some(stats) = tiered_stats {
        print_tiered_stats(&opts.test, stats, tiered_promotion_elapsed);
        if !tick_limit_reached
            && (stats.tier != TieredExecutionTier::Compiled
                || stats.promotion != TieredPromotionStatus::Promoted
                || stats.compiled_evaluations == 0
                || tiered_promotion_elapsed.is_none())
        {
            let detail = tiered_promotion_error
                .as_deref()
                .unwrap_or("the compiled tier was not adopted and exercised");
            println!(
                "CELOX_TEST_RESULT test={} status=fail elapsed_ns={}",
                opts.test,
                elapsed.as_nanos()
            );
            return Err(CeloxHeliodorError::TestFailed {
                message: format!("tiered JIT benchmark did not exercise generated code: {detail}"),
            });
        }
    }
    if opts.tick_limit.is_some() {
        println!(
            "CELOX_TEST_TICK_LIMIT test={} ticks={} reached={}",
            opts.test, ticks, tick_limit_reached
        );
    }

    match result {
        TestResult::Pass if tick_limit_reached => {
            println!(
                "CELOX_TEST_RESULT test={} status=tick-limit elapsed_ns={}",
                opts.test,
                elapsed.as_nanos()
            );
            Ok(())
        }
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
            Err(CeloxHeliodorError::TestFailed { message })
        }
    }
}

fn print_tiered_stats(
    test: &str,
    stats: TieredExecutionStats,
    promotion_elapsed: Option<Duration>,
) {
    let tier = match stats.tier {
        TieredExecutionTier::Interpreter => "interpreter",
        TieredExecutionTier::Compiled => "compiled",
        _ => "unknown",
    };
    let promotion = match stats.promotion {
        TieredPromotionStatus::Pending => "pending",
        TieredPromotionStatus::Failed => "failed",
        TieredPromotionStatus::Disabled => "disabled",
        TieredPromotionStatus::Promoted => "promoted",
        _ => "unknown",
    };
    let promoted_after = stats
        .promoted_after_interpreted_evaluations
        .map(|value| value.to_string())
        .unwrap_or_else(|| "NA".to_string());
    let promotion_elapsed_ns = promotion_elapsed
        .map(|elapsed| elapsed.as_nanos().to_string())
        .unwrap_or_else(|| "NA".to_string());
    println!(
        "CELOX_TIERED_STATS test={test} tier={tier} promotion={promotion} interpreted_evaluations={} compiled_evaluations={} promoted_after_interpreted_evaluations={promoted_after} promotion_elapsed_ns={promotion_elapsed_ns} safe_point_polls={} split_apply_deferrals={} threshold_deferrals={}",
        stats.interpreted_evaluations,
        stats.compiled_evaluations,
        stats.safe_point_polls,
        stats.split_apply_deferrals,
        stats.threshold_deferrals,
    );
}

fn print_celox_timing(
    test: &str,
    compile_elapsed: Duration,
    execute_elapsed: Duration,
    jit_execute_elapsed: Option<Duration>,
    execute_cpu_elapsed: Option<Duration>,
) {
    let jit_execute_ns = jit_execute_elapsed
        .map(|elapsed| elapsed.as_nanos().to_string())
        .unwrap_or_else(|| "NA".to_string());
    if let Some(execute_cpu_elapsed) = execute_cpu_elapsed {
        println!(
            "CELOX_TEST_TIMING test={test} compile_ns={} execute_ns={} jit_execute_ns={jit_execute_ns} execute_cpu_ns={}",
            compile_elapsed.as_nanos(),
            execute_elapsed.as_nanos(),
            execute_cpu_elapsed.as_nanos()
        );
    } else {
        println!(
            "CELOX_TEST_TIMING test={test} compile_ns={} execute_ns={} jit_execute_ns={jit_execute_ns}",
            compile_elapsed.as_nanos(),
            execute_elapsed.as_nanos()
        );
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

#[derive(Debug, thiserror::Error)]
enum CliValueError {
    #[error("invalid positive integer: {value}")]
    InvalidPositiveInteger {
        value: String,
        #[source]
        source: std::num::ParseIntError,
    },
    #[error("value must be greater than zero")]
    NonPositiveInteger,
    #[error("invalid native memory width: {value}")]
    InvalidNativeMemoryWidth { value: String },
    #[error("invalid native profile block: {value}")]
    InvalidNativeProfileBlock { value: String },
    #[error("invalid block number in native profile block: {value}")]
    InvalidNativeProfileBlockNumber {
        value: String,
        #[source]
        source: std::num::ParseIntError,
    },
    #[error("invalid sample count in native profile block: {value}")]
    InvalidNativeProfileSampleCount {
        value: String,
        #[source]
        source: std::num::ParseIntError,
    },
    #[error("invalid pass override: {value}")]
    InvalidPassOverride { value: String },
    #[error("unknown SIR pass: {name}")]
    UnknownSirPass { name: String },
}

fn parse_positive_u64(value: &str) -> Result<u64, CliValueError> {
    let parsed = value
        .parse::<u64>()
        .map_err(|source| CliValueError::InvalidPositiveInteger {
            value: value.to_owned(),
            source,
        })?;
    if parsed == 0 {
        Err(CliValueError::NonPositiveInteger)
    } else {
        Ok(parsed)
    }
}

fn parse_native_memory_width(value: &str) -> Result<usize, CliValueError> {
    match value {
        "64" => Ok(64),
        "128" => Ok(128),
        _ => Err(CliValueError::InvalidNativeMemoryWidth {
            value: value.to_owned(),
        }),
    }
}

fn parse_native_profile_block(value: &str) -> Result<celox::NativeProfileBlock, CliValueError> {
    let (function_and_block, samples) =
        value
            .rsplit_once(':')
            .ok_or_else(|| CliValueError::InvalidNativeProfileBlock {
                value: value.to_owned(),
            })?;
    let (function, block) = function_and_block.rsplit_once(':').ok_or_else(|| {
        CliValueError::InvalidNativeProfileBlock {
            value: value.to_owned(),
        }
    })?;
    let block = block.strip_prefix("bb").unwrap_or(block);
    let block =
        block
            .parse::<u32>()
            .map_err(|source| CliValueError::InvalidNativeProfileBlockNumber {
                value: value.to_owned(),
                source,
            })?;
    let samples = samples.parse::<u64>().map_err(|source| {
        CliValueError::InvalidNativeProfileSampleCount {
            value: value.to_owned(),
            source,
        }
    })?;
    if function.is_empty() || samples == 0 {
        return Err(CliValueError::InvalidNativeProfileBlock {
            value: value.to_owned(),
        });
    }
    Ok(celox::NativeProfileBlock {
        function: function.to_string(),
        block,
        samples,
    })
}

fn parse_pass_override(value: &str) -> Result<(bool, SirPass), CliValueError> {
    let (enable, name) = if let Some(name) = value.strip_prefix('+') {
        (true, name)
    } else if let Some(name) = value.strip_prefix('-') {
        (false, name)
    } else {
        return Err(CliValueError::InvalidPassOverride {
            value: value.to_owned(),
        });
    };
    let pass = SirPass::parse(name).ok_or_else(|| CliValueError::UnknownSirPass {
        name: name.to_owned(),
    })?;
    Ok((enable, pass))
}

fn load_sources(
    project_path: &Path,
    source_files: &[PathBuf],
) -> Result<(Vec<(String, PathBuf)>, Metadata), CeloxHeliodorError> {
    let toml_path = Metadata::search_from(project_path)?;
    let mut metadata = Metadata::load(&toml_path)?;
    let paths: Vec<PathBuf> = if source_files.is_empty() {
        metadata
            .paths::<&str>(&[], false, false)?
            .into_iter()
            .filter(|path| !path.example)
            .map(|path| path.src)
            .collect()
    } else {
        source_files
            .iter()
            .map(|path| {
                if path.is_absolute() {
                    path.clone()
                } else {
                    project_path.join(path)
                }
            })
            .collect()
    };
    let mut sources = Vec::with_capacity(paths.len());
    for path in paths {
        let content = fs::read_to_string(&path)?;
        sources.push((content, path));
    }
    Ok((sources, metadata))
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use clap::Parser;

    use super::{Cli, CliValueError, OptimizationLevel, parse_positive_u64};

    #[test]
    fn numeric_parser_preserves_parse_error_source() {
        let error = parse_positive_u64("not-a-number").unwrap_err();

        assert!(matches!(
            &error,
            CliValueError::InvalidPositiveInteger { .. }
        ));
        assert!(error.source().unwrap().is::<std::num::ParseIntError>());
    }

    #[test]
    fn opt_level_is_case_insensitive() {
        for (value, expected) in [
            ("O0", OptimizationLevel::O0),
            ("o0", OptimizationLevel::O0),
            ("O1", OptimizationLevel::O1),
            ("o1", OptimizationLevel::O1),
            ("O2", OptimizationLevel::O2),
            ("o2", OptimizationLevel::O2),
        ] {
            let cli = Cli::try_parse_from([
                "celox-heliodor",
                "--project",
                ".",
                "--test",
                "test_soc_linux_boot",
                "--opt-level",
                value,
            ])
            .unwrap();

            assert_eq!(cli.opt_level, expected);
        }
    }
}
