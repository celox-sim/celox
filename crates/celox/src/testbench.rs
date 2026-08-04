//! Native testbench execution for Veryl `#[test]` modules.
//!
//! Testbench expressions are compiled to a flat bytecode (`TbOpcode`) and
//! evaluated by a stack-based VM that reads directly from the simulator's
//! memory buffer.  Signals ≤64 bits use native `u64` arithmetic with zero
//! heap allocation; wider signals fall back to `BigUint`.

use crate::RuntimeErrorCode;
use crate::backend::memory_layout::{
    RUNTIME_EVENT_HEADER_SIZE, RUNTIME_EVENT_SLOT_ARG_COUNT_OFFSET,
    RUNTIME_EVENT_SLOT_PAYLOAD_OFFSET, RUNTIME_EVENT_SLOT_SEQ_OFFSET,
    RUNTIME_EVENT_SLOT_SITE_OFFSET, RUNTIME_EVENT_WRITING,
};
use crate::backend::traits::SimBackend;
use crate::ir::SignalRef;
use crate::simulator::{RuntimeEvent, RuntimeFormatContext, Simulator};
pub use celox_testbench::SourceLocation;
use celox_testbench::{
    DisplayFormatArg, ExecutableArgument, ExecutableAssertMessage, ExecutableClockCount,
    ExecutableLoopBound, ExecutableStatement, ExecutableTestbench, TestbenchOperator as Op,
    TestbenchStatement as GenericTestbenchStatement, TestbenchValue as TbValue, format_display_arg,
};
use num_bigint::{BigInt, BigUint, Sign};
use num_traits::ToPrimitive as _;
use std::sync::atomic::{AtomicU64, Ordering};

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
    pub message: Option<String>,
    pub location: Option<SourceLocation>,
}

/// Detailed test result with assertion outcomes observed before the test
/// finishes or stops on a fatal failure.
#[derive(Debug, Clone)]
pub struct TestResultDetailed {
    pub passed: bool,
    pub assertions: Vec<AssertionResult>,
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
    #[error("dynamic signed for-loop bound exceeds host i128")]
    SignedLoopBoundOutOfRange,
}

pub(crate) type TestbenchStatement<B> = ExecutableStatement<<B as SimBackend>::Event, SignalRef>;

pub(crate) type CompiledAssertArg = ExecutableArgument;

fn format_assert_arg(arg: &CompiledAssertArg, memory: *mut u8, spec: Option<char>) -> String {
    let value = arg.expr.eval_value(memory);
    let value = value.to_biguint();
    format_display_arg(
        &DisplayFormatArg {
            value: &value,
            mask: None,
            width: arg.width,
            signed: arg.signed,
            is_string: arg.is_string,
        },
        spec,
    )
}

fn render_assert_message(
    message: &Option<AssertMessage>,
    memory: *mut u8,
    current_time: u64,
) -> Option<String> {
    match message {
        None => None,
        Some(AssertMessage::DynamicArgs(args)) => Some(
            args.iter()
                .map(|arg| format_assert_arg(arg, memory, Some('x')))
                .collect::<Vec<_>>()
                .join(" "),
        ),
        Some(AssertMessage::Formatted { template, args }) => {
            let mut rendered = String::new();
            let mut chars = template.chars().peekable();
            let mut arg_idx = 0usize;
            while let Some(ch) = chars.next() {
                if ch != '%' {
                    rendered.push(ch);
                    continue;
                }
                match chars.peek().copied() {
                    Some('%') => {
                        chars.next();
                        rendered.push('%');
                    }
                    Some(spec) => {
                        chars.next();
                        let spec = if spec.is_ascii_digit() {
                            while matches!(chars.peek(), Some('0'..='9')) {
                                chars.next();
                            }
                            chars.next().unwrap_or(spec)
                        } else {
                            spec
                        };
                        match spec {
                            'h' | 'H' | 'x' | 'X' | 'd' | 'D' | 'i' | 'I' | 'o' | 'O' | 'b'
                            | 'B' | 'c' | 'C' | 's' | 'S' => {
                                if let Some(arg) = args.get(arg_idx) {
                                    rendered.push_str(&format_assert_arg(arg, memory, Some(spec)));
                                }
                                arg_idx += 1;
                            }
                            'm' | 'M' => rendered.push_str("<hierarchy>"),
                            't' | 'T' => rendered.push_str(&current_time.to_string()),
                            _ => {
                                rendered.push('%');
                                rendered.push(spec);
                            }
                        }
                    }
                    None => rendered.push('%'),
                }
            }
            Some(rendered)
        }
    }
}

