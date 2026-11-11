//! Binary operations on value planes.

use super::*;

pub(super) fn compile_binary(
    dst: &RegisterId,
    lhs: &RegisterId,
    op: &BinaryOp,
    rhs: &RegisterId,
    unit: &ExecutionUnit<RegionedAbsoluteAddr>,
    four_state: bool,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let d_num = locals.reg_map[dst].num_chunks;
    let l_num = locals.reg_map[lhs].num_chunks;
    let r_num = locals.reg_map[rhs].num_chunks;
    let d_width = unit.register_map[dst].width();
    let l_width = unit.register_map[lhs].width();
    let r_width = unit.register_map[rhs].width();

    if d_num == 1 && l_num == 1 && r_num == 1 {
        compile_binary_narrow(
            dst, lhs, op, rhs, d_width, l_width, r_width, four_state, locals, instrs,
        );
    } else {
        compile_binary_wide(dst, lhs, op, rhs, d_width, unit, four_state, locals, instrs);
    }
}

pub(super) fn compile_binary_narrow(
    dst: &RegisterId,
    lhs: &RegisterId,
    op: &BinaryOp,
    rhs: &RegisterId,
    d_width: usize,
    l_width: usize,
    r_width: usize,
    four_state: bool,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let d = locals.reg_map[dst].clone();
    let l = locals.reg_map[lhs].clone();
    let r = locals.reg_map[rhs].clone();

    if matches!(op, BinaryOp::EqCase | BinaryOp::NeCase) {
        instrs.push(Instruction::LocalGet(l.value_idx));
        instrs.push(Instruction::LocalGet(r.value_idx));
        instrs.push(Instruction::I64Eq);
        if four_state {
            if let Some(mask_idx) = l.mask_idx {
                instrs.push(Instruction::LocalGet(mask_idx));
            } else {
                instrs.push(Instruction::I64Const(0));
            }
            if let Some(mask_idx) = r.mask_idx {
                instrs.push(Instruction::LocalGet(mask_idx));
            } else {
                instrs.push(Instruction::I64Const(0));
            }
            instrs.push(Instruction::I64Eq);
            instrs.push(Instruction::I32And);
        }
        instrs.push(Instruction::I64ExtendI32U);
        if matches!(op, BinaryOp::NeCase) {
            instrs.push(Instruction::I64Const(1));
            instrs.push(Instruction::I64Xor);
        }
        emit_mask_to_width(instrs, d_width);
        instrs.push(Instruction::LocalSet(d.value_idx));
        if let Some(mask_idx) = d.mask_idx {
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(mask_idx));
        }
        return;
    }

    if four_state && matches!(op, BinaryOp::EqWildcard | BinaryOp::NeWildcard) {
        let compare_mask = locals.alloc(1);
        instrs.push(Instruction::LocalGet(
            r.mask_idx.expect("four-state rhs mask"),
        ));
        instrs.push(Instruction::I64Const(-1));
        instrs.push(Instruction::I64Xor);
        emit_mask_to_width(instrs, l_width.max(r_width));
        instrs.push(Instruction::LocalSet(compare_mask));

        instrs.push(Instruction::LocalGet(l.value_idx));
        instrs.push(Instruction::LocalGet(compare_mask));
        instrs.push(Instruction::I64And);
        instrs.push(Instruction::LocalGet(r.value_idx));
        instrs.push(Instruction::LocalGet(compare_mask));
        instrs.push(Instruction::I64And);
        if matches!(op, BinaryOp::EqWildcard) {
            instrs.push(Instruction::I64Eq);
        } else {
            instrs.push(Instruction::I64Ne);
        }
        instrs.push(Instruction::I64ExtendI32U);
        emit_mask_to_width(instrs, d_width);
        instrs.push(Instruction::LocalSet(d.value_idx));

        compile_binary_mask_narrow(
            &d,
            &l,
            op,
            &r,
            d_width,
            l_width.max(r_width),
            l_width,
            locals,
            instrs,
        );
        normalize_reg_with_mask(&d, d_width, instrs);
        return;
    }

    // Load operands
    instrs.push(Instruction::LocalGet(l.value_idx));
    instrs.push(Instruction::LocalGet(r.value_idx));

    match op {
        BinaryOp::Float { .. } => {
            unreachable!("float operations are lowered before integer operations")
        }
        BinaryOp::Add => instrs.push(Instruction::I64Add),
        BinaryOp::Sub => instrs.push(Instruction::I64Sub),
        BinaryOp::Mul => instrs.push(Instruction::I64Mul),
        BinaryOp::DivU | BinaryOp::DivS | BinaryOp::RemU | BinaryOp::RemS => {
            let signed = matches!(op, BinaryOp::DivS | BinaryOp::RemS);
            let quotient = matches!(op, BinaryOp::DivU | BinaryOp::DivS);
            let tmp_l = locals.alloc(1);
            let tmp_r = locals.alloc(1);
            instrs.push(Instruction::LocalSet(tmp_r));
            instrs.push(Instruction::LocalSet(tmp_l));
            if signed {
                emit_sign_extend(instrs, tmp_l, l_width.max(1));
                instrs.push(Instruction::LocalSet(tmp_l));
                emit_sign_extend(instrs, tmp_r, r_width.max(1));
                instrs.push(Instruction::LocalSet(tmp_r));
            }
            instrs.push(Instruction::LocalGet(tmp_r));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Result(
                ValType::I64,
            )));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::Else);
            if signed {
                // WebAssembly signed division traps on MIN / -1. SIR arithmetic
                // wraps that quotient to MIN and defines the remainder as zero.
                instrs.push(Instruction::LocalGet(tmp_l));
                instrs.push(Instruction::I64Const(i64::MIN));
                instrs.push(Instruction::I64Eq);
                instrs.push(Instruction::LocalGet(tmp_r));
                instrs.push(Instruction::I64Const(-1));
                instrs.push(Instruction::I64Eq);
                instrs.push(Instruction::I32And);
                instrs.push(Instruction::If(wasm_encoder::BlockType::Result(
                    ValType::I64,
                )));
                if quotient {
                    instrs.push(Instruction::LocalGet(tmp_l));
                } else {
                    instrs.push(Instruction::I64Const(0));
                }
                instrs.push(Instruction::Else);
                instrs.push(Instruction::LocalGet(tmp_l));
                instrs.push(Instruction::LocalGet(tmp_r));
                instrs.push(if quotient {
                    Instruction::I64DivS
                } else {
                    Instruction::I64RemS
                });
                instrs.push(Instruction::End);
            } else {
                instrs.push(Instruction::LocalGet(tmp_l));
                instrs.push(Instruction::LocalGet(tmp_r));
                instrs.push(if quotient {
                    Instruction::I64DivU
                } else {
                    Instruction::I64RemU
                });
            }
            instrs.push(Instruction::End);
        }
        BinaryOp::And => instrs.push(Instruction::I64And),
        BinaryOp::Or => instrs.push(Instruction::I64Or),
        BinaryOp::Xor => instrs.push(Instruction::I64Xor),
        BinaryOp::Shl => {
            instrs.push(Instruction::I64Shl);
            emit_logical_shift_saturation(instrs, r.value_idx);
        }
        BinaryOp::Shr => {
            instrs.push(Instruction::I64ShrU);
            emit_logical_shift_saturation(instrs, r.value_idx);
        }
        BinaryOp::Sar => {
            // Arithmetic shift right — need sign extension first.
            // Pop both operands, sign-extend lhs, then shift.
            // We already pushed lhs, rhs. We need to reorganize.
            // Let's redo: remove the last two pushes and do it properly.
            let len = instrs.len();
            instrs.truncate(len - 2);
            // Sign-extend lhs to 64-bit based on its logical width
            emit_sign_extend(instrs, l.value_idx, l_width);
            emit_sar_count(instrs, r.value_idx);
            instrs.push(Instruction::I64ShrS);
        }
        BinaryOp::Eq => {
            instrs.push(Instruction::I64Eq);
            instrs.push(Instruction::I64ExtendI32U);
        }
        BinaryOp::Ne => {
            instrs.push(Instruction::I64Ne);
            instrs.push(Instruction::I64ExtendI32U);
        }
        BinaryOp::LtU => {
            instrs.push(Instruction::I64LtU);
            instrs.push(Instruction::I64ExtendI32U);
        }
        BinaryOp::LtS => {
            let len = instrs.len();
            instrs.truncate(len - 2);
            emit_sign_extend(instrs, l.value_idx, l_width.max(1));
            emit_sign_extend(instrs, r.value_idx, r_width.max(1));
            instrs.push(Instruction::I64LtS);
            instrs.push(Instruction::I64ExtendI32U);
        }
        BinaryOp::LeU => {
            instrs.push(Instruction::I64LeU);
            instrs.push(Instruction::I64ExtendI32U);
        }
        BinaryOp::LeS => {
            let len = instrs.len();
            instrs.truncate(len - 2);
            emit_sign_extend(instrs, l.value_idx, l_width.max(1));
            emit_sign_extend(instrs, r.value_idx, r_width.max(1));
            instrs.push(Instruction::I64LeS);
            instrs.push(Instruction::I64ExtendI32U);
        }
        BinaryOp::GtU => {
            instrs.push(Instruction::I64GtU);
            instrs.push(Instruction::I64ExtendI32U);
        }
        BinaryOp::GtS => {
            let len = instrs.len();
            instrs.truncate(len - 2);
            emit_sign_extend(instrs, l.value_idx, l_width.max(1));
            emit_sign_extend(instrs, r.value_idx, r_width.max(1));
            instrs.push(Instruction::I64GtS);
            instrs.push(Instruction::I64ExtendI32U);
        }
        BinaryOp::GeU => {
            instrs.push(Instruction::I64GeU);
            instrs.push(Instruction::I64ExtendI32U);
        }
        BinaryOp::GeS => {
            let len = instrs.len();
            instrs.truncate(len - 2);
            emit_sign_extend(instrs, l.value_idx, l_width.max(1));
            emit_sign_extend(instrs, r.value_idx, r_width.max(1));
            instrs.push(Instruction::I64GeS);
            instrs.push(Instruction::I64ExtendI32U);
        }
        BinaryOp::LogicAnd => {
            // (a != 0) & (b != 0)
            let len = instrs.len();
            instrs.truncate(len - 2);
            instrs.push(Instruction::LocalGet(l.value_idx));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::I64Ne);
            instrs.push(Instruction::LocalGet(r.value_idx));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::I64Ne);
            instrs.push(Instruction::I32And);
            instrs.push(Instruction::I64ExtendI32U);
        }
        BinaryOp::LogicOr => {
            let len = instrs.len();
            instrs.truncate(len - 2);
            instrs.push(Instruction::LocalGet(l.value_idx));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::I64Ne);
            instrs.push(Instruction::LocalGet(r.value_idx));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::I64Ne);
            instrs.push(Instruction::I32Or);
            instrs.push(Instruction::I64ExtendI32U);
        }
        BinaryOp::EqWildcard | BinaryOp::NeWildcard => {
            // For 2-state (no mask), wildcard eq/ne is the same as regular eq/ne.
            if matches!(op, BinaryOp::EqWildcard) {
                instrs.push(Instruction::I64Eq);
            } else {
                instrs.push(Instruction::I64Ne);
            }
            instrs.push(Instruction::I64ExtendI32U);
        }
        BinaryOp::EqCase | BinaryOp::NeCase => unreachable!("handled above"),
    }

    // Mask to destination width
    emit_mask_to_width(instrs, d_width);
    instrs.push(Instruction::LocalSet(d.value_idx));

    if four_state {
        compile_binary_mask_narrow(
            &d,
            &l,
            op,
            &r,
            d_width,
            l_width.max(r_width),
            l_width,
            locals,
            instrs,
        );
        finish_binary_four_state(&d, op, &r, d_width, instrs);
    }
}

