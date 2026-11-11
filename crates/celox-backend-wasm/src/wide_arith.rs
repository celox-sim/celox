//! Multi-chunk multiplication, shifts and signed magnitudes.

use super::*;

pub(super) fn emit_u64_mul_hi(
    instrs: &mut Vec<Instruction<'static>>,
    lhs_local: u32,
    rhs_local: u32,
    dst_local: u32,
    locals: &mut LocalAllocator,
) {
    let mask = locals.alloc(1);
    let a0 = locals.alloc(1);
    let a1 = locals.alloc(1);
    let b0 = locals.alloc(1);
    let b1 = locals.alloc(1);
    let lo_lo = locals.alloc(1);
    let lo_hi = locals.alloc(1);
    let hi_lo = locals.alloc(1);
    let hi_hi = locals.alloc(1);
    let cross = locals.alloc(1);

    instrs.push(Instruction::I64Const(0xFFFF_FFFFu64 as i64));
    instrs.push(Instruction::LocalSet(mask));

    instrs.push(Instruction::LocalGet(lhs_local));
    instrs.push(Instruction::LocalGet(mask));
    instrs.push(Instruction::I64And);
    instrs.push(Instruction::LocalSet(a0));
    instrs.push(Instruction::LocalGet(lhs_local));
    instrs.push(Instruction::I64Const(32));
    instrs.push(Instruction::I64ShrU);
    instrs.push(Instruction::LocalSet(a1));

    instrs.push(Instruction::LocalGet(rhs_local));
    instrs.push(Instruction::LocalGet(mask));
    instrs.push(Instruction::I64And);
    instrs.push(Instruction::LocalSet(b0));
    instrs.push(Instruction::LocalGet(rhs_local));
    instrs.push(Instruction::I64Const(32));
    instrs.push(Instruction::I64ShrU);
    instrs.push(Instruction::LocalSet(b1));

    instrs.push(Instruction::LocalGet(a0));
    instrs.push(Instruction::LocalGet(b0));
    instrs.push(Instruction::I64Mul);
    instrs.push(Instruction::LocalSet(lo_lo));

    instrs.push(Instruction::LocalGet(a0));
    instrs.push(Instruction::LocalGet(b1));
    instrs.push(Instruction::I64Mul);
    instrs.push(Instruction::LocalSet(lo_hi));

    instrs.push(Instruction::LocalGet(a1));
    instrs.push(Instruction::LocalGet(b0));
    instrs.push(Instruction::I64Mul);
    instrs.push(Instruction::LocalSet(hi_lo));

    instrs.push(Instruction::LocalGet(a1));
    instrs.push(Instruction::LocalGet(b1));
    instrs.push(Instruction::I64Mul);
    instrs.push(Instruction::LocalSet(hi_hi));

    instrs.push(Instruction::LocalGet(lo_lo));
    instrs.push(Instruction::I64Const(32));
    instrs.push(Instruction::I64ShrU);
    instrs.push(Instruction::LocalGet(lo_hi));
    instrs.push(Instruction::LocalGet(mask));
    instrs.push(Instruction::I64And);
    instrs.push(Instruction::I64Add);
    instrs.push(Instruction::LocalGet(hi_lo));
    instrs.push(Instruction::LocalGet(mask));
    instrs.push(Instruction::I64And);
    instrs.push(Instruction::I64Add);
    instrs.push(Instruction::LocalSet(cross));

    instrs.push(Instruction::LocalGet(hi_hi));
    instrs.push(Instruction::LocalGet(lo_hi));
    instrs.push(Instruction::I64Const(32));
    instrs.push(Instruction::I64ShrU);
    instrs.push(Instruction::I64Add);
    instrs.push(Instruction::LocalGet(hi_lo));
    instrs.push(Instruction::I64Const(32));
    instrs.push(Instruction::I64ShrU);
    instrs.push(Instruction::I64Add);
    instrs.push(Instruction::LocalGet(cross));
    instrs.push(Instruction::I64Const(32));
    instrs.push(Instruction::I64ShrU);
    instrs.push(Instruction::I64Add);
    instrs.push(Instruction::LocalSet(dst_local));
}

