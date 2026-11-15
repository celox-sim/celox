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
use num_traits::{ToPrimitive as _, Zero as _};
use rand::{RngExt as _, SeedableRng as _};
use rand_pcg::Pcg64;
#[expect(
    clippy::disallowed_types,
    reason = "FFI/JIT: accesses sequence words of the runtime-event ring buffer that JIT-compiled code also writes and a drain thread reads"
)]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::task::Poll;

mod components;
mod events;
mod exec;
use components::*;
use events::*;
use exec::*;

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

/// The value of `expr` with its unknown bits read as 0, as a two-state use
/// reads them (IEEE 1800-2023 6.11.2). A condition is true when this is
/// nonzero: when the value has a known one bit (12.4, 16.3).
fn eval_expr<B: SimBackend>(
    sim: &mut Simulator<B>,
    expr: &celox_testbench::CompiledExpr,
) -> TbValue {
    let (value, mask) = eval_expr_four_state(sim, expr);
    if mask.is_zero() {
        return value;
    }
    let value = value.to_biguint();
    let known = &value ^ (&value & mask);
    match known.to_u64() {
        Some(known) if expr.kernel().is_some_and(|kernel| kernel.width <= 64) => {
            TbValue::U64(known)
        }
        _ => TbValue::Wide(known),
    }
}

/// The value of `expr` and its unknown bits.
fn eval_expr_four_state<B: SimBackend>(
    sim: &mut Simulator<B>,
    expr: &celox_testbench::CompiledExpr,
) -> (TbValue, BigUint) {
    let (value, mask, writes) = eval_backend_expr_tracked(&mut sim.backend, expr);
    sim.dirty |= writes
        .iter()
        .any(|&(offset, len)| !private_testbench_range(sim, offset, len));
    (value, mask)
}

/// The value bits of `expr`: an unknown bit reads as its value-plane bit.
pub(crate) fn eval_backend_expr<B: SimBackend>(
    backend: &mut B,
    expr: &celox_testbench::CompiledExpr,
) -> TbValue {
    eval_backend_expr_tracked(backend, expr).0
}

/// Run `expr`: its value, its unknown bits and the ranges it wrote.
fn eval_backend_expr_tracked<B: SimBackend>(
    backend: &mut B,
    expr: &celox_testbench::CompiledExpr,
) -> (TbValue, BigUint, Vec<(usize, usize)>) {
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
    // A four-state result slot holds its mask plane after its value plane.
    let mask = if kernel.four_state {
        let plane = kernel.width.div_ceil(8);
        BigUint::from_bytes_le(unsafe {
            std::slice::from_raw_parts(memory.add(kernel.result + plane), plane)
        })
    } else {
        BigUint::ZERO
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
    (value, mask, writes)
}

fn private_testbench_range<B: SimBackend>(sim: &Simulator<B>, offset: usize, len: usize) -> bool {
    if !sim.components.is_empty() || sim.schedule.timed {
        return false;
    }
    let ranges = sim.heap.testbench_hardware_ranges.get_or_init(|| {
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
    // Formatting keeps unknown bits (IEEE 1800-2023 21.2.1.3).
    let (value, mask) = eval_expr_four_state(sim, &arg.expr);
    let width = match &value {
        TbValue::String(bytes) => bytes.len() * 8,
        _ => arg.width,
    };
    let (value, mask) = (value.to_biguint(), (!mask.is_zero()).then_some(mask));
    format_veryl_display_arg(
        &DisplayFormatArg {
            value: &value,
            mask: mask.as_ref(),
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
        record_passes: false,
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
        record_passes: true,
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
    /// Whether passing assertions are recorded, as the detailed API reports
    /// them.
    record_passes: bool,
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

#[expect(
    clippy::disallowed_types,
    reason = "FFI/JIT: acquire-loads a sequence word of the runtime-event ring buffer that JIT-compiled code also writes"
)]
fn load_u64_acquire(base: *const u8, byte_offset: usize) -> u64 {
    unsafe { (*(base.add(byte_offset) as *const AtomicU64)).load(Ordering::Acquire) }
}

#[expect(
    clippy::disallowed_types,
    reason = "FFI/JIT: release-stores a sequence word of the runtime-event ring buffer that JIT-compiled code also writes and a drain thread reads"
)]
fn store_u64_release(base: *mut u8, byte_offset: usize, value: u64) {
    unsafe {
        (*(base.add(byte_offset) as *const AtomicU64)).store(value, Ordering::Release);
    }
}

#[cfg(all(test, feature = "host-runtime"))]
mod tests;
