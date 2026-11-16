//! Running the testbench of a design on each backend.
use super::*;

/// What running a testbench on a backend returns: the result, the ticks it
/// took and whether they reached the limit, the compile and execution
/// times (wall, generated code, CPU), and what tiered execution reports.
pub(super) type Outcome = (
    TestResult,
    u64,
    bool,
    Duration,
    Duration,
    Option<Duration>,
    Option<Duration>,
    Option<TieredExecutionStats>,
    Option<String>,
    Option<TieredExecutionTiming>,
);

/// Run the testbench on the native backend: from the image the run was
/// given, through the build cache, or compiled here.
#[cfg(any(
    all(target_arch = "x86_64", not(feature = "arm64-codegen")),
    all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
))]
pub(super) fn run_native(
    opts: &Options,
    builder: celox::SimulatorBuilder<'_, Simulator>,
    native_image: &mut Option<celox::NativeProgramImage>,
    build_cache: &Option<build_cache::BuildCache>,
    compile_start: Instant,
) -> Result<Outcome, RunError> {
    let mut sim = if let Some(image) = native_image.take() {
        if let Some(image_path) = &opts.native_image_input {
            report!(
                opts.report,
                "CELOX_NATIVE_IMAGE test={} mode=loaded path={}",
                opts.test,
                image_path.display()
            );
        }
        builder.build_native_from_image(image)?
    } else if let Some(cache) = build_cache {
        let compiled = builder.compile_native()?;
        cache.store(compiled.program_image());
        compiled.initialize()?
    } else {
        builder.build_native()?
    };
    super::super::report_warnings(sim.warnings())
        .map_err(|message| RunError::TestFailed { message })?;
    let testbench = compile_initial_testbench(&sim).ok_or(RunError::MissingInitialBlock)?;
    let compile_elapsed = compile_start.elapsed();
    if opts.report {
        sim.start_native_execution_timing();
    }
    let execute_cpu_start = process_cpu_time();
    let execute_start = Instant::now();
    let (result, ticks, tick_limit_reached) = if let Some(limit) = opts.tick_limit {
        let limited = run_compiled_testbench_with_tick_limit(&mut sim, &testbench, limit);
        (limited.result, limited.ticks, limited.tick_limit_reached)
    } else {
        (run_compiled_testbench(&mut sim, &testbench), 0, false)
    };
    sim.flush_vcd()?;
    write_settle_profile(
        opts.settle_profile.as_deref(),
        sim.layout(),
        sim.settle_profile(),
    )?;
    let jit_execute_elapsed = sim
        .finish_native_execution_timing()
        .map(|timing| timing.elapsed());
    Ok((
        result,
        ticks,
        tick_limit_reached,
        compile_elapsed,
        execute_start.elapsed(),
        jit_execute_elapsed,
        process_cpu_time()
            .zip(execute_cpu_start)
            .map(|(end, start)| end.saturating_sub(start)),
        None,
        None,
        None,
    ))
}

/// Run the testbench on the Cranelift backend.
#[cfg(feature = "cranelift")]
pub(super) fn run_cranelift(
    opts: &Options,
    builder: celox::SimulatorBuilder<'_, Simulator>,
    compile_start: Instant,
) -> Result<Outcome, RunError> {
    let mut sim = builder.build_cranelift()?;
    super::super::report_warnings(sim.warnings())
        .map_err(|message| RunError::TestFailed { message })?;
    let testbench = compile_initial_testbench(&sim).ok_or(RunError::MissingInitialBlock)?;
    let compile_elapsed = compile_start.elapsed();
    let execute_cpu_start = process_cpu_time();
    let execute_start = Instant::now();
    let (result, ticks, tick_limit_reached) = if let Some(limit) = opts.tick_limit {
        let limited = run_compiled_testbench_with_tick_limit(&mut sim, &testbench, limit);
        (limited.result, limited.ticks, limited.tick_limit_reached)
    } else {
        (run_compiled_testbench(&mut sim, &testbench), 0, false)
    };
    sim.flush_vcd()?;
    write_settle_profile(
        opts.settle_profile.as_deref(),
        sim.layout(),
        sim.settle_profile(),
    )?;
    Ok((
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
    ))
}

