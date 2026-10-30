//! Native testbench execution for Veryl `#[test]` modules.
//!
//! Processes and expressions execute generated host machine code. Persistent
//! continuations retain ordinary state slots across clock waits. Expressions
//! and loop arithmetic use the shared SIR backend; host requests provide timing,
//! diagnostics and external component services.

use crate::HashMap;
use crate::RuntimeErrorCode;
use crate::backend::memory_layout::{
    RUNTIME_EVENT_HEADER_SIZE, RUNTIME_EVENT_SLOT_ARG_COUNT_OFFSET,
    RUNTIME_EVENT_SLOT_PAYLOAD_OFFSET, RUNTIME_EVENT_SLOT_SEQ_OFFSET,
    RUNTIME_EVENT_SLOT_SITE_OFFSET, RUNTIME_EVENT_WRITING,
};
use crate::backend::traits::{EventHandle, SimBackend};
use crate::ir::SignalRef;
use crate::simulator::{RuntimeEvent, RuntimeFormatContext, Simulator, message_diagnostic};
use celox_design::Severity;
pub use celox_testbench::SourceLocation;
use celox_testbench::{
    DisplayFormatArg, ExecutableArgument, ExecutableAssertMessage, ExecutableClockCount,
    ExecutableLoopBound, ExecutableStatement, ExecutableTestbench, FieldSpec,
    TestbenchStatement as GenericTestbenchStatement, TestbenchTarget, TestbenchValue as TbValue,
    format_veryl_display_arg, pad_veryl_field,
};
use num_bigint::BigUint;
use rand::{RngExt as _, SeedableRng as _};
use rand_pcg::Pcg64;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::Poll;

mod clock_batch;
mod compiled;
mod native;

// ── Public types ───────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestResult {
    Pass,
    Fail(String),
}

/// Result of a single `$assert` evaluation.
#[derive(Debug, Clone)]
pub struct AssertionResult {
    pub passed: bool,
    /// Byte-exact HDL message; decode explicitly when text is required.
    pub message: Option<Vec<u8>>,
    pub location: Option<SourceLocation>,
}

/// Detailed test result with assertion outcomes observed before the test
/// finishes or stops on a fatal failure.
#[derive(Debug, Clone)]
pub struct TestResultDetailed {
    pub passed: bool,
    pub assertions: Vec<AssertionResult>,
    pub error: Option<String>,
}

/// Opaque, precompiled native testbench program for a built simulator.
pub type CompiledTestbench<B> = ExecutableTestbench<<B as SimBackend>::Event, SignalRef>;

/// Result of executing a testbench with an optional tick limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LimitedTestbenchResult {
    pub result: TestResult,
    pub ticks: u64,
    pub tick_limit_reached: bool,
}

pub(crate) type AssertMessage = ExecutableAssertMessage;

/// Clock cycle count: either a compile-time constant or a runtime expression.
pub type ClockCount = ExecutableClockCount;

pub type LoopBound = ExecutableLoopBound;

#[derive(Debug, thiserror::Error)]
enum TestbenchEvaluationError {
    #[error("eval_comb: {source}")]
    EvalComb {
        #[source]
        source: RuntimeErrorCode,
    },
}

pub(crate) type TestbenchStatement<B> = ExecutableStatement<<B as SimBackend>::Event, SignalRef>;

pub(crate) type CompiledAssertArg = ExecutableArgument;

fn eval_expr<B: SimBackend>(
    sim: &mut Simulator<B>,
    expr: &celox_testbench::CompiledExpr,
) -> TbValue {
    let (value, writes) = eval_backend_expr_tracked(&mut sim.backend, expr);
    sim.dirty |= writes
        .iter()
        .any(|&(offset, len)| !private_testbench_range(sim, offset, len));
    value
}

pub(crate) fn eval_backend_expr<B: SimBackend>(
    backend: &mut B,
    expr: &celox_testbench::CompiledExpr,
) -> TbValue {
    eval_backend_expr_tracked(backend, expr).0
}

fn eval_backend_expr_tracked<B: SimBackend>(
    backend: &mut B,
    expr: &celox_testbench::CompiledExpr,
) -> (TbValue, Vec<(usize, usize)>) {
    let kernel = expr
        .kernel()
        .expect("expression was compiled before state layout");
    backend
        .run_process(kernel.entry)
        .unwrap_or_else(|error| panic!("testbench expression kernel: {error}"));
    let (memory, _) = backend.memory_as_ptr();
    // The kernel result is an ordinary, layout-owned state slot.
    let bytes =
        unsafe { std::slice::from_raw_parts(memory.add(kernel.result), kernel.width.div_ceil(8)) };
    let value = if kernel.is_string {
        let handle =
            celox_heap::Handle::from_le_bytes(bytes.try_into().expect("string handle width"));
        TbValue::String(backend.heap().string(handle as celox_heap::Handle).to_vec())
    } else if kernel.width <= 64 {
        let mut value = [0u8; 8];
        value[..bytes.len()].copy_from_slice(bytes);
        TbValue::U64(u64::from_le_bytes(value))
    } else {
        TbValue::Wide(BigUint::from_bytes_le(bytes))
    };
    let writes = kernel
        .writes
        .iter()
        .map(|(offset, size)| (*offset, *size))
        .collect::<Vec<_>>();
    if let Some(trace) = backend.layout().trace.as_ref() {
        for &(offset, len) in &writes {
            // SAFETY: bound bytecode writes valid locations in this image, and
            // the simulator cannot execute concurrently with this evaluation.
            unsafe {
                trace.mark_range(memory.cast_mut(), offset, len);
            }
        }
    }
    for &(offset, len) in &writes {
        backend.mark_settle_range(offset, len);
    }
    (value, writes)
}

fn private_testbench_range<B: SimBackend>(sim: &Simulator<B>, offset: usize, len: usize) -> bool {
    if !sim.components.is_empty() || sim.schedule.timed {
        return false;
    }
    let ranges = sim.testbench_hardware_ranges.get_or_init(|| {
        let layout = sim.backend.layout();
        let mut ranges: Vec<_> = sim
            .program
            .rtl_accesses
            .iter()
            .chain(sim.backend.id_to_addr_slice())
            .filter_map(|addr| {
                let start = *layout.offsets.get(addr)?;
                let planes =
                    if layout.four_state && layout.is_4states.get(addr).copied().unwrap_or(false) {
                        2
                    } else {
                        1
                    };
                Some(start..start + layout.plane_size(addr) * planes)
            })
            .collect();
        ranges.sort_unstable_by_key(|range| (range.start, range.end));
        let mut merged: Vec<std::ops::Range<usize>> = Vec::new();
        for range in ranges {
            if let Some(last) = merged.last_mut()
                && range.start <= last.end
            {
                last.end = last.end.max(range.end);
            } else {
                merged.push(range);
            }
        }
        merged
    });
    let first = ranges.partition_point(|range| range.end <= offset);
    ranges
        .get(first)
        .is_none_or(|range| range.start >= offset.saturating_add(len.max(1)))
}

fn write_testbench_signal<B: SimBackend>(
    sim: &mut Simulator<B>,
    signal: SignalRef,
    write: impl FnOnce(&mut Simulator<B>),
) {
    let dirty = sim.dirty;
    let private = private_testbench_range(sim, signal.offset, signal.width.div_ceil(8));
    write(sim);
    if private {
        sim.dirty = dirty;
    }
}

/// TB observations need settled hardware, but repeated observations without
/// intervening writes must not execute the hardware again. Keep the public
/// explicit-evaluation API unchanged.
fn settle_testbench<B: SimBackend>(sim: &mut Simulator<B>) -> Result<(), RuntimeErrorCode> {
    if sim.dirty {
        sim.eval_comb()?;
    }
    Ok(())
}

fn settle_testbench_expr<B: SimBackend>(
    sim: &mut Simulator<B>,
    expr: &celox_testbench::CompiledExpr,
) -> Result<(), RuntimeErrorCode> {
    let schema = &sim.program.runtime_schema;
    // Only defer pure combinational work. Stateful comb, observers, process
    // kernels and external components can observe intervening TB writes.
    if !expr.observes_hardware()
        && schema.persistent_comb_state.is_empty()
        && schema.comb_observers.is_empty()
        && !sim.schedule.timed
        && sim.components.is_empty()
    {
        return Ok(());
    }
    settle_testbench(sim)
}

fn format_assert_arg<B: SimBackend>(
    sim: &mut Simulator<B>,
    arg: &CompiledAssertArg,
    spec: char,
    field: FieldSpec,
) -> Vec<u8> {
    let value = eval_expr(sim, &arg.expr);
    let width = match &value {
        TbValue::String(bytes) => bytes.len() * 8,
        _ => arg.width,
    };
    let value = value.to_biguint();
    format_veryl_display_arg(
        &DisplayFormatArg {
            value: &value,
            mask: None,
            width,
            signed: arg.signed,
            is_string: arg.is_string,
            float_precision: arg.float_precision,
        },
        spec,
        field,
    )
}

fn render_assert_message<B: SimBackend>(
    sim: &mut Simulator<B>,
    message: &Option<AssertMessage>,
    current_time: u64,
) -> Option<Vec<u8>> {
    match message {
        None => None,
        Some(AssertMessage::DynamicArgs(args)) => Some(
            args.iter()
                .map(|arg| {
                    format_assert_arg(
                        sim,
                        arg,
                        if arg.float_precision.is_some() {
                            'g'
                        } else {
                            'x'
                        },
                        FieldSpec::default(),
                    )
                })
                .collect::<Vec<_>>()
                .join(&b' '),
        ),
        Some(AssertMessage::Formatted { template, args }) => {
            let mut rendered = Vec::new();
            let mut chars = template.chars().peekable();
            let mut arg_idx = 0usize;
            while let Some(ch) = chars.next() {
                if ch != '%' {
                    rendered.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes());
                    continue;
                }
                match chars.peek().copied() {
                    Some('%') => {
                        chars.next();
                        rendered.push(b'%');
                    }
                    Some(_) => {
                        let field = FieldSpec::parse(&mut chars, true);
                        let Some(spec) = chars.next() else {
                            rendered.push(b'%');
                            break;
                        };
                        match spec {
                            'h' | 'H' | 'x' | 'X' | 'd' | 'D' | 'i' | 'I' | 'o' | 'O' | 'b'
                            | 'B' | 'c' | 'C' | 's' | 'S' | 'f' | 'F' | 'e' | 'E' | 'g' | 'G' => {
                                if let Some(arg) = args.get(arg_idx) {
                                    rendered.extend_from_slice(&format_assert_arg(
                                        sim, arg, spec, field,
                                    ));
                                }
                                arg_idx += 1;
                            }
                            'm' | 'M' => rendered.extend_from_slice(&pad_veryl_field(
                                b"<hierarchy>".to_vec(),
                                spec,
                                field,
                            )),
                            't' | 'T' => rendered.extend_from_slice(&pad_veryl_field(
                                current_time.to_string().into_bytes(),
                                spec,
                                field,
                            )),
                            _ => {
                                rendered.push(b'%');
                                rendered
                                    .extend_from_slice(spec.encode_utf8(&mut [0; 4]).as_bytes());
                            }
                        }
                    }
                    None => rendered.push(b'%'),
                }
            }
            Some(rendered)
        }
    }
}

