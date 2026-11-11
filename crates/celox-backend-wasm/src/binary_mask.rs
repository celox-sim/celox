//! Mask planes of four-state binary operations.

use super::*;

pub(super) fn compile_binary_mask_narrow(
    dst: &RegLocal,
    lhs: &RegLocal,
    op: &BinaryOp,
    rhs: &RegLocal,
    d_width: usize,
    operand_width: usize,
    lhs_width: usize,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let (Some(dst_mask), Some(lhs_mask), Some(rhs_mask)) =
        (dst.mask_idx, lhs.mask_idx, rhs.mask_idx)
    else {
        return;
    };

    match op {
        BinaryOp::And => {
            instrs.push(Instruction::LocalGet(lhs_mask));
            instrs.push(Instruction::LocalGet(rhs_mask));
            instrs.push(Instruction::I64And);
            instrs.push(Instruction::LocalGet(lhs_mask));
            instrs.push(Instruction::LocalGet(rhs.value_idx));
            instrs.push(Instruction::I64And);
            instrs.push(Instruction::I64Or);
            instrs.push(Instruction::LocalGet(rhs_mask));
            instrs.push(Instruction::LocalGet(lhs.value_idx));
            instrs.push(Instruction::I64And);
            instrs.push(Instruction::I64Or);
            emit_mask_to_width(instrs, d_width);
            instrs.push(Instruction::LocalSet(dst_mask));
        }
        BinaryOp::Or => {
            instrs.push(Instruction::LocalGet(lhs_mask));
            instrs.push(Instruction::LocalGet(rhs_mask));
            instrs.push(Instruction::I64And);
            instrs.push(Instruction::LocalGet(lhs_mask));
            instrs.push(Instruction::LocalGet(rhs.value_idx));
            instrs.push(Instruction::I64Const(-1));
            instrs.push(Instruction::I64Xor);
            instrs.push(Instruction::I64And);
            instrs.push(Instruction::I64Or);
            instrs.push(Instruction::LocalGet(rhs_mask));
            instrs.push(Instruction::LocalGet(lhs.value_idx));
            instrs.push(Instruction::I64Const(-1));
            instrs.push(Instruction::I64Xor);
            instrs.push(Instruction::I64And);
            instrs.push(Instruction::I64Or);
            emit_mask_to_width(instrs, d_width);
            instrs.push(Instruction::LocalSet(dst_mask));
        }
        BinaryOp::Xor => {
            instrs.push(Instruction::LocalGet(lhs_mask));
            instrs.push(Instruction::LocalGet(rhs_mask));
            instrs.push(Instruction::I64Or);
            emit_mask_to_width(instrs, d_width);
            instrs.push(Instruction::LocalSet(dst_mask));
        }
        BinaryOp::LogicAnd => {
            let l_vm = locals.alloc(1);
            let r_vm = locals.alloc(1);
            let any_x = locals.alloc(1);

            instrs.push(Instruction::LocalGet(lhs.value_idx));
            instrs.push(Instruction::LocalGet(lhs_mask));
            instrs.push(Instruction::I64Or);
            instrs.push(Instruction::LocalSet(l_vm));
            instrs.push(Instruction::LocalGet(rhs.value_idx));
            instrs.push(Instruction::LocalGet(rhs_mask));
            instrs.push(Instruction::I64Or);
            instrs.push(Instruction::LocalSet(r_vm));
            instrs.push(Instruction::LocalGet(lhs_mask));
            instrs.push(Instruction::LocalGet(rhs_mask));
            instrs.push(Instruction::I64Or);
            instrs.push(Instruction::LocalSet(any_x));

            instrs.push(Instruction::LocalGet(l_vm));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(dst_mask));
            instrs.push(Instruction::Else);
            instrs.push(Instruction::LocalGet(r_vm));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(dst_mask));
            instrs.push(Instruction::Else);
            instrs.push(Instruction::LocalGet(any_x));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(dst_mask));
            instrs.push(Instruction::Else);
            instrs.push(Instruction::I64Const(
                chunk_mask_for_width(0, 1, d_width) as i64
            ));
            instrs.push(Instruction::LocalSet(dst_mask));
            instrs.push(Instruction::End);
            instrs.push(Instruction::End);
            instrs.push(Instruction::End);
        }
        BinaryOp::LogicOr => {
            let l_def_true = locals.alloc(1);
            let r_def_true = locals.alloc(1);
            let any_x = locals.alloc(1);

            instrs.push(Instruction::LocalGet(lhs.value_idx));
            instrs.push(Instruction::LocalGet(lhs_mask));
            instrs.push(Instruction::I64Const(-1));
            instrs.push(Instruction::I64Xor);
            instrs.push(Instruction::I64And);
            instrs.push(Instruction::LocalSet(l_def_true));
            instrs.push(Instruction::LocalGet(rhs.value_idx));
            instrs.push(Instruction::LocalGet(rhs_mask));
            instrs.push(Instruction::I64Const(-1));
            instrs.push(Instruction::I64Xor);
            instrs.push(Instruction::I64And);
            instrs.push(Instruction::LocalSet(r_def_true));
            instrs.push(Instruction::LocalGet(lhs_mask));
            instrs.push(Instruction::LocalGet(rhs_mask));
            instrs.push(Instruction::I64Or);
            instrs.push(Instruction::LocalSet(any_x));

            instrs.push(Instruction::LocalGet(l_def_true));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::LocalGet(r_def_true));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::LocalGet(any_x));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(dst_mask));
            instrs.push(Instruction::Else);
            instrs.push(Instruction::I64Const(
                chunk_mask_for_width(0, 1, d_width) as i64
            ));
            instrs.push(Instruction::LocalSet(dst_mask));
            instrs.push(Instruction::End);
            instrs.push(Instruction::Else);
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(dst_mask));
            instrs.push(Instruction::End);
            instrs.push(Instruction::Else);
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(dst_mask));
            instrs.push(Instruction::End);
        }
        BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar => {
            let rhs_has_x = locals.alloc(1);
            instrs.push(Instruction::LocalGet(rhs_mask));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::I64Ne);
            instrs.push(Instruction::I64ExtendI32U);
            instrs.push(Instruction::LocalSet(rhs_has_x));

            instrs.push(Instruction::LocalGet(rhs_has_x));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            if matches!(op, BinaryOp::Sar) {
                emit_sign_extend(instrs, lhs_mask, lhs_width);
                emit_sar_count(instrs, rhs.value_idx);
            } else {
                instrs.push(Instruction::LocalGet(lhs_mask));
                instrs.push(Instruction::LocalGet(rhs.value_idx));
            }
            match op {
                BinaryOp::Shl => {
                    instrs.push(Instruction::I64Shl);
                    emit_logical_shift_saturation(instrs, rhs.value_idx);
                }
                BinaryOp::Shr => {
                    instrs.push(Instruction::I64ShrU);
                    emit_logical_shift_saturation(instrs, rhs.value_idx);
                }
                BinaryOp::Sar => instrs.push(Instruction::I64ShrS),
                _ => unreachable!(),
            }
            emit_mask_to_width(instrs, d_width);
            instrs.push(Instruction::LocalSet(dst_mask));
            instrs.push(Instruction::Else);
            instrs.push(Instruction::I64Const(
                chunk_mask_for_width(0, 1, d_width) as i64
            ));
            instrs.push(Instruction::LocalSet(dst_mask));
            instrs.push(Instruction::End);
        }
        BinaryOp::Eq | BinaryOp::Ne | BinaryOp::EqWildcard | BinaryOp::NeWildcard => {
            let compare_mask = locals.alloc(1);
            let lhs_unknown = locals.alloc(1);
            let mismatch = locals.alloc(1);

            instrs.push(Instruction::LocalGet(rhs_mask));
            instrs.push(Instruction::I64Const(-1));
            instrs.push(Instruction::I64Xor);
            emit_mask_to_width(instrs, operand_width);
            instrs.push(Instruction::LocalSet(compare_mask));

            instrs.push(Instruction::LocalGet(lhs_mask));
            if matches!(op, BinaryOp::Eq | BinaryOp::Ne) {
                instrs.push(Instruction::LocalGet(rhs_mask));
                instrs.push(Instruction::I64Or);
            } else {
                instrs.push(Instruction::LocalGet(compare_mask));
                instrs.push(Instruction::I64And);
            }
            instrs.push(Instruction::LocalSet(lhs_unknown));

            instrs.push(Instruction::LocalGet(lhs.value_idx));
            instrs.push(Instruction::LocalGet(rhs.value_idx));
            instrs.push(Instruction::I64Xor);
            instrs.push(Instruction::LocalGet(compare_mask));
            instrs.push(Instruction::I64And);
            instrs.push(Instruction::LocalGet(lhs_mask));
            instrs.push(Instruction::I64Const(-1));
            instrs.push(Instruction::I64Xor);
            instrs.push(Instruction::I64And);
            instrs.push(Instruction::LocalSet(mismatch));

            instrs.push(Instruction::LocalGet(mismatch));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::LocalGet(lhs_unknown));
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
        _ => {
            let any_x = locals.alloc(1);
            instrs.push(Instruction::LocalGet(lhs_mask));
            instrs.push(Instruction::LocalGet(rhs_mask));
            instrs.push(Instruction::I64Or);
            if matches!(
                op,
                BinaryOp::DivU | BinaryOp::DivS | BinaryOp::RemU | BinaryOp::RemS
            ) {
                // IEEE 1800-2023 11.4.3: a zero divisor also produces all X.
                instrs.push(Instruction::LocalGet(rhs.value_idx));
                instrs.push(Instruction::I64Eqz);
                instrs.push(Instruction::I64ExtendI32U);
                instrs.push(Instruction::I64Or);
            }
            instrs.push(Instruction::LocalSet(any_x));
            instrs.push(Instruction::LocalGet(any_x));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(dst_mask));
            instrs.push(Instruction::Else);
            instrs.push(Instruction::I64Const(
                chunk_mask_for_width(0, 1, d_width) as i64
            ));
            instrs.push(Instruction::LocalSet(dst_mask));
            instrs.push(Instruction::End);
        }
    }
}

