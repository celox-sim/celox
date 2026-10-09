//! Running the process kernels of a `#[test]` module.
//!
//! The `initial` blocks are compiled into process kernels (see the Veryl
//! frontend's process lowering), and the timed scheduler of
//! [`celox_runtime::SimulationState`] runs them: clock waits become fused
//! ticks, a reset wait releases the reset when it ends, and the host serves
//! the random number and component requests a kernel makes through scratch
//! state.

use super::{
    AssertionResult, CompiledTestbench, LimitedTestbenchResult, RandomTable, SourceLocation,
    TestResult, TestResultDetailed, apply_component_writes, execution_random_seed, forward_display,
    resize_component_return, root_testbench_name,
};
use crate::backend::SimBackend;
use crate::simulator::{RuntimeEvent, RuntimeFormatContext, Simulator};
use celox_design::{HostRequest, HostValue};
use num_bigint::BigUint;
use num_traits::ToPrimitive as _;

/// Whether the simulator's testbench runs as process kernels.
pub(crate) fn enabled<B: SimBackend>(sim: &Simulator<B>) -> bool {
    !sim.program.runtime_schema.processes.is_empty()
}

/// What a run of the kernels observed.
struct KernelRun {
    assertions: Vec<AssertionResult>,
    error: Option<String>,
    finished: bool,
    ticks: u64,
    tick_limit_reached: bool,
}

pub(crate) fn run_limited<B: SimBackend>(
    sim: &mut Simulator<B>,
    testbench: &CompiledTestbench<B>,
    tick_limit: Option<u64>,
    require_finish: bool,
) -> LimitedTestbenchResult {
    let run = run(sim, testbench, tick_limit);
    let failed_messages = run
        .assertions
        .iter()
        .filter(|assertion| !assertion.passed)
        .map(|assertion| {
            assertion
                .message
                .clone()
                .unwrap_or_else(|| "assertion failed".to_string())
        })
        .collect::<Vec<_>>();
    let result = match run.error {
        Some(message) => {
            if failed_messages.is_empty() {
                TestResult::Fail(message)
            } else if failed_messages.last().is_some_and(|m| m == &message) {
                TestResult::Fail(failed_messages.join("\n"))
            } else {
                let mut combined = failed_messages;
                combined.push(message);
                TestResult::Fail(combined.join("\n"))
            }
        }
        None => {
            if !failed_messages.is_empty() {
                TestResult::Fail(failed_messages.join("\n"))
            } else if require_finish && !run.finished {
                TestResult::Fail("testbench returned without reaching $finish".to_string())
            } else {
                TestResult::Pass
            }
        }
    };
    LimitedTestbenchResult {
        result,
        ticks: run.ticks,
        tick_limit_reached: run.tick_limit_reached,
    }
}

pub(crate) fn run_detailed<B: SimBackend>(
    sim: &mut Simulator<B>,
    testbench: &CompiledTestbench<B>,
) -> TestResultDetailed {
    let run = run(sim, testbench, None);
    let passed = run.error.is_none() && run.assertions.iter().all(|a| a.passed);
    TestResultDetailed {
        passed,
        assertions: run.assertions,
        error: run.error,
    }
}