fn sim_set_u64<B: SimBackend>(sim: &mut crate::Simulator<B>, sig: SignalRef, value: u64) {
    match sig.width {
        0..=8 => sim.set(sig, value as u8),
        9..=16 => sim.set(sig, value as u16),
        17..=32 => sim.set(sig, value as u32),
        33..=64 => sim.set(sig, value),
        _ => sim.set_wide(sig, BigUint::from(value)),
    }
}

// ── Expression compiler ────────────────────────────────────────────────

// ── Executor ───────────────────────────────────────────────────────────

enum ExecResult {
    Continue,
    Break,
    Finished,
    Fail(String),
}

impl ExecResult {
    fn should_stop(&self) -> bool {
        !matches!(self, ExecResult::Continue)
    }
}

impl From<ExecResult> for TestResult {
    fn from(r: ExecResult) -> Self {
        match r {
            ExecResult::Continue | ExecResult::Break | ExecResult::Finished => TestResult::Pass,
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
            sim.eval_comb()
                .map_err(|source| TestbenchEvaluationError::EvalComb { source })?;
            let (ptr, _) = sim.memory_as_mut_ptr();
            expr.eval_u64(ptr)
        }
    })
}

fn eval_loop_bound<B: SimBackend>(
    sim: &mut Simulator<B>,
    bound: &LoopBound,
) -> Result<EvaluatedLoopBound, TestbenchEvaluationError> {
    match bound {
        // ForBound::Const no longer carries source signedness; use the signed
        // i32 induction-variable semantics for static Veryl loop bounds.
        LoopBound::Static(v) => Ok(EvaluatedLoopBound::Signed(*v as i128)),
        LoopBound::Dynamic {
            expr,
            width,
            signed,
        } => {
            sim.eval_comb()
                .map_err(|source| TestbenchEvaluationError::EvalComb { source })?;
            let (ptr, _) = sim.memory_as_mut_ptr();
            let value = expr.eval_value(ptr);
            if *signed {
                decode_signed_loop_bound(value, *width)
            } else {
                match value {
                    TbValue::U64(v) => match usize::try_from(v) {
                        Ok(v) => Ok(EvaluatedLoopBound::Unsigned(v)),
                        Err(_) => Ok(EvaluatedLoopBound::UnsignedWide(BigUint::from(v))),
                    },
                    TbValue::Wide(v) => match v.to_usize() {
                        Some(v) => Ok(EvaluatedLoopBound::Unsigned(v)),
                        None => Ok(EvaluatedLoopBound::UnsignedWide(v)),
                    },
                }
            }
        }
    }
}

enum EvaluatedLoopBound {
    Unsigned(usize),
    UnsignedWide(BigUint),
    Signed(i128),
    SignedWide(BigInt),
}

fn decode_signed_loop_bound(
    value: TbValue,
    width: usize,
) -> Result<EvaluatedLoopBound, TestbenchEvaluationError> {
    let width = width.max(1);
    match value {
        TbValue::U64(v) => {
            let raw = if width >= 64 {
                v as u128
            } else {
                (v as u128) & ((1u128 << width) - 1)
            };
            Ok(EvaluatedLoopBound::Signed(sign_extend_u128(raw, width)))
        }
        TbValue::Wide(v) => {
            if width > 128 {
                let signed = sign_extend_biguint(v, width);
                return Ok(match signed.to_i128() {
                    Some(v) => EvaluatedLoopBound::Signed(v),
                    None => EvaluatedLoopBound::SignedWide(signed),
                });
            }
            let raw = v
                .to_u128()
                .ok_or(TestbenchEvaluationError::SignedLoopBoundOutOfRange)?;
            Ok(EvaluatedLoopBound::Signed(sign_extend_u128(raw, width)))
        }
    }
}

