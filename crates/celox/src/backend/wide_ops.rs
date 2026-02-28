//! Wide-integer (>64-bit) multi-chunk arithmetic for the Cranelift JIT backend.
//!
//! Values wider than 64 bits are represented as `Vec<Value>` where each element
//! is an `i64` chunk in little-endian order (chunk 0 = LSB).

use cranelift::prelude::*;
use cranelift_frontend::FunctionBuilder;

use crate::ir::{BinaryOp, UnaryOp};

use super::translator::core::{cast_type, get_chunk_as_i64};

// ─────────────────────────────────────────────────────────
//  Wide Binary Operations
// ─────────────────────────────────────────────────────────

/// Emit Cranelift IR for a wide (multi-chunk) binary operation.
///
/// Returns the result chunks (`Vec<Value>`, each `i64`).
pub fn emit_wide_binary(
    builder: &mut FunctionBuilder,
    op: &BinaryOp,
    l_chunks: &[Value],
    r_chunks: &[Value],
    num_chunks: usize,
    l_width: usize,
) -> Vec<Value> {
    match op {
        // 1. Bitwise operations ─────────────────────────────
        BinaryOp::And | BinaryOp::Or | BinaryOp::Xor => {
            emit_wide_bitwise(builder, op, l_chunks, r_chunks, num_chunks)
        }

        // 2. Addition (carry propagation) ───────────────────
        BinaryOp::Add => emit_wide_add(builder, l_chunks, r_chunks, num_chunks),

        // 3. Subtraction (borrow propagation) ───────────────
        BinaryOp::Sub => emit_wide_sub(builder, l_chunks, r_chunks, num_chunks),

        // 4. Logical shifts ─────────────────────────────────
        BinaryOp::Shr | BinaryOp::Shl => {
            emit_wide_shift(builder, op, l_chunks, r_chunks, num_chunks)
        }

        // 5. Unsigned and equality comparisons ──────────────
        BinaryOp::Eq
        | BinaryOp::Ne
        | BinaryOp::LtU
        | BinaryOp::GtU
        | BinaryOp::LeU
        | BinaryOp::GeU => emit_wide_unsigned_cmp(builder, op, l_chunks, r_chunks, num_chunks),

        // 6. Multiplication (schoolbook O(n²)) ──────────────
        BinaryOp::Mul => emit_wide_mul(builder, l_chunks, r_chunks, num_chunks),

        // 7. Arithmetic right shift (sign extension) ────────
        BinaryOp::Sar => emit_wide_sar(builder, l_chunks, r_chunks, num_chunks, l_width),

        // 8. Signed comparisons ─────────────────────────────
        BinaryOp::LtS | BinaryOp::LeS | BinaryOp::GtS | BinaryOp::GeS => {
            emit_wide_signed_cmp(builder, op, l_chunks, r_chunks, num_chunks)
        }

        // 9. Division / remainder ───────────────────────────
        BinaryOp::Div | BinaryOp::Rem => {
            emit_wide_divrem(builder, op, l_chunks, r_chunks, num_chunks)
        }

        // In Veryl syntax, `&&` / `||` map to `BinaryOp::LogicAnd` / `BinaryOp::LogicOr`.
        // These are always routed to `emit_wide_logic_andor` by the translator.
        BinaryOp::LogicAnd | BinaryOp::LogicOr => {
            unreachable!("LogicAnd/LogicOr must be handled by emit_wide_logic_andor")
        }

        // EqWildcard/NeWildcard: value computation is same as Eq/Ne.
        // Wildcard mask semantics are handled separately in arith.rs.
        BinaryOp::EqWildcard => emit_wide_unsigned_cmp(builder, &BinaryOp::Eq, l_chunks, r_chunks, num_chunks),
        BinaryOp::NeWildcard => emit_wide_unsigned_cmp(builder, &BinaryOp::Ne, l_chunks, r_chunks, num_chunks),
    }
}