/// Wide shift left/right using word-offset + bit-shift decomposition.
pub(super) fn emit_wide_shift(
    op: &BinaryOp,
    l: &RegLocal,
    r: &RegLocal,
    d: &RegLocal,
    d_chunks: usize,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    // shift_amt = r[0]
    // word_off = shift_amt >> 6
    // bit_off = shift_amt & 63
    let word_off = locals.alloc(1);
    let bit_off = locals.alloc(1);
    let inv_bit = locals.alloc(1);
    let acc = locals.alloc(1);

    instrs.push(Instruction::LocalGet(r.value_idx));
    instrs.push(Instruction::I64Const(6));
    instrs.push(Instruction::I64ShrU);
    instrs.push(Instruction::LocalSet(word_off));

    instrs.push(Instruction::LocalGet(r.value_idx));
    instrs.push(Instruction::I64Const(63));
    instrs.push(Instruction::I64And);
    instrs.push(Instruction::LocalSet(bit_off));

    instrs.push(Instruction::I64Const(64));
    instrs.push(Instruction::LocalGet(bit_off));
    instrs.push(Instruction::I64Sub);
    instrs.push(Instruction::LocalSet(inv_bit));

    // For each result chunk, find the source chunks and combine.
    // Shr: d[i] = (l[i+word_off] >> bit_off) | (l[i+word_off+1] << inv_bit)
    // Shl: d[i] = (l[i-word_off] << bit_off) | (l[i-word_off-1] >> inv_bit)
    // Use a local accumulator to keep the WASM stack shape trivial.
    for i in 0..d_chunks {
        instrs.push(Instruction::I64Const(0));
        instrs.push(Instruction::LocalSet(acc));
        for j in 0..l.num_chunks {
            let src_idx = j as i64;
            let target_idx = i as i64;

            instrs.push(Instruction::I64Const(target_idx));
            if matches!(op, BinaryOp::Shr) {
                instrs.push(Instruction::LocalGet(word_off));
                instrs.push(Instruction::I64Add);
            } else {
                instrs.push(Instruction::LocalGet(word_off));
                instrs.push(Instruction::I64Sub);
            }
            instrs.push(Instruction::I64Const(src_idx));
            instrs.push(Instruction::I64Eq);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::LocalGet(acc));
            instrs.push(Instruction::LocalGet(l.value_idx + j as u32));
            if matches!(op, BinaryOp::Shr) {
                instrs.push(Instruction::LocalGet(bit_off));
                instrs.push(Instruction::I64ShrU);
            } else {
                instrs.push(Instruction::LocalGet(bit_off));
                instrs.push(Instruction::I64Shl);
            }
            instrs.push(Instruction::I64Or);
            instrs.push(Instruction::LocalSet(acc));
            instrs.push(Instruction::End);

            let next_src = if matches!(op, BinaryOp::Shr) {
                i as i64 + 1
            } else {
                i as i64 - 1
            };
            instrs.push(Instruction::I64Const(next_src));
            if matches!(op, BinaryOp::Shr) {
                instrs.push(Instruction::LocalGet(word_off));
                instrs.push(Instruction::I64Add);
            } else {
                instrs.push(Instruction::LocalGet(word_off));
                instrs.push(Instruction::I64Sub);
            }
            instrs.push(Instruction::I64Const(src_idx));
            instrs.push(Instruction::I64Eq);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::LocalGet(bit_off));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::I64Ne);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::LocalGet(acc));
            instrs.push(Instruction::LocalGet(l.value_idx + j as u32));
            if matches!(op, BinaryOp::Shr) {
                instrs.push(Instruction::LocalGet(inv_bit));
                instrs.push(Instruction::I64Shl);
            } else {
                instrs.push(Instruction::LocalGet(inv_bit));
                instrs.push(Instruction::I64ShrU);
            }
            instrs.push(Instruction::I64Or);
            instrs.push(Instruction::LocalSet(acc));
            instrs.push(Instruction::End);
            instrs.push(Instruction::End);
        }
        instrs.push(Instruction::LocalGet(acc));
        instrs.push(Instruction::LocalSet(d.value_idx + i as u32));
    }
}

