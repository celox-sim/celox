//! `--dump-ir-dir`: the traces of a native compile, written to a directory.
use super::*;

/// `--dump-ir-dir`: compile natively with every trace, write the traces to
/// `output_dir` and, with `--dump-ir-and-run`, run the testbench on what
/// was compiled.
pub(super) fn dump_native_ir(
    opts: &Options,
    builder: celox::SimulatorBuilder<'_, Simulator>,
    output_dir: PathBuf,
    project_root: &Path,
    total_start: Instant,
) -> Result<(), RunError> {
    fs::create_dir_all(&output_dir)?;
    let (compiled, mut trace) = builder
        .trace_pre_optimized_sir()
        .trace_post_optimized_sir()
        .trace_settle_plan()
        .trace_mir()
        .compile_native_with_trace()?;
    if let Some(output_path) = &opts.native_image_output {
        if let Some(parent) = output_path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        build_cache::write_relative_image(compiled.program_image(), project_root, output_path)?;
        report!(
            opts.report,
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
        let _ = writeln!(
            std::io::stderr().lock(),
            "wrote pre-optimized SIR ({} bytes) to {}",
            pre_optimized_sir.len(),
            pre_sir_path.display()
        );
    }
    if let Some(sir) = &sir {
        fs::write(&sir_path, sir)?;
        let _ = writeln!(
            std::io::stderr().lock(),
            "wrote post-optimized SIR ({} bytes) to {}",
            sir.len(),
            sir_path.display()
        );
    }
    let _pre_optimized_sir = pre_optimized_sir.ok_or(RunError::MissingTrace {
        artifact: "pre-optimized SIR",
    })?;
    let _sir = sir.ok_or(RunError::MissingTrace {
        artifact: "post-optimized SIR",
    })?;
    let native_sir = trace
        .native_optimized_sir
        .take()
        .ok_or(RunError::MissingTrace {
            artifact: "native optimized SIR",
        })?;
    let mir = trace
        .mir
        .take()
        .ok_or(RunError::MissingTrace { artifact: "MIR" })?;
    let reactive_graph = trace
        .reactive_event_graph
        .take()
        .ok_or(RunError::MissingTrace {
            artifact: "reactive event graph",
        })?;
    let state_layout = trace
        .native_state_layout
        .take()
        .ok_or(RunError::MissingTrace {
            artifact: "native state-layout analysis",
        })?;
    if let Some(plan) = trace.settle_plan.take() {
        let plan_path = output_dir.join("settle_plan.txt");
        fs::write(&plan_path, &plan)?;
        let _ = writeln!(
            std::io::stderr().lock(),
            "wrote settle plan ({} bytes) to {}",
            plan.len(),
            plan_path.display()
        );
    }
    let native_sir_path = output_dir.join("native_optimized.sir");
    let mir_path = output_dir.join("mir.txt");
    let reactive_graph_path = output_dir.join("reactive_event_graph.txt");
    let state_layout_path = output_dir.join("native_state_layout.txt");
    fs::write(&native_sir_path, &native_sir)?;
    fs::write(&mir_path, &mir)?;
    fs::write(&reactive_graph_path, &reactive_graph)?;
    fs::write(&state_layout_path, &state_layout)?;
    let _ = writeln!(
        std::io::stderr().lock(),
        "wrote native optimized SIR ({} bytes) to {}",
        native_sir.len(),
        native_sir_path.display()
    );
    let _ = writeln!(
        std::io::stderr().lock(),
        "wrote full native MIR ({} bytes) to {}",
        mir.len(),
        mir_path.display()
    );
    let _ = writeln!(
        std::io::stderr().lock(),
        "wrote reactive event graph ({} bytes) to {}",
        reactive_graph.len(),
        reactive_graph_path.display()
    );
    let _ = writeln!(
        std::io::stderr().lock(),
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
        let testbench = compile_initial_testbench(&sim).ok_or(RunError::MissingInitialBlock)?;
        #[cfg(any(
            all(target_arch = "x86_64", not(feature = "arm64-codegen")),
            all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
        ))]
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
        #[cfg(any(
            all(target_arch = "x86_64", not(feature = "arm64-codegen")),
            all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
        ))]
        let jit_execute_elapsed = sim
            .finish_native_execution_timing()
            .map(|timing| timing.elapsed());
        #[cfg(not(any(
            all(target_arch = "x86_64", not(feature = "arm64-codegen")),
            all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
        )))]
        let jit_execute_elapsed = None;
        let execute_elapsed = execute_start.elapsed();
        let execute_cpu_elapsed = process_cpu_time()
            .zip(execute_cpu_start)
            .map(|(end, start)| end.saturating_sub(start));
        print_celox_timing(
            opts.report,
            &opts.test,
            compile_elapsed,
            execute_elapsed,
            jit_execute_elapsed,
            execute_cpu_elapsed,
        );
        if opts.tick_limit.is_some() {
            report!(
                opts.report,
                "CELOX_TEST_TICK_LIMIT test={} ticks={} reached={}",
                opts.test,
                ticks,
                tick_limit_reached
            );
        }
        let elapsed = total_start.elapsed();
        return match result {
            TestResult::Pass if tick_limit_reached => {
                report!(
                    opts.report,
                    "CELOX_TEST_RESULT test={} status=tick-limit elapsed_ns={}",
                    opts.test,
                    elapsed.as_nanos()
                );
                Err(RunError::TestFailed {
                    message: format!(
                        "simulation reached --max-steps {} testbench ticks before completion",
                        opts.tick_limit.unwrap()
                    ),
                })
            }
            TestResult::Pass => {
                report!(
                    opts.report,
                    "CELOX_TEST_RESULT test={} status=pass elapsed_ns={}",
                    opts.test,
                    elapsed.as_nanos()
                );
                Ok(())
            }
            TestResult::Fail(message) => {
                report!(
                    opts.report,
                    "CELOX_TEST_RESULT test={} status=fail elapsed_ns={}",
                    opts.test,
                    elapsed.as_nanos()
                );
                Err(RunError::TestFailed { message })
            }
        };
    }
    report!(
        opts.report,
        "CELOX_TEST_RESULT test={} status=trace-only elapsed_ns={}",
        opts.test,
        total_start.elapsed().as_nanos()
    );
    Ok(())
}