fn sign_extend_u128(raw: u128, width: usize) -> i128 {
    let width = width.max(1);
    if width >= 128 {
        raw as i128
    } else {
        let sign_bit = 1u128 << (width - 1);
        if raw & sign_bit == 0 {
            raw as i128
        } else {
            raw as i128 - ((1u128 << width) as i128)
        }
    }
}

fn truncate_i128_to_width(value: i128, width: usize, signed: bool) -> i128 {
    let width = width.max(1);
    if width >= 128 {
        return value;
    }
    let raw = (value as u128) & ((1u128 << width) - 1);
    if signed {
        sign_extend_u128(raw, width)
    } else {
        raw as i128
    }
}

fn truncate_bigint_to_width(value: BigInt, width: usize, signed: bool) -> BigInt {
    let width = width.max(1);
    let modulus = BigInt::from(1u8) << width;
    let mut wrapped = value % &modulus;
    if wrapped.sign() == Sign::Minus {
        wrapped += &modulus;
    }
    if signed && wrapped >= (BigInt::from(1u8) << (width - 1)) {
        wrapped - modulus
    } else {
        wrapped
    }
}

fn advance_bigint_counter(
    current: &BigInt,
    step: usize,
    step_op: Option<Op>,
    reverse: bool,
    width: usize,
    signed: bool,
) -> BigInt {
    let step_value = BigInt::from(step);
    let raw = if reverse {
        current - &step_value
    } else {
        match step_op {
            Some(Op::Mul) => current * &step_value,
            Some(Op::BitOr) => current | &step_value,
            Some(Op::BitXor) => current ^ &step_value,
            Some(Op::LogicShiftL | Op::ArithShiftL) => {
                if step >= width.max(1) {
                    BigInt::from(0u8)
                } else {
                    current << step
                }
            }
            _ => current + &step_value,
        }
    };
    truncate_bigint_to_width(raw, width, signed)
}

fn truncate_usize_to_width(value: usize, width: usize) -> usize {
    if width >= usize::BITS as usize {
        value
    } else if width == 0 {
        0
    } else {
        value & ((1usize << width) - 1)
    }
}

fn sign_extend_biguint(raw: BigUint, width: usize) -> BigInt {
    let width = width.max(1);
    let sign_bit = BigUint::from(1u8) << (width - 1);
    if raw < sign_bit {
        BigInt::from(raw)
    } else {
        BigInt::from(raw) - (BigInt::from(1u8) << width)
    }
}

fn mask_to_width(value: BigUint, width: usize) -> BigUint {
    if width == 0 {
        BigUint::from(0u8)
    } else {
        value & ((BigUint::from(1u8) << width) - BigUint::from(1u8))
    }
}

fn sim_set_i128<B: SimBackend>(
    sim: &mut crate::Simulator<B>,
    sig: SignalRef,
    width: usize,
    value: i128,
) {
    if width <= 64 {
        sim_set_u64(sim, sig, value as u64);
    } else if value >= 0 {
        sim.set_wide(sig, BigUint::from(value as u128));
    } else {
        let modulus = BigUint::from(1u8) << width;
        let mag = BigUint::from(value.unsigned_abs());
        sim.set_wide(sig, modulus - mag);
    }
}

fn sim_set_biguint<B: SimBackend>(sim: &mut crate::Simulator<B>, sig: SignalRef, value: BigUint) {
    if sig.width <= 64 {
        sim_set_u64(sim, sig, value.to_u64().unwrap_or(0));
    } else {
        sim.set_wide(sig, mask_to_width(value, sig.width));
    }
}

fn sim_set_bigint<B: SimBackend>(
    sim: &mut crate::Simulator<B>,
    sig: SignalRef,
    width: usize,
    value: BigInt,
) {
    if width <= 128 {
        sim_set_i128(sim, sig, width, value.try_into().unwrap_or(0));
        return;
    }
    if value.sign() != Sign::Minus {
        sim_set_biguint(
            sim,
            sig,
            value.try_into().unwrap_or_else(|_| BigUint::from(0u8)),
        );
    } else {
        let modulus = BigUint::from(1u8) << width;
        sim_set_biguint(sim, sig, modulus - value.magnitude().clone());
    }
}