/// Emit Cranelift IR for wide LogicAnd / LogicOr.
///
/// These reduce both operands to a bool first, so they don't fit the normal
/// chunk-wise pattern. Returns the result chunks.
pub fn emit_wide_logic_andor(
    builder: &mut FunctionBuilder,
    op: &BinaryOp,
    l_chunks: &[Value],
    r_chunks: &[Value],
    num_chunks: usize,
) -> Vec<Value> {
    let reduce_to_bool = |b: &mut FunctionBuilder, chunks: &[Value]| -> Value {
        let mut accumulated = b.ins().iconst(types::I64, 0);
        for &chunk in chunks {
            accumulated = b.ins().bor(accumulated, chunk);
        }
        b.ins().icmp_imm(IntCC::NotEqual, accumulated, 0)
    };

    let l_bool = reduce_to_bool(builder, l_chunks);
    let r_bool = reduce_to_bool(builder, r_chunks);

    let res_bool = if matches!(op, BinaryOp::LogicAnd) {
        builder.ins().band(l_bool, r_bool)
    } else {
        builder.ins().bor(l_bool, r_bool)
    };

    let one = builder.ins().iconst(types::I64, 1);
    let zero = builder.ins().iconst(types::I64, 0);
    let res_val = builder.ins().select(res_bool, one, zero);

    let mut res_chunks = Vec::with_capacity(num_chunks);
    res_chunks.push(res_val);
    for _ in 1..num_chunks {
        res_chunks.push(zero);
    }
    res_chunks
}

// ─────────────────────────────────────────────────────────
//  Wide Unary Operations
// ─────────────────────────────────────────────────────────

/// Emit Cranelift IR for a wide (multi-chunk) unary operation.
///
/// `common_logical_width` is the total logical width in bits (e.g. 128).
pub fn emit_wide_unary(
    builder: &mut FunctionBuilder,
    op: &UnaryOp,
    r_chunks: &[Value],
    num_chunks: usize,
    common_logical_width: usize,
) -> Vec<Value> {
    match op {
        UnaryOp::Minus => emit_wide_negate(builder, r_chunks, num_chunks),
        UnaryOp::Ident => emit_wide_ident(builder, r_chunks, num_chunks),
        UnaryOp::BitNot => emit_wide_bitnot(builder, r_chunks, num_chunks),
        UnaryOp::LogicNot => emit_wide_logical_not(builder, r_chunks, num_chunks),
        UnaryOp::Or => emit_wide_reduction_or(builder, r_chunks, num_chunks),
        UnaryOp::Xor => emit_wide_reduction_xor(builder, r_chunks, num_chunks),
        UnaryOp::And => {
            emit_wide_reduction_and(builder, r_chunks, num_chunks, common_logical_width)
        }
    }
}

// ═════════════════════════════════════════════════════════
//  Private helpers – Binary
// ═════════════════════════════════════════════════════════

fn emit_wide_bitwise(
    builder: &mut FunctionBuilder,
    op: &BinaryOp,
    l_chunks: &[Value],
    r_chunks: &[Value],
    num_chunks: usize,
) -> Vec<Value> {
    let mut res = Vec::with_capacity(num_chunks);
    for i in 0..num_chunks {
        let l = get_chunk_as_i64(builder, l_chunks, i);
        let r = get_chunk_as_i64(builder, r_chunks, i);
        let v = match op {
            BinaryOp::And => builder.ins().band(l, r),
            BinaryOp::Or => builder.ins().bor(l, r),
            BinaryOp::Xor => builder.ins().bxor(l, r),
            _ => unreachable!(),
        };
        res.push(v);
    }
    res
}

fn emit_wide_add(
    builder: &mut FunctionBuilder,
    l_chunks: &[Value],
    r_chunks: &[Value],
    num_chunks: usize,
) -> Vec<Value> {
    let mut res = Vec::with_capacity(num_chunks);
    let mut carry = None;
    for i in 0..num_chunks {
        let l = get_chunk_as_i64(builder, l_chunks, i);
        let r = get_chunk_as_i64(builder, r_chunks, i);
        let (sum, cout) = match carry {
            None => {
                let s = builder.ins().iadd(l, r);
                let c = builder.ins().icmp(IntCC::UnsignedLessThan, s, l);
                (s, c)
            }
            Some(cin) => {
                let cin_i64 = builder.ins().uextend(types::I64, cin);
                let s1 = builder.ins().iadd(l, r);
                let c1 = builder.ins().icmp(IntCC::UnsignedLessThan, s1, l);
                let s2 = builder.ins().iadd(s1, cin_i64);
                let c2 = builder.ins().icmp(IntCC::UnsignedLessThan, s2, s1);
                let cout = builder.ins().bor(c1, c2);
                (s2, cout)
            }
        };
        res.push(sum);
        carry = Some(cout);
    }
    res
}

