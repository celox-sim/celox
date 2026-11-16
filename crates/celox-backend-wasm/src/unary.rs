//! Unary operations and bit counts.

use super::*;

/// `ToPlanes` and `FromPlanes`: whole 64-bit words move between the value
/// and mask locals of a four-state register and the value locals of a
/// two-state one.
pub(super) fn compile_planes(
    dst: &RegisterId,
    op: &UnaryOp,
    src: &RegisterId,
    locals: &LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let d = locals.reg_map[dst].clone();
    let s = locals.reg_map[src].clone();
    let copy = |instrs: &mut Vec<Instruction<'static>>, from: Option<u32>, to: u32| {
        instrs.push(match from {
            Some(local) => Instruction::LocalGet(local),
            None => Instruction::I64Const(0),
        });
        instrs.push(Instruction::LocalSet(to));
    };
    if *op == UnaryOp::ToPlanes {
        for c in 0..s.num_chunks as u32 {
            copy(instrs, Some(s.value_idx + c), d.value_idx + c);
            copy(
                instrs,
                s.mask_idx.map(|mask| mask + c),
                d.value_idx + s.num_chunks as u32 + c,
            );
        }
        if let Some(mask) = d.mask_idx {
            for c in 0..d.num_chunks as u32 {
                copy(instrs, None, mask + c);
            }
        }
    } else {
        for c in 0..d.num_chunks as u32 {
            copy(instrs, Some(s.value_idx + c), d.value_idx + c);
            if let Some(mask) = d.mask_idx {
                copy(
                    instrs,
                    Some(s.value_idx + d.num_chunks as u32 + c),
                    mask + c,
                );
            }
        }
    }
}