fn as_bigint_bound(bound: &EvaluatedLoopBound) -> Option<BigInt> {
    match bound {
        EvaluatedLoopBound::Unsigned(v) => Some(BigInt::from(*v)),
        EvaluatedLoopBound::UnsignedWide(v) => Some(BigInt::from(v.clone())),
        EvaluatedLoopBound::Signed(v) => Some(BigInt::from(*v)),
        EvaluatedLoopBound::SignedWide(v) => Some(v.clone()),
    }
}

fn exec_for_loop<B: SimBackend>(
    sim: &mut Simulator<B>,
    loop_var: &Option<(SignalRef, usize, bool)>,
    start: &LoopBound,
    end: &LoopBound,
    inclusive: bool,
    step: usize,
    step_op: Option<Op>,
    reverse: bool,
    mut exec_body: impl FnMut(&mut Simulator<B>) -> ExecResult,
) -> ExecResult {
    let mut start = match eval_loop_bound(sim, start) {
        Ok(v) => v,
        Err(error) => return ExecResult::Fail(error.to_string()),
    };
    let mut end = match eval_loop_bound(sim, end) {
        Ok(v) => v,
        Err(error) => return ExecResult::Fail(error.to_string()),
    };

    let initially_wide = matches!(start, EvaluatedLoopBound::UnsignedWide(_))
        || matches!(end, EvaluatedLoopBound::UnsignedWide(_))
        || matches!(start, EvaluatedLoopBound::SignedWide(_))
        || matches!(end, EvaluatedLoopBound::SignedWide(_));
    if let Some((_, width, true)) = loop_var.as_ref()
        && !initially_wide
    {
        let width = (*width).max(1);
        let unsigned_max = if width <= usize::BITS as usize {
            Some((1usize << (width - 1)) - 1)
        } else {
            None
        };
        let unsigned_value = |bound: &EvaluatedLoopBound| match bound {
            EvaluatedLoopBound::Unsigned(value) => Some(*value),
            _ => None,
        };
        let start_out_of_range = unsigned_max
            .zip(unsigned_value(&start))
            .is_some_and(|(max, value)| value > max);
        let end_out_of_range =
            unsigned_max
                .zip(unsigned_value(&end))
                .is_some_and(|(max, value)| {
                    value > max && !(!inclusive && value == max.saturating_add(1))
                });
        if start_out_of_range || end_out_of_range {
            return ExecResult::Fail("non-progressing stepped for loop".to_string());
        }
        start = match start {
            EvaluatedLoopBound::Unsigned(value) => EvaluatedLoopBound::Signed(value as i128),
            other => other,
        };
        end = match end {
            EvaluatedLoopBound::Unsigned(value) => EvaluatedLoopBound::Signed(value as i128),
            other => other,
        };
    }

    let has_unsigned_wide = matches!(start, EvaluatedLoopBound::UnsignedWide(_))
        || matches!(end, EvaluatedLoopBound::UnsignedWide(_));
    let has_signed_wide = matches!(start, EvaluatedLoopBound::SignedWide(_))
        || matches!(end, EvaluatedLoopBound::SignedWide(_));
    if has_unsigned_wide || has_signed_wide {
        let start = as_bigint_bound(&start).expect("big loop bound");
        let end = as_bigint_bound(&end).expect("big loop bound");
        let mut step_body = |sim: &mut Simulator<B>, i: BigInt| -> ExecResult {
            if let Some((sig, width, _)) = loop_var {
                sim_set_bigint(sim, *sig, *width, i);
            }
            exec_body(sim)
        };
        if reverse {
            if inclusive {
                if end < start {
                    return ExecResult::Continue;
                }
                if end == start {
                    let (width, signed) = loop_var
                        .as_ref()
                        .map_or((usize::BITS as usize, false), |(_, width, signed)| {
                            (*width, *signed)
                        });
                    let current = truncate_bigint_to_width(end, width, signed);
                    let result = step_body(sim, current.clone());
                    if matches!(result, ExecResult::Break) {
                        return ExecResult::Continue;
                    }
                    if result.should_stop() {
                        return result;
                    }
                    let next =
                        advance_bigint_counter(&current, step, step_op, reverse, width, signed);
                    return if next >= current {
                        ExecResult::Fail("non-progressing stepped for loop".to_string())
                    } else {
                        ExecResult::Continue
                    };
                }
            } else if end <= start {
                return ExecResult::Continue;
            }
        } else if inclusive {
            if start > end {
                return ExecResult::Continue;
            }
            if start == end {
                let (width, signed) = loop_var
                    .as_ref()
                    .map_or((usize::BITS as usize, false), |(_, width, signed)| {
                        (*width, *signed)
                    });
                let current = truncate_bigint_to_width(start, width, signed);
                let result = step_body(sim, current.clone());
                if matches!(result, ExecResult::Break) {
                    return ExecResult::Continue;
                }
                if result.should_stop() {
                    return result;
                }
                let next = advance_bigint_counter(&current, step, step_op, reverse, width, signed);
                return if next <= current {
                    ExecResult::Fail("non-progressing stepped for loop".to_string())
                } else {
                    ExecResult::Continue
                };
            }
        } else if start >= end {
            return ExecResult::Continue;
        }
        return ExecResult::Fail(if loop_var.as_ref().is_some_and(|(_, _, signed)| *signed) {
            "dynamic signed for-loop bound exceeds host i128".to_string()
        } else {
            "dynamic for-loop bound exceeds host usize".to_string()
        });
    }

    let (start_signed, end_signed) = match (start, end) {
        (EvaluatedLoopBound::Unsigned(start), EvaluatedLoopBound::Unsigned(end)) => {
            (None, Some((start, end)))
        }
        (EvaluatedLoopBound::Signed(start), EvaluatedLoopBound::Signed(end)) => {
            (Some((start, end)), None)
        }
        (EvaluatedLoopBound::Signed(start), EvaluatedLoopBound::Unsigned(end)) => {
            (Some((start, end as i128)), None)
        }
        (EvaluatedLoopBound::Unsigned(start), EvaluatedLoopBound::Signed(end)) => {
            (Some((start as i128, end)), None)
        }
        _ => unreachable!("wide loop bounds handled above"),
    };

    if let Some((start, end)) = start_signed {
        let truncate_counter = |value| {
            loop_var.as_ref().map_or(value, |(_, width, signed)| {
                truncate_i128_to_width(value, *width, *signed)
            })
        };
        let mut step_body = |sim: &mut Simulator<B>, i: i128| -> ExecResult {
            if let Some((sig, width, _)) = loop_var {
                sim_set_i128(sim, *sig, *width, i);
            }
            exec_body(sim)
        };

        let step_i = step as i128;
        if reverse {
            let mut i = truncate_counter(if inclusive { end } else { end.wrapping_sub(1) });
            while i >= start {
                let r = step_body(sim, i);
                if matches!(r, ExecResult::Break) {
                    return ExecResult::Continue;
                }
                if r.should_stop() {
                    return r;
                }
                let next = truncate_counter(i.wrapping_sub(step_i));
                if next >= i {
                    return ExecResult::Fail("non-progressing stepped for loop".to_string());
                }
                i = next;
            }
        } else if let Some(op) = step_op {
            let mut i = truncate_counter(start);
            while if inclusive { i <= end } else { i < end } {
                let r = step_body(sim, i);
                if matches!(r, ExecResult::Break) {
                    return ExecResult::Continue;
                }
                if r.should_stop() {
                    return r;
                }
                let new_i = match op {
                    Op::Mul => i.wrapping_mul(step_i),
                    Op::BitOr => i | step_i,
                    Op::BitXor => i ^ step_i,
                    Op::LogicShiftL | Op::ArithShiftL => {
                        if step >= i128::BITS as usize {
                            0
                        } else {
                            i.checked_shl(step as u32).unwrap_or(0)
                        }
                    }
                    _ => i.wrapping_add(step_i),
                };
                let new_i = truncate_counter(new_i);
                if new_i <= i {
                    return ExecResult::Fail("non-progressing stepped for loop".to_string());
                }
                i = new_i;
            }
        } else {
            let mut i = truncate_counter(start);
            while if inclusive { i <= end } else { i < end } {
                let r = step_body(sim, i);
                if matches!(r, ExecResult::Break) {
                    return ExecResult::Continue;
                }
                if r.should_stop() {
                    return r;
                }
                let next = i.wrapping_add(step_i);
                let next = truncate_counter(next);
                if next <= i {
                    return ExecResult::Fail("non-progressing stepped for loop".to_string());
                }
                i = next;
            }
        }

        return ExecResult::Continue;
    }

    let (start, end) = end_signed.expect("unsigned loop bounds expected");
    let truncate_counter = |value| {
        loop_var.as_ref().map_or(value, |(_, width, _)| {
            truncate_usize_to_width(value, *width)
        })
    };

    let mut step_body = |sim: &mut Simulator<B>, i: usize| -> ExecResult {
        if let Some((sig, _, _)) = loop_var {
            sim_set_u64(sim, *sig, i as u64);
        }
        exec_body(sim)
    };

    if reverse {
        let mut i = truncate_counter(if inclusive { end } else { end.wrapping_sub(1) });
        while i >= start {
            let r = step_body(sim, i);
            if matches!(r, ExecResult::Break) {
                return ExecResult::Continue;
            }
            if r.should_stop() {
                return r;
            }
            let next = truncate_counter(i.wrapping_sub(step));
            if next >= i {
                return ExecResult::Fail("non-progressing stepped for loop".to_string());
            }
            i = next;
        }
    } else if let Some(op) = step_op {
        let mut i = truncate_counter(start);
        while if inclusive { i <= end } else { i < end } {
            let r = step_body(sim, i);
            if matches!(r, ExecResult::Break) {
                return ExecResult::Continue;
            }
            if r.should_stop() {
                return r;
            }
            let new_i = match op {
                Op::Mul => i.wrapping_mul(step),
                Op::BitOr => i | step,
                Op::BitXor => i ^ step,
                Op::LogicShiftL | Op::ArithShiftL => {
                    if step >= usize::BITS as usize {
                        0
                    } else {
                        i << step
                    }
                }
                _ => i.wrapping_add(step),
            };
            let new_i = truncate_counter(new_i);
            if new_i <= i {
                return ExecResult::Fail("non-progressing stepped for loop".to_string());
            }
            i = new_i;
        }
    } else {
        let mut i = truncate_counter(start);
        while if inclusive { i <= end } else { i < end } {
            let r = step_body(sim, i);
            if matches!(r, ExecResult::Break) {
                return ExecResult::Continue;
            }
            if r.should_stop() {
                return r;
            }
            let next = i.wrapping_add(step);
            let next = truncate_counter(next);
            if next <= i {
                return ExecResult::Fail("non-progressing stepped for loop".to_string());
            }
            i = next;
        }
    }

    ExecResult::Continue
}