fn emit_wide_sub(
    builder: &mut FunctionBuilder,
    l_chunks: &[Value],
    r_chunks: &[Value],
    num_chunks: usize,
) -> Vec<Value> {
    let mut res = Vec::with_capacity(num_chunks);
    let mut borrow = None;
    for i in 0..num_chunks {
        let l = get_chunk_as_i64(builder, l_chunks, i);
        let r = get_chunk_as_i64(builder, r_chunks, i);
        let (diff, bout) = match borrow {
            None => {
                let d = builder.ins().isub(l, r);
                let b = builder.ins().icmp(IntCC::UnsignedGreaterThan, r, l);
                (d, b)
            }
            Some(bin) => {
                let bin_i64 = builder.ins().uextend(types::I64, bin);
                let d1 = builder.ins().isub(l, r);
                let b1 = builder.ins().icmp(IntCC::UnsignedGreaterThan, r, l);
                let d2 = builder.ins().isub(d1, bin_i64);
                let b2 = builder.ins().icmp(IntCC::UnsignedGreaterThan, bin_i64, d1);
                let bout = builder.ins().bor(b1, b2);
                (d2, bout)
            }
        };
        res.push(diff);
        borrow = Some(bout);
    }
    res
}

pub(crate) fn emit_wide_shift(
    builder: &mut FunctionBuilder,
    op: &BinaryOp,
    l_chunks: &[Value],
    r_chunks: &[Value],
    num_chunks: usize,
) -> Vec<Value> {
    let shift_amt_raw = r_chunks[0];
    let shift_amt_total = cast_type(builder, shift_amt_raw, types::I64);
    let bit_shift = builder.ins().band_imm(shift_amt_total, 63);
    let word_offset_val = builder.ins().ushr_imm(shift_amt_total, 6);
    let sixty_four = builder.ins().iconst(types::I64, 64);
    let inv_bit_shift = builder.ins().isub(sixty_four, bit_shift);
    let has_bit_shift = builder.ins().icmp_imm(IntCC::NotEqual, bit_shift, 0);

    let mut res = Vec::with_capacity(num_chunks);
    for i in 0..num_chunks {
        let mut cur_word = builder.ins().iconst(types::I64, 0);
        let mut nxt_word = builder.ins().iconst(types::I64, 0);

        let (idx_cur, idx_nxt) = if matches!(op, BinaryOp::Shr) {
            let base = builder.ins().iadd_imm(word_offset_val, i as i64);
            let next = builder.ins().iadd_imm(base, 1);
            (base, next)
        } else {
            let base = builder.ins().irsub_imm(word_offset_val, i as i64);
            let prev = builder.ins().iadd_imm(base, -1);
            (base, prev)
        };

        for (src_i, &src_val) in l_chunks.iter().enumerate() {
            let src_val_i64 = cast_type(builder, src_val, types::I64);
            let is_cur = builder.ins().icmp_imm(IntCC::Equal, idx_cur, src_i as i64);
            let is_nxt = builder.ins().icmp_imm(IntCC::Equal, idx_nxt, src_i as i64);
            cur_word = builder.ins().select(is_cur, src_val_i64, cur_word);
            nxt_word = builder.ins().select(is_nxt, src_val_i64, nxt_word);
        }

        let chunk_res = if matches!(op, BinaryOp::Shr) {
            let low = builder.ins().ushr(cur_word, bit_shift);
            let high = builder.ins().ishl(nxt_word, inv_bit_shift);
            let zero = builder.ins().iconst(types::I64, 0);
            let high_part = builder.ins().select(has_bit_shift, high, zero);
            builder.ins().bor(low, high_part)
        } else {
            let high = builder.ins().ishl(cur_word, bit_shift);
            let low = builder.ins().ushr(nxt_word, inv_bit_shift);
            let zero = builder.ins().iconst(types::I64, 0);
            let low_part = builder.ins().select(has_bit_shift, low, zero);
            builder.ins().bor(high, low_part)
        };
        res.push(chunk_res);
    }
    res
}