fn run<B: SimBackend>(
    sim: &mut Simulator<B>,
    testbench: &CompiledTestbench<B>,
    tick_limit: Option<u64>,
) -> KernelRun {
    let failed = |message: String| KernelRun {
        assertions: Vec::new(),
        error: Some(message),
        finished: false,
        ticks: 0,
        tick_limit_reached: false,
    };
    let test_name = root_testbench_name(sim);
    let use_4state = sim.backend.layout().four_state;
    let seed = execution_random_seed(testbench.configured_random_seed());
    let initial_writes = match sim.components.initialize(
        testbench.components(),
        testbench.component_bindings(),
        testbench.component_libraries(),
        testbench.component_file_base(),
        seed,
        &test_name,
        use_4state,
        &mut sim.backend,
    ) {
        Ok(writes) => writes,
        Err(message) => return failed(message),
    };
    if let Some(writer) = sim.vcd_writer.as_mut()
        && let Err(error) = writer.add_external_signals(&sim.components.trace_descriptors())
    {
        return failed(format!("component VCD registration failed: {error}"));
    }
    apply_component_writes(sim, initial_writes);
    sim.testbench_random = Some(RandomTable::new(seed));
    // A run starts every process at its beginning, whatever an earlier run
    // of the same simulator left in the control slots.
    for slots in &sim.program.runtime_schema.processes.clone() {
        for slot in slots.iter() {
            let signal = sim.backend.resolve_signal(slot);
            sim.backend.set_wide(signal, BigUint::ZERO);
        }
    }

    // The scheduler samples the clocks it detects edges on from settled
    // state.
    if sim.dirty {
        if let Err(error) = sim.eval_comb_checked() {
            return failed(error.to_string());
        }
        sim.dirty = false;
    }
    let mut state = crate::simulation::simulation_state(sim);
    state.set_tick_budget(tick_limit);
    let mut assertions = Vec::new();
    let mut error = None;
    let mut finished = sim.components.finish_requested();
    let progress_every = sim
        .diagnostics
        .testbench_progress_every
        .filter(|every| *every != 0);
    let mut reported_ticks = 0;
    while !finished {
        let stepped = state.step(sim);
        if let Some(every) = progress_every {
            // A step may run many fused ticks; report each multiple it passed.
            let mut tick = (reported_ticks / every + 1) * every;
            while tick <= state.ticks() {
                tracing::debug!("[testbench-progress] tick={tick}");
                tick += every;
            }
            reported_ticks = state.ticks();
        }
        let drained = drain(sim, &mut assertions, state.time());
        if let Some(fatal) = drained.fatal {
            error = Some(fatal);
            finished = true;
        }
        match stepped {
            Ok(Some(_)) => {}
            Ok(None) => finished = true,
            Err(code) => {
                if error.is_none() {
                    error = Some(code.to_string());
                }
                finished = true;
            }
        }
        if drained.finished || state.is_finished() || sim.components.finish_requested() {
            finished = true;
        }
    }
    sim.testbench_random = None;
    if let Err(message) = sim.components.finish(state.time())
        && error.is_none()
    {
        error = Some(message);
    }
    let finished = state.is_finished() || sim.components.finish_requested();
    KernelRun {
        assertions,
        error,
        finished,
        ticks: state.ticks(),
        tick_limit_reached: tick_limit.is_some_and(|limit| state.ticks() >= limit)
            && !state.all_processes_done()
            && !state.is_finished(),
    }
}

struct Drained {
    fatal: Option<String>,
    finished: bool,
}

/// Record the assertions the kernels evaluated and forward their output.
fn drain<B: SimBackend>(
    sim: &mut Simulator<B>,
    assertions: &mut Vec<AssertionResult>,
    time: u64,
) -> Drained {
    let ctx = RuntimeFormatContext {
        tb_time: Some(time),
        scope: None,
    };
    let mut drained = Drained {
        fatal: None,
        finished: false,
    };
    for (site, event) in sim.collect_sited_runtime_events(ctx) {
        let location = site.and_then(|site| {
            sim.program
                .runtime_schema
                .runtime_event_sites
                .get(site)?
                .location
                .as_ref()
                .map(|location| SourceLocation {
                    file: location.file.clone(),
                    line: location.line,
                    column: location.column,
                })
        });
        match event {
            RuntimeEvent::AssertPass { message } => assertions.push(AssertionResult {
                passed: true,
                message: Some(message),
                location,
            }),
            RuntimeEvent::AssertContinue { message } => assertions.push(AssertionResult {
                passed: false,
                message: Some(message),
                location,
            }),
            RuntimeEvent::AssertFatal { message } => {
                if drained.fatal.is_none() {
                    drained.fatal = Some(message.clone());
                }
                assertions.push(AssertionResult {
                    passed: false,
                    message: Some(message),
                    location,
                });
            }
            RuntimeEvent::Missed { count } => assertions.push(AssertionResult {
                passed: false,
                message: Some(format!("missed {count} runtime events")),
                location: None,
            }),
            RuntimeEvent::Display { message } => forward_display(&message, true),
            RuntimeEvent::Write { message } => forward_display(&message, false),
            RuntimeEvent::Finish => drained.finished = true,
        }
    }
    drained
}