pub(crate) fn run_testbench<B: SimBackend>(
    sim: &mut Simulator<B>,
    stmts: &[TestbenchStatement<B>],
) -> TestResult {
    run_testbench_limited(sim, stmts, None).result
}

fn run_testbench_limited<B: SimBackend>(
    sim: &mut Simulator<B>,
    stmts: &[TestbenchStatement<B>],
    tick_limit: Option<u64>,
) -> LimitedTestbenchResult {
    let mut ctx = DetailedExecContext {
        assertions: Vec::new(),
        current_time: 0,
        tick_limit,
        tick_limit_reached: false,
    };
    let result = exec_detailed(sim, stmts, &mut ctx);
    let failed_messages = ctx
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
        ExecResult::Continue | ExecResult::Break | ExecResult::Finished => {
            if failed_messages.is_empty() {
                TestResult::Pass
            } else {
                TestResult::Fail(failed_messages.join("\n"))
            }
        }
    };
    LimitedTestbenchResult {
        result,
        ticks: ctx.current_time,
        tick_limit_reached: ctx.tick_limit_reached,
    }
}

/// Compile the root module's initial block into an executable native testbench.
pub fn compile_initial_testbench<B: SimBackend>(
    sim: &Simulator<B>,
) -> Option<CompiledTestbench<B>> {
    let semantic = sim.program().testbench.clone()?;
    celox_runtime::bind_testbench_program(sim.backend_ref(), semantic)
}