pub(crate) fn emit_wide_unsigned_cmp(
    builder: &mut FunctionBuilder,
    op: &BinaryOp,
    l_chunks: &[Value],
    r_chunks: &[Value],
    num_chunks: usize,
) -> Vec<Value> {
    let res_b1 = match op {
        BinaryOp::Eq | BinaryOp::Ne => {
            let mut cond = builder
                .ins()
                .iconst(types::I8, if matches!(op, BinaryOp::Eq) { 1 } else { 0 });
            for i in 0..num_chunks {
                let l = get_chunk_as_i64(builder, l_chunks, i);
                let r = get_chunk_as_i64(builder, r_chunks, i);
                let eq = builder.ins().icmp(IntCC::Equal, l, r);
                cond = if matches!(op, BinaryOp::Eq) {
                    builder.ins().band(cond, eq)
                } else {
                    let neq = builder.ins().bnot(eq);
                    builder.ins().bor(cond, neq)
                };
            }
            cond
        }
        _ => {
            // LtU, GtU, LeU, GeU
            // Result when both operands are equal:
            // LeU/GeU => true, LtU/GtU => false.
            let init_val = if matches!(op, BinaryOp::LeU | BinaryOp::GeU) {
                1i64
            } else {
                0i64
            };
            let mut res = builder.ins().iconst(types::I8, init_val);
            for i in 0..num_chunks {
                let l = get_chunk_as_i64(builder, l_chunks, i);
                let r = get_chunk_as_i64(builder, r_chunks, i);
                let eq = builder.ins().icmp(IntCC::Equal, l, r);
                let cmp = builder.ins().icmp(
                    match op {
                        BinaryOp::LtU | BinaryOp::LeU => IntCC::UnsignedLessThan,
                        BinaryOp::GtU | BinaryOp::GeU => IntCC::UnsignedGreaterThan,
                        _ => unreachable!(),
                    },
                    l,
                    r,
                );
                res = builder.ins().select(eq, res, cmp);
            }
            res
        }
    };

    let mut result = Vec::with_capacity(num_chunks);
    result.push(builder.ins().uextend(types::I64, res_b1));
    for _ in 1..num_chunks {
        result.push(builder.ins().iconst(types::I64, 0));
    }
    result
}

fn emit_wide_mul(
    builder: &mut FunctionBuilder,
    l_chunks: &[Value],
    r_chunks: &[Value],
    num_chunks: usize,
) -> Vec<Value> {
    let mut acc: Vec<Value> = (0..num_chunks)
        .map(|_| builder.ins().iconst(types::I64, 0))
        .collect();

    for i in 0..num_chunks {
        let a_i = get_chunk_as_i64(builder, l_chunks, i);
        let mut carry = builder.ins().iconst(types::I64, 0);
        for j in 0..num_chunks {
            let k = i + j;
            if k >= num_chunks {
                break;
            }
            let b_j = get_chunk_as_i64(builder, r_chunks, j);
            let lo = builder.ins().imul(a_i, b_j);
            let hi = builder.ins().umulhi(a_i, b_j);
            let sum1 = builder.ins().iadd(acc[k], lo);
            let c1 = builder.ins().icmp(IntCC::UnsignedLessThan, sum1, acc[k]);
            let sum2 = builder.ins().iadd(sum1, carry);
            let c2 = builder.ins().icmp(IntCC::UnsignedLessThan, sum2, sum1);
            acc[k] = sum2;
            let c1_ext = builder.ins().uextend(types::I64, c1);
            let c2_ext = builder.ins().uextend(types::I64, c2);
            carry = builder.ins().iadd(hi, c1_ext);
            carry = builder.ins().iadd(carry, c2_ext);
        }
    }
    acc
}