/// Run the testbench on the interpreter.
pub(super) fn run_interpreter(
    opts: &Options,
    builder: celox::SimulatorBuilder<'_, Simulator>,
    compile_start: Instant,
) -> Result<Outcome, RunError> {
    let mut sim = builder.build_interpreter()?;
    super::super::report_warnings(sim.warnings())
        .map_err(|message| RunError::TestFailed { message })?;
    let testbench = compile_initial_testbench(&sim).ok_or(RunError::MissingInitialBlock)?;
    let compile_elapsed = compile_start.elapsed();
    let execute_cpu_start = process_cpu_time();
    let execute_start = Instant::now();
    let (result, ticks, tick_limit_reached) = if let Some(limit) = opts.tick_limit {
        let limited = run_compiled_testbench_with_tick_limit(&mut sim, &testbench, limit);
        (limited.result, limited.ticks, limited.tick_limit_reached)
    } else {
        (run_compiled_testbench(&mut sim, &testbench), 0, false)
    };
    sim.flush_vcd()?;
    write_settle_profile(
        opts.settle_profile.as_deref(),
        sim.layout(),
        sim.settle_profile(),
    )?;
    Ok((
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
    ))
}

/// Run the testbench on the WebAssembly backend.
#[cfg(feature = "wasm-runtime")]
pub(super) fn run_wasm(
    opts: &Options,
    builder: celox::SimulatorBuilder<'_, Simulator>,
    compile_start: Instant,
) -> Result<Outcome, RunError> {
    let mut sim = builder.build_wasm()?;
    super::super::report_warnings(sim.warnings())
        .map_err(|message| RunError::TestFailed { message })?;
    let testbench = compile_initial_testbench(&sim).ok_or(RunError::MissingInitialBlock)?;
    let compile_elapsed = compile_start.elapsed();
    let execute_cpu_start = process_cpu_time();
    let execute_start = Instant::now();
    let (result, ticks, tick_limit_reached) = if let Some(limit) = opts.tick_limit {
        let limited = run_compiled_testbench_with_tick_limit(&mut sim, &testbench, limit);
        (limited.result, limited.ticks, limited.tick_limit_reached)
    } else {
        (run_compiled_testbench(&mut sim, &testbench), 0, false)
    };
    sim.flush_vcd()?;
    write_settle_profile(
        opts.settle_profile.as_deref(),
        sim.layout(),
        sim.settle_profile(),
    )?;
    Ok((
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
    ))
}

/// Run the testbench on the tiered backend. `generated_code_exercised`
/// reports whether the run reached generated code.
pub(super) fn run_tiered(
    opts: &Options,
    builder: celox::SimulatorBuilder<'_, Simulator>,
    compile_start: Instant,
    generated_code_exercised: &mut bool,
) -> Result<Outcome, RunError> {
    let mut sim = builder.build_tiered()?;
    super::super::report_warnings(sim.warnings())
        .map_err(|message| RunError::TestFailed { message })?;
    let testbench = compile_initial_testbench(&sim).ok_or(RunError::MissingInitialBlock)?;
    // `build_tiered` returns after constructing the interpreter and
    // starting code generation. The execution interval therefore
    // includes the background compile and promotion, which is the
    // end-to-end latency tiered execution is intended to improve.
    let compile_elapsed = compile_start.elapsed();
    if opts.report {
        sim.start_tiered_execution_timing();
    }
    let execute_cpu_start = process_cpu_time();
    let execute_start = Instant::now();
    let (result, ticks, tick_limit_reached) = if let Some(limit) = opts.tick_limit {
        let limited = run_compiled_testbench_with_tick_limit(&mut sim, &testbench, limit);
        (limited.result, limited.ticks, limited.tick_limit_reached)
    } else {
        (run_compiled_testbench(&mut sim, &testbench), 0, false)
    };
    let execute_elapsed = execute_start.elapsed();
    sim.flush_vcd()?;
    write_settle_profile(
        opts.settle_profile.as_deref(),
        sim.layout(),
        sim.settle_profile(),
    )?;
    let timing = sim.finish_tiered_execution_timing();
    let stats = sim.tiered_execution_stats();
    *generated_code_exercised = stats.tier == TieredExecutionTier::Compiled
        && stats.promotion == TieredPromotionStatus::Promoted
        && stats.compiled_evaluations > 0
        && timing
            .and_then(TieredExecutionTiming::promotion_elapsed)
            .is_some();
    #[cfg(any(
        all(target_arch = "x86_64", not(feature = "arm64-codegen")),
        all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
    ))]
    if let Some(hotspot) = sim.hotspot_stats() {
        *generated_code_exercised = hotspot.compiled_calls > 0 || hotspot.compiled_block_calls > 0;
        if opts.report {
            print_hotspot_stats(&opts.test, &hotspot);
        }
    }
    let promotion_error = sim.promotion_error().map(ToString::to_string);
    Ok((
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
        timing,
    ))
}