fn sim_set_u64<B: SimBackend>(sim: &mut crate::Simulator<B>, sig: SignalRef, value: u64) {
    write_testbench_signal(sim, sig, |sim| match sig.width {
        0..=8 => sim.set(sig, value as u8),
        9..=16 => sim.set(sig, value as u16),
        17..=32 => sim.set(sig, value as u32),
        33..=64 => sim.set(sig, value),
        _ => sim.set_wide(sig, BigUint::from(value)),
    });
}

fn sim_set_target<B: SimBackend>(
    sim: &mut crate::Simulator<B>,
    target: &TestbenchTarget<SignalRef, celox_testbench::CompiledExpr>,
    value: TbValue,
) {
    if let TbValue::String(bytes) = value {
        // A selection names a string element by its bit offset.
        let index = target.selection.as_ref().map_or(0, |selection| {
            eval_expr(sim, &selection.offset).to_u64() as usize / celox_design::HANDLE_WIDTH
        });
        if index < target.signal.width / celox_design::HANDLE_WIDTH {
            let handle = sim.backend.heap_mut().intern(&bytes);
            let element = SignalRef {
                offset: target.signal.offset + index * celox_heap::HANDLE_BYTES,
                width: celox_design::HANDLE_WIDTH,
                is_4state: false,
                array_layout: None,
            };
            write_testbench_signal(sim, element, |sim| sim.set(element, handle));
        }
        return;
    }
    let Some(selection) = &target.selection else {
        match value {
            TbValue::String(_) => unreachable!(),
            TbValue::U64(value) => sim_set_u64(sim, target.signal, value),
            TbValue::Wide(value) => {
                write_testbench_signal(sim, target.signal, |sim| sim.set_wide(target.signal, value))
            }
        }
        return;
    };

    let offset = eval_expr(sim, &selection.offset).to_u64() as usize;
    let width = selection
        .width
        .min(target.signal.width.saturating_sub(offset));
    if width == 0 {
        return;
    }

    let (root, root_mask) =
        if private_testbench_range(sim, target.signal.offset, target.signal.width.div_ceil(8)) {
            sim.backend.get_four_state(target.signal)
        } else {
            sim.get_four_state(target.signal)
        };
    let selected_mask = width_mask(width) << offset;
    let clear_mask = width_mask(target.signal.width) ^ &selected_mask;
    let selected_value = (value.to_biguint() & width_mask(width)) << offset;
    write_testbench_signal(sim, target.signal, |sim| {
        sim.set_four_state(
            target.signal,
            (root & &clear_mask) | selected_value,
            root_mask & clear_mask,
        )
    });
}

/// Collect the heap between concurrent process polls. The saved temporaries of
/// suspended processes are outside state memory, so their handles are roots.
fn collect_testbench_heap<B: SimBackend>(
    sim: &mut Simulator<B>,
    private_signals: &[SignalRef],
    locals: &[Vec<(BigUint, BigUint)>],
) {
    if !sim.backend.heap().should_collect() {
        return;
    }
    let slots = sim.handle_slots();
    let saved_roots: Vec<celox_heap::Root> = private_signals
        .iter()
        .enumerate()
        .filter_map(|(index, signal)| {
            let slot = slots
                .binary_search_by_key(&signal.offset, |&(offset, _)| offset)
                .ok()?;
            Some((index, signal, slots[slot].1))
        })
        .flat_map(|(index, signal, kind)| {
            let elements = signal.width / celox_design::HANDLE_WIDTH;
            locals.iter().flat_map(move |process| {
                let digits = process[index].0.to_u32_digits();
                (0..elements).map(move |element| celox_heap::Root {
                    kind,
                    handle: digits.get(element).copied().unwrap_or(0),
                })
            })
        })
        .collect();
    sim.maybe_collect_heap(saved_roots);
}

fn execution_random_seed(seed: Option<u64>) -> u64 {
    seed.unwrap_or_else(rand::random)
}

// Keep the generator, seed derivation, and range sampling byte-for-byte
// compatible with Veryl's `$tb::random` runtime.
struct RandomTable {
    base_seed: u64,
    rngs: HashMap<String, (Pcg64, u64)>,
}

impl RandomTable {
    fn new(base_seed: u64) -> Self {
        Self {
            base_seed,
            rngs: HashMap::default(),
        }
    }

    fn derive_seed(base_seed: u64, handle: &str) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for byte in base_seed
            .to_le_bytes()
            .iter()
            .copied()
            .chain(handle.bytes())
        {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }

    fn entry(&mut self, handle: &str) -> &mut (Pcg64, u64) {
        let base_seed = self.base_seed;
        self.rngs.entry(handle.to_owned()).or_insert_with(|| {
            let seed = Self::derive_seed(base_seed, handle);
            (Pcg64::seed_from_u64(seed), seed)
        })
    }

    fn seed(&mut self, handle: &str, seed: u64) {
        self.rngs
            .insert(handle.to_owned(), (Pcg64::seed_from_u64(seed), seed));
    }

    fn get_seed(&mut self, handle: &str) -> u64 {
        self.entry(handle).1
    }

    fn get(&mut self, handle: &str, width: u32) -> u64 {
        let mask = random_mask(width);
        self.entry(handle).0.random_range(0..=mask)
    }

    fn get_range(&mut self, handle: &str, min: u64, max: u64, width: u32, signed: bool) -> u64 {
        let mask = random_mask(width);
        if signed {
            let min = sign_extend_random(min & mask, width);
            let max = sign_extend_random(max & mask, width);
            let (lo, hi) = if min <= max { (min, max) } else { (max, min) };
            let sample: i64 = self.entry(handle).0.random_range(lo..=hi);
            (sample as u64) & mask
        } else {
            let min = min & mask;
            let max = max & mask;
            let (lo, hi) = if min <= max { (min, max) } else { (max, min) };
            self.entry(handle).0.random_range(lo..=hi)
        }
    }
}

fn random_mask(width: u32) -> u64 {
    if width >= 64 {
        u64::MAX
    } else {
        (1u64 << width) - 1
    }
}

fn sign_extend_random(raw: u64, width: u32) -> i64 {
    if width == 0 || width >= 64 {
        raw as i64
    } else {
        let shift = 64 - width;
        ((raw << shift) as i64) >> shift
    }
}

// ── Expression compiler ────────────────────────────────────────────────

// ── Executor ───────────────────────────────────────────────────────────

enum ExecResult {
    Continue,
    Finished,
    Fail(String),
}

impl From<ExecResult> for TestResult {
    fn from(r: ExecResult) -> Self {
        match r {
            ExecResult::Continue | ExecResult::Finished => TestResult::Pass,
            ExecResult::Fail(m) => TestResult::Fail(m),
        }
    }
}

#[inline(never)]
fn eval_clock_count<B: SimBackend>(
    sim: &mut Simulator<B>,
    count: &ClockCount,
) -> Result<u64, TestbenchEvaluationError> {
    Ok(match count {
        ClockCount::Static(n) => *n,
        ClockCount::Dynamic(expr) => {
            settle_testbench_expr(sim, expr)
                .map_err(|source| TestbenchEvaluationError::EvalComb { source })?;
            eval_expr(sim, expr).to_u64()
        }
    })
}

fn width_mask(width: usize) -> BigUint {
    if width == 0 {
        BigUint::from(0u8)
    } else {
        (BigUint::from(1u8) << width) - BigUint::from(1u8)
    }
}

fn random_value_for_destination(
    value: u64,
    source_width: u32,
    signed: bool,
    destination_width: usize,
) -> BigUint {
    let source_width = source_width as usize;
    let source_mask = width_mask(source_width);
    let source_value = BigUint::from(value) & &source_mask;
    if destination_width < source_width {
        source_value & width_mask(destination_width)
    } else if signed
        && source_width > 0
        && source_width <= 64
        && source_value.bit((source_width - 1) as u64)
    {
        source_value | (width_mask(destination_width) ^ source_mask)
    } else {
        source_value
    }
}

fn sim_set_random_target<B: SimBackend>(
    sim: &mut crate::Simulator<B>,
    target: &TestbenchTarget<SignalRef, celox_testbench::CompiledExpr>,
    value: u64,
    source_width: u32,
    signed: bool,
) {
    let value = random_value_for_destination(value, source_width, signed, target.width);
    sim_set_target(sim, target, TbValue::Wide(value));
}

pub(crate) fn run_testbench<B: SimBackend>(
    sim: &mut Simulator<B>,
    testbench: &CompiledTestbench<B>,
) -> TestResult {
    run_testbench_limited(sim, testbench, None, false).result
}