/// Execute a previously compiled native testbench against a built simulator.
pub fn run_compiled_testbench<B: SimBackend>(
    sim: &mut Simulator<B>,
    tb: &CompiledTestbench<B>,
) -> TestResult {
    run_testbench(sim, tb.statements())
}

/// Execute at most `tick_limit` simulator ticks from a compiled testbench.
///
/// Reaching the limit is reported separately from the testbench result so a
/// performance prefix cannot be mistaken for a completed test.
pub fn run_compiled_testbench_with_tick_limit<B: SimBackend>(
    sim: &mut Simulator<B>,
    tb: &CompiledTestbench<B>,
    tick_limit: u64,
) -> LimitedTestbenchResult {
    run_testbench_limited(sim, tb.statements(), Some(tick_limit))
}

/// Run the testbench and return assertion results observed before the test
/// finishes or stops on a fatal failure.
pub(crate) fn run_testbench_detailed<B: SimBackend>(
    sim: &mut Simulator<B>,
    stmts: &[TestbenchStatement<B>],
) -> TestResultDetailed {
    let mut ctx = DetailedExecContext {
        assertions: Vec::new(),
        current_time: 0,
        tick_limit: None,
        tick_limit_reached: false,
    };
    let result = exec_detailed(sim, stmts, &mut ctx);
    let passed = !matches!(result, ExecResult::Fail(_)) && ctx.assertions.iter().all(|a| a.passed);
    TestResultDetailed {
        passed,
        assertions: ctx.assertions,
    }
}