pub(super) fn compile_unary(
    dst: &RegisterId,
    op: &UnaryOp,
    src: &RegisterId,
    unit: &ExecutionUnit<RegionedAbsoluteAddr>,
    four_state: bool,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let d = locals.reg_map[dst].clone();
    let s = locals.reg_map[src].clone();
    let d_width = unit.register_map[dst].width();
    let s_width = unit.register_map[src].width();

    match op {
        UnaryOp::Float { .. } | UnaryOp::ToPlanes | UnaryOp::FromPlanes => {
            unreachable!("float and plane operations are lowered before integer operations")
        }
        UnaryOp::Ident | UnaryOp::ToTwoState => {
            for c in 0..d.num_chunks {
                emit_wide_get_chunk(instrs, &s, c);
                if matches!(op, UnaryOp::ToTwoState)
                    && four_state
                    && let Some(src_mask) = s.mask_idx
                {
                    if c < s.num_chunks {
                        instrs.push(Instruction::LocalGet(src_mask + c as u32));
                    } else {
                        instrs.push(Instruction::I64Const(0));
                    }
                    instrs.push(Instruction::I64Const(-1));
                    instrs.push(Instruction::I64Xor);
                    instrs.push(Instruction::I64And);
                }
                emit_chunk_mask_to_width(instrs, c, d.num_chunks, d_width);
                instrs.push(Instruction::LocalSet(d.value_idx + c as u32));
            }
        }
        UnaryOp::Minus => {
            if d.num_chunks == 1 {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalGet(s.value_idx));
                instrs.push(Instruction::I64Sub);
                emit_mask_to_width(instrs, d_width);
                instrs.push(Instruction::LocalSet(d.value_idx));
            } else {
                // Wide negate: two's complement = NOT + 1
                let carry = locals.alloc(1);
                instrs.push(Instruction::I64Const(1));
                instrs.push(Instruction::LocalSet(carry));
                for c in 0..d.num_chunks {
                    emit_wide_get_chunk(instrs, &s, c);
                    instrs.push(Instruction::I64Const(-1));
                    instrs.push(Instruction::I64Xor);
                    instrs.push(Instruction::LocalGet(carry));
                    instrs.push(Instruction::I64Add);
                    instrs.push(Instruction::LocalSet(d.value_idx + c as u32));

                    // Adding the one-bit carry to the complemented chunk
                    // overflows precisely when the result wraps to zero.
                    instrs.push(Instruction::LocalGet(carry));
                    instrs.push(Instruction::LocalGet(d.value_idx + c as u32));
                    instrs.push(Instruction::I64Eqz);
                    instrs.push(Instruction::I64ExtendI32U);
                    instrs.push(Instruction::I64And);
                    instrs.push(Instruction::LocalSet(carry));
                }
                let top_bits = d_width % 64;
                if top_bits > 0 {
                    let top = d.num_chunks - 1;
                    let mask = (1u64 << top_bits) - 1;
                    instrs.push(Instruction::LocalGet(d.value_idx + top as u32));
                    instrs.push(Instruction::I64Const(mask as i64));
                    instrs.push(Instruction::I64And);
                    instrs.push(Instruction::LocalSet(d.value_idx + top as u32));
                }
            }
        }
        UnaryOp::BitNot => {
            for c in 0..d.num_chunks {
                emit_wide_get_chunk(instrs, &s, c);
                instrs.push(Instruction::I64Const(-1i64));
                instrs.push(Instruction::I64Xor);
                instrs.push(Instruction::LocalSet(d.value_idx + c as u32));
            }
            // Mask top chunk
            let top_bits = d_width % 64;
            if top_bits > 0 && top_bits < 64 {
                let top = d.num_chunks - 1;
                let mask = (1u64 << top_bits) - 1;
                instrs.push(Instruction::LocalGet(d.value_idx + top as u32));
                instrs.push(Instruction::I64Const(mask as i64));
                instrs.push(Instruction::I64And);
                instrs.push(Instruction::LocalSet(d.value_idx + top as u32));
            }
        }
        UnaryOp::LogicNot => {
            // !(|src) — true if src is all zeros
            if s.num_chunks == 1 {
                instrs.push(Instruction::LocalGet(s.value_idx));
                instrs.push(Instruction::I64Eqz);
                instrs.push(Instruction::I64ExtendI32U);
            } else {
                // OR all chunks, then eqz
                instrs.push(Instruction::LocalGet(s.value_idx));
                for c in 1..s.num_chunks {
                    instrs.push(Instruction::LocalGet(s.value_idx + c as u32));
                    instrs.push(Instruction::I64Or);
                }
                instrs.push(Instruction::I64Eqz);
                instrs.push(Instruction::I64ExtendI32U);
            }
            instrs.push(Instruction::LocalSet(d.value_idx));
            for c in 1..d.num_chunks {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(d.value_idx + c as u32));
            }
        }
        UnaryOp::And => {
            // Reduction AND: all bits set within width
            if s.num_chunks == 1 {
                let mask = if s_width >= 64 {
                    u64::MAX
                } else {
                    (1u64 << s_width) - 1
                };
                instrs.push(Instruction::LocalGet(s.value_idx));
                instrs.push(Instruction::I64Const(mask as i64));
                instrs.push(Instruction::I64And);
                instrs.push(Instruction::I64Const(mask as i64));
                instrs.push(Instruction::I64Eq);
                instrs.push(Instruction::I64ExtendI32U);
            } else {
                // All full chunks must be 0xFFFFFFFFFFFFFFFF, top chunk must match mask
                instrs.push(Instruction::I64Const(1)); // accumulator
                for c in 0..s.num_chunks {
                    instrs.push(Instruction::LocalGet(s.value_idx + c as u32));
                    let expected = if c == s.num_chunks - 1 {
                        let top_bits = s_width % 64;
                        if top_bits == 0 {
                            u64::MAX
                        } else {
                            (1u64 << top_bits) - 1
                        }
                    } else {
                        u64::MAX
                    };
                    instrs.push(Instruction::I64Const(expected as i64));
                    instrs.push(Instruction::I64Eq);
                    instrs.push(Instruction::I64ExtendI32U);
                    instrs.push(Instruction::I64And);
                }
            }
            instrs.push(Instruction::LocalSet(d.value_idx));
            for c in 1..d.num_chunks {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(d.value_idx + c as u32));
            }
        }
        UnaryOp::Or => {
            // Reduction OR: any bit set
            if s.num_chunks == 1 {
                instrs.push(Instruction::LocalGet(s.value_idx));
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::I64Ne);
                instrs.push(Instruction::I64ExtendI32U);
            } else {
                instrs.push(Instruction::LocalGet(s.value_idx));
                for c in 1..s.num_chunks {
                    instrs.push(Instruction::LocalGet(s.value_idx + c as u32));
                    instrs.push(Instruction::I64Or);
                }
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::I64Ne);
                instrs.push(Instruction::I64ExtendI32U);
            }
            instrs.push(Instruction::LocalSet(d.value_idx));
            for c in 1..d.num_chunks {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(d.value_idx + c as u32));
            }
        }
        UnaryOp::Xor => {
            // Reduction XOR: parity of all bits
            if s.num_chunks == 1 {
                instrs.push(Instruction::LocalGet(s.value_idx));
                instrs.push(Instruction::I64Popcnt);
                instrs.push(Instruction::I64Const(1));
                instrs.push(Instruction::I64And); // bit 0 of popcount = parity
            } else {
                // XOR all chunks together, then popcount
                instrs.push(Instruction::LocalGet(s.value_idx));
                for c in 1..s.num_chunks {
                    instrs.push(Instruction::LocalGet(s.value_idx + c as u32));
                    instrs.push(Instruction::I64Xor);
                }
                instrs.push(Instruction::I64Popcnt);
                instrs.push(Instruction::I64Const(1));
                instrs.push(Instruction::I64And);
            }
            instrs.push(Instruction::LocalSet(d.value_idx));
            for c in 1..d.num_chunks {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(d.value_idx + c as u32));
            }
        }
        UnaryOp::PopCount | UnaryOp::CountLeadingZeros | UnaryOp::CountTrailingZeros => {
            compile_count_unary_value(&d, op, &s, d_width, s_width, locals, instrs);
        }
    }

    if four_state {
        compile_unary_mask(&d, op, &s, d_width, s_width, locals, instrs);
        if !matches!(op, UnaryOp::Ident | UnaryOp::ToTwoState) {
            normalize_reg_with_mask(&d, d_width, instrs);
        }
    }
}