pub(super) fn compile_binary_mask_wide(
    dst: &RegLocal,
    lhs: &RegLocal,
    op: &BinaryOp,
    rhs: &RegLocal,
    d_width: usize,
    operand_width: usize,
    lhs_width: usize,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let (Some(dst_mask_idx), Some(lhs_mask_idx), Some(rhs_mask_idx)) =
        (dst.mask_idx, lhs.mask_idx, rhs.mask_idx)
    else {
        return;
    };

    let dst_mask = RegLocal {
        value_idx: dst_mask_idx,
        num_chunks: dst.num_chunks,
        mask_idx: None,
    };
    let lhs_mask = RegLocal {
        value_idx: lhs_mask_idx,
        num_chunks: lhs.num_chunks,
        mask_idx: None,
    };
    let rhs_mask = RegLocal {
        value_idx: rhs_mask_idx,
        num_chunks: rhs.num_chunks,
        mask_idx: None,
    };

    match op {
        BinaryOp::And | BinaryOp::Or | BinaryOp::Xor => {
            for c in 0..dst.num_chunks {
                match op {
                    BinaryOp::And => {
                        emit_wide_get_chunk(instrs, &lhs_mask, c);
                        emit_wide_get_chunk(instrs, &rhs_mask, c);
                        instrs.push(Instruction::I64And);
                        emit_wide_get_chunk(instrs, &lhs_mask, c);
                        emit_wide_get_chunk(instrs, rhs, c);
                        instrs.push(Instruction::I64And);
                        instrs.push(Instruction::I64Or);
                        emit_wide_get_chunk(instrs, &rhs_mask, c);
                        emit_wide_get_chunk(instrs, lhs, c);
                        instrs.push(Instruction::I64And);
                        instrs.push(Instruction::I64Or);
                    }
                    BinaryOp::Or => {
                        emit_wide_get_chunk(instrs, &lhs_mask, c);
                        emit_wide_get_chunk(instrs, &rhs_mask, c);
                        instrs.push(Instruction::I64And);
                        emit_wide_get_chunk(instrs, &lhs_mask, c);
                        emit_wide_get_chunk(instrs, rhs, c);
                        instrs.push(Instruction::I64Const(-1));
                        instrs.push(Instruction::I64Xor);
                        instrs.push(Instruction::I64And);
                        instrs.push(Instruction::I64Or);
                        emit_wide_get_chunk(instrs, &rhs_mask, c);
                        emit_wide_get_chunk(instrs, lhs, c);
                        instrs.push(Instruction::I64Const(-1));
                        instrs.push(Instruction::I64Xor);
                        instrs.push(Instruction::I64And);
                        instrs.push(Instruction::I64Or);
                    }
                    BinaryOp::Xor => {
                        emit_wide_get_chunk(instrs, &lhs_mask, c);
                        emit_wide_get_chunk(instrs, &rhs_mask, c);
                        instrs.push(Instruction::I64Or);
                    }
                    _ => unreachable!(),
                }
                emit_chunk_mask_to_width(instrs, c, dst.num_chunks, d_width);
                instrs.push(Instruction::LocalSet(dst_mask.value_idx + c as u32));
            }
        }
        BinaryOp::LogicAnd => {
            let l_vm = locals.alloc(1);
            let r_vm = locals.alloc(1);
            let any_x = locals.alloc(1);

            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(l_vm));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(r_vm));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(any_x));

            for c in 0..lhs.num_chunks.max(rhs.num_chunks) {
                emit_wide_get_chunk(instrs, lhs, c);
                emit_wide_get_chunk(instrs, &lhs_mask, c);
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalGet(l_vm));
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalSet(l_vm));

                emit_wide_get_chunk(instrs, rhs, c);
                emit_wide_get_chunk(instrs, &rhs_mask, c);
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalGet(r_vm));
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalSet(r_vm));

                emit_wide_get_chunk(instrs, &lhs_mask, c);
                emit_wide_get_chunk(instrs, &rhs_mask, c);
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalGet(any_x));
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalSet(any_x));
            }

            instrs.push(Instruction::LocalGet(l_vm));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(dst_mask.value_idx));
            instrs.push(Instruction::Else);
            instrs.push(Instruction::LocalGet(r_vm));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(dst_mask.value_idx));
            instrs.push(Instruction::Else);
            instrs.push(Instruction::LocalGet(any_x));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(dst_mask.value_idx));
            instrs.push(Instruction::Else);
            instrs.push(Instruction::I64Const(
                chunk_mask_for_width(0, 1, d_width) as i64
            ));
            instrs.push(Instruction::LocalSet(dst_mask.value_idx));
            instrs.push(Instruction::End);
            instrs.push(Instruction::End);
            instrs.push(Instruction::End);
            for c in 1..dst.num_chunks {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(dst_mask.value_idx + c as u32));
            }
        }
        BinaryOp::LogicOr => {
            let l_def_true = locals.alloc(1);
            let r_def_true = locals.alloc(1);
            let any_x = locals.alloc(1);

            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(l_def_true));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(r_def_true));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(any_x));

            for c in 0..lhs.num_chunks.max(rhs.num_chunks) {
                emit_wide_get_chunk(instrs, lhs, c);
                emit_wide_get_chunk(instrs, &lhs_mask, c);
                instrs.push(Instruction::I64Const(-1));
                instrs.push(Instruction::I64Xor);
                instrs.push(Instruction::I64And);
                instrs.push(Instruction::LocalGet(l_def_true));
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalSet(l_def_true));

                emit_wide_get_chunk(instrs, rhs, c);
                emit_wide_get_chunk(instrs, &rhs_mask, c);
                instrs.push(Instruction::I64Const(-1));
                instrs.push(Instruction::I64Xor);
                instrs.push(Instruction::I64And);
                instrs.push(Instruction::LocalGet(r_def_true));
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalSet(r_def_true));

                emit_wide_get_chunk(instrs, &lhs_mask, c);
                emit_wide_get_chunk(instrs, &rhs_mask, c);
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalGet(any_x));
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalSet(any_x));
            }

            instrs.push(Instruction::LocalGet(l_def_true));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::LocalGet(r_def_true));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::LocalGet(any_x));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(dst_mask.value_idx));
            instrs.push(Instruction::Else);
            instrs.push(Instruction::I64Const(
                chunk_mask_for_width(0, 1, d_width) as i64
            ));
            instrs.push(Instruction::LocalSet(dst_mask.value_idx));
            instrs.push(Instruction::End);
            instrs.push(Instruction::Else);
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(dst_mask.value_idx));
            instrs.push(Instruction::End);
            instrs.push(Instruction::Else);
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(dst_mask.value_idx));
            instrs.push(Instruction::End);
            for c in 1..dst.num_chunks {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(dst_mask.value_idx + c as u32));
            }
        }
        BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar => {
            let rhs_has_x = locals.alloc(1);
            let shifted_mask = RegLocal {
                value_idx: locals.alloc(dst.num_chunks),
                num_chunks: dst.num_chunks,
                mask_idx: None,
            };

            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(rhs_has_x));
            for c in 0..rhs_mask.num_chunks {
                emit_wide_get_chunk(instrs, &rhs_mask, c);
                instrs.push(Instruction::LocalGet(rhs_has_x));
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalSet(rhs_has_x));
            }

            match op {
                BinaryOp::Shl | BinaryOp::Shr => {
                    emit_wide_shift(
                        op,
                        &lhs_mask,
                        rhs,
                        &shifted_mask,
                        dst.num_chunks,
                        locals,
                        instrs,
                    );
                }
                BinaryOp::Sar => {
                    emit_wide_sar(
                        &lhs_mask,
                        rhs,
                        &shifted_mask,
                        dst.num_chunks,
                        lhs_width,
                        locals,
                        instrs,
                    );
                }
                _ => unreachable!(),
            }

            instrs.push(Instruction::LocalGet(rhs_has_x));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            for c in 0..dst.num_chunks {
                instrs.push(Instruction::LocalGet(shifted_mask.value_idx + c as u32));
                emit_chunk_mask_to_width(instrs, c, dst.num_chunks, d_width);
                instrs.push(Instruction::LocalSet(dst_mask.value_idx + c as u32));
            }
            instrs.push(Instruction::Else);
            for c in 0..dst.num_chunks {
                let chunk_mask = chunk_mask_for_width(c, dst.num_chunks, d_width);
                instrs.push(Instruction::I64Const(chunk_mask as i64));
                instrs.push(Instruction::LocalSet(dst_mask.value_idx + c as u32));
            }
            instrs.push(Instruction::End);
        }
        BinaryOp::Eq | BinaryOp::Ne | BinaryOp::EqWildcard | BinaryOp::NeWildcard => {
            let any_unknown = locals.alloc(1);
            let any_mismatch = locals.alloc(1);
            let operand_chunks = lhs.num_chunks.max(rhs.num_chunks);

            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(any_unknown));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(any_mismatch));

            for c in 0..operand_chunks {
                let compare_mask = locals.alloc(1);
                emit_wide_get_chunk(instrs, &rhs_mask, c);
                instrs.push(Instruction::I64Const(-1));
                instrs.push(Instruction::I64Xor);
                emit_chunk_mask_to_width(instrs, c, operand_chunks, operand_width);
                instrs.push(Instruction::LocalSet(compare_mask));

                emit_wide_get_chunk(instrs, &lhs_mask, c);
                if matches!(op, BinaryOp::Eq | BinaryOp::Ne) {
                    emit_wide_get_chunk(instrs, &rhs_mask, c);
                    instrs.push(Instruction::I64Or);
                } else {
                    instrs.push(Instruction::LocalGet(compare_mask));
                    instrs.push(Instruction::I64And);
                }
                instrs.push(Instruction::LocalGet(any_unknown));
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalSet(any_unknown));

                emit_wide_get_chunk(instrs, lhs, c);
                emit_wide_get_chunk(instrs, rhs, c);
                instrs.push(Instruction::I64Xor);
                instrs.push(Instruction::LocalGet(compare_mask));
                instrs.push(Instruction::I64And);
                emit_wide_get_chunk(instrs, &lhs_mask, c);
                instrs.push(Instruction::I64Const(-1));
                instrs.push(Instruction::I64Xor);
                instrs.push(Instruction::I64And);
                instrs.push(Instruction::LocalGet(any_mismatch));
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalSet(any_mismatch));
            }

            instrs.push(Instruction::LocalGet(any_mismatch));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::LocalGet(any_unknown));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(dst_mask.value_idx));
            instrs.push(Instruction::Else);
            instrs.push(Instruction::I64Const(1));
            instrs.push(Instruction::LocalSet(dst_mask.value_idx));
            instrs.push(Instruction::End);
            instrs.push(Instruction::Else);
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(dst_mask.value_idx));
            instrs.push(Instruction::End);
            for c in 1..dst.num_chunks {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(dst_mask.value_idx + c as u32));
            }
        }
        _ => {
            let any_x = locals.alloc(1);
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(any_x));
            for c in 0..lhs_mask.num_chunks.max(rhs_mask.num_chunks) {
                emit_wide_get_chunk(instrs, &lhs_mask, c);
                emit_wide_get_chunk(instrs, &rhs_mask, c);
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalGet(any_x));
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalSet(any_x));
            }
            if matches!(
                op,
                BinaryOp::DivU | BinaryOp::DivS | BinaryOp::RemU | BinaryOp::RemS
            ) {
                instrs.push(Instruction::I64Const(0));
                for c in 0..rhs.num_chunks {
                    emit_wide_get_chunk(instrs, rhs, c);
                    instrs.push(Instruction::I64Or);
                }
                instrs.push(Instruction::I64Eqz);
                instrs.push(Instruction::I64ExtendI32U);
                instrs.push(Instruction::LocalGet(any_x));
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalSet(any_x));
            }
            for c in 0..dst.num_chunks {
                let chunk_mask = chunk_mask_for_width(c, dst.num_chunks, d_width);
                instrs.push(Instruction::LocalGet(any_x));
                instrs.push(Instruction::I64Eqz);
                instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(dst_mask.value_idx + c as u32));
                instrs.push(Instruction::Else);
                instrs.push(Instruction::I64Const(chunk_mask as i64));
                instrs.push(Instruction::LocalSet(dst_mask.value_idx + c as u32));
                instrs.push(Instruction::End);
            }
        }
    }
}