fn run_testbench_limited<B: SimBackend>(
    sim: &mut Simulator<B>,
    testbench: &CompiledTestbench<B>,
    tick_limit: Option<u64>,
    require_finish: bool,
) -> LimitedTestbenchResult {
    let test_name = root_testbench_name(sim);
    let use_4state = sim.backend.layout().four_state;
    let initial_writes = match sim.components.initialize(
        testbench.components(),
        testbench.component_bindings(),
        testbench.component_libraries(),
        testbench.component_file_base(),
        execution_random_seed(testbench.configured_random_seed()),
        &test_name,
        use_4state,
        &mut sim.backend,
    ) {
        Ok(writes) => writes,
        Err(message) => {
            return LimitedTestbenchResult {
                result: TestResult::Fail(message),
                ticks: 0,
                tick_limit_reached: false,
            };
        }
    };
    if let Some(writer) = sim.vcd_writer.as_mut()
        && let Err(error) = writer.add_external_signals(&sim.components.trace_descriptors())
    {
        return LimitedTestbenchResult {
            result: TestResult::Fail(format!("component VCD registration failed: {error}")),
            ticks: 0,
            tick_limit_reached: false,
        };
    }
    apply_component_writes(sim, initial_writes);
    sim.component_simulation = sim
        .components
        .has_scheduled_components()
        .then(|| Box::new(crate::simulation::simulation_state(sim)));
    let mut ctx = DetailedExecContext {
        assertions: Vec::new(),
        current_time: 0,
        scheduler_time: None,
        tick_limit,
        tick_limit_reached: false,
        random: RandomTable::new(execution_random_seed(testbench.configured_random_seed())),
    };
    let mut result = if sim.components.finish_requested() {
        ExecResult::Finished
    } else {
        exec_testbench(sim, testbench, &mut ctx)
    };
    if let Err(message) = sim.components.finish(ctx.simulation_time())
        && !matches!(result, ExecResult::Fail(_))
    {
        result = ExecResult::Fail(message);
    }
    let failed_messages = ctx
        .assertions
        .iter()
        .filter(|assertion| !assertion.passed)
        .map(|assertion| {
            assertion
                .message
                .as_deref()
                .map(message_diagnostic)
                .unwrap_or_else(|| "assertion failed".to_string())
        })
        .collect::<Vec<_>>();
    let finished = matches!(result, ExecResult::Finished);
    let result = match result {
        ExecResult::Fail(message) => {
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
        ExecResult::Continue | ExecResult::Finished => {
            if !failed_messages.is_empty() {
                TestResult::Fail(failed_messages.join("\n"))
            } else if require_finish && !finished {
                TestResult::Fail("testbench returned without reaching $finish".to_string())
            } else {
                TestResult::Pass
            }
        }
    };
    LimitedTestbenchResult {
        result,
        ticks: ctx.current_time,
        tick_limit_reached: ctx.tick_limit_reached,
    }
}

/// Compile the elaborated initial processes into an executable native testbench.
pub fn compile_initial_testbench<B: SimBackend>(
    sim: &Simulator<B>,
) -> Option<CompiledTestbench<B>> {
    let semantic = sim.program().testbench.clone()?;
    celox_runtime::bind_testbench_program(
        sim.backend_ref(),
        semantic,
        &sim.program().runtime_schema.rtl_writes,
    )
}

/// Execute a previously compiled native testbench against a built simulator.
pub fn run_compiled_testbench<B: SimBackend>(
    sim: &mut Simulator<B>,
    tb: &CompiledTestbench<B>,
) -> TestResult {
    run_testbench(sim, tb)
}

/// Execute a compiled testbench, requiring explicit completion via `$finish`.
///
/// Unlike [`run_compiled_testbench`], all processes falling through is a
/// failure. Assertion and execution failures remain failures even if `$finish`
/// is reached. Use this for self-checking tests that require a completion marker.
pub fn run_compiled_testbench_to_finish<B: SimBackend>(
    sim: &mut Simulator<B>,
    tb: &CompiledTestbench<B>,
) -> TestResult {
    run_testbench_limited(sim, tb, None, true).result
}

/// Execute at most `tick_limit` simulator ticks from a compiled testbench.
///
/// Reaching the limit is reported separately from the testbench result so a
/// performance prefix cannot be mistaken for a completed test. Concurrent waits
/// share clock edges; simultaneous rising edges count as one simulator tick.
/// At the limit, pending falling edges and reset releases are drained without
/// resuming initial processes or starting another rising tick.
pub fn run_compiled_testbench_with_tick_limit<B: SimBackend>(
    sim: &mut Simulator<B>,
    tb: &CompiledTestbench<B>,
    tick_limit: u64,
) -> LimitedTestbenchResult {
    run_testbench_limited(sim, tb, Some(tick_limit), false)
}

/// Run the testbench and return assertion results observed before the test
/// finishes or stops on a fatal failure.
pub(crate) fn run_testbench_detailed<B: SimBackend>(
    sim: &mut Simulator<B>,
    testbench: &CompiledTestbench<B>,
) -> TestResultDetailed {
    let test_name = root_testbench_name(sim);
    let use_4state = sim.backend.layout().four_state;
    let initial_writes = match sim.components.initialize(
        testbench.components(),
        testbench.component_bindings(),
        testbench.component_libraries(),
        testbench.component_file_base(),
        execution_random_seed(testbench.configured_random_seed()),
        &test_name,
        use_4state,
        &mut sim.backend,
    ) {
        Ok(writes) => writes,
        Err(message) => {
            return TestResultDetailed {
                passed: false,
                assertions: Vec::new(),
                error: Some(message),
            };
        }
    };
    if let Some(writer) = sim.vcd_writer.as_mut()
        && let Err(error) = writer.add_external_signals(&sim.components.trace_descriptors())
    {
        return TestResultDetailed {
            passed: false,
            assertions: Vec::new(),
            error: Some(format!("component VCD registration failed: {error}")),
        };
    }
    apply_component_writes(sim, initial_writes);
    sim.component_simulation = sim
        .components
        .has_scheduled_components()
        .then(|| Box::new(crate::simulation::simulation_state(sim)));
    let mut ctx = DetailedExecContext {
        assertions: Vec::new(),
        current_time: 0,
        scheduler_time: None,
        tick_limit: None,
        tick_limit_reached: false,
        random: RandomTable::new(execution_random_seed(testbench.configured_random_seed())),
    };
    let result = if sim.components.finish_requested() {
        ExecResult::Finished
    } else {
        exec_testbench(sim, testbench, &mut ctx)
    };
    let mut error = match result {
        ExecResult::Fail(message) => Some(message),
        ExecResult::Continue | ExecResult::Finished => None,
    };
    if let Err(message) = sim.components.finish(ctx.simulation_time())
        && error.is_none()
    {
        error = Some(message);
    }
    let passed = error.is_none() && ctx.assertions.iter().all(|a| a.passed);
    TestResultDetailed {
        passed,
        assertions: ctx.assertions,
        error,
    }
}

struct DetailedExecContext {
    assertions: Vec<AssertionResult>,
    current_time: u64,
    // Physical event time for concurrent processes; single-process runs use ticks.
    scheduler_time: Option<u64>,
    tick_limit: Option<u64>,
    tick_limit_reached: bool,
    random: RandomTable,
}

impl DetailedExecContext {
    fn simulation_time(&self) -> u64 {
        self.scheduler_time.unwrap_or(self.current_time)
    }

    fn at_tick_limit(&self) -> bool {
        self.tick_limit
            .is_some_and(|limit| self.current_time >= limit)
    }
}

fn assert_event_args(message: &Option<AssertMessage>) -> &[CompiledAssertArg] {
    match message {
        Some(AssertMessage::Formatted { args, .. }) | Some(AssertMessage::DynamicArgs(args)) => {
            args
        }
        None => &[],
    }
}

fn write_u64_volatile(base: *mut u8, byte_offset: usize, value: u64) {
    unsafe {
        std::ptr::write_volatile(base.add(byte_offset) as *mut u64, value);
    }
}

fn load_u64_acquire(base: *const u8, byte_offset: usize) -> u64 {
    unsafe { (*(base.add(byte_offset) as *const AtomicU64)).load(Ordering::Acquire) }
}

fn store_u64_release(base: *mut u8, byte_offset: usize, value: u64) {
    unsafe {
        (*(base.add(byte_offset) as *const AtomicU64)).store(value, Ordering::Release);
    }
}

struct DrainedAssertionEvents {
    last_message: Option<Vec<u8>>,
    fatal_message: Option<Vec<u8>>,
    /// The design executed `$finish`, which ends the run (IEEE 1800-2023 20.2).
    finished: bool,
}

/// Forward testbench `$display` / `$write` output without contaminating the
/// machine-readable test result emitted by the CLI on stdout.  In particular,
/// UART models commonly use one `$write("%c", ...)` per transmitted byte, so
/// preserve the event bytes verbatim and flush it promptly.
fn forward_display(message: &[u8], newline: bool) {
    use std::io::Write as _;

    let mut stderr = std::io::stderr().lock();
    let _ = stderr.write_all(message);
    if newline {
        let _ = stderr.write_all(b"\n");
    }
    let _ = stderr.flush();
}

fn publish_tb_assert_event<B: SimBackend>(
    sim: &mut Simulator<B>,
    site_id: u32,
    message: &Option<AssertMessage>,
) {
    let args = assert_event_args(message)
        .iter()
        .map(|arg| eval_expr(sim, &arg.expr).to_biguint())
        .collect::<Vec<_>>();
    let layout = sim.layout();
    let Some(site_layout) = layout
        .runtime_event_site_layouts
        .get(site_id as usize)
        .cloned()
    else {
        return;
    };
    let capacity = layout.runtime_event_capacity;
    if capacity == 0 {
        return;
    }
    let slot_size = layout.runtime_event_slot_size;
    let (event_ptr_const, buffer_size) = sim.backend_ref().runtime_event_buffer_as_ptr();
    if RUNTIME_EVENT_HEADER_SIZE > buffer_size {
        return;
    }
    let event_ptr = event_ptr_const as *mut u8;
    let seq = load_u64_acquire(event_ptr_const, 0);
    let slot = (seq as usize) & (capacity - 1);
    let slot_base = RUNTIME_EVENT_HEADER_SIZE + slot * slot_size;
    if slot_base + slot_size > buffer_size {
        return;
    }

    store_u64_release(
        event_ptr,
        slot_base + RUNTIME_EVENT_SLOT_SEQ_OFFSET,
        RUNTIME_EVENT_WRITING,
    );
    write_u64_volatile(
        event_ptr,
        slot_base + RUNTIME_EVENT_SLOT_SITE_OFFSET,
        site_id as u64,
    );
    let arg_count = args.len().min(site_layout.args.len());
    write_u64_volatile(
        event_ptr,
        slot_base + RUNTIME_EVENT_SLOT_ARG_COUNT_OFFSET,
        arg_count as u64,
    );

    for (idx, value) in args.iter().take(arg_count).enumerate() {
        let arg_layout = &site_layout.args[idx];
        let words = value.to_u64_digits();
        for word_idx in 0..arg_layout.word_count {
            let value_word = words.get(word_idx).copied().unwrap_or(0);
            write_u64_volatile(
                event_ptr,
                slot_base
                    + RUNTIME_EVENT_SLOT_PAYLOAD_OFFSET
                    + (arg_layout.value_word_offset + word_idx) * 8,
                value_word,
            );
            write_u64_volatile(
                event_ptr,
                slot_base
                    + RUNTIME_EVENT_SLOT_PAYLOAD_OFFSET
                    + (arg_layout.mask_word_offset + word_idx) * 8,
                0,
            );
        }
    }

    store_u64_release(event_ptr, slot_base + RUNTIME_EVENT_SLOT_SEQ_OFFSET, seq);
    store_u64_release(event_ptr, 0, seq.wrapping_add(1));
}

fn drain_runtime_assertions<B: SimBackend>(
    sim: &mut Simulator<B>,
    ctx: &mut DetailedExecContext,
    location: Option<&SourceLocation>,
) -> DrainedAssertionEvents {
    let mut last_message = None;
    let mut fatal_message = None;
    let mut finished = false;
    let format_ctx = RuntimeFormatContext {
        tb_time: Some(ctx.simulation_time()),
        scope: None,
    };
    for event in sim.drain_runtime_events_deferred_with_context(format_ctx) {
        match event {
            // Only errors fail a test (IEEE 1800-2023 20.10).
            RuntimeEvent::Report(report) if report.severity < Severity::Error => {
                forward_display(&report.message, true)
            }
            RuntimeEvent::Report(report) => {
                let message = report.message;
                last_message = Some(message.clone());
                if report.severity == Severity::Fatal && fatal_message.is_none() {
                    fatal_message = Some(message.clone());
                }
                ctx.assertions.push(AssertionResult {
                    passed: false,
                    message: Some(message),
                    location: location.cloned(),
                });
            }
            RuntimeEvent::Missed { count } => {
                let message = format!("missed {count} runtime events").into_bytes();
                last_message = Some(message.clone());
                ctx.assertions.push(AssertionResult {
                    passed: false,
                    message: Some(message),
                    location: None,
                });
            }
            RuntimeEvent::Display { message } => forward_display(&message, true),
            RuntimeEvent::Write { message } => forward_display(&message, false),
            // The testbench has no interactive mode to suspend into.
            RuntimeEvent::Finish | RuntimeEvent::Stop => finished = true,
        }
    }
    DrainedAssertionEvents {
        last_message,
        fatal_message,
        finished,
    }
}

struct ProcessWait<Event> {
    clock: Event,
    remaining: u64,
    period: u64,
    resume_at: Option<u64>,
    reset: Option<(SignalRef, Option<Event>, u8)>,
}

struct ProcessClock<Event> {
    event: Event,
    period: u64,
    next_edge: u64,
    falling_edge: Option<u64>,
}

// Clock generators start low even when the design uses four-state storage.
// Otherwise X's nonzero payload can make the first scheduled 1 look like a
// steady high to the edge detector, losing the first clock/reset cycle.
// Returns the clock events, declared generators first.
fn initialize_process_clocks<B: SimBackend>(
    sim: &mut Simulator<B>,
    testbench: &CompiledTestbench<B>,
) -> Vec<B::Event> {
    fn referenced<B: SimBackend>(statements: &[TestbenchStatement<B>], clocks: &mut Vec<B::Event>) {
        for statement in statements {
            match statement {
                GenericTestbenchStatement::ClockNext { clock_event, .. }
                | GenericTestbenchStatement::ResetAssert { clock_event, .. } => {
                    clocks.push(*clock_event);
                }
                GenericTestbenchStatement::If {
                    then_block,
                    else_block,
                    ..
                } => {
                    referenced::<B>(then_block, clocks);
                    referenced::<B>(else_block, clocks);
                }
                GenericTestbenchStatement::For { body, .. } => referenced::<B>(body, clocks),
                _ => {}
            }
        }
    }
    let mut clocks: Vec<_> = testbench.clocks().iter().map(|clock| clock.event).collect();
    referenced::<B>(testbench.statements(), &mut clocks);
    let mut unique: Vec<B::Event> = Vec::with_capacity(clocks.len());
    for event in clocks {
        if unique.iter().all(|known| known.id() != event.id()) {
            let signal = sim.backend.resolve_signal(&event.addr());
            sim_set_u64(sim, signal, 0);
            unique.push(event);
        }
    }
    unique
}

fn exec_testbench<B: SimBackend>(
    sim: &mut Simulator<B>,
    testbench: &CompiledTestbench<B>,
    ctx: &mut DetailedExecContext,
) -> ExecResult {
    let blocks: Vec<_> = testbench.processes().collect();
    let clocks = initialize_process_clocks(sim, testbench);
    // A finished kernel returns without passing a collection point, so
    // collect its temporaries here; no process is suspended anymore.
    if let Some(result) = compiled::run(sim, testbench, ctx) {
        sim.heap_safepoint();
        return result;
    }
    // One process on one clock spends time only in that clock's periods, so
    // stepping them in place is the whole schedule.
    if blocks.is_empty() {
        return ExecResult::Continue;
    }
    if blocks.len() == 1 && clocks.len() <= 1 {
        let mut process = match native::Process::<B>::compile(blocks[0], &testbench.kernels()[0]) {
            Ok(process) => process,
            Err(error) => return ExecResult::Fail(format!("testbench compilation: {error}")),
        };
        let result = match process.resume(sim, ctx, &mut None, &mut Vec::new(), false) {
            Poll::Ready(result) => result,
            Poll::Pending => unreachable!("synchronous clock execution cannot suspend"),
        };
        sim.heap_safepoint();
        return result;
    }
    // Suspended processes keep their temporaries outside state memory, so
    // only the scheduler loop collects, with those temporaries as roots.
    let blocked = std::mem::replace(&mut sim.heap_safepoints_blocked, true);
    let result = exec_processes(sim, testbench, ctx, &blocks);
    sim.heap_safepoints_blocked = blocked;
    result
}

fn exec_processes<B: SimBackend>(
    sim: &mut Simulator<B>,
    testbench: &CompiledTestbench<B>,
    ctx: &mut DetailedExecContext,
    blocks: &[&[TestbenchStatement<B>]],
) -> ExecResult {
    // The compiler's function slots are shared by inline call sites. Keep
    // only these temporaries private; design variables remain shared between
    // initial processes and all RTL. Capture masks as well as payloads.
    let initial_locals: Vec<_> = testbench
        .private_signals()
        .iter()
        .map(|&signal| sim.backend.get_four_state(signal))
        .collect();
    let mut locals = vec![initial_locals; blocks.len()];
    let mut waits: Vec<Option<ProcessWait<B::Event>>> = blocks.iter().map(|_| None).collect();
    let mut reset_edges = Vec::new();
    let mut processes = match blocks
        .iter()
        .zip(testbench.kernels())
        .map(|(block, kernel)| native::Process::<B>::compile(block, kernel).map(Some))
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(processes) => processes,
        Err(error) => return ExecResult::Fail(format!("testbench compilation: {error}")),
    };
    // Every clock generator runs from time 0, whether or not a process
    // waits on it.
    let mut clocks: Vec<ProcessClock<B::Event>> = testbench
        .clocks()
        .iter()
        .map(|clock| ProcessClock {
            event: clock.event,
            period: clock.period.max(2),
            next_edge: 0,
            falling_edge: None,
        })
        .collect();
    // Capture the baseline before runnable processes drive reset. Reset
    // assertions are signal writes AND events at this timestamp, even if
    // their clock's next edge is still in the future. Rebasing after the
    // writes would silently absorb the asynchronous transition.
    {
        if sim.dirty
            && let Err(error) = settle_testbench(sim)
        {
            return ExecResult::Fail(format!("eval_comb: {error}"));
        }
        let mut state = sim
            .component_simulation
            .take()
            .unwrap_or_else(|| Box::new(crate::simulation::simulation_state(sim)));
        state.synchronize_event_values(&sim.backend);
        sim.component_simulation = Some(state);
    }
    let clock_batch = clock_batch::ClockBatch::new(sim);
    let mut now = 0;
    // Reuse per-slot batches instead of allocating on each rising and falling
    // edge; their capacities can grow when a wait introduces another clock.
    let mut falling = Vec::with_capacity(clocks.len());
    let mut fired = Vec::with_capacity(clocks.len());
    let mut resets = Vec::with_capacity(waits.len());
    let mut events = Vec::with_capacity(clocks.len() + waits.len());
    let mut moves = Vec::with_capacity(clocks.len());
    loop {
        ctx.scheduler_time = Some(now);
        let draining = ctx.at_tick_limit();
        if draining {
            ctx.tick_limit_reached = true;
            // Stop starting work, but finish edges/releases already scheduled
            // by the final permitted tick before returning the simulator state.
            if clocks.iter().all(|clock| clock.falling_edge.is_none())
                && waits
                    .iter()
                    .all(|wait| wait.as_ref().is_none_or(|wait| wait.resume_at.is_none()))
            {
                return ExecResult::Finished;
            }
        }
        // A waiter resumes at the end of its last cycle. Collect all edge
        // polarities for this timestamp before evaluating clocked logic.
        falling.clear();
        falling.extend(clocks.iter_mut().filter_map(|clock| {
            if clock.falling_edge == Some(now) {
                clock.falling_edge = None;
                Some(clock.event)
            } else {
                None
            }
        }));
        for wait in &mut waits {
            if let Some(pending) = wait.as_ref()
                && pending.resume_at == Some(now)
            {
                if let Some((signal, event, value)) = pending.reset {
                    sim_set_u64(sim, signal, value.into());
                    if let Some(event) = event {
                        reset_edges.push((event, value));
                    }
                }
                *wait = None;
            }
        }
        // Resume ready processes in declaration order until their next wait,
        // completion, or a global stop. Each native call returns before another
        // process or a clock domain can run.
        let mut stopped = false;
        for ((process, wait), locals) in processes.iter_mut().zip(&mut waits).zip(&mut locals) {
            if draining || wait.is_some() {
                continue;
            }
            let Some(executable) = process else { continue };
            for (&signal, (value, mask)) in testbench.private_signals().iter().zip(locals.iter()) {
                write_testbench_signal(sim, signal, |sim| {
                    sim.set_four_state(signal, value.clone(), mask.clone());
                });
            }
            let result = executable.resume(sim, ctx, wait, &mut reset_edges, true);
            for (&signal, value) in testbench.private_signals().iter().zip(locals.iter_mut()) {
                *value = sim.backend.get_four_state(signal);
            }
            match result {
                Poll::Ready(ExecResult::Continue) => *process = None,
                Poll::Ready(ExecResult::Finished) => {
                    stopped = true;
                    break;
                }
                Poll::Ready(result) => return result,
                Poll::Pending => debug_assert!(wait.is_some()),
            }
        }
        // No process is running, so every string handle is in state memory or
        // in a suspended process's saved temporaries.
        collect_testbench_heap(sim, testbench.private_signals(), &locals);
        for wait in &waits {
            if let Some(wait) = wait.as_ref()
                && wait.remaining != 0
            {
                if let Some(clock) = clocks
                    .iter_mut()
                    .find(|clock| clock.event.id() == wait.clock.id())
                {
                    clock.next_edge = clock.next_edge.max(now);
                } else {
                    clocks.push(ProcessClock {
                        event: wait.clock,
                        period: wait.period.max(2),
                        next_edge: now,
                        falling_edge: None,
                    });
                }
            }
        }
        fired.clear();
        fired.extend(
            clocks
                .iter()
                .filter(|clock| !draining && !stopped && clock.next_edge == now)
                .map(|clock| clock.event),
        );
        {
            resets.clear();
            resets.extend(waits.iter().filter_map(|wait| {
                let wait = wait.as_ref()?;
                wait.reset.map(|(_, event, _)| (wait.clock, event))
            }));
            events.clear();
            events.extend(
                falling
                    .iter()
                    .map(|&event| (event, 0))
                    .chain(fired.iter().map(|&event| (event, 1)))
                    .chain(reset_edges.drain(..)),
            );
            let result = step_process_events(
                sim,
                &events,
                &resets,
                now,
                !fired.is_empty(),
                ctx,
                &mut moves,
                clock_batch.as_ref(),
            );
            if !matches!(result, ExecResult::Continue) {
                return result;
            }
            for clock in &mut clocks {
                if fired.iter().any(|event| event.id() == clock.event.id()) {
                    let Some(next) = now.checked_add(clock.period) else {
                        return ExecResult::Fail("testbench clock time overflow".into());
                    };
                    clock.next_edge = next;
                    clock.falling_edge = Some(now + clock.period - clock.period / 2);
                }
            }
            for wait in &mut waits {
                if let Some(wait) = wait.as_mut()
                    && wait.remaining != 0
                    && let Some(clock) = clocks
                        .iter()
                        .find(|clock| clock.event.id() == wait.clock.id())
                    && fired.iter().any(|event| event.id() == wait.clock.id())
                {
                    wait.remaining -= 1;
                    if wait.remaining == 0 {
                        wait.resume_at = Some(clock.next_edge);
                    }
                }
            }
        }
        if stopped {
            return ExecResult::Finished;
        }
        if processes.iter().all(Option::is_none) {
            return ExecResult::Continue;
        }
        let next_edge = clocks
            .iter()
            .filter(|_| !ctx.at_tick_limit())
            .map(|clock| clock.next_edge)
            .min();
        let next_falling = clocks.iter().filter_map(|clock| clock.falling_edge).min();
        let next_resume = waits
            .iter()
            .filter_map(|wait| wait.as_ref().and_then(|wait| wait.resume_at))
            .min();
        if let Some(next) = [next_edge, next_falling, next_resume]
            .into_iter()
            .flatten()
            .min()
        {
            now = next;
        } else if ctx.at_tick_limit() {
            ctx.tick_limit_reached = true;
            return ExecResult::Finished;
        } else {
            unreachable!("a live process has a pending wait");
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn step_process_events<B: SimBackend>(
    sim: &mut Simulator<B>,
    events: &[(B::Event, u8)],
    resets: &[(B::Event, Option<B::Event>)],
    time: u64,
    rising: bool,
    ctx: &mut DetailedExecContext,
    moves: &mut Vec<(B::Event, celox_runtime::Edge)>,
    clock_batch: Option<&clock_batch::ClockBatch>,
) -> ExecResult {
    let mut state = sim
        .component_simulation
        .take()
        .unwrap_or_else(|| Box::new(crate::simulation::simulation_state(sim)));
    let edges = sim.event_edges();
    let schema = &sim.program.runtime_schema;
    // Plain RTL needs simultaneous edge evaluation, not the timed process
    // queue. The edge engine already skips inactive clock edges that cannot
    // run a domain. Keep reset transitions on the queue: reset_gen writes
    // their levels before submitting them, so its saved baseline matters.
    // Latches need inactive phases settled before the next edge. VCD needs
    // the queue's per-timestamp publication, even when no domain fires.
    // A TB assignment can change an unrelated event (including an async
    // reset). Preserve the queue's old baseline until it detects that change.
    // Dirty combinational logic may propagate such a change, so it also takes
    // the queue path even when no event signal has changed yet.
    let direct_edges = !events.is_empty()
        && !sim.dirty
        && !state.has_unsampled_event_changes(&sim.backend)
        && sim.vcd_writer.is_none()
        && schema.persistent_comb_state.is_empty()
        && sim.components.is_empty()
        && !sim.schedule.timed
        && schema.time_slots.is_empty()
        && schema.watches.is_empty()
        && schema.comb_observers.is_empty()
        && schema.sample_shadows.is_empty()
        && events.iter().all(|(event, _)| {
            matches!(
                edges[event.id()].kind,
                Some(
                    celox_design::DomainKind::ClockPosedge | celox_design::DomainKind::ClockNegedge
                )
            )
        });
    if !direct_edges
        && sim.dirty
        && let Err(error) = settle_testbench(sim)
    {
        sim.component_simulation = Some(state);
        return ExecResult::Fail(format!("eval_comb: {error}"));
    }
    for (clock, reset) in resets {
        sim.components
            .begin_reset_clock_cycles(clock.id(), reset.map(|event| event.id()));
    }
    // Submit all simultaneous edges before stepping: every clocked process
    // samples pre-edge values, before any nonblocking assignment is committed.
    let result = if direct_edges {
        moves.clear();
        moves.extend(events.iter().map(|&(event, value)| {
            (
                event,
                if value == 0 {
                    celox_runtime::Edge::Falling
                } else {
                    celox_runtime::Edge::Rising
                },
            )
        }));
        let result = clock_batch
            .and_then(|batch| batch.run(sim, events, time))
            .unwrap_or_else(|| sim.edges(moves).map_err(|error| error.to_string()))
            .map(|()| None);
        // A later reset can return to the queue. Its pre-write baseline must
        // include every clock and derived-domain update performed here.
        state.synchronize_event_values(&sim.backend);
        result
    } else {
        for &(event, value) in events {
            state.schedule(
                event,
                sim.backend.resolve_signal(&event.addr()),
                time,
                value,
            );
        }
        if events.is_empty() {
            state.settle_at(sim, time)
        } else {
            state.step(sim)
        }
        .map_err(|error| error.to_string())
    };
    sim.components.end_reset_cycles();
    sim.component_simulation = Some(state);
    if rising {
        ctx.current_time = ctx.current_time.saturating_add(1);
    }
    let drained = drain_runtime_assertions(sim, ctx, None);
    if let Some(message) = drained.fatal_message {
        return ExecResult::Fail(message_diagnostic(&message));
    }
    if let Err(error) = result {
        return ExecResult::Fail(error.to_string());
    }
    if drained.finished || sim.components.finish_requested() {
        return ExecResult::Finished;
    }
    ExecResult::Continue
}

fn exec_one_detailed<B: SimBackend>(
    sim: &mut Simulator<B>,
    stmt: &TestbenchStatement<B>,
    ctx: &mut DetailedExecContext,
) -> ExecResult {
    let tick_limit_reached = |ctx: &mut DetailedExecContext| {
        let reached = ctx
            .tick_limit
            .is_some_and(|limit| ctx.current_time >= limit);
        if reached {
            ctx.tick_limit_reached = true;
        }
        reached
    };
    if tick_limit_reached(ctx) {
        return ExecResult::Finished;
    }
    match stmt {
        GenericTestbenchStatement::ClockNext {
            clock_event, count, ..
        } => match eval_clock_count(sim, count) {
            Ok(n) => {
                let progress_every = sim.diagnostics.testbench_progress_every;
                let mut remaining = n;
                while remaining != 0 {
                    if tick_limit_reached(ctx) {
                        return ExecResult::Finished;
                    }
                    let mut batch = remaining;
                    if let Some(limit) = ctx.tick_limit {
                        batch = batch.min(limit.saturating_sub(ctx.current_time));
                    }
                    if let Some(every) = progress_every.filter(|every| *every != 0) {
                        batch = batch.min(every - ctx.current_time % every);
                    }
                    let (completed, result) = if sim.components.has_scheduled_components() {
                        (1, tick_component_clock(sim, *clock_event, ctx.current_time))
                    } else {
                        let (completed, result) = clock_periods(sim, *clock_event, batch);
                        (completed, result.map_err(|error| error.to_string()))
                    };
                    if completed == 0 || completed > batch {
                        return ExecResult::Fail(
                            "backend made invalid progress in a deferred tick batch".into(),
                        );
                    }
                    ctx.current_time = ctx.current_time.saturating_add(completed);
                    remaining -= completed;
                    if let Err(e) = result {
                        let drained = drain_runtime_assertions(sim, ctx, None);
                        if let Some(message) = drained.fatal_message {
                            return ExecResult::Fail(message_diagnostic(&message));
                        }
                        return ExecResult::Fail(e);
                    }
                    if let Some(every) = progress_every
                        && every != 0
                        && ctx.current_time.is_multiple_of(every)
                    {
                        tracing::debug!("[testbench-progress] tick={}", ctx.current_time);
                    }
                    let drained = drain_runtime_assertions(sim, ctx, None);
                    if let Some(message) = drained.fatal_message {
                        return ExecResult::Fail(message_diagnostic(&message));
                    }
                    if drained.finished || sim.components.finish_requested() {
                        return ExecResult::Finished;
                    }
                }
                ExecResult::Continue
            }
            Err(error) => ExecResult::Fail(error.to_string()),
        },
        GenericTestbenchStatement::ResetAssert {
            reset_signal,
            reset_event,
            clock_event,
            duration,
            assert_value,
            deassert_value,
            ..
        } => match eval_clock_count(sim, duration) {
            Ok(duration) => {
                let duration = duration.max(1);
                sim_set_u64(sim, *reset_signal, (*assert_value).into());
                let mut remaining = duration;
                while remaining != 0 {
                    if tick_limit_reached(ctx) {
                        return ExecResult::Finished;
                    }
                    let mut batch = remaining;
                    if let Some(limit) = ctx.tick_limit {
                        batch = batch.min(limit.saturating_sub(ctx.current_time));
                    }
                    let component_aware = sim.components.has_scheduled_components();
                    let (completed, result) = if component_aware {
                        (
                            1,
                            tick_component_reset(
                                sim,
                                *reset_event,
                                *clock_event,
                                ctx.current_time.saturating_add(1),
                            ),
                        )
                    } else {
                        let (completed, result) = clock_periods(sim, *clock_event, batch);
                        (completed, result.map_err(|error| error.to_string()))
                    };
                    if completed == 0 || completed > batch {
                        return ExecResult::Fail(
                            "backend made invalid progress in a reset tick batch".into(),
                        );
                    }
                    ctx.current_time = ctx.current_time.saturating_add(completed);
                    remaining -= completed;
                    if let Err(e) = result {
                        let drained = drain_runtime_assertions(sim, ctx, None);
                        if let Some(message) = drained.fatal_message {
                            return ExecResult::Fail(message_diagnostic(&message));
                        }
                        return ExecResult::Fail(format!("reset: {e}"));
                    }
                    let drained = drain_runtime_assertions(sim, ctx, None);
                    if let Some(message) = drained.fatal_message {
                        return ExecResult::Fail(message_diagnostic(&message));
                    }
                    if drained.finished || sim.components.finish_requested() {
                        return ExecResult::Finished;
                    }
                }
                sim_set_u64(sim, *reset_signal, (*deassert_value).into());
                ExecResult::Continue
            }
            Err(error) => ExecResult::Fail(error.to_string()),
        },
        GenericTestbenchStatement::Assert {
            expr,
            site_id,
            continue_on_fail,
            message,
            location,
        } => {
            let settle = if message.is_some() {
                settle_testbench(sim)
            } else {
                settle_testbench_expr(sim, expr)
            };
            if let Err(e) = settle {
                return ExecResult::Fail(format!("eval_comb: {e}"));
            }
            let passed = !eval_expr(sim, expr).is_zero();
            if passed {
                let rendered_message = render_assert_message(sim, message, ctx.simulation_time());
                ctx.assertions.push(AssertionResult {
                    passed,
                    message: rendered_message.clone(),
                    location: location.clone(),
                });
                ExecResult::Continue
            } else {
                publish_tb_assert_event(sim, *site_id, message);
                let rendered_message = drain_runtime_assertions(sim, ctx, location.as_ref())
                    .last_message
                    .or_else(|| render_assert_message(sim, message, ctx.simulation_time()));
                if !continue_on_fail {
                    ExecResult::Fail(message_diagnostic(
                        &rendered_message.unwrap_or_else(|| b"assertion failed".to_vec()),
                    ))
                } else {
                    ExecResult::Continue
                }
            }
        }
        GenericTestbenchStatement::Display { message, newline } => {
            if let Err(e) = settle_testbench(sim) {
                return ExecResult::Fail(format!("eval_comb: {e}"));
            }
            let rendered =
                render_assert_message(sim, message, ctx.simulation_time()).unwrap_or_default();
            forward_display(&rendered, *newline);
            ExecResult::Continue
        }
        GenericTestbenchStatement::If { .. }
        | GenericTestbenchStatement::For { .. }
        | GenericTestbenchStatement::Assign { .. }
        | GenericTestbenchStatement::Break => {
            unreachable!("statement is compiled into the process kernel")
        }
        GenericTestbenchStatement::WriteMemory { signal, writes } => {
            if let Err(e) = settle_testbench(sim) {
                return ExecResult::Fail(format!("eval_comb: {e}"));
            }
            write_testbench_signal(sim, *signal, |sim| {
                sim.apply_testbench_memory_writes(*signal, writes)
            });
            ExecResult::Continue
        }
        GenericTestbenchStatement::RandomSeed { handle, value } => {
            if let Err(e) = settle_testbench(sim) {
                return ExecResult::Fail(format!("eval_comb: {e}"));
            }
            ctx.random.seed(handle, eval_expr(sim, value).to_u64());
            ExecResult::Continue
        }
        GenericTestbenchStatement::RandomGet {
            handle,
            width,
            signed,
            ret,
        } => {
            let value = ctx.random.get(handle, *width);
            if let Some(ret) = ret {
                if ret.selection.is_some()
                    && let Err(e) = settle_testbench(sim)
                {
                    return ExecResult::Fail(format!("eval_comb: {e}"));
                }
                sim_set_random_target(sim, ret, value, *width, *signed);
            }
            ExecResult::Continue
        }
        GenericTestbenchStatement::RandomGetRange {
            handle,
            min,
            max,
            width,
            signed,
            ret,
        } => {
            if let Err(e) = settle_testbench(sim) {
                return ExecResult::Fail(format!("eval_comb: {e}"));
            }
            let min = eval_expr(sim, min).to_u64();
            let max = eval_expr(sim, max).to_u64();
            let value = ctx.random.get_range(handle, min, max, *width, *signed);
            if let Some(ret) = ret {
                sim_set_random_target(sim, ret, value, *width, *signed);
            }
            ExecResult::Continue
        }
        GenericTestbenchStatement::RandomGetSeed { handle, ret } => {
            let seed = ctx.random.get_seed(handle);
            if let Some(ret) = ret {
                if ret.selection.is_some()
                    && let Err(e) = settle_testbench(sim)
                {
                    return ExecResult::Fail(format!("eval_comb: {e}"));
                }
                sim_set_target(sim, ret, TbValue::U64(seed));
            }
            ExecResult::Continue
        }
        GenericTestbenchStatement::ComponentMethod {
            instance,
            method,
            args,
            ret,
            ret_width,
            ret_signed,
            ret_strict,
        } => {
            if sim.dirty
                && let Err(error) = settle_testbench(sim)
            {
                return ExecResult::Fail(format!("eval_comb: {error}"));
            }
            let host_args = args
                .iter()
                .map(|arg| {
                    crate::component::host_value_from_argument(
                        eval_expr(sim, &arg.expr),
                        arg.width,
                        arg.is_string,
                    )
                })
                .collect::<Vec<_>>();
            let (returned, writes) = match sim.components.call_method(
                instance,
                method,
                &host_args,
                ctx.simulation_time(),
                &mut sim.backend,
            ) {
                Ok(value) => value,
                Err(message) => return ExecResult::Fail(message),
            };
            apply_component_writes(sim, writes);
            if sim.components.finish_requested() {
                return ExecResult::Finished;
            }
            let Some(ret) = ret else {
                return ExecResult::Continue;
            };
            let Some((value, width)) = crate::component::host_bits(&returned) else {
                return ExecResult::Fail(format!(
                    "component method `{instance}.{method}` returned no bit value"
                ));
            };
            if let Some(expected) = ret_width
                && width != *expected
            {
                return ExecResult::Fail(format!(
                    "component method `{method}` declares a {expected}-bit return value but returned {width} bits"
                ));
            }
            if ret_width.is_none() && *ret_strict && width > 64 {
                return ExecResult::Fail(format!(
                    "component method `{method}` returned {width} bits; the expression form carries at most 64 bits"
                ));
            }
            sim_set_target(
                sim,
                ret,
                TbValue::Wide(resize_component_return(
                    value,
                    width as usize,
                    *ret_signed,
                    ret.width,
                )),
            );
            ExecResult::Continue
        }
        GenericTestbenchStatement::Finish => ExecResult::Finished,
    }
}

/// Run up to `count` periods of a testbench clock generator's signal, each a
/// rise followed by a fall, and return how many ran. A run that the event's
/// own domain alone reacts to fires that domain once per period with the
/// signal left low, which is all a period does then.
fn clock_periods<B: SimBackend>(
    sim: &mut Simulator<B>,
    event: B::Event,
    count: u64,
) -> (u64, Result<(), crate::RuntimeErrorCode>) {
    if sim.period_runs_only_active_edge(event) {
        return sim.tick_deferred_comb_many(event, count);
    }
    let result = sim
        .edge(event, celox_runtime::Edge::Rising)
        .and_then(|()| sim.edge(event, celox_runtime::Edge::Falling));
    (1, result)
}

fn tick_component_reset<B: SimBackend>(
    sim: &mut Simulator<B>,
    reset_event: Option<B::Event>,
    clock_event: B::Event,
    time: u64,
) -> Result<(), String> {
    sim.components
        .begin_reset_cycles(reset_event.map(|event| event.id()));
    let result = tick_component_clock(sim, clock_event, time.saturating_sub(1));
    sim.components.end_reset_cycles();
    result
}

fn tick_component_clock<B: SimBackend>(
    sim: &mut Simulator<B>,
    event: B::Event,
    completed_cycles: u64,
) -> Result<(), String> {
    let mut state = sim
        .component_simulation
        .take()
        .ok_or_else(|| "component event scheduler is not initialized".to_string())?;
    if sim.dirty {
        if let Err(error) = settle_testbench(sim) {
            sim.component_simulation = Some(state);
            return Err(error.to_string());
        }
    }
    state.synchronize_event_values(&sim.backend);
    let signal = sim.backend.resolve_signal(&event.addr());
    let high_time = completed_cycles.saturating_mul(2);
    state.schedule(event, signal, high_time, 1);
    let result = state
        .step(sim)
        .and_then(|_| {
            state.schedule(event, signal, high_time.saturating_add(1), 0);
            state.step(sim)
        })
        .map(|_| ())
        .map_err(|error| error.to_string());
    sim.component_simulation = Some(state);
    result
}

fn apply_component_writes<B: SimBackend>(
    sim: &mut Simulator<B>,
    writes: Vec<crate::component::ComponentWrite>,
) {
    if writes.is_empty() {
        return;
    }
    for write in writes {
        write.apply(&mut sim.backend);
    }
    sim.dirty = true;
}

fn root_testbench_name<B: SimBackend>(sim: &Simulator<B>) -> String {
    sim.program
        .design
        .root_instance()
        .map(|instance| instance.module_name.clone())
        .unwrap_or_else(|| "testbench".to_string())
}

fn resize_component_return(
    value: BigUint,
    source_width: usize,
    signed: bool,
    destination_width: usize,
) -> BigUint {
    if destination_width == 0 {
        return BigUint::default();
    }
    let source_mask = if source_width == 0 {
        BigUint::default()
    } else {
        (BigUint::from(1u8) << source_width) - BigUint::from(1u8)
    };
    let mut value = value & source_mask;
    if signed
        && source_width != 0
        && source_width < destination_width
        && value.bit((source_width - 1) as u64)
    {
        let destination_mask = (BigUint::from(1u8) << destination_width) - BigUint::from(1u8);
        let extension = destination_mask ^ ((BigUint::from(1u8) << source_width) - 1u8);
        value |= extension;
    }
    let destination_mask = (BigUint::from(1u8) << destination_width) - BigUint::from(1u8);
    value & destination_mask
}

#[cfg(all(test, feature = "host-runtime"))]
mod tests {
    use std::error::Error as _;

    use super::*;
    use crate::{Simulator, TestResult};

    #[test]
    fn evaluation_error_preserves_runtime_error_source() {
        let error = TestbenchEvaluationError::EvalComb {
            source: RuntimeErrorCode::InternalError,
        };

        assert!(error.source().unwrap().is::<RuntimeErrorCode>());
    }

    #[test]
    fn random_values_sign_extend_for_wider_destinations() {
        assert_eq!(
            random_value_for_destination(0x80, 8, true, 16),
            BigUint::from(0xff80u16)
        );
        assert_eq!(
            random_value_for_destination(0x7f, 8, true, 16),
            BigUint::from(0x007fu16)
        );
        assert_eq!(
            random_value_for_destination(0x80, 8, false, 16),
            BigUint::from(0x0080u16)
        );
    }

    #[test]
    fn component_returns_resize_with_declared_signedness() {
        assert_eq!(
            resize_component_return(BigUint::from(0x80u8), 8, true, 16),
            BigUint::from(0xff80u16)
        );
        assert_eq!(
            resize_component_return(BigUint::from(0x80u8), 8, false, 16),
            BigUint::from(0x0080u16)
        );
        assert_eq!(
            resize_component_return(BigUint::from(0x1234u16), 16, false, 8),
            BigUint::from(0x34u8)
        );
    }

    #[test]
    fn traced_build_registers_compiled_testbench_runtime_event_sites() {
        let code = r#"
            #[test(t)]
            module t {
                initial {
                    $assert_continue(1'b0, "continue failure");
                    $finish();
                }
            }
        "#;
        let mut sim = Simulator::builder(code, "t").build_with_trace().unwrap();
        let tb = compile_initial_testbench(&sim).unwrap();

        assert_eq!(
            run_compiled_testbench(&mut sim, &tb),
            TestResult::Fail("continue failure".to_string()),
        );
    }

    #[test]
    fn expression_evaluation_preserves_sparse_vcd_tracking() {
        let code = r#"
            #[test(t)]
            module t {
                var a: logic<8>;
                var b: logic<8>;
                initial {
                    a = 1;
                    b = a;
                    if b == 1 {
                        $assert(a == b);
                        $display("a=%h", a);
                    }
                    $finish();
                }
            }
        "#;
        let dir = tempfile::tempdir().unwrap();
        let mut sim = Simulator::builder(code, "t")
            .vcd(dir.path().join("testbench.vcd"))
            .build()
            .unwrap();
        let tb = compile_initial_testbench(&sim).unwrap();
        sim.dump(0);
        assert_eq!(run_compiled_testbench(&mut sim, &tb), TestResult::Pass);
        sim.dump(1);
        let before = sim.vcd_statistics().unwrap();
        sim.dump(2);
        assert_eq!(
            sim.vcd_statistics().unwrap().comparisons,
            before.comparisons
        );
    }

    #[test]
    fn private_tb_writes_do_not_invalidate_hardware_but_port_aliases_do() {
        let source = r#"
            module Dut(a: input logic<8>, q: output logic<8>) { assign q = a + 1; }
            #[test(Top)] module Top {
                var a: logic<8>;
                var q: logic<8>;
                var scratch: logic<8>;
                inst dut: Dut(a, q);
                initial { scratch = 3; a = scratch; $assert(q == 4); $finish(); }
            }
        "#;
        let mut sim = Simulator::builder(source, "Top")
            .build_interpreter()
            .unwrap();
        let scratch = sim.signal("scratch");
        let a = sim.signal("a");
        let q = sim.signal("q");
        settle_testbench(&mut sim).unwrap();
        sim_set_u64(&mut sim, scratch, 3);
        assert!(!sim.dirty);
        sim_set_u64(&mut sim, a, 3);
        assert!(sim.dirty);
        settle_testbench(&mut sim).unwrap();
        assert_eq!(sim.get_as::<u8>(q), 4);
    }

    // Retained comb state keeps TB edges out of the compiled scheduler, so a
    // two-state case covering every selector value must not retain any.
    #[test]
    fn complete_binary_case_retains_no_comb_state() {
        let arms = (0..4)
            .map(|i| format!("2'd{i}: q = d + 8'd{i};"))
            .collect::<String>();
        let source = format!(
            "module Top(s: input logic<2>, d: input logic<8>, q: output logic<8>) {{
                always_comb {{ case s {{ {arms} }} }}
            }}"
        );
        let sim = Simulator::builder(&source, "Top")
            .build_interpreter()
            .unwrap();
        assert!(sim.program.runtime_schema.persistent_comb_state.is_empty());
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn native_image_retains_latch_observation_and_tb_dependencies() {
        let source = include_str!("../testdata/veryl/latch_initial.veryl");
        let image = Simulator::builder(source, "Top")
            .allow_inferred_latches(true)
            .compile_native()
            .unwrap()
            .into_program_image();
        let image =
            crate::NativeProgramImage::from_container_bytes(&image.to_container_bytes().unwrap())
                .unwrap();
        let mut sim = Simulator::builder("", "Top")
            .build_native_from_image(image)
            .unwrap();
        assert!(!sim.program.runtime_schema.persistent_comb_state.is_empty());
        assert!(!sim.program.rtl_accesses.is_empty());
        let tb = compile_initial_testbench(&sim).unwrap();
        assert_eq!(
            run_compiled_testbench_to_finish(&mut sim, &tb),
            TestResult::Pass
        );
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    fn native_clock_batches_preserve_odd_periods_and_tb_before_edge_order() {
        let source = r#"
            module Counter(clk: input clock, rst: input reset_async_low, q: output logic<8>) {
                always_ff (clk, rst) { if_reset { q = 0; } else { q += 1; } }
            }
            #[test(Top)] module Top {
                inst fast: $tb::clock_gen #(period: 3);
                inst slow: $tb::clock_gen #(period: 4);
                var rst_n: reset_async_low;
                var enable: logic;
                var gated: reset_async_low;
                assign gated = rst_n && enable;
                var a: logic<8>;
                var b: logic<8>;
                inst ca: Counter(clk: fast, rst: gated, q: a);
                inst cb: Counter(clk: slow, rst: gated, q: b);
                initial {
                    rst_n = 1;
                    enable = 0;
                    fast.next(4);
                    $assert(a == 0 && b == 0);
                    enable = 1;
                    fast.next(4);
                    // At time 24 both clocks are due, but the TB runs first.
                    $assert(a == 4 && b == 3);
                    fast.next(4);
                    $assert(a == 8 && b == 6);
                    $finish();
                }
                initial { slow.next(100); }
            }
        "#;
        for mode in [
            crate::SchedulerMode::Reference,
            crate::SchedulerMode::Compiled,
        ] {
            let mut sim = Simulator::builder(source, "Top")
                .four_state(false)
                .scheduler_mode(mode)
                .build_native()
                .unwrap();
            let batch = clock_batch::ClockBatch::new(&sim);
            assert_eq!(batch.is_some(), mode != crate::SchedulerMode::Reference);
            let tb = compile_initial_testbench(&sim).unwrap();
            assert_eq!(
                run_compiled_testbench_to_finish(&mut sim, &tb),
                TestResult::Pass
            );
            if let Some(batch) = batch {
                assert_eq!(sim.backend.get_as::<u64>(batch.count_slot()), 1);
            }
        }
    }

    #[test]
    fn native_tb_scheduler_runs_processes_and_multiple_clocks_without_host_roundtrips() {
        let source = r#"
            module Counter(clk: input clock, q: output bit<32>) {
                always_ff { q += 1; }
            }
            #[test(Top)] module Top {
                inst fast: $tb::clock_gen #(period: 3);
                inst slow: $tb::clock_gen #(period: 4);
                var a: u32; var b: u32; var observed: u32;
                inst ca: Counter(clk: fast, q: a);
                inst cb: Counter(clk: slow, q: b);
                initial { for i in 0..128 { fast.next(); observed = a; } $finish(); }
                initial { for i in 0..128 { slow.next(); } }
            }
        "#;
        // No clock-wait batching in the source: both TB processes resume each cycle.
        let mut results = Vec::new();
        for mode in [
            crate::SchedulerMode::Reference,
            crate::SchedulerMode::Compiled,
        ] {
            let mut sim = Simulator::builder(source, "Top")
                .four_state(false)
                .scheduler_mode(mode)
                .build_native()
                .unwrap();
            let tb = compile_initial_testbench(&sim).unwrap();
            compiled::take_counters();
            assert_eq!(
                run_compiled_testbench_to_finish(&mut sim, &tb),
                TestResult::Pass
            );
            let counters = compiled::take_counters();
            results.push((
                sim.get_as::<u32>(sim.signal("a")),
                sim.get_as::<u32>(sim.signal("b")),
                sim.get_as::<u32>(sim.signal("observed")),
            ));
            if mode == crate::SchedulerMode::Compiled {
                assert!(counters.0 < 10, "native entries: {counters:?}");
                assert!(counters.1 < 3, "host edge batches: {counters:?}");
            }
        }
        assert_eq!(results[0], results[1]);
        assert_eq!(results[1], (128, 96, 128));
    }

    #[test]
    fn compiled_tb_settle_continuations_are_not_limited_to_one_byte() {
        let mut source =
            String::from("#[test(Top)] module Top { var a: bit<8>; let q: bit<8> = a + 1; ");
        for _ in 0..260 {
            source.push_str("initial { $assert(q == 1); } ");
        }
        source.push_str("initial { $finish(); } }");
        let mut sim = Simulator::builder(&source, "Top")
            .four_state(false)
            .build_interpreter()
            .unwrap();
        let tb = compile_initial_testbench(&sim).unwrap();
        assert_eq!(
            run_compiled_testbench_to_finish(&mut sim, &tb),
            TestResult::Pass
        );
    }

    #[test]
    fn native_tb_dynamic_clock_count_preserves_dut_writes() {
        let source = r#"
            module Register(clk: input clock, d: input bit<32>, q: output bit<32>) {
                let next_value: bit<32> = d + 1;
                always_ff { q = next_value; }
            }
            #[test(Top)] module Top {
                inst clk: $tb::clock_gen;
                var d: u32; var q: u32;
                inst dut: Register(clk, d, q);
                function change_input() -> u32 { d = 7; return 1; }
                initial {
                    d=1; clk.next(); $assert(q==2);
                    clk.next(change_input()); $assert(q==8); $finish();
                }
            }
        "#;
        fn run<B: SimBackend>(mut sim: Simulator<B>) {
            let tb = compile_initial_testbench(&sim).unwrap();
            assert_eq!(
                run_compiled_testbench_to_finish(&mut sim, &tb),
                TestResult::Pass
            );
        }
        run(Simulator::builder(source, "Top")
            .four_state(false)
            .build_native()
            .unwrap());
        run(Simulator::builder(source, "Top")
            .four_state(false)
            .build_interpreter()
            .unwrap());
        #[cfg(feature = "cranelift")]
        run(Simulator::builder(source, "Top")
            .four_state(false)
            .build()
            .unwrap());
    }

    #[test]
    fn concurrent_tb_preserves_direct_async_reset_writes() {
        let source = r#"
            module Counter(clk: input clock, rst: input reset_async_high,
                           q: output logic<8>) {
                always_ff (clk, rst) { if_reset { q = 0; } else { q += 1; } }
            }
            #[test(Top)] module Top {
                inst fast: $tb::clock_gen #(period: 2);
                inst slow: $tb::clock_gen #(period: 20);
                var rst: reset_async_high;
                var q: logic<8>;
                inst dut: Counter(clk: slow, rst, q);
                initial {
                    rst = 0;
                    fast.next();
                    $assert(q == 1);
                    rst = 1;
                    __OBSERVE__
                    fast.next();
                    $assert(q == 0);
                    $finish();
                }
                initial { slow.next(100); }
            }
        "#;
        // Reading the reset settles hardware and clears `dirty`; its edge
        // must still be detected against the queue's previous sample.
        for observation in ["", "$assert(rst == 1);"] {
            let source = source.replace("__OBSERVE__", observation);
            let mut sim = Simulator::builder(&source, "Top")
                .build_interpreter()
                .unwrap();
            let tb = compile_initial_testbench(&sim).unwrap();
            assert_eq!(
                run_compiled_testbench_to_finish(&mut sim, &tb),
                TestResult::Pass
            );
        }
    }

    #[test]
    fn concurrent_tb_preserves_low_phase_latch_updates() {
        let source = r#"
            module Pipeline(clk: input clock, upstream: output logic<8>,
                            held: output logic<8>, downstream: output logic<8>) {
                always_ff (clk) { upstream += 1; downstream = held; }
                always_comb { if !clk { held = upstream; } }
            }
            #[test(Top)] module Top {
                inst clk: $tb::clock_gen #(period: 2);
                var upstream: logic<8>;
                var held: logic<8>;
                var downstream: logic<8>;
                inst dut: Pipeline(clk, upstream, held, downstream);
                initial {
                    clk.next(4);
                    $assert(upstream == 4);
                    $assert(held == 4);
                    $assert(downstream == 3);
                    $finish();
                }
                initial { clk.next(100); }
            }
        "#;
        let mut sim = Simulator::builder(source, "Top")
            .allow_inferred_latches(true)
            .build_interpreter()
            .unwrap();
        assert!(!sim.program.runtime_schema.persistent_comb_state.is_empty());
        let tb = compile_initial_testbench(&sim).unwrap();
        assert_eq!(
            run_compiled_testbench_to_finish(&mut sim, &tb),
            TestResult::Pass
        );
    }

    #[test]
    fn concurrent_tb_publishes_each_clock_phase_to_vcd() {
        let source = r#"
            module Counter(clk: input clock, q: output logic<8>) {
                always_ff (clk) { q += 1; }
            }
            #[test(Top)] module Top {
                inst clk: $tb::clock_gen #(period: 2);
                var q: logic<8>;
                inst dut: Counter(clk, q);
                initial { clk.next(4); $assert(q == 4); $finish(); }
                initial { clk.next(100); }
            }
        "#;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("concurrent.vcd");
        let mut sim = Simulator::builder(source, "Top")
            .vcd(&path)
            .build_interpreter()
            .unwrap();
        let tb = compile_initial_testbench(&sim).unwrap();
        assert_eq!(
            run_compiled_testbench_to_finish(&mut sim, &tb),
            TestResult::Pass
        );
        sim.flush_vcd().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let timestamps: Vec<_> = text
            .lines()
            .filter_map(|line| line.strip_prefix('#'))
            .map(|time| time.parse::<u64>().unwrap())
            .collect();
        for falling_edge in [1, 3, 5, 7] {
            assert!(
                timestamps.contains(&falling_edge),
                "missing low phase {falling_edge}: {timestamps:?}"
            );
        }
    }

    #[test]
    fn constant_clock_wait_loop_binds_as_one_wait() {
        let source = r#"
            module Counter(clk: input clock, q: output logic<32>) {
                always_ff (clk) { q += 1; }
            }
            #[test(Top)] module Top {
                inst clk: $tb::clock_gen;
                var q: logic<32>;
                inst dut: Counter(clk, q);
                initial {
                    for _i in 0..(2 * (2 + 2)) { clk.next(1); }
                    $assert(q == 8);
                    $finish();
                }
            }
        "#;
        let mut sim = Simulator::builder(source, "Top")
            .build_interpreter()
            .unwrap();
        let tb = compile_initial_testbench(&sim).unwrap();
        assert!(matches!(
            tb.statements().first(),
            Some(GenericTestbenchStatement::ClockNext {
                count: ClockCount::Static(8),
                ..
            })
        ));
        assert_eq!(
            run_compiled_testbench_to_finish(&mut sim, &tb),
            TestResult::Pass,
            "q={}, counters={:?}",
            sim.get_as::<u32>(sim.signal("q")),
            compiled::take_counters()
        );
    }

    #[test]
    fn compiled_reads_share_settlement_until_the_next_hardware_write() {
        let mut sim = Simulator::builder(
            r#"
            #[test(Top)] module Top {
                var a: logic<8>; var q: logic<8>;
                var x: logic<8>; var y: logic<8>; var z: logic<8>;
                assign q = a + 1;
                initial {
                    a = 1;
                    x = q;
                    y = q;
                    a = 7;
                    z = q;
                    $assert(x == 2 && y == 2 && z == 8);
                    $finish();
                }
            }
        "#,
            "Top",
        )
        .build()
        .unwrap();
        let tb = compile_initial_testbench(&sim).unwrap();
        let settlements = tb.kernels()[0]
            .requests
            .iter()
            .filter(|request| matches!(request, celox_testbench::KernelRequest::Settle { .. }))
            .count();
        assert_eq!(settlements, 2);
        assert_eq!(
            run_compiled_testbench_to_finish(&mut sim, &tb),
            TestResult::Pass
        );
    }

    #[test]
    fn compiled_kernel_stores_invalidate_settled_hardware() {
        let mut sim = Simulator::builder(
            r#"
            #[test(Top)] module Top {
                var a: logic<8>; var q: logic<8>;
                assign q = a + 1;
                initial { a = 7; $finish(); }
            }
        "#,
            "Top",
        )
        .dead_store_policy(crate::DeadStorePolicy::Off)
        .build_interpreter()
        .unwrap();
        let q = sim.signal("q");
        let tb = compile_initial_testbench(&sim).unwrap();
        settle_testbench(&mut sim).unwrap();
        let GenericTestbenchStatement::Assign { expr, .. } = &tb.statements()[0] else {
            panic!("assignment")
        };
        assert_eq!(eval_expr(&mut sim, expr).to_u64(), 7);
        assert!(!sim.dirty);
        assert_eq!(
            run_compiled_testbench_to_finish(&mut sim, &tb),
            TestResult::Pass
        );
        assert!(sim.dirty);
        settle_testbench(&mut sim).unwrap();
        assert_eq!(sim.get_as::<u8>(q), 8);
    }

    #[test]
    fn compiled_kernel_stores_notify_vcd_without_exposing_memory() {
        let dir = tempfile::tempdir().unwrap();
        let mut sim = Simulator::builder(
            r#"
            #[test(Top)] module Top {var a: logic<8>; initial {a=7; $finish();}}
        "#,
            "Top",
        )
        .vcd(dir.path().join("expression.vcd"))
        .build()
        .unwrap();
        let a = sim.signal("a");
        let tb = compile_initial_testbench(&sim).unwrap();
        sim.dump(0);
        let before = sim.vcd_statistics().unwrap();
        assert_eq!(
            run_compiled_testbench_to_finish(&mut sim, &tb),
            TestResult::Pass
        );
        assert_eq!(sim.get_as::<u8>(a), 7);
        sim.dump(1);
        let after = sim.vcd_statistics().unwrap();
        assert_eq!(after.changes, before.changes + 1);
        sim.dump(2);
        assert_eq!(sim.vcd_statistics().unwrap().comparisons, after.comparisons);
    }

    #[test]
    fn compiles_initial_display_and_write() {
        let code = r#"
            #[test(t)]
            module t {
                initial {
                    $display("answer=%h", 8'h2a);
                    $write("!");
                    $finish();
                }
            }
        "#;
        let sim = Simulator::builder(code, "t").build_with_trace().unwrap();
        let tb = compile_initial_testbench(&sim).unwrap();

        assert!(matches!(
            tb.statements().first(),
            Some(GenericTestbenchStatement::Display { newline: true, .. })
        ));
        assert!(matches!(
            tb.statements().get(1),
            Some(GenericTestbenchStatement::Display { newline: false, .. })
        ));
        assert!(matches!(
            tb.statements().get(2),
            Some(GenericTestbenchStatement::Finish)
        ));
    }

    #[test]
    fn limited_runner_stops_without_completing_the_testbench() {
        let code = r#"
            #[test(t)]
            module t {
                inst clk: $tb::clock_gen;
                initial {
                    clk.next(10);
                    $finish();
                }
            }
        "#;
        let mut sim = Simulator::builder(code, "t").build_with_trace().unwrap();
        let tb = compile_initial_testbench(&sim).unwrap();

        assert_eq!(
            run_compiled_testbench_with_tick_limit(&mut sim, &tb, 3),
            LimitedTestbenchResult {
                result: TestResult::Pass,
                ticks: 3,
                tick_limit_reached: true,
            }
        );
    }
    #[test]
    fn zero_tick_limit_does_not_evaluate_compiled_control_flow() {
        let source = r#"
            #[test(Top)] module Top {
                var touched: u32;
                function touch() -> u32 { touched = 1; return 1; }
                initial { if touch() == 1 { $finish(); } }
            }
        "#;
        let mut sim = Simulator::builder(source, "Top")
            .build_interpreter()
            .unwrap();
        let touched = sim.signal("touched");
        let program = compile_initial_testbench(&sim).unwrap();
        let result = run_compiled_testbench_with_tick_limit(&mut sim, &program, 0);
        assert!(result.tick_limit_reached);
        assert_eq!(result.ticks, 0);
        assert_eq!(sim.get_as::<u32>(touched), 0);
    }
}
