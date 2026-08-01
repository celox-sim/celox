use std::{
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use celox::testbench::{
    compile_initial_testbench, run_compiled_testbench, run_compiled_testbench_with_tick_limit,
};
use celox::{OptLevel, OptimizeOptions, Simulator, SirPass, TestResult};
use veryl_metadata::Metadata;

struct Options {
    project: PathBuf,
    test: String,
    source_files: Vec<PathBuf>,
    opt_level: OptLevel,
    backend: Backend,
    four_state: bool,
    compile_only: bool,
    tick_limit: Option<u64>,
    dump_ir_dir: Option<PathBuf>,
    dump_ir_and_run: bool,
    native_profile_blocks: Vec<celox::NativeProfileBlock>,
    pass_overrides: Vec<(bool, SirPass)>,
    native_memory_width: usize,
    x86_slp: bool,
}

#[derive(Clone, Copy)]
enum Backend {
    Native,
    Cranelift,
}

impl Backend {
    fn as_str(self) -> &'static str {
        match self {
            Backend::Native => "native",
            Backend::Cranelift => "cranelift",
        }
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let opts = parse_args().map_err(|e| format!("{e}\n\n{}", usage()))?;
    if !opts.native_profile_blocks.is_empty() && opts.dump_ir_dir.is_none() {
        return Err("--native-profile-block requires --dump-ir-dir".into());
    }
    if opts.dump_ir_and_run && opts.dump_ir_dir.is_none() {
        return Err("--dump-ir-and-run requires --dump-ir-dir".into());
    }
    let (sources, metadata) = load_sources(&opts.project, &opts.source_files)?;
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
    let mut builder = Simulator::from_sources(source_refs, &opts.test)
        .with_metadata(metadata)
        .opt_level(opts.opt_level)
        .optimize_options(optimize_options)
        .x86_slp(opts.x86_slp)
        .four_state(opts.four_state);
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
        if matches!(opts.backend, Backend::Cranelift) {
            return Err("--dump-ir-dir is only supported with --backend native".into());
        }
        fs::create_dir_all(&output_dir)?;
        let trace_result = builder
            .trace_pre_optimized_sir()
            .trace_post_optimized_sir()
            .trace_mir()
            .build_with_trace();
        let celox::CompilationTraceResult { res, mut trace } = trace_result;
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
        let mut sim = res.map_err(|error| format!("Celox build failed: {error:?}"))?;
        let _pre_optimized_sir =
            pre_optimized_sir.ok_or("pre-optimized SIR trace was not captured")?;
        let _sir = sir.ok_or("post-optimized SIR trace was not captured")?;
        let native_sir = trace
            .native_optimized_sir
            .take()
            .ok_or("native optimized SIR trace was not captured")?;
        let mir = trace.mir.take().ok_or("MIR trace was not captured")?;
        let reactive_graph = trace
            .reactive_event_graph
            .take()
            .ok_or("reactive event graph was not captured")?;
        let state_layout = trace
            .native_state_layout
            .take()
            .ok_or("native state-layout analysis was not captured")?;
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
            let testbench = compile_initial_testbench(&sim)
                .ok_or("no initial block found — this module is not a native testbench")?;
            let execute_cpu_start = process_cpu_time();
            let execute_start = Instant::now();
            let (result, ticks, tick_limit_reached) = if let Some(limit) = opts.tick_limit {
                let limited = run_compiled_testbench_with_tick_limit(&mut sim, &testbench, limit);
                (limited.result, limited.ticks, limited.tick_limit_reached)
            } else {
                (run_compiled_testbench(&mut sim, &testbench), 0, false)
            };
            let execute_elapsed = execute_start.elapsed();
            let execute_cpu_elapsed = process_cpu_time()
                .zip(execute_cpu_start)
                .map(|(end, start)| end.saturating_sub(start));
            if let Some(execute_cpu_elapsed) = execute_cpu_elapsed {
                println!(
                    "CELOX_TEST_TIMING test={} compile_ns={} execute_ns={} execute_cpu_ns={}",
                    opts.test,
                    compile_elapsed.as_nanos(),
                    execute_elapsed.as_nanos(),
                    execute_cpu_elapsed.as_nanos()
                );
            } else {
                println!(
                    "CELOX_TEST_TIMING test={} compile_ns={} execute_ns={}",
                    opts.test,
                    compile_elapsed.as_nanos(),
                    execute_elapsed.as_nanos()
                );
            }
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
                    Err(message.into())
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
    if opts.compile_only {
        let compile_start = Instant::now();
        match opts.backend {
            Backend::Native => {
                let _sim = builder.build_native()?;
            }
            Backend::Cranelift => {
                let _sim = builder.build_cranelift()?;
            }
        }
        let compile_elapsed = compile_start.elapsed();
        let elapsed = total_start.elapsed();
        println!(
            "CELOX_TEST_TIMING test={} compile_ns={} execute_ns=0",
            opts.test,
            compile_elapsed.as_nanos()
        );
        println!(
            "CELOX_TEST_RESULT test={} status=compile-only elapsed_ns={}",
            opts.test,
            elapsed.as_nanos()
        );
        return Ok(());
    }

    let compile_start = Instant::now();
    let (result, ticks, tick_limit_reached, compile_elapsed, execute_elapsed, execute_cpu_elapsed) =
        match opts.backend {
            Backend::Native => {
                let mut sim = builder.build_native()?;
                let testbench = compile_initial_testbench(&sim)
                    .ok_or("no initial block found — this module is not a native testbench")?;
                let compile_elapsed = compile_start.elapsed();
                let execute_cpu_start = process_cpu_time();
                let execute_start = Instant::now();
                let (result, ticks, tick_limit_reached) = if let Some(limit) = opts.tick_limit {
                    let limited =
                        run_compiled_testbench_with_tick_limit(&mut sim, &testbench, limit);
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
                    process_cpu_time()
                        .zip(execute_cpu_start)
                        .map(|(end, start)| end.saturating_sub(start)),
                )
            }
            Backend::Cranelift => {
                let mut sim = builder.build_cranelift()?;
                let testbench = compile_initial_testbench(&sim)
                    .ok_or("no initial block found — this module is not a native testbench")?;
                let compile_elapsed = compile_start.elapsed();
                let execute_cpu_start = process_cpu_time();
                let execute_start = Instant::now();
                let (result, ticks, tick_limit_reached) = if let Some(limit) = opts.tick_limit {
                    let limited =
                        run_compiled_testbench_with_tick_limit(&mut sim, &testbench, limit);
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
                    process_cpu_time()
                        .zip(execute_cpu_start)
                        .map(|(end, start)| end.saturating_sub(start)),
                )
            }
        };
    let elapsed = total_start.elapsed();
    if let Some(execute_cpu_elapsed) = execute_cpu_elapsed {
        println!(
            "CELOX_TEST_TIMING test={} compile_ns={} execute_ns={} execute_cpu_ns={}",
            opts.test,
            compile_elapsed.as_nanos(),
            execute_elapsed.as_nanos(),
            execute_cpu_elapsed.as_nanos()
        );
    } else {
        println!(
            "CELOX_TEST_TIMING test={} compile_ns={} execute_ns={}",
            opts.test,
            compile_elapsed.as_nanos(),
            execute_elapsed.as_nanos()
        );
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
            Err(message.into())
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

fn parse_args() -> Result<Options, String> {
    let mut project = None;
    let mut test = None;
    let mut source_files = Vec::new();
    let mut opt_level = OptLevel::O2;
    let mut backend = Backend::Native;
    let mut four_state = false;
    let mut compile_only = false;
    let mut tick_limit = None;
    let mut dump_ir_dir = None;
    let mut dump_ir_and_run = false;
    let mut native_profile_blocks = Vec::new();
    let mut pass_overrides = Vec::new();
    let mut native_memory_width = if cfg!(target_arch = "x86_64") {
        128
    } else {
        64
    };
    let mut x86_slp = cfg!(target_arch = "x86_64");
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
            "--backend" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--backend requires native or cranelift".to_string())?;
                backend = parse_backend(&value)?;
            }
            "--source-file" => {
                source_files.push(PathBuf::from(
                    args.next()
                        .ok_or_else(|| "--source-file requires a path".to_string())?,
                ));
            }
            "--sir-pass" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--sir-pass requires +name or -name".to_string())?;
                pass_overrides.push(parse_pass_override(&value)?);
            }
            "--four-state" => four_state = true,
            "--compile-only" => compile_only = true,
            "--tick-limit" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--tick-limit requires a positive integer".to_string())?;
                let value = value
                    .parse::<u64>()
                    .map_err(|_| format!("invalid tick limit: {value}"))?;
                if value == 0 {
                    return Err("--tick-limit must be greater than zero".to_string());
                }
                tick_limit = Some(value);
            }
            "--dump-ir-dir" => {
                dump_ir_dir =
                    Some(PathBuf::from(args.next().ok_or_else(|| {
                        "--dump-ir-dir requires a directory".to_string()
                    })?));
            }
            "--dump-ir-and-run" => dump_ir_and_run = true,
            "--native-profile-block" => {
                let value = args.next().ok_or_else(|| {
                    "--native-profile-block requires FUNCTION:BLOCK:SAMPLES".to_string()
                })?;
                native_profile_blocks.push(parse_native_profile_block(&value)?);
            }
            "--native-memory-width" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--native-memory-width requires 64 or 128".to_string())?;
                native_memory_width = match value.as_str() {
                    "64" => 64,
                    "128" => 128,
                    _ => return Err(format!("invalid native memory width: {value}")),
                };
            }
            "--x86-slp" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--x86-slp requires on or off".to_string())?;
                x86_slp = match value.as_str() {
                    "on" => true,
                    "off" => false,
                    _ => return Err(format!("invalid x86 SLP setting: {value}")),
                };
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
        opt_level,
        backend,
        four_state,
        compile_only,
        tick_limit,
        dump_ir_dir,
        dump_ir_and_run,
        native_profile_blocks,
        pass_overrides,
        native_memory_width,
        x86_slp,
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