// Shifts transport X/Z bits unchanged; only an unknown count makes all bits X.
pub(super) fn finish_binary_four_state(
    dst: &RegLocal,
    op: &BinaryOp,
    rhs: &RegLocal,
    width: usize,
    instrs: &mut Vec<Instruction<'static>>,
) {
    if !matches!(op, BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar) {
        normalize_reg_with_mask(dst, width, instrs);
        return;
    }
    if let Some(mask) = rhs.mask_idx {
        instrs.push(Instruction::I64Const(0));
        for c in 0..rhs.num_chunks {
            instrs.push(Instruction::LocalGet(mask + c as u32));
            instrs.push(Instruction::I64Or);
        }
        instrs.push(Instruction::I64Const(0));
        instrs.push(Instruction::I64Ne);
        instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
        for c in 0..dst.num_chunks {
            instrs.push(Instruction::I64Const(
                chunk_mask_for_width(c, dst.num_chunks, width) as i64,
            ));
            instrs.push(Instruction::LocalSet(dst.value_idx + c as u32));
        }
        instrs.push(Instruction::End);
    }
}

pub(super) fn normalize_reg_with_mask(
    reg: &RegLocal,
    width: usize,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let Some(mask_idx) = reg.mask_idx else {
        return;
    };

    for c in 0..reg.num_chunks {
        instrs.push(Instruction::LocalGet(reg.value_idx + c as u32));
        instrs.push(Instruction::LocalGet(mask_idx + c as u32));
        instrs.push(Instruction::I64Or);
        emit_chunk_mask_to_width(instrs, c, reg.num_chunks, width);
        instrs.push(Instruction::LocalSet(reg.value_idx + c as u32));
    }
}
