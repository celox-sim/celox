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

pub(crate) struct FileTable {
    files: crate::HashMap<String, std::fs::File>,
    base: Option<std::path::PathBuf>,
}

impl FileTable {
    fn flush(&mut self) -> Result<(), String> {
        use std::io::Write;
        for file in self.files.values_mut() {
            file.flush().map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}

/// Whether the simulator's testbench runs as process kernels: the schema
/// marks them (a design's own processes alone do not make a kernel
/// testbench).
pub(crate) fn enabled<B: SimBackend>(sim: &Simulator<B>) -> bool {
    sim.program.runtime_schema.testbench_kernels.is_some()
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
                .as_deref()
                .map(super::message_diagnostic)
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
        let _ = sim.components.finish(0);
        return failed(format!("component VCD registration failed: {error}"));
    }
    apply_component_writes(sim, initial_writes);
    sim.testbench_random = Some(RandomTable::new(seed));
    sim.testbench_files = Some(FileTable {
        files: crate::HashMap::default(),
        base: testbench.component_file_base().map(|base| base.to_owned()),
    });
    // A run starts every process at its beginning, whatever an earlier run
    // of the same simulator left in the control slots, and every process
    // clock low, as the bytecode testbench does: the first `clk.next()`
    // then counts a real rising edge.
    for slots in &sim.program.runtime_schema.processes.clone() {
        for slot in slots.iter() {
            let signal = sim.backend.resolve_signal(slot);
            sim.backend.set_wide(signal, BigUint::ZERO);
        }
        for clock in &slots.clocks {
            let signal = sim.backend.resolve_signal(&clock.signal);
            sim.backend.set_wide(signal, BigUint::ZERO);
            sim.dirty = true;
        }
    }
    sim.testbench_events.clear();

    // The scheduler samples the clocks it detects edges on from settled
    // state.
    if sim.dirty && !sim.components.finish_requested() {
        if let Err(error) = sim.eval_comb_checked() {
            // The components were installed above; take them down again.
            let _ = sim.components.finish(0);
            sim.testbench_random = None;
            sim.testbench_files = None;
            return failed(error.to_string());
        }
        sim.dirty = false;
    }
    let mut state = crate::simulation::simulation_state(sim);
    state.set_tick_budget(tick_limit);
    let mut assertions = Vec::new();
    let mut error = None;
    // A `$finish` of the design's own logic arrives as a runtime event; it
    // ends the run like a kernel's.
    let mut finish_observed = false;
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
            Ok(Some(_)) => {
                if sim
                    .program
                    .runtime_schema
                    .testbench_kernels
                    .is_some_and(|first| state.processes_done_from(first))
                {
                    finished = true;
                }
            }
            Ok(None) => finished = true,
            Err(code) => {
                if error.is_none() {
                    error = Some(code.to_string());
                }
                finished = true;
            }
        }
        finish_observed |= drained.finished;
        if finish_observed || state.is_finished() || sim.components.finish_requested() {
            finished = true;
        }
    }
    sim.testbench_random = None;
    if let Some(mut files) = sim.testbench_files.take()
        && let Err(message) = files.flush()
        && error.is_none()
    {
        error = Some(message);
    }

    // A completion a component requested is read before the teardown
    // clears the components.
    let component_finished = sim.components.finish_requested();
    if let Err(message) = sim.components.finish(state.time())
        && error.is_none()
    {
        error = Some(message);
    }
    let finished = finish_observed || state.is_finished() || component_finished;
    KernelRun {
        assertions,
        error,
        finished,
        ticks: state.ticks(),
        tick_limit_reached: tick_limit.is_some_and(|limit| state.ticks() >= limit)
            && !sim
                .program
                .runtime_schema
                .testbench_kernels
                .is_some_and(|first| state.processes_done_from(first))
            && !finished,
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
    // The events the kernels' yields let the executor spill, then the ring.
    let mut events = std::mem::take(&mut sim.testbench_events);
    events.extend(sim.collect_sited_runtime_events(ctx));
    for (site, event) in events {
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
                message: site
                    .and_then(|id| sim.program.runtime_schema.runtime_event_sites.get(id))
                    .filter(|site| site.template.is_some() || !site.arg_widths.is_empty())
                    .map(|_| message),
                location,
            }),
            RuntimeEvent::AssertContinue { message } => assertions.push(AssertionResult {
                passed: false,
                message: Some(message),
                location,
            }),
            RuntimeEvent::AssertFatal { message } => {
                if drained.fatal.is_none() {
                    drained.fatal = Some(super::message_diagnostic(&message));
                }
                assertions.push(AssertionResult {
                    passed: false,
                    message: Some(message),
                    location,
                });
            }
            RuntimeEvent::Missed { count } => assertions.push(AssertionResult {
                passed: false,
                message: Some(format!("missed {count} runtime events").into_bytes()),
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
    let close_file = matches!(request, HostRequest::FileClose { .. });
    match request {
        HostRequest::FileOpen {
            handle,
            path,
            append,
        } => {
            let bytes = read(sim, &path).to_bytes_be();
            let path = String::from_utf8(bytes)
                .map_err(|error| format!("testbench file path: {error}"))?;
            let files = sim
                .testbench_files
                .as_mut()
                .ok_or("files are only available while a testbench runs")?;
            let path = match &files.base {
                Some(base) => base.join(path),
                None => path.into(),
            };
            let file = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .append(append)
                .truncate(!append)
                .open(&path)
                .map_err(|error| format!("testbench file {}: {error}", path.display()))?;
            if let Some(mut previous) = files.files.insert(handle, file) {
                use std::io::Write;
                previous.flush().map_err(|error| error.to_string())?;
            }
        }
        HostRequest::FileWrite {
            handle,
            template,
            args,
        } => {
            use std::io::Write;
            let bytes = crate::simulator::render_host_message(sim, template, &args, time);
            let files = sim
                .testbench_files
                .as_mut()
                .ok_or("files are only available while a testbench runs")?;
            files
                .files
                .get_mut(&handle)
                .ok_or_else(|| format!("testbench file {handle} is not open"))?
                .write_all(&bytes)
                .map_err(|error| error.to_string())?;
        }
        HostRequest::FileFlush { handle } | HostRequest::FileClose { handle } => {
            use std::io::Write;

            let files = sim
                .testbench_files
                .as_mut()
                .ok_or("files are only available while a testbench runs")?;
            let file = files
                .files
                .get_mut(&handle)
                .ok_or_else(|| format!("testbench file {handle} is not open"))?;
            file.flush().map_err(|error| error.to_string())?;
            if close_file {
                files.files.remove(&handle);
            }
        }

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
            // Effectful arguments have a lowering barrier. A clean call
            // must not replay combinational displays/assertions.
            if sim.dirty {
                sim.eval_comb_checked().map_err(|error| error.to_string())?;
                sim.dirty = false;
            }
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
                if sim.dirty {
                    sim.eval_comb_checked().map_err(|error| error.to_string())?;
                    sim.dirty = false;
                }
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
            // Invalid return contracts must not expose derived output effects.
            if sim.dirty {
                sim.eval_comb_checked().map_err(|error| error.to_string())?;
                sim.dirty = false;
            }
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

#[cfg(test)]
mod event_yield_settle_tests {
    use super::*;
    use crate::RuntimeErrorCode;
    use celox_runtime::SimulationExecutor;

    struct CountingExecutor<B: SimBackend> {
        sim: Simulator<B>,
        settles: usize,
        combs: usize,
        fused: u64,
        combs_before_edge: Option<usize>,
    }
    impl<B: SimBackend> SimulationExecutor for CountingExecutor<B> {
        type Backend = B;
        fn host_request_changes_design(&self, process: usize, request: usize) -> bool {
            SimulationExecutor::host_request_changes_design(&self.sim, process, request)
        }
        fn host_request(
            &mut self,
            process: usize,
            request: usize,
            time: u64,
        ) -> Result<bool, RuntimeErrorCode> {
            SimulationExecutor::host_request(&mut self.sim, process, request, time)
        }
        fn backend(&self) -> &B {
            &self.sim.backend
        }
        fn backend_mut(&mut self) -> &mut B {
            &mut self.sim.backend
        }
        fn eval_comb(&mut self) -> Result<(), RuntimeErrorCode> {
            self.combs += 1;
            SimulationExecutor::eval_comb(&mut self.sim)
        }
        fn process_is_clean(&self, index: usize) -> bool {
            SimulationExecutor::process_is_clean(&self.sim, index)
        }
        fn fused_ticks_allowed(&self) -> bool {
            SimulationExecutor::fused_ticks_allowed(&self.sim)
        }
        fn tick_many(
            &mut self,
            event: B::Event,
            count: u64,
        ) -> (u64, Result<(), RuntimeErrorCode>) {
            self.combs_before_edge.get_or_insert(self.combs);
            let result = SimulationExecutor::tick_many(&mut self.sim, event, count);
            self.fused = self.fused.max(count);
            result
        }
        fn run_process(&mut self, index: usize, time: u64) -> Result<(), RuntimeErrorCode> {
            SimulationExecutor::run_process(&mut self.sim, index, time)?;
            let slot = self.sim.program.runtime_schema.processes[index].status;
            let signal = self.sim.backend.resolve_signal(&slot);
            if celox_design::ProcessStatus::from_code(self.sim.backend.get_as(signal))
                == Some(celox_design::ProcessStatus::Settle)
            {
                self.settles += 1;
            }
            Ok(())
        }
        fn eval_apply_ff_at(&mut self, event: B::Event) -> Result<(), RuntimeErrorCode> {
            self.combs_before_edge.get_or_insert(self.combs);
            SimulationExecutor::eval_apply_ff_at(&mut self.sim, event)
        }
        fn eval_only_ff_at(&mut self, event: B::Event) -> Result<(), RuntimeErrorCode> {
            SimulationExecutor::eval_only_ff_at(&mut self.sim, event)
        }
        fn apply_ff_at(&mut self, event: B::Event) -> Result<(), RuntimeErrorCode> {
            SimulationExecutor::apply_ff_at(&mut self.sim, event)
        }
        fn finish_requested(&mut self, time: u64) -> bool {
            SimulationExecutor::finish_requested(&mut self.sim, time)
        }
    }
    fn count<B: SimBackend>(mut sim: Simulator<B>, iterations: usize) -> usize {
        let limit = sim.signal("limit");
        sim.modify(|io| {
            io.set(limit, iterations as u32);
        })
        .unwrap();
        let mut state = crate::simulation::simulation_state(&sim);
        let mut executor = CountingExecutor {
            sim,
            settles: 0,
            combs: 0,
            fused: 0,
            combs_before_edge: None,
        };
        for _ in 0..10000 {
            if state.step(&mut executor).unwrap().is_none() || state.is_finished() {
                return executor.settles;
            }
        }
        panic!("process did not finish");
    }
    #[test]
    fn clean_random_host_requests_preserve_settled_design_state() {
        fn count_host<B: SimBackend>(mut sim: Simulator<B>) -> usize {
            sim.testbench_random = Some(RandomTable::new(7));
            sim.testbench_files = Some(FileTable {
                files: crate::HashMap::default(),
                base: None,
            });
            let mut state = crate::simulation::simulation_state(&sim);
            let mut executor = CountingExecutor {
                sim,
                settles: 0,
                combs: 0,
                fused: 0,
                combs_before_edge: None,
            };
            state.step(&mut executor).unwrap();
            assert!(state.is_finished());
            assert_eq!(
                executor
                    .sim
                    .backend
                    .get_as::<u8>(executor.sim.signal("derived")),
                1
            );
            executor.combs
        }
        let source = |body: &str| {
            format!(
                "#[test(Top)]module Top{{var random:$tb::random::<u8>;var f:$tb::file;var source:u8;var derived:u8;always_comb{{derived=source;}}initial{{source=1;{body}$finish();}}}}"
            )
        };
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("clean.txt");
        let file_body = format!(
            "f.open(\"{}\");f.write(\"x\");f.flush();f.close();",
            file.display()
        );
        macro_rules! check {
            ($build:ident) => {{
                let baseline = source("");
                let expected = count_host(Simulator::builder(&baseline, "Top").$build().unwrap());
                for body in ["random.seed(7);", file_body.as_str()] {
                    let seeded = source(body);
                    assert_eq!(
                        count_host(Simulator::builder(&seeded, "Top").$build().unwrap()),
                        expected
                    );
                }
            }};
        }
        check!(build_interpreter);
        check!(build_cranelift);
        check!(build_wasm);
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check!(build_native);
    }
    #[test]
    fn private_loop_counters_do_not_request_settles() {
        let code = r#"#[test(Top)]module Top(limit:input u32){initial{for i in 0..limit{$assert(1);}$finish();}}"#;
        macro_rules! check {
            ($method:ident) => {{
                assert_eq!(
                    count(
                        Simulator::builder(code, "Top")
                            .testbench_kernels(true)
                            .$method()
                            .unwrap(),
                        3
                    ),
                    0
                );
            }};
        }
        check!(build_interpreter);
        check!(build_cranelift);
        check!(build_wasm);
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check!(build_native);
    }
    #[test]
    fn clean_conditional_paths_do_not_request_a_settle() {
        for statement in [
            "if limit != 0 {source=1;}",
            "case limit {0:{} default:{source=1;}}",
        ] {
            let code = format!(
                "#[test(Top)]module Top(limit:input u32){{var source:u32;var derived:u32;always_comb{{derived=source;}}initial{{{statement}$assert(derived==source);$finish();}}}}"
            );
            macro_rules! check {
                ($method:ident) => {{
                    for (input, expected) in [(0, 0), (1, 1)] {
                        assert_eq!(
                            count(
                                Simulator::builder(&code, "Top")
                                    .testbench_kernels(true)
                                    .$method()
                                    .unwrap(),
                                input
                            ),
                            expected,
                            "{statement}, input={input}"
                        );
                    }
                }};
            }
            check!(build_interpreter);
            check!(build_cranelift);
            check!(build_wasm);
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check!(build_native);
        }
    }

    #[test]
    fn event_yield_does_not_settle_a_write_twice() {
        let code = r#"#[test(Top)]module Top(limit:input u32){var source:u32;var derived:u32;
            always_comb{derived=source;}
            function mark(x:input u32,o:output u32)->u32{o=x;return 1;}
            initial{for i in 0..limit{$assert(1,"tb %d",mark(i,source));$assert(derived==i,"read");$assert(1);}$assert(derived==limit-1,"end");$finish();}}"#;
        macro_rules! check {
            ($method:ident) => {{
                let run = |n| {
                    count(
                        Simulator::builder(code, "Top")
                            .testbench_kernels(true)
                            .$method()
                            .unwrap(),
                        n,
                    )
                };
                let small_delta = run(2) - run(1);
                let before_yield = run(85);
                let at_yield = run(86);
                let after_yield = run(87);
                assert_eq!(
                    at_yield - before_yield,
                    small_delta,
                    "256th event settled twice"
                );
                assert_eq!(
                    after_yield - at_yield,
                    small_delta,
                    "post-yield write must settle"
                );
            }};
        }
        check!(build_interpreter);
        check!(build_cranelift);
        check!(build_wasm);
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check!(build_native);
    }
    fn execution_counts<B: SimBackend>(sim: Simulator<B>, steps: usize) -> (usize, u64, u64) {
        let mut state = crate::simulation::simulation_state(&sim);
        let mut executor = CountingExecutor {
            sim,
            settles: 0,
            combs: 0,
            fused: 0,
            combs_before_edge: None,
        };
        for _ in 0..steps {
            if state.step(&mut executor).unwrap().is_none() {
                break;
            }
            if state.is_finished() {
                break;
            }
        }
        if let Ok(q) = executor.sim.try_signal("q") {
            assert_eq!(executor.sim.backend.get_as::<u32>(q), state.ticks() as u32);
        }
        if let (Ok(source), Ok(derived)) = (
            executor.sim.try_signal("source"),
            executor.sim.try_signal("derived"),
        ) {
            assert_eq!(
                executor.sim.backend.get_as::<u32>(derived),
                executor.sim.backend.get_as::<u32>(source)
            );
        }
        (
            executor.combs_before_edge.unwrap_or(executor.combs),
            executor.fused,
            state.ticks(),
        )
    }
    #[test]
    fn kernel_output_after_wait_preserves_fusion_but_design_output_does_not() {
        for task in ["$display(\"done\");", "$write(\"done\");"] {
            let code = format!(
                "#[test(Top)]module Top{{inst clk:$tb::clock_gen;var q:u32;always_ff(clk){{q+=1;}}initial{{clk.next(1000);{task}$assert(q==1000);$finish();}}}}"
            );
            macro_rules! check {
                ($method:ident) => {{
                    let (_, fused, ticks) =
                        execution_counts(Simulator::builder(&code, "Top").$method().unwrap(), 3000);
                    assert_eq!(fused, 1000);
                    assert_eq!(ticks, 1000);
                }};
            }
            check!(build_interpreter);
            check!(build_cranelift);
            check!(build_wasm);
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check!(build_native);
        }
        let code = "#[test(Top)]module Top{inst clk:$tb::clock_gen;always_ff(clk){$display(\"edge\");}initial{clk.next(1000);$finish();}}";
        let (_, fused, _) = execution_counts(
            Simulator::builder(code, "Top").build_interpreter().unwrap(),
            2,
        );
        assert_eq!(fused, 0);
    }
    #[test]
    fn clean_clock_wait_continuations_do_not_repeat_comb_settles() {
        for (body, expected) in [
            ("source=1;", 1),
            ("source=1;clk.next(1);", 2),
            ("source=1;$assert(derived==1);source=2;", 2),
            ("source=1;$assert(derived==1);clk.next(write());", 3),
        ] {
            let code = format!(
                "#[test(Top)]module Top{{inst clk:$tb::clock_gen;var source:u32;var derived:u32;always_comb{{derived=source;$assert(1,\"comb\");}}function write()->u32{{source=2;return 1;}}initial{{{body}}}}}"
            );
            macro_rules! check {
                ($method:ident) => {{
                    let (combs, _, _) =
                        execution_counts(Simulator::builder(&code, "Top").$method().unwrap(), 1);
                    assert_eq!(combs, expected, "{body}");
                }};
            }
            check!(build_interpreter);
            check!(build_cranelift);
            check!(build_wasm);
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check!(build_native);
        }
    }
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn native_image_preserves_clean_continuation_after_optimization() {
        let code = "#[test(Top)]module Top{var source:u32;var derived:u32;always_comb{derived=source;}initial{source=1;}}";
        let original = Simulator::builder(code, "Top")
            .opt_level(crate::OptLevel::O2)
            .build_native()
            .unwrap();
        let bytes = original
            .shared_code()
            .program_image()
            .to_container_bytes()
            .unwrap();
        let image = crate::NativeProgramImage::from_container_bytes(&bytes).unwrap();
        assert!(image.runtime_schema().processes[0].comb_clean.is_some());
        let restored = Simulator::builder("unused", "unused")
            .build_native_from_image(image)
            .unwrap();
        assert_eq!(execution_counts(restored, 1).0, 1);
    }
    #[test]
    fn repeated_kernel_entries_restart_the_event_yield_count() {
        let code = r#"#[test(Top)]module Top{var source:u32;var derived:u32;
            always_comb{derived=source;}
            function mark(o:output u32)->u32{o=1;return 1;}
            initial{$assert(1,"value %d",mark(source));$finish();}}
        "#;
        fn check<B: SimBackend>(sim: Simulator<B>) {
            let source = sim.signal("source");
            let derived = sim.signal("derived");
            let mut executor = CountingExecutor {
                sim,
                settles: 0,
                combs: 0,
                fused: 0,
                combs_before_edge: None,
            };
            for run in 1..=256 {
                // Restart control slots as the public runner does, retaining
                // the same compiled kernel and its other scratch storage.
                for slots in &executor.sim.program.runtime_schema.processes {
                    for address in slots.iter() {
                        let signal = executor.sim.backend.resolve_signal(address);
                        executor.sim.backend.set(signal, 0u64);
                    }
                }
                executor.sim.backend.set(source, 0u32);
                executor.eval_comb().unwrap();
                let mut state = crate::simulation::simulation_state(&executor.sim);
                let before = executor.settles;
                while !state.is_finished() {
                    assert!(state.step(&mut executor).unwrap().is_some());
                }
                assert_eq!(
                    executor.settles - before,
                    0,
                    "run {run} inherited an event-yield budget"
                );
                assert_eq!(executor.sim.backend.get_as::<u32>(source), 1);
                assert_eq!(executor.sim.backend.get_as::<u32>(derived), 1);
            }
        }
        check(Simulator::builder(code, "Top").build_interpreter().unwrap());
        check(Simulator::builder(code, "Top").build_cranelift().unwrap());
        check(Simulator::builder(code, "Top").build_wasm().unwrap());
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(Simulator::builder(code, "Top").build_native().unwrap());
    }
}