pub(super) fn compile_unary_mask(
    dst: &RegLocal,
    op: &UnaryOp,
    src: &RegLocal,
    d_width: usize,
    s_width: usize,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let (Some(dst_mask), Some(src_mask)) = (dst.mask_idx, src.mask_idx) else {
        return;
    };

    match op {
        UnaryOp::Float { .. } | UnaryOp::ToPlanes | UnaryOp::FromPlanes => {
            unreachable!("float and plane operations are lowered before integer operations")
        }
        UnaryOp::Ident | UnaryOp::BitNot => {
            for c in 0..dst.num_chunks {
                if c < src.num_chunks {
                    instrs.push(Instruction::LocalGet(src_mask + c as u32));
                } else {
                    instrs.push(Instruction::I64Const(0));
                }
                let width = s_width.min(d_width);
                emit_chunk_mask_to_width(instrs, c, dst.num_chunks, width);
                instrs.push(Instruction::LocalSet(dst_mask + c as u32));
            }
        }
        UnaryOp::ToTwoState => {
            for c in 0..dst.num_chunks {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(dst_mask + c as u32));
            }
        }
        UnaryOp::Minus => {
            let has_x = locals.alloc(1);
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(has_x));
            for c in 0..src.num_chunks {
                instrs.push(Instruction::LocalGet(has_x));
                instrs.push(Instruction::LocalGet(src_mask + c as u32));
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalSet(has_x));
            }
            instrs.push(Instruction::LocalGet(has_x));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            for c in 0..dst.num_chunks {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(dst_mask + c as u32));
            }
            instrs.push(Instruction::Else);
            for c in 0..dst.num_chunks {
                let chunk_mask = chunk_mask_for_width(c, dst.num_chunks, d_width);
                instrs.push(Instruction::I64Const(chunk_mask as i64));
                instrs.push(Instruction::LocalSet(dst_mask + c as u32));
            }
            instrs.push(Instruction::End);
        }
        UnaryOp::PopCount | UnaryOp::CountLeadingZeros | UnaryOp::CountTrailingZeros => {
            let has_x = locals.alloc(1);
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(has_x));
            for c in 0..src.num_chunks {
                instrs.push(Instruction::LocalGet(has_x));
                instrs.push(Instruction::LocalGet(src_mask + c as u32));
                if c + 1 == src.num_chunks && !s_width.is_multiple_of(64) {
                    let valid_bits = s_width % 64;
                    instrs.push(Instruction::I64Const(((1u64 << valid_bits) - 1) as i64));
                    instrs.push(Instruction::I64And);
                }
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalSet(has_x));
            }
            instrs.push(Instruction::LocalGet(has_x));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            for c in 0..dst.num_chunks {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(dst_mask + c as u32));
            }
            instrs.push(Instruction::Else);
            for c in 0..dst.num_chunks {
                let chunk_mask = chunk_mask_for_width(c, dst.num_chunks, d_width);
                instrs.push(Instruction::I64Const(chunk_mask as i64));
                instrs.push(Instruction::LocalSet(dst_mask + c as u32));
            }
            instrs.push(Instruction::End);
        }
        UnaryOp::And | UnaryOp::LogicNot | UnaryOp::Or | UnaryOp::Xor => {
            let has_x = locals.alloc(1);
            let definite = locals.alloc(1);
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(has_x));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(definite));

            for c in 0..src.num_chunks {
                let chunk_width = if c + 1 == src.num_chunks {
                    let top_bits = s_width % 64;
                    if top_bits == 0 { 64 } else { top_bits }
                } else {
                    64
                };
                let chunk_mask = chunk_mask_for_width(c, src.num_chunks, s_width);

                instrs.push(Instruction::LocalGet(has_x));
                instrs.push(Instruction::LocalGet(src_mask + c as u32));
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalSet(has_x));

                match op {
                    UnaryOp::And => {
                        instrs.push(Instruction::LocalGet(definite));
                        instrs.push(Instruction::LocalGet(src.value_idx + c as u32));
                        instrs.push(Instruction::I64Const(-1));
                        instrs.push(Instruction::I64Xor);
                        instrs.push(Instruction::LocalGet(src_mask + c as u32));
                        instrs.push(Instruction::I64Const(-1));
                        instrs.push(Instruction::I64Xor);
                        instrs.push(Instruction::I64And);
                        instrs.push(Instruction::I64Const(chunk_mask as i64));
                        instrs.push(Instruction::I64And);
                        instrs.push(Instruction::I64Or);
                        instrs.push(Instruction::LocalSet(definite));
                    }
                    UnaryOp::LogicNot | UnaryOp::Or => {
                        instrs.push(Instruction::LocalGet(definite));
                        instrs.push(Instruction::LocalGet(src.value_idx + c as u32));
                        instrs.push(Instruction::LocalGet(src_mask + c as u32));
                        instrs.push(Instruction::I64Const(-1));
                        instrs.push(Instruction::I64Xor);
                        instrs.push(Instruction::I64And);
                        instrs.push(Instruction::I64Const(chunk_mask as i64));
                        instrs.push(Instruction::I64And);
                        instrs.push(Instruction::I64Or);
                        instrs.push(Instruction::LocalSet(definite));
                    }
                    UnaryOp::Xor => {
                        let _ = chunk_width;
                    }
                    _ => unreachable!(),
                }
            }

            match op {
                UnaryOp::And | UnaryOp::LogicNot | UnaryOp::Or => {
                    instrs.push(Instruction::LocalGet(definite));
                    instrs.push(Instruction::I64Eqz);
                    instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
                    instrs.push(Instruction::LocalGet(has_x));
                    instrs.push(Instruction::I64Eqz);
                    instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
                    instrs.push(Instruction::I64Const(0));
                    instrs.push(Instruction::LocalSet(dst_mask));
                    instrs.push(Instruction::Else);
                    instrs.push(Instruction::I64Const(1));
                    instrs.push(Instruction::LocalSet(dst_mask));
                    instrs.push(Instruction::End);
                    instrs.push(Instruction::Else);
                    instrs.push(Instruction::I64Const(0));
                    instrs.push(Instruction::LocalSet(dst_mask));
                    instrs.push(Instruction::End);
                }
                UnaryOp::Xor => {
                    instrs.push(Instruction::LocalGet(has_x));
                    instrs.push(Instruction::I64Eqz);
                    instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
                    instrs.push(Instruction::I64Const(0));
                    instrs.push(Instruction::LocalSet(dst_mask));
                    instrs.push(Instruction::Else);
                    instrs.push(Instruction::I64Const(1));
                    instrs.push(Instruction::LocalSet(dst_mask));
                    instrs.push(Instruction::End);
                }
                _ => unreachable!(),
            }

            for c in 1..dst.num_chunks {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(dst_mask + c as u32));
            }
        }
    }
}