fn parse_backend(value: &str) -> Result<Backend, String> {
    match value {
        "native" => Ok(Backend::Native),
        "cranelift" => Ok(Backend::Cranelift),
        _ => Err(format!("invalid backend: {value}")),
    }
}

fn parse_native_profile_block(value: &str) -> Result<celox::NativeProfileBlock, String> {
    let (function_and_block, samples) = value
        .rsplit_once(':')
        .ok_or_else(|| format!("invalid native profile block: {value}"))?;
    let (function, block) = function_and_block
        .rsplit_once(':')
        .ok_or_else(|| format!("invalid native profile block: {value}"))?;
    let block = block.strip_prefix("bb").unwrap_or(block);
    let block = block
        .parse::<u32>()
        .map_err(|_| format!("invalid block number in native profile block: {value}"))?;
    let samples = samples
        .parse::<u64>()
        .map_err(|_| format!("invalid sample count in native profile block: {value}"))?;
    if function.is_empty() || samples == 0 {
        return Err(format!("invalid native profile block: {value}"));
    }
    Ok(celox::NativeProfileBlock {
        function: function.to_string(),
        block,
        samples,
    })
}

fn parse_pass_override(value: &str) -> Result<(bool, SirPass), String> {
    let (enable, name) = if let Some(name) = value.strip_prefix('+') {
        (true, name)
    } else if let Some(name) = value.strip_prefix('-') {
        (false, name)
    } else {
        return Err(format!("invalid pass override: {value}"));
    };
    let pass = SirPass::parse(name).ok_or_else(|| format!("unknown SIR pass: {name}"))?;
    Ok((enable, pass))
}

fn usage() -> &'static str {
    "usage: cargo run -p celox --example run_veryl_project_test -- --project <dir> --test <module> [--source-file <path> ...] [--backend native|cranelift] [--opt-level O2] [--sir-pass +/-name ...] [--native-memory-width 64|128] [--x86-slp on|off] [--four-state] [--compile-only] [--tick-limit N] [--dump-ir-dir <dir>] [--dump-ir-and-run] [--native-profile-block FUNCTION:BLOCK:SAMPLES ...]"
}

fn load_sources(
    project_path: &Path,
    source_files: &[PathBuf],
) -> Result<(Vec<(String, PathBuf)>, Metadata), Box<dyn Error>> {
    let toml_path = Metadata::search_from(project_path)?;
    let mut metadata = Metadata::load(&toml_path)?;
    let paths: Vec<PathBuf> = if source_files.is_empty() {
        metadata
            .paths::<&str>(&[], false, false)?
            .into_iter()
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