/// Serve host request `request` of process `process` at `time`.
pub(crate) fn serve_host_request<B: SimBackend>(
    sim: &mut Simulator<B>,
    process: usize,
    request: usize,
    time: u64,
) -> Result<bool, String> {
    let request = sim
        .program
        .runtime_schema
        .processes
        .get(process)
        .and_then(|slots| slots.host_requests.get(request))
        .cloned()
        .ok_or_else(|| format!("process {process} made unknown host request {request}"))?;
    match request {
        HostRequest::RandomSeed { handle, value } => {
            let seed = read(sim, &value).to_u64().unwrap_or(u64::MAX);
            random(sim)?.seed(&handle, seed);
        }
        HostRequest::RandomGet { handle, result } => {
            let value = random(sim)?.get(&handle, result.width as u32);
            write(sim, &result, BigUint::from(value));
        }
        HostRequest::RandomGetRange {
            handle,
            min,
            max,
            result,
        } => {
            let min = read(sim, &min).to_u64().unwrap_or(u64::MAX);
            let max = read(sim, &max).to_u64().unwrap_or(u64::MAX);
            let value =
                random(sim)?.get_range(&handle, min, max, result.width as u32, result.signed);
            write(sim, &result, BigUint::from(value));
        }
        HostRequest::RandomGetSeed { handle, result } => {
            let seed = random(sim)?.get_seed(&handle);
            write(sim, &result, BigUint::from(seed));
        }
        HostRequest::Component {
            instance,
            method,
            args,
            result,
            declared_width,
            strict,
        } => {
            // The component reads settled design state.
            sim.eval_comb_checked().map_err(|error| error.to_string())?;
            let host_args = args
                .iter()
                .map(|arg| {
                    crate::component::host_value_from_bits(read(sim, arg), arg.width, arg.is_string)
                })
                .collect::<Vec<_>>();
            let (returned, writes) = sim.components.call_method(
                &instance,
                &method,
                &host_args,
                time,
                &mut sim.backend,
            )?;
            apply_component_writes(sim, writes);
            if sim.components.finish_requested() {
                return Ok(false);
            }
            let Some(result) = result else {
                return Ok(true);
            };
            let Some((value, width)) = crate::component::host_bits(&returned) else {
                return Err(format!(
                    "component method `{instance}.{method}` returned no bit value"
                ));
            };
            if let Some(expected) = declared_width
                && width as usize != expected
            {
                return Err(format!(
                    "component method `{method}` declares a {expected}-bit return value but returned {width} bits"
                ));
            }
            if declared_width.is_none() && strict && width > 64 {
                return Err(format!(
                    "component method `{method}` returned {width} bits; the expression form carries at most 64 bits"
                ));
            }
            let value = resize_component_return(value, width as usize, result.signed, result.width);
            write(sim, &result, value);
        }
    }
    Ok(true)
}

fn random<B: SimBackend>(sim: &mut Simulator<B>) -> Result<&mut RandomTable, String> {
    sim.testbench_random
        .as_mut()
        .ok_or_else(|| "random numbers are only available while a testbench runs".to_string())
}

fn read<B: SimBackend>(sim: &Simulator<B>, value: &HostValue<celox_design::StateAddr>) -> BigUint {
    let signal = sim.backend.resolve_signal(&value.signal);
    sim.backend.get(signal)
}

fn write<B: SimBackend>(
    sim: &mut Simulator<B>,
    target: &HostValue<celox_design::StateAddr>,
    value: BigUint,
) {
    let signal = sim.backend.resolve_signal(&target.signal);
    let mask = (BigUint::from(1u8) << target.width) - BigUint::from(1u8);
    sim.backend.set_wide(signal, value & mask);
}