struct DetailedExecContext {
    assertions: Vec<AssertionResult>,
    current_time: u64,
    tick_limit: Option<u64>,
    tick_limit_reached: bool,
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
    last_message: Option<String>,
    fatal_message: Option<String>,
}

/// Forward testbench `$display` / `$write` output without contaminating the
/// machine-readable test result emitted by the CLI on stdout.  In particular,
/// UART models commonly use one `$write("%c", ...)` per transmitted byte, so
/// preserve the event text verbatim and flush it promptly.
fn forward_display(message: &str, newline: bool) {
    use std::io::Write as _;

    let mut stderr = std::io::stderr().lock();
    let _ = stderr.write_all(message.as_bytes());
    if newline {
        let _ = stderr.write_all(b"\n");
    }
    let _ = stderr.flush();
}

fn publish_tb_assert_event<B: SimBackend>(
    sim: &mut Simulator<B>,
    site_id: u32,
    message: &Option<AssertMessage>,
    memory: *mut u8,
) {
    let args = assert_event_args(message)
        .iter()
        .map(|arg| arg.expr.eval_value(memory).to_biguint())
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
    let format_ctx = RuntimeFormatContext {
        tb_time: Some(ctx.current_time),
        scope: None,
    };
    for event in sim.drain_runtime_events_deferred_with_context(format_ctx) {
        match event {
            RuntimeEvent::AssertContinue { message } => {
                last_message = Some(message.clone());
                ctx.assertions.push(AssertionResult {
                    passed: false,
                    message: Some(message),
                    location: location.cloned(),
                });
            }
            RuntimeEvent::AssertFatal { message } => {
                last_message = Some(message.clone());
                if fatal_message.is_none() {
                    fatal_message = Some(message.clone());
                }
                ctx.assertions.push(AssertionResult {
                    passed: false,
                    message: Some(message),
                    location: location.cloned(),
                });
            }
            RuntimeEvent::Missed { count } => {
                let message = format!("missed {count} runtime events");
                last_message = Some(message.clone());
                ctx.assertions.push(AssertionResult {
                    passed: false,
                    message: Some(message),
                    location: None,
                });
            }
            RuntimeEvent::Display { message } => forward_display(&message, false),
        }
    }
    DrainedAssertionEvents {
        last_message,
        fatal_message,
    }
}