/// Wide arithmetic right shift (sign-extending).
pub(super) fn emit_wide_sar(
    l: &RegLocal,
    r: &RegLocal,
    d: &RegLocal,
    d_chunks: usize,
    l_width: usize,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let word_off = locals.alloc(1);
    let bit_off = locals.alloc(1);
    let inv_bit = locals.alloc(1);
    let acc = locals.alloc(1);
    let cur_word = locals.alloc(1);
    let next_word = locals.alloc(1);
    let sign_fill = locals.alloc(1);

    instrs.push(Instruction::LocalGet(r.value_idx));
    instrs.push(Instruction::I64Const(6));
    instrs.push(Instruction::I64ShrU);
    instrs.push(Instruction::LocalSet(word_off));

    instrs.push(Instruction::LocalGet(r.value_idx));
    instrs.push(Instruction::I64Const(63));
    instrs.push(Instruction::I64And);
    instrs.push(Instruction::LocalSet(bit_off));

    instrs.push(Instruction::I64Const(64));
    instrs.push(Instruction::LocalGet(bit_off));
    instrs.push(Instruction::I64Sub);
    instrs.push(Instruction::LocalSet(inv_bit));

    let msb_chunk = ((l_width - 1) / 64) as u32;
    let msb_bit = ((l_width - 1) % 64) as i64;
    instrs.push(Instruction::I64Const(0));
    instrs.push(Instruction::LocalSet(sign_fill));
    instrs.push(Instruction::LocalGet(l.value_idx + msb_chunk));
    instrs.push(Instruction::I64Const(msb_bit));
    instrs.push(Instruction::I64ShrU);
    instrs.push(Instruction::I64Const(1));
    instrs.push(Instruction::I64And);
    instrs.push(Instruction::I32WrapI64);
    instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
    instrs.push(Instruction::I64Const(-1));
    instrs.push(Instruction::LocalSet(sign_fill));
    instrs.push(Instruction::End);

    // A partial top word must repeat its logical sign into physical padding
    // before it contributes either the main word or the cross-word carry.
    let extended_msb = locals.alloc(1);
    emit_sign_extend(instrs, l.value_idx + msb_chunk, (msb_bit + 1) as usize);
    instrs.push(Instruction::LocalSet(extended_msb));

    for i in 0..d_chunks {
        instrs.push(Instruction::LocalGet(sign_fill));
        instrs.push(Instruction::LocalSet(cur_word));
        instrs.push(Instruction::LocalGet(sign_fill));
        instrs.push(Instruction::LocalSet(next_word));

        for j in 0..l.num_chunks {
            let src_idx = j as i64;
            let source = if j as u32 == msb_chunk {
                extended_msb
            } else if j as u32 > msb_chunk {
                sign_fill
            } else {
                l.value_idx + j as u32
            };

            instrs.push(Instruction::I64Const(i as i64));
            instrs.push(Instruction::LocalGet(word_off));
            instrs.push(Instruction::I64Add);
            instrs.push(Instruction::I64Const(src_idx));
            instrs.push(Instruction::I64Eq);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::LocalGet(source));
            instrs.push(Instruction::LocalSet(cur_word));
            instrs.push(Instruction::End);

            instrs.push(Instruction::I64Const(i as i64 + 1));
            instrs.push(Instruction::LocalGet(word_off));
            instrs.push(Instruction::I64Add);
            instrs.push(Instruction::I64Const(src_idx));
            instrs.push(Instruction::I64Eq);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::LocalGet(source));
            instrs.push(Instruction::LocalSet(next_word));
            instrs.push(Instruction::End);
        }

        instrs.push(Instruction::LocalGet(cur_word));
        instrs.push(Instruction::LocalGet(bit_off));
        instrs.push(Instruction::I64ShrU);
        instrs.push(Instruction::LocalSet(acc));

        instrs.push(Instruction::LocalGet(bit_off));
        instrs.push(Instruction::I64Const(0));
        instrs.push(Instruction::I64Ne);
        instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
        instrs.push(Instruction::LocalGet(acc));
        instrs.push(Instruction::LocalGet(next_word));
        instrs.push(Instruction::LocalGet(inv_bit));
        instrs.push(Instruction::I64Shl);
        instrs.push(Instruction::I64Or);
        instrs.push(Instruction::LocalSet(acc));
        instrs.push(Instruction::End);

        instrs.push(Instruction::LocalGet(acc));
        instrs.push(Instruction::LocalSet(d.value_idx + i as u32));
    }
}