pub(crate) fn emit_wide_sar(
    builder: &mut FunctionBuilder,
    l_chunks: &[Value],
    r_chunks: &[Value],
    num_chunks: usize,
    l_width: usize,
) -> Vec<Value> {
    let shift_amt_raw = r_chunks[0];
    let shift_amt_total = cast_type(builder, shift_amt_raw, types::I64);
    let bit_shift = builder.ins().band_imm(shift_amt_total, 63);
    let word_offset_val = builder.ins().ushr_imm(shift_amt_total, 6);
    let sixty_four = builder.ins().iconst(types::I64, 64);
    let inv_bit_shift = builder.ins().isub(sixty_four, bit_shift);
    let has_bit_shift = builder.ins().icmp_imm(IntCC::NotEqual, bit_shift, 0);

    // Calculate sign fill based on the logical MSB of the operand
    let msb_bit_idx = (l_width - 1) % 64;
    let msb_chunk_idx = (l_width - 1) / 64;
    let msb_chunk = get_chunk_as_i64(builder, l_chunks, msb_chunk_idx);
    let sign_bit = builder.ins().ushr_imm(msb_chunk, msb_bit_idx as i64);
    let is_negative = builder.ins().band_imm(sign_bit, 1);
    let zero = builder.ins().iconst(types::I64, 0);
    let all_ones = builder.ins().iconst(types::I64, -1);
    let sign_fill = builder.ins().select(is_negative, all_ones, zero);

    let mut res = Vec::with_capacity(num_chunks);
    for i in 0..num_chunks {
        let mut cur_word = sign_fill;
        let mut nxt_word = sign_fill;

        let idx_cur = builder.ins().iadd_imm(word_offset_val, i as i64);
        let idx_nxt = builder.ins().iadd_imm(idx_cur, 1);

        for (src_i, &src_val) in l_chunks.iter().enumerate() {
            let src_val_i64 = cast_type(builder, src_val, types::I64);
            let is_cur = builder.ins().icmp_imm(IntCC::Equal, idx_cur, src_i as i64);
            let is_nxt = builder.ins().icmp_imm(IntCC::Equal, idx_nxt, src_i as i64);
            cur_word = builder.ins().select(is_cur, src_val_i64, cur_word);
            nxt_word = builder.ins().select(is_nxt, src_val_i64, nxt_word);
        }

        let low = builder.ins().ushr(cur_word, bit_shift);
        let high = builder.ins().ishl(nxt_word, inv_bit_shift);
        let zero = builder.ins().iconst(types::I64, 0);
        let high_part = builder.ins().select(has_bit_shift, high, zero);
        res.push(builder.ins().bor(low, high_part));
    }
    res
}

fn emit_wide_signed_cmp(
    builder: &mut FunctionBuilder,
    op: &BinaryOp,
    l_chunks: &[Value],
    r_chunks: &[Value],
    num_chunks: usize,
) -> Vec<Value> {
    // Result when both operands are equal:
    // LeS/GeS => true, LtS/GtS => false.
    let init_val = if matches!(op, BinaryOp::LeS | BinaryOp::GeS) {
        1i64
    } else {
        0i64
    };
    let mut res = builder.ins().iconst(types::I8, init_val);
    for i in 0..num_chunks {
        let l = get_chunk_as_i64(builder, l_chunks, i);
        let r = get_chunk_as_i64(builder, r_chunks, i);
        let eq = builder.ins().icmp(IntCC::Equal, l, r);
        let cmp = if i == num_chunks - 1 {
            // The most-significant chunk uses signed (strict) comparison.
            builder.ins().icmp(
                match op {
                    BinaryOp::LtS | BinaryOp::LeS => IntCC::SignedLessThan,
                    BinaryOp::GtS | BinaryOp::GeS => IntCC::SignedGreaterThan,
                    _ => unreachable!(),
                },
                l,
                r,
            )
        } else {
            // Lower chunks use unsigned (strict) comparison.
            builder.ins().icmp(
                match op {
                    BinaryOp::LtS | BinaryOp::LeS => IntCC::UnsignedLessThan,
                    BinaryOp::GtS | BinaryOp::GeS => IntCC::UnsignedGreaterThan,
                    _ => unreachable!(),
                },
                l,
                r,
            )
        };
        res = builder.ins().select(eq, res, cmp);
    }

    let mut result = Vec::with_capacity(num_chunks);
    result.push(builder.ins().uextend(types::I64, res));
    for _ in 1..num_chunks {
        result.push(builder.ins().iconst(types::I64, 0));
    }
    result
}