pub(super) fn emit_masked_source_chunk(
    src: &RegLocal,
    chunk: usize,
    source_width: usize,
    instrs: &mut Vec<Instruction<'static>>,
) {
    instrs.push(Instruction::LocalGet(src.value_idx + chunk as u32));
    if chunk + 1 == src.num_chunks && !source_width.is_multiple_of(64) {
        let valid_bits = source_width % 64;
        instrs.push(Instruction::I64Const(((1u64 << valid_bits) - 1) as i64));
        instrs.push(Instruction::I64And);
    }
}

pub(super) fn compile_count_unary_value(
    dst: &RegLocal,
    op: &UnaryOp,
    src: &RegLocal,
    destination_width: usize,
    source_width: usize,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    debug_assert!(source_width > 0);
    let count = locals.alloc(1);
    instrs.push(Instruction::I64Const(0));
    instrs.push(Instruction::LocalSet(count));

    match op {
        UnaryOp::PopCount => {
            for chunk in 0..src.num_chunks {
                instrs.push(Instruction::LocalGet(count));
                emit_masked_source_chunk(src, chunk, source_width, instrs);
                instrs.push(Instruction::I64Popcnt);
                instrs.push(Instruction::I64Add);
                instrs.push(Instruction::LocalSet(count));
            }
        }
        UnaryOp::CountLeadingZeros | UnaryOp::CountTrailingZeros => {
            let active = locals.alloc(1);
            let source_chunk = locals.alloc(1);
            instrs.push(Instruction::I64Const(1));
            instrs.push(Instruction::LocalSet(active));

            if matches!(op, UnaryOp::CountLeadingZeros) {
                for chunk in (0..src.num_chunks).rev() {
                    compile_zero_count_chunk(
                        src,
                        chunk,
                        source_width,
                        true,
                        count,
                        active,
                        source_chunk,
                        instrs,
                    );
                }
            } else {
                for chunk in 0..src.num_chunks {
                    compile_zero_count_chunk(
                        src,
                        chunk,
                        source_width,
                        false,
                        count,
                        active,
                        source_chunk,
                        instrs,
                    );
                }
            }
        }
        _ => unreachable!(),
    }

    instrs.push(Instruction::LocalGet(count));
    emit_mask_to_width(instrs, destination_width);
    instrs.push(Instruction::LocalSet(dst.value_idx));
    for chunk in 1..dst.num_chunks {
        instrs.push(Instruction::I64Const(0));
        instrs.push(Instruction::LocalSet(dst.value_idx + chunk as u32));
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn compile_zero_count_chunk(
    src: &RegLocal,
    chunk: usize,
    source_width: usize,
    leading: bool,
    count: u32,
    active: u32,
    source_chunk: u32,
    instrs: &mut Vec<Instruction<'static>>,
) {
    instrs.push(Instruction::LocalGet(active));
    instrs.push(Instruction::I64Const(0));
    instrs.push(Instruction::I64Ne);
    instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));

    emit_masked_source_chunk(src, chunk, source_width, instrs);
    instrs.push(Instruction::LocalSet(source_chunk));
    let valid_bits = if chunk + 1 == src.num_chunks && !source_width.is_multiple_of(64) {
        source_width % 64
    } else {
        64
    };

    if leading {
        instrs.push(Instruction::LocalGet(count));
        instrs.push(Instruction::LocalGet(source_chunk));
        instrs.push(Instruction::I64Clz);
        if valid_bits < 64 {
            instrs.push(Instruction::I64Const((64 - valid_bits) as i64));
            instrs.push(Instruction::I64Sub);
        }
        instrs.push(Instruction::I64Add);
        instrs.push(Instruction::LocalSet(count));

        instrs.push(Instruction::LocalGet(source_chunk));
        instrs.push(Instruction::I64Eqz);
        instrs.push(Instruction::I64ExtendI32U);
        instrs.push(Instruction::LocalSet(active));
    } else {
        instrs.push(Instruction::LocalGet(source_chunk));
        instrs.push(Instruction::I64Eqz);
        instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
        instrs.push(Instruction::LocalGet(count));
        instrs.push(Instruction::I64Const(valid_bits as i64));
        instrs.push(Instruction::I64Add);
        instrs.push(Instruction::LocalSet(count));
        instrs.push(Instruction::Else);
        instrs.push(Instruction::LocalGet(count));
        instrs.push(Instruction::LocalGet(source_chunk));
        instrs.push(Instruction::I64Ctz);
        instrs.push(Instruction::I64Add);
        instrs.push(Instruction::LocalSet(count));
        instrs.push(Instruction::I64Const(0));
        instrs.push(Instruction::LocalSet(active));
        instrs.push(Instruction::End);
    }
    instrs.push(Instruction::End);
}