pub(super) fn prepare_wide_signed_magnitude(
    source: &RegLocal,
    width: usize,
    num_chunks: usize,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) -> (RegLocal, Option<u32>) {
    debug_assert!(width > 0);
    let sign = locals.alloc(1);
    let sign_bit = width - 1;
    emit_wide_get_chunk(instrs, source, sign_bit / 64);
    instrs.push(Instruction::I64Const((sign_bit % 64) as i64));
    instrs.push(Instruction::I64ShrU);
    instrs.push(Instruction::I64Const(1));
    instrs.push(Instruction::I64And);
    instrs.push(Instruction::LocalSet(sign));

    let extended = RegLocal {
        value_idx: locals.alloc(num_chunks),
        num_chunks,
        mask_idx: None,
    };
    let source_chunks = width.div_ceil(64);
    let top_bits = width % 64;
    for chunk in 0..num_chunks {
        if chunk >= source_chunks {
            instrs.push(Instruction::LocalGet(sign));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Result(
                ValType::I64,
            )));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::Else);
            instrs.push(Instruction::I64Const(-1));
            instrs.push(Instruction::End);
        } else {
            emit_wide_get_chunk(instrs, source, chunk);
            if chunk + 1 == source_chunks && top_bits != 0 {
                let low_mask = (1u64 << top_bits) - 1;
                instrs.push(Instruction::I64Const(low_mask as i64));
                instrs.push(Instruction::I64And);
                instrs.push(Instruction::LocalGet(sign));
                instrs.push(Instruction::I64Eqz);
                instrs.push(Instruction::If(wasm_encoder::BlockType::Result(
                    ValType::I64,
                )));
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::Else);
                instrs.push(Instruction::I64Const((!low_mask) as i64));
                instrs.push(Instruction::End);
                instrs.push(Instruction::I64Or);
            }
        }
        instrs.push(Instruction::LocalSet(extended.value_idx + chunk as u32));
    }

    (
        conditional_negate_wide_local(&extended, sign, num_chunks, locals, instrs),
        Some(sign),
    )
}

pub(super) fn conditional_negate_wide_local(
    source: &RegLocal,
    negate: u32,
    num_chunks: usize,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) -> RegLocal {
    let negated = RegLocal {
        value_idx: locals.alloc(num_chunks),
        num_chunks,
        mask_idx: None,
    };
    let carry = locals.alloc(1);
    instrs.push(Instruction::I64Const(1));
    instrs.push(Instruction::LocalSet(carry));
    for chunk in 0..num_chunks {
        emit_wide_get_chunk(instrs, source, chunk);
        instrs.push(Instruction::I64Const(-1));
        instrs.push(Instruction::I64Xor);
        instrs.push(Instruction::LocalGet(carry));
        instrs.push(Instruction::I64Add);
        instrs.push(Instruction::LocalTee(negated.value_idx + chunk as u32));
        instrs.push(Instruction::I64Eqz);
        instrs.push(Instruction::LocalGet(carry));
        instrs.push(Instruction::I64Const(0));
        instrs.push(Instruction::I64Ne);
        instrs.push(Instruction::I32And);
        instrs.push(Instruction::I64ExtendI32U);
        instrs.push(Instruction::LocalSet(carry));
    }

    let result = RegLocal {
        value_idx: locals.alloc(num_chunks),
        num_chunks,
        mask_idx: None,
    };
    for chunk in 0..num_chunks {
        instrs.push(Instruction::LocalGet(negate));
        instrs.push(Instruction::I64Eqz);
        instrs.push(Instruction::If(wasm_encoder::BlockType::Result(
            ValType::I64,
        )));
        emit_wide_get_chunk(instrs, source, chunk);
        instrs.push(Instruction::Else);
        emit_wide_get_chunk(instrs, &negated, chunk);
        instrs.push(Instruction::End);
        instrs.push(Instruction::LocalSet(result.value_idx + chunk as u32));
    }
    result
}

pub(super) fn emit_wide_get_chunk(
    instrs: &mut Vec<Instruction<'static>>,
    reg: &RegLocal,
    chunk: usize,
) {
    if chunk < reg.num_chunks {
        instrs.push(Instruction::LocalGet(reg.value_idx + chunk as u32));
    } else {
        instrs.push(Instruction::I64Const(0));
    }
}