fn emit_wide_divrem(
    builder: &mut FunctionBuilder,
    op: &BinaryOp,
    l_chunks: &[Value],
    r_chunks: &[Value],
    num_chunks: usize,
) -> Vec<Value> {
    let total_bits = num_chunks * 64;

    let mut q_chunks: Vec<Value> = (0..num_chunks)
        .map(|_| builder.ins().iconst(types::I64, 0))
        .collect();
    let mut rem_chunks: Vec<Value> = (0..num_chunks)
        .map(|_| builder.ins().iconst(types::I64, 0))
        .collect();

    for bit in (0..total_bits).rev() {
        let chunk_idx = bit / 64;
        let bit_idx = bit % 64;

        // remainder <<= 1
        for c in (0..num_chunks).rev() {
            let shifted = builder.ins().ishl_imm(rem_chunks[c], 1);
            if c > 0 {
                let carry_bit = builder.ins().ushr_imm(rem_chunks[c - 1], 63);
                rem_chunks[c] = builder.ins().bor(shifted, carry_bit);
            } else {
                rem_chunks[c] = shifted;
            }
        }

        // remainder[0] |= (dividend[chunk_idx] >> bit_idx) & 1
        let dividend_chunk = get_chunk_as_i64(builder, l_chunks, chunk_idx);
        let extracted = builder.ins().ushr_imm(dividend_chunk, bit_idx as i64);
        let one_bit = builder.ins().band_imm(extracted, 1);
        rem_chunks[0] = builder.ins().bor(rem_chunks[0], one_bit);

        // if remainder >= divisor (multi-chunk unsigned GE)
        let mut ge_result = builder.ins().iconst(types::I8, 1);
        for c in 0..num_chunks {
            let rc = rem_chunks[c];
            let dc = get_chunk_as_i64(builder, r_chunks, c);
            let eq = builder.ins().icmp(IntCC::Equal, rc, dc);
            let gt = builder
                .ins()
                .icmp(IntCC::UnsignedGreaterThanOrEqual, rc, dc);
            ge_result = builder.ins().select(eq, ge_result, gt);
        }

        // conditional: remainder -= divisor
        let mut new_rem = Vec::with_capacity(num_chunks);
        {
            let mut borrow: Option<Value> = None;
            for c in 0..num_chunks {
                let rc = rem_chunks[c];
                let dc = get_chunk_as_i64(builder, r_chunks, c);
                let (diff, bout) = match borrow {
                    None => {
                        let d = builder.ins().isub(rc, dc);
                        let b = builder.ins().icmp(IntCC::UnsignedGreaterThan, dc, rc);
                        (d, b)
                    }
                    Some(bin) => {
                        let bin_i64 = builder.ins().uextend(types::I64, bin);
                        let d1 = builder.ins().isub(rc, dc);
                        let b1 = builder.ins().icmp(IntCC::UnsignedGreaterThan, dc, rc);
                        let d2 = builder.ins().isub(d1, bin_i64);
                        let b2 = builder.ins().icmp(IntCC::UnsignedGreaterThan, bin_i64, d1);
                        let bout = builder.ins().bor(b1, b2);
                        (d2, bout)
                    }
                };
                new_rem.push(builder.ins().select(ge_result, diff, rc));
                borrow = Some(bout);
            }
        }
        rem_chunks = new_rem;

        // quotient[chunk_idx] |= ge ? (1 << bit_idx) : 0
        let bit_mask = builder.ins().iconst(types::I64, 1i64 << bit_idx);
        let zero = builder.ins().iconst(types::I64, 0);
        let masked = builder.ins().select(ge_result, bit_mask, zero);
        q_chunks[chunk_idx] = builder.ins().bor(q_chunks[chunk_idx], masked);
    }

    if matches!(op, BinaryOp::Div) {
        q_chunks
    } else {
        rem_chunks
    }
}

// ═════════════════════════════════════════════════════════
//  Private helpers – Unary
// ═════════════════════════════════════════════════════════

fn emit_wide_negate(
    builder: &mut FunctionBuilder,
    r_chunks: &[Value],
    num_chunks: usize,
) -> Vec<Value> {
    // Two's complement negate: ~chunks + 1 with carry propagation
    let mut res = Vec::with_capacity(num_chunks);
    let mut carry: Option<Value> = None;
    for i in 0..num_chunks {
        let r = get_chunk_as_i64(builder, r_chunks, i);
        let inv = builder.ins().bnot(r);
        let (sum, cout) = match carry {
            None => {
                let one = builder.ins().iconst(types::I64, 1);
                let s = builder.ins().iadd(inv, one);
                let c = builder.ins().icmp(IntCC::UnsignedLessThan, s, inv);
                (s, c)
            }
            Some(cin) => {
                let cin_i64 = builder.ins().uextend(types::I64, cin);
                let s1 = builder.ins().iadd(inv, cin_i64);
                let c = builder.ins().icmp(IntCC::UnsignedLessThan, s1, inv);
                (s1, c)
            }
        };
        res.push(sum);
        carry = Some(cout);
    }
    res
}