/// Like [`exec`] but collects assertion results into `ctx` instead of
/// short-circuiting on the first failure.
fn exec_detailed<B: SimBackend>(
    sim: &mut Simulator<B>,
    stmts: &[TestbenchStatement<B>],
    ctx: &mut DetailedExecContext,
) -> ExecResult {
    for stmt in stmts {
        let r = exec_one_detailed(sim, stmt, ctx);
        // Stop on control-flow transfers or hard errors. Assertion failures that
        // are allowed to continue return `Continue` and are recorded in `ctx`.
        if matches!(
            r,
            ExecResult::Break | ExecResult::Finished | ExecResult::Fail(_)
        ) {
            return r;
        }
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
        GenericTestbenchStatement::ClockNext { clock_event, count } => {
            match eval_clock_count(sim, count) {
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
                        let (completed, result) = sim.tick_deferred_comb_many(*clock_event, batch);
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
                                return ExecResult::Fail(message);
                            }
                            return ExecResult::Fail(format!("{e}"));
                        }
                        if let Some(every) = progress_every
                            && every != 0
                            && ctx.current_time.is_multiple_of(every)
                        {
                            tracing::debug!("[testbench-progress] tick={}", ctx.current_time);
                        }
                        drain_runtime_assertions(sim, ctx, None);
                    }
                    ExecResult::Continue
                }
                Err(error) => ExecResult::Fail(error.to_string()),
            }
        }
        GenericTestbenchStatement::ResetAssert {
            reset_signal,
            clock_event,
            duration,
            assert_value,
            deassert_value,
        } => match eval_clock_count(sim, duration) {
            Ok(duration) => {
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
                    let (completed, result) = sim.tick_deferred_comb_many(*clock_event, batch);
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
                            return ExecResult::Fail(message);
                        }
                        return ExecResult::Fail(format!("reset: {e}"));
                    }
                    drain_runtime_assertions(sim, ctx, None);
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
            if let Err(e) = sim.eval_comb() {
                return ExecResult::Fail(format!("eval_comb: {e}"));
            }
            let (ptr, _) = sim.memory_as_mut_ptr();
            let passed = expr.eval_bool(ptr);
            if passed {
                let rendered_message = render_assert_message(message, ptr, ctx.current_time);
                ctx.assertions.push(AssertionResult {
                    passed,
                    message: rendered_message.clone(),
                    location: location.clone(),
                });
                ExecResult::Continue
            } else {
                publish_tb_assert_event(sim, *site_id, message, ptr);
                let rendered_message = drain_runtime_assertions(sim, ctx, location.as_ref())
                    .last_message
                    .or_else(|| render_assert_message(message, ptr, ctx.current_time));
                if !continue_on_fail {
                    ExecResult::Fail(
                        rendered_message.unwrap_or_else(|| "assertion failed".to_string()),
                    )
                } else {
                    ExecResult::Continue
                }
            }
        }
        GenericTestbenchStatement::Display { message, newline } => {
            if let Err(e) = sim.eval_comb() {
                return ExecResult::Fail(format!("eval_comb: {e}"));
            }
            let (ptr, _) = sim.memory_as_mut_ptr();
            let rendered =
                render_assert_message(message, ptr, ctx.current_time).unwrap_or_default();
            forward_display(&rendered, *newline);
            ExecResult::Continue
        }
        GenericTestbenchStatement::If {
            expr,
            then_block,
            else_block,
        } => {
            if let Err(e) = sim.eval_comb() {
                return ExecResult::Fail(format!("eval_comb: {e}"));
            }
            let (ptr, _) = sim.memory_as_mut_ptr();
            if expr.eval_bool(ptr) {
                exec_detailed(sim, then_block, ctx)
            } else {
                exec_detailed(sim, else_block, ctx)
            }
        }
        GenericTestbenchStatement::For {
            loop_var,
            start,
            end,
            inclusive,
            step,
            step_op,
            reverse,
            body,
        } => exec_for_loop(
            sim,
            loop_var,
            start,
            end,
            *inclusive,
            *step,
            *step_op,
            *reverse,
            |sim| exec_detailed(sim, body, ctx),
        ),
        GenericTestbenchStatement::Assign { dst, expr } => {
            if let Err(e) = sim.eval_comb() {
                return ExecResult::Fail(format!("eval_comb: {e}"));
            }
            let (ptr, _) = sim.memory_as_mut_ptr();
            let val = expr.eval_value(ptr);
            match val {
                TbValue::U64(v) => sim_set_u64(sim, *dst, v),
                TbValue::Wide(v) => sim.set_wide(*dst, v),
            }
            ExecResult::Continue
        }
        GenericTestbenchStatement::Break => ExecResult::Break,
        GenericTestbenchStatement::Finish => ExecResult::Finished,
    }
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
}