pub(super) fn compile_binary_wide(
    dst: &RegisterId,
    lhs: &RegisterId,
    op: &BinaryOp,
    rhs: &RegisterId,
    d_width: usize,
    unit: &ExecutionUnit<RegionedAbsoluteAddr>,
    four_state: bool,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let d = locals.reg_map[dst].clone();
    let l = locals.reg_map[lhs].clone();
    let r = locals.reg_map[rhs].clone();
    let d_chunks = d.num_chunks;
    let l_width = unit.register_map[lhs].width();
    let r_width = unit.register_map[rhs].width();

    if matches!(op, BinaryOp::EqCase | BinaryOp::NeCase) {
        let operand_chunks = l.num_chunks.max(r.num_chunks);
        instrs.push(Instruction::I64Const(1));
        for c in 0..operand_chunks {
            emit_wide_get_chunk(instrs, &l, c);
            emit_wide_get_chunk(instrs, &r, c);
            instrs.push(Instruction::I64Eq);
            instrs.push(Instruction::I64ExtendI32U);
            instrs.push(Instruction::I64And);
            if four_state {
                if let Some(mask_idx) = l.mask_idx.filter(|_| c < l.num_chunks) {
                    instrs.push(Instruction::LocalGet(mask_idx + c as u32));
                } else {
                    instrs.push(Instruction::I64Const(0));
                }
                if let Some(mask_idx) = r.mask_idx.filter(|_| c < r.num_chunks) {
                    instrs.push(Instruction::LocalGet(mask_idx + c as u32));
                } else {
                    instrs.push(Instruction::I64Const(0));
                }
                instrs.push(Instruction::I64Eq);
                instrs.push(Instruction::I64ExtendI32U);
                instrs.push(Instruction::I64And);
            }
        }
        if matches!(op, BinaryOp::NeCase) {
            instrs.push(Instruction::I64Const(1));
            instrs.push(Instruction::I64Xor);
        }
        instrs.push(Instruction::LocalSet(d.value_idx));
        for c in 1..d_chunks {
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(d.value_idx + c as u32));
        }
        if let Some(mask_idx) = d.mask_idx {
            for c in 0..d_chunks {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(mask_idx + c as u32));
            }
        }
        return;
    }

    if four_state && matches!(op, BinaryOp::EqWildcard | BinaryOp::NeWildcard) {
        let operand_width = l_width.max(r_width);
        let operand_chunks = l.num_chunks.max(r.num_chunks);
        let compare_mask = RegLocal {
            value_idx: locals.alloc(operand_chunks),
            num_chunks: operand_chunks,
            mask_idx: None,
        };
        for c in 0..operand_chunks {
            if c < r.num_chunks {
                instrs.push(Instruction::LocalGet(
                    r.mask_idx.expect("four-state rhs mask") + c as u32,
                ));
                instrs.push(Instruction::I64Const(-1));
                instrs.push(Instruction::I64Xor);
                emit_chunk_mask_to_width(instrs, c, operand_chunks, operand_width);
                instrs.push(Instruction::LocalSet(compare_mask.value_idx + c as u32));
            } else {
                let chunk_mask = chunk_mask_for_width(c, operand_chunks, operand_width);
                instrs.push(Instruction::I64Const(chunk_mask as i64));
                instrs.push(Instruction::LocalSet(compare_mask.value_idx + c as u32));
            }
        }

        instrs.push(Instruction::I64Const(1));
        for c in 0..operand_chunks {
            emit_wide_get_chunk(instrs, &l, c);
            emit_wide_get_chunk(instrs, &compare_mask, c);
            instrs.push(Instruction::I64And);
            emit_wide_get_chunk(instrs, &r, c);
            emit_wide_get_chunk(instrs, &compare_mask, c);
            instrs.push(Instruction::I64And);
            instrs.push(Instruction::I64Eq);
            instrs.push(Instruction::I64ExtendI32U);
            instrs.push(Instruction::I64And);
        }
        if matches!(op, BinaryOp::NeWildcard) {
            instrs.push(Instruction::I64Const(1));
            instrs.push(Instruction::I64Xor);
        }
        instrs.push(Instruction::LocalSet(d.value_idx));
        for c in 1..d_chunks {
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(d.value_idx + c as u32));
        }

        compile_binary_mask_wide(
            &d,
            &l,
            op,
            &r,
            d_width,
            operand_width,
            l_width,
            locals,
            instrs,
        );
        normalize_reg_with_mask(&d, d_width, instrs);
        return;
    }

    match op {
        BinaryOp::Float { .. } => {
            unreachable!("float operations are lowered before integer operations")
        }
        // 1. Chunk-wise bitwise ops
        BinaryOp::And | BinaryOp::Or | BinaryOp::Xor => {
            for c in 0..d_chunks {
                emit_wide_get_chunk(instrs, &l, c);
                emit_wide_get_chunk(instrs, &r, c);
                match op {
                    BinaryOp::And => instrs.push(Instruction::I64And),
                    BinaryOp::Or => instrs.push(Instruction::I64Or),
                    BinaryOp::Xor => instrs.push(Instruction::I64Xor),
                    _ => unreachable!(),
                }
                instrs.push(Instruction::LocalSet(d.value_idx + c as u32));
            }
        }
        // 2. Addition with carry propagation
        BinaryOp::Add => {
            let carry = locals.alloc(1); // carry flag (0 or 1)
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(carry));
            for c in 0..d_chunks {
                // s1 = l[c] + r[c]
                emit_wide_get_chunk(instrs, &l, c);
                emit_wide_get_chunk(instrs, &r, c);
                instrs.push(Instruction::I64Add);
                // c1 = s1 < l[c] (unsigned overflow)
                instrs.push(Instruction::LocalTee(d.value_idx + c as u32));
                emit_wide_get_chunk(instrs, &l, c);
                instrs.push(Instruction::I64LtU);
                instrs.push(Instruction::I64ExtendI32U); // c1 as i64
                if c > 0 {
                    // s2 = s1 + carry
                    instrs.push(Instruction::LocalGet(d.value_idx + c as u32));
                    instrs.push(Instruction::LocalGet(carry));
                    instrs.push(Instruction::I64Add);
                    instrs.push(Instruction::LocalTee(d.value_idx + c as u32));
                    // c2 = s2 < s1 (carry from adding carry_in)
                    // But s1 was overwritten. Use: c2 = (carry != 0 && s2 == 0 when s1 was MAX)
                    // Simpler: c2 = (carry != 0) & (s2 < carry) ... no.
                    // Actually: if carry_in was 1 and s1 was 0xFFFF...FFFF, then s2 wraps.
                    // c2 = (s2 < carry_in)
                    instrs.push(Instruction::LocalGet(carry));
                    instrs.push(Instruction::I64LtU);
                    instrs.push(Instruction::I64ExtendI32U);
                    // total carry = c1 | c2
                    instrs.push(Instruction::I64Or);
                }
                instrs.push(Instruction::LocalSet(carry));
            }
        }
        // 3. Subtraction with borrow propagation
        BinaryOp::Sub => {
            let borrow = locals.alloc(1);
            let d1 = locals.alloc(1);
            let c1 = locals.alloc(1);
            let c2 = locals.alloc(1);
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(borrow));
            for c in 0..d_chunks {
                emit_wide_get_chunk(instrs, &l, c);
                emit_wide_get_chunk(instrs, &r, c);
                instrs.push(Instruction::I64Sub);
                instrs.push(Instruction::LocalSet(d1));

                emit_wide_get_chunk(instrs, &l, c);
                emit_wide_get_chunk(instrs, &r, c);
                instrs.push(Instruction::I64LtU);
                instrs.push(Instruction::I64ExtendI32U);
                instrs.push(Instruction::LocalSet(c1));
                instrs.push(Instruction::LocalGet(d1));
                instrs.push(Instruction::LocalGet(borrow));
                instrs.push(Instruction::I64Sub);
                instrs.push(Instruction::LocalSet(d.value_idx + c as u32));
                instrs.push(Instruction::LocalGet(d1));
                instrs.push(Instruction::LocalGet(borrow));
                instrs.push(Instruction::I64LtU);
                instrs.push(Instruction::I64ExtendI32U);
                instrs.push(Instruction::LocalSet(c2));
                instrs.push(Instruction::LocalGet(c1));
                instrs.push(Instruction::LocalGet(c2));
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalSet(borrow));
            }
        }
        // 4. Shift operations
        BinaryOp::Shl | BinaryOp::Shr => {
            emit_wide_shift(op, &l, &r, &d, d_chunks, locals, instrs);
        }
        BinaryOp::Sar => {
            emit_wide_sar(&l, &r, &d, d_chunks, l_width, locals, instrs);
        }
        // 5. Eq/Ne
        BinaryOp::Eq | BinaryOp::Ne | BinaryOp::EqWildcard | BinaryOp::NeWildcard => {
            let max_chunks = l.num_chunks.max(r.num_chunks);
            instrs.push(Instruction::I64Const(1)); // accumulator
            for c in 0..max_chunks {
                emit_wide_get_chunk(instrs, &l, c);
                emit_wide_get_chunk(instrs, &r, c);
                instrs.push(Instruction::I64Eq);
                instrs.push(Instruction::I64ExtendI32U);
                instrs.push(Instruction::I64And);
            }
            if matches!(op, BinaryOp::Ne | BinaryOp::NeWildcard) {
                instrs.push(Instruction::I64Const(1));
                instrs.push(Instruction::I64Xor);
            }
            instrs.push(Instruction::LocalSet(d.value_idx));
            for c in 1..d_chunks {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(d.value_idx + c as u32));
            }
        }
        // 6. Unsigned comparisons (LtU, LeU, GtU, GeU)
        BinaryOp::LtU | BinaryOp::LeU | BinaryOp::GtU | BinaryOp::GeU => {
            let result = locals.alloc(1);
            let decided = locals.alloc(1);
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(result));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(decided));
            for c in (0..l.num_chunks.max(r.num_chunks)).rev() {
                let lc = if c < l.num_chunks {
                    l.value_idx + c as u32
                } else {
                    u32::MAX
                };
                let rc = if c < r.num_chunks {
                    r.value_idx + c as u32
                } else {
                    u32::MAX
                };

                // Get chunks (0 if out of range)
                if lc != u32::MAX {
                    instrs.push(Instruction::LocalGet(lc));
                } else {
                    instrs.push(Instruction::I64Const(0));
                }
                if rc != u32::MAX {
                    instrs.push(Instruction::LocalGet(rc));
                } else {
                    instrs.push(Instruction::I64Const(0));
                }
                let tmp_l = locals.alloc(1);
                let tmp_r = locals.alloc(1);
                instrs.push(Instruction::LocalSet(tmp_r));
                instrs.push(Instruction::LocalSet(tmp_l));

                instrs.push(Instruction::LocalGet(decided));
                instrs.push(Instruction::I64Eqz);
                instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
                instrs.push(Instruction::LocalGet(tmp_l));
                instrs.push(Instruction::LocalGet(tmp_r));
                instrs.push(Instruction::I64Ne);
                instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
                instrs.push(Instruction::LocalGet(tmp_l));
                instrs.push(Instruction::LocalGet(tmp_r));
                match op {
                    BinaryOp::LtU | BinaryOp::LeU => instrs.push(Instruction::I64LtU),
                    BinaryOp::GtU | BinaryOp::GeU => instrs.push(Instruction::I64GtU),
                    _ => unreachable!(),
                }
                instrs.push(Instruction::I64ExtendI32U);
                instrs.push(Instruction::LocalSet(result));
                instrs.push(Instruction::I64Const(1));
                instrs.push(Instruction::LocalSet(decided));
                instrs.push(Instruction::End);
                instrs.push(Instruction::End);
            }
            if matches!(op, BinaryOp::LeU | BinaryOp::GeU) {
                instrs.push(Instruction::LocalGet(decided));
                instrs.push(Instruction::I64Eqz);
                instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
                instrs.push(Instruction::I64Const(1));
                instrs.push(Instruction::LocalSet(result));
                instrs.push(Instruction::End);
            }
            instrs.push(Instruction::LocalGet(result));
            instrs.push(Instruction::LocalSet(d.value_idx));
            for c in 1..d_chunks {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(d.value_idx + c as u32));
            }
        }
        // 7. Signed comparisons
        BinaryOp::LtS | BinaryOp::LeS | BinaryOp::GtS | BinaryOp::GeS => {
            // Compare MSB sign bit first, then unsigned comparison for remaining
            // For now, use same as unsigned but check sign of top chunk
            let result = locals.alloc(1);
            let l_sign = locals.alloc(1);
            let r_sign = locals.alloc(1);
            let top = l.num_chunks.max(r.num_chunks) - 1;
            let sign_bit = ((l_width.max(r_width) - 1) % 64) as i64;
            // The sign bit can be below bit 63 in a partial top chunk.
            emit_wide_get_chunk(instrs, &l, top);
            instrs.push(Instruction::I64Const(sign_bit));
            instrs.push(Instruction::I64ShrU);
            instrs.push(Instruction::I64Const(1));
            instrs.push(Instruction::I64And);
            instrs.push(Instruction::LocalSet(l_sign));
            emit_wide_get_chunk(instrs, &r, top);
            instrs.push(Instruction::I64Const(sign_bit));
            instrs.push(Instruction::I64ShrU);
            instrs.push(Instruction::I64Const(1));
            instrs.push(Instruction::I64And);
            instrs.push(Instruction::LocalSet(r_sign));

            // If signs differ: negative < positive
            instrs.push(Instruction::LocalGet(l_sign));
            instrs.push(Instruction::LocalGet(r_sign));
            instrs.push(Instruction::I64Ne);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            // Signs differ
            match op {
                BinaryOp::LtS | BinaryOp::LeS => {
                    // l < r iff l is negative (l_sign=1, r_sign=0)
                    instrs.push(Instruction::LocalGet(l_sign));
                }
                BinaryOp::GtS | BinaryOp::GeS => {
                    // l > r iff l is positive (l_sign=0, r_sign=1) → r_sign
                    instrs.push(Instruction::LocalGet(r_sign));
                }
                _ => unreachable!(),
            }
            instrs.push(Instruction::LocalSet(result));
            instrs.push(Instruction::Else);
            // Signs same: compare from MSB to LSB and stop at the first differing chunk.
            let decided = locals.alloc(1);
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(result));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(decided));
            for c in (0..l.num_chunks.max(r.num_chunks)).rev() {
                let tmp_l = locals.alloc(1);
                let tmp_r = locals.alloc(1);
                emit_wide_get_chunk(instrs, &l, c);
                instrs.push(Instruction::LocalSet(tmp_l));
                emit_wide_get_chunk(instrs, &r, c);
                instrs.push(Instruction::LocalSet(tmp_r));
                instrs.push(Instruction::LocalGet(decided));
                instrs.push(Instruction::I64Eqz);
                instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
                instrs.push(Instruction::LocalGet(tmp_l));
                instrs.push(Instruction::LocalGet(tmp_r));
                instrs.push(Instruction::I64Ne);
                instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
                instrs.push(Instruction::LocalGet(tmp_l));
                instrs.push(Instruction::LocalGet(tmp_r));
                match op {
                    BinaryOp::LtS | BinaryOp::LeS => instrs.push(Instruction::I64LtU),
                    BinaryOp::GtS | BinaryOp::GeS => instrs.push(Instruction::I64GtU),
                    _ => unreachable!(),
                }
                instrs.push(Instruction::I64ExtendI32U);
                instrs.push(Instruction::LocalSet(result));
                instrs.push(Instruction::I64Const(1));
                instrs.push(Instruction::LocalSet(decided));
                instrs.push(Instruction::End);
                instrs.push(Instruction::End);
            }
            if matches!(op, BinaryOp::LeS | BinaryOp::GeS) {
                instrs.push(Instruction::LocalGet(decided));
                instrs.push(Instruction::I64Eqz);
                instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
                instrs.push(Instruction::I64Const(1));
                instrs.push(Instruction::LocalSet(result));
                instrs.push(Instruction::End);
            }
            instrs.push(Instruction::End); // end if (sign differ)
            instrs.push(Instruction::LocalGet(result));
            instrs.push(Instruction::LocalSet(d.value_idx));
            for c in 1..d_chunks {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(d.value_idx + c as u32));
            }
        }
        // 8. LogicAnd / LogicOr — reduce to bool first
        BinaryOp::LogicAnd | BinaryOp::LogicOr => {
            // l_bool = (|l_chunks) != 0
            emit_wide_get_chunk(instrs, &l, 0);
            for c in 1..l.num_chunks {
                emit_wide_get_chunk(instrs, &l, c);
                instrs.push(Instruction::I64Or);
            }
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::I64Ne);
            // r_bool = (|r_chunks) != 0
            emit_wide_get_chunk(instrs, &r, 0);
            for c in 1..r.num_chunks {
                emit_wide_get_chunk(instrs, &r, c);
                instrs.push(Instruction::I64Or);
            }
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::I64Ne);
            // combine
            if matches!(op, BinaryOp::LogicAnd) {
                instrs.push(Instruction::I32And);
            } else {
                instrs.push(Instruction::I32Or);
            }
            instrs.push(Instruction::I64ExtendI32U);
            instrs.push(Instruction::LocalSet(d.value_idx));
            for c in 1..d_chunks {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(d.value_idx + c as u32));
            }
        }
        // 9. Mul / Div / Rem — placeholder for now
        BinaryOp::Mul => {
            let carry = locals.alloc(1);
            let tmp_lo = locals.alloc(1);
            let tmp_rhs = locals.alloc(1);
            let tmp_hi = locals.alloc(1);
            let sum1 = locals.alloc(1);
            let c1 = locals.alloc(1);
            let c2 = locals.alloc(1);

            for c in 0..d_chunks {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(d.value_idx + c as u32));
            }

            for i in 0..d_chunks {
                emit_wide_get_chunk(instrs, &l, i);
                instrs.push(Instruction::LocalSet(tmp_lo));
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(carry));

                for j in 0..d_chunks {
                    let k = i + j;
                    if k >= d_chunks {
                        break;
                    }

                    emit_wide_get_chunk(instrs, &r, j);
                    instrs.push(Instruction::LocalSet(tmp_rhs));
                    instrs.push(Instruction::LocalGet(tmp_rhs));
                    instrs.push(Instruction::LocalGet(tmp_lo));
                    instrs.push(Instruction::I64Mul);
                    instrs.push(Instruction::LocalSet(sum1));

                    emit_u64_mul_hi(instrs, tmp_lo, tmp_rhs, tmp_hi, locals);

                    instrs.push(Instruction::LocalGet(d.value_idx + k as u32));
                    instrs.push(Instruction::LocalGet(sum1));
                    instrs.push(Instruction::I64Add);
                    instrs.push(Instruction::LocalSet(sum1));

                    instrs.push(Instruction::LocalGet(sum1));
                    instrs.push(Instruction::LocalGet(d.value_idx + k as u32));
                    instrs.push(Instruction::I64LtU);
                    instrs.push(Instruction::I64ExtendI32U);
                    instrs.push(Instruction::LocalSet(c1));

                    instrs.push(Instruction::LocalGet(sum1));
                    instrs.push(Instruction::LocalGet(carry));
                    instrs.push(Instruction::I64Add);
                    instrs.push(Instruction::LocalTee(d.value_idx + k as u32));
                    instrs.push(Instruction::LocalGet(sum1));
                    instrs.push(Instruction::I64LtU);
                    instrs.push(Instruction::I64ExtendI32U);
                    instrs.push(Instruction::LocalSet(c2));

                    instrs.push(Instruction::LocalGet(tmp_hi));
                    instrs.push(Instruction::LocalGet(c1));
                    instrs.push(Instruction::I64Add);
                    instrs.push(Instruction::LocalGet(c2));
                    instrs.push(Instruction::I64Add);
                    instrs.push(Instruction::LocalSet(carry));
                }
            }
        }
        BinaryOp::DivU | BinaryOp::DivS | BinaryOp::RemU | BinaryOp::RemS => {
            let signed = matches!(op, BinaryOp::DivS | BinaryOp::RemS);
            let quotient = matches!(op, BinaryOp::DivU | BinaryOp::DivS);
            let (dividend, lhs_negative) = if signed {
                prepare_wide_signed_magnitude(&l, l_width, d_chunks, locals, instrs)
            } else {
                (l.clone(), None)
            };
            let (divisor, rhs_negative) = if signed {
                prepare_wide_signed_magnitude(&r, r_width, d_chunks, locals, instrs)
            } else {
                (r.clone(), None)
            };
            let divisor_is_zero = locals.alloc(1);
            instrs.push(Instruction::I64Const(0));
            for c in 0..d_chunks {
                emit_wide_get_chunk(instrs, &divisor, c);
                instrs.push(Instruction::I64Or);
            }
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::I64ExtendI32U);
            instrs.push(Instruction::LocalSet(divisor_is_zero));

            let q = locals.alloc(d_chunks);
            let rem = locals.alloc(d_chunks);
            let cand = locals.alloc(d_chunks);
            let ge = locals.alloc(1);
            let borrow = locals.alloc(1);
            let d1 = locals.alloc(1);
            let c1 = locals.alloc(1);
            let c2 = locals.alloc(1);

            for c in 0..d_chunks {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(q + c as u32));
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(rem + c as u32));
            }

            for bit in (0..d_width).rev() {
                let chunk_idx = bit / 64;
                let bit_idx = bit % 64;

                for c in (0..d_chunks).rev() {
                    instrs.push(Instruction::LocalGet(rem + c as u32));
                    instrs.push(Instruction::I64Const(1));
                    instrs.push(Instruction::I64Shl);
                    if c > 0 {
                        instrs.push(Instruction::LocalGet(rem + (c as u32 - 1)));
                        instrs.push(Instruction::I64Const(63));
                        instrs.push(Instruction::I64ShrU);
                        instrs.push(Instruction::I64Or);
                    }
                    instrs.push(Instruction::LocalSet(rem + c as u32));
                }

                instrs.push(Instruction::LocalGet(rem));
                emit_wide_get_chunk(instrs, &dividend, chunk_idx);
                instrs.push(Instruction::I64Const(bit_idx as i64));
                instrs.push(Instruction::I64ShrU);
                instrs.push(Instruction::I64Const(1));
                instrs.push(Instruction::I64And);
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalSet(rem));

                instrs.push(Instruction::I64Const(1));
                instrs.push(Instruction::LocalSet(ge));
                for c in 0..d_chunks {
                    instrs.push(Instruction::LocalGet(rem + c as u32));
                    emit_wide_get_chunk(instrs, &divisor, c);
                    instrs.push(Instruction::I64Ne);
                    instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
                    instrs.push(Instruction::LocalGet(rem + c as u32));
                    emit_wide_get_chunk(instrs, &divisor, c);
                    instrs.push(Instruction::I64GtU);
                    instrs.push(Instruction::I64ExtendI32U);
                    instrs.push(Instruction::LocalSet(ge));
                    instrs.push(Instruction::End);
                }

                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(borrow));
                for c in 0..d_chunks {
                    instrs.push(Instruction::LocalGet(rem + c as u32));
                    emit_wide_get_chunk(instrs, &divisor, c);
                    instrs.push(Instruction::I64Sub);
                    instrs.push(Instruction::LocalSet(d1));

                    instrs.push(Instruction::LocalGet(rem + c as u32));
                    emit_wide_get_chunk(instrs, &divisor, c);
                    instrs.push(Instruction::I64LtU);
                    instrs.push(Instruction::I64ExtendI32U);
                    instrs.push(Instruction::LocalSet(c1));

                    instrs.push(Instruction::LocalGet(d1));
                    instrs.push(Instruction::LocalGet(borrow));
                    instrs.push(Instruction::I64Sub);
                    instrs.push(Instruction::LocalSet(cand + c as u32));

                    instrs.push(Instruction::LocalGet(d1));
                    instrs.push(Instruction::LocalGet(borrow));
                    instrs.push(Instruction::I64LtU);
                    instrs.push(Instruction::I64ExtendI32U);
                    instrs.push(Instruction::LocalSet(c2));

                    instrs.push(Instruction::LocalGet(c1));
                    instrs.push(Instruction::LocalGet(c2));
                    instrs.push(Instruction::I64Or);
                    instrs.push(Instruction::LocalSet(borrow));
                }

                instrs.push(Instruction::LocalGet(ge));
                instrs.push(Instruction::I32WrapI64);
                instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
                for c in 0..d_chunks {
                    instrs.push(Instruction::LocalGet(cand + c as u32));
                    instrs.push(Instruction::LocalSet(rem + c as u32));
                }
                instrs.push(Instruction::LocalGet(q + chunk_idx as u32));
                instrs.push(Instruction::I64Const((1u64 << bit_idx) as i64));
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalSet(q + chunk_idx as u32));
                instrs.push(Instruction::End);
            }

            let magnitude = RegLocal {
                value_idx: if quotient { q } else { rem },
                num_chunks: d_chunks,
                mask_idx: None,
            };
            let signed_result = if signed {
                let result_negative = if quotient {
                    let result = locals.alloc(1);
                    instrs.push(Instruction::LocalGet(lhs_negative.unwrap()));
                    instrs.push(Instruction::LocalGet(rhs_negative.unwrap()));
                    instrs.push(Instruction::I64Xor);
                    instrs.push(Instruction::LocalSet(result));
                    result
                } else {
                    lhs_negative.unwrap()
                };
                conditional_negate_wide_local(&magnitude, result_negative, d_chunks, locals, instrs)
            } else {
                magnitude
            };
            for c in 0..d_chunks {
                instrs.push(Instruction::LocalGet(divisor_is_zero));
                instrs.push(Instruction::I64Eqz);
                instrs.push(Instruction::If(wasm_encoder::BlockType::Result(
                    ValType::I64,
                )));
                emit_wide_get_chunk(instrs, &signed_result, c);
                instrs.push(Instruction::Else);
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::End);
                instrs.push(Instruction::LocalSet(d.value_idx + c as u32));
            }
        }
        BinaryOp::EqCase | BinaryOp::NeCase => unreachable!("handled above"),
    }

    // Mask top chunk to width
    let top_chunk = d_chunks - 1;
    let top_bits = d_width % 64;
    if top_bits > 0 && top_bits < 64 {
        let mask = (1u64 << top_bits) - 1;
        instrs.push(Instruction::LocalGet(d.value_idx + top_chunk as u32));
        instrs.push(Instruction::I64Const(mask as i64));
        instrs.push(Instruction::I64And);
        instrs.push(Instruction::LocalSet(d.value_idx + top_chunk as u32));
    }

    if four_state {
        compile_binary_mask_wide(
            &d,
            &l,
            op,
            &r,
            d_width,
            l_width.max(r_width),
            l_width,
            locals,
            instrs,
        );
        finish_binary_four_state(&d, op, &r, d_width, instrs);
    }
}