fn emit_wide_ident(
    builder: &mut FunctionBuilder,
    r_chunks: &[Value],
    num_chunks: usize,
) -> Vec<Value> {
    (0..num_chunks)
        .map(|i| get_chunk_as_i64(builder, r_chunks, i))
        .collect()
}

fn emit_wide_bitnot(
    builder: &mut FunctionBuilder,
    r_chunks: &[Value],
    num_chunks: usize,
) -> Vec<Value> {
    (0..num_chunks)
        .map(|i| {
            let r = get_chunk_as_i64(builder, r_chunks, i);
            builder.ins().bnot(r)
        })
        .collect()
}

fn emit_wide_logical_not(
    builder: &mut FunctionBuilder,
    r_chunks: &[Value],
    num_chunks: usize,
) -> Vec<Value> {
    let mut accumulated = builder.ins().iconst(types::I64, 0);
    for i in 0..num_chunks {
        let r = get_chunk_as_i64(builder, r_chunks, i);
        accumulated = builder.ins().bor(accumulated, r);
    }
    let is_zero = builder.ins().icmp_imm(IntCC::Equal, accumulated, 0);
    let one = builder.ins().iconst(types::I64, 1);
    let zero = builder.ins().iconst(types::I64, 0);

    let mut res = Vec::with_capacity(num_chunks);
    res.push(builder.ins().select(is_zero, one, zero));
    for _ in 1..num_chunks {
        res.push(builder.ins().iconst(types::I64, 0));
    }
    res
}

fn emit_wide_reduction_or(
    builder: &mut FunctionBuilder,
    r_chunks: &[Value],
    num_chunks: usize,
) -> Vec<Value> {
    let mut accumulated = builder.ins().iconst(types::I64, 0);
    for i in 0..num_chunks {
        let r = get_chunk_as_i64(builder, r_chunks, i);
        accumulated = builder.ins().bor(accumulated, r);
    }
    let is_nz = builder.ins().icmp_imm(IntCC::NotEqual, accumulated, 0);
    let one = builder.ins().iconst(types::I64, 1);
    let zero = builder.ins().iconst(types::I64, 0);

    let mut res = Vec::with_capacity(num_chunks);
    res.push(builder.ins().select(is_nz, one, zero));
    for _ in 1..num_chunks {
        res.push(builder.ins().iconst(types::I64, 0));
    }
    res
}

fn emit_wide_reduction_xor(
    builder: &mut FunctionBuilder,
    r_chunks: &[Value],
    num_chunks: usize,
) -> Vec<Value> {
    let mut parity = builder.ins().iconst(types::I64, 0);
    for i in 0..num_chunks {
        let r = get_chunk_as_i64(builder, r_chunks, i);
        let pc = builder.ins().popcnt(r);
        parity = builder.ins().bxor(parity, pc);
    }

    let mut res = Vec::with_capacity(num_chunks);
    res.push(builder.ins().band_imm(parity, 1));
    for _ in 1..num_chunks {
        res.push(builder.ins().iconst(types::I64, 0));
    }
    res
}

fn emit_wide_reduction_and(
    builder: &mut FunctionBuilder,
    r_chunks: &[Value],
    num_chunks: usize,
    common_logical_width: usize,
) -> Vec<Value> {
    let mut all_ones = builder.ins().iconst(types::I8, 1);
    for i in 0..num_chunks {
        let r = get_chunk_as_i64(builder, r_chunks, i);
        let expected = if i == num_chunks - 1 {
            let remaining = common_logical_width - i * 64;
            if remaining >= 64 {
                -1i64
            } else {
                ((1u64 << remaining) - 1) as i64
            }
        } else {
            -1i64
        };
        let exp_val = builder.ins().iconst(types::I64, expected);
        let eq = builder.ins().icmp(IntCC::Equal, r, exp_val);
        all_ones = builder.ins().band(all_ones, eq);
    }
    let one = builder.ins().iconst(types::I64, 1);
    let zero = builder.ins().iconst(types::I64, 0);

    let mut res = Vec::with_capacity(num_chunks);
    res.push(builder.ins().select(all_ones, one, zero));
    for _ in 1..num_chunks {
        res.push(builder.ins().iconst(types::I64, 0));
    }
    res
}
