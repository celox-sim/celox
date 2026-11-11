//! Calls of generated units and of external functions.

use super::*;

/// Run `function` in the frame based `frame` bytes into the stack: guard the
/// stack and the call depth (trapping), store the arguments, call it, end the
/// running unit with its status when that is nonzero, and load `dst` from its
/// output slots.
#[allow(clippy::too_many_arguments)]
pub(super) fn compile_call(
    function: FunctionId,
    frame: RegisterId,
    args: &[(usize, RegisterId)],
    outputs: &[usize],
    dst: Option<RegisterId>,
    unit: &ExecutionUnit<RegionedAbsoluteAddr>,
    four_state: bool,
    calls: &Calls<'_>,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let stack = calls
        .stack
        .expect("the layout of a program that calls functions has a call stack");
    let callee = &calls.functions[function.0 as usize].frame;

    // The frame, `offset` bytes past the stack base, must lie in the stack.
    // Producers keep calls within it, so a frame outside is a broken
    // invariant, not a simulation error.
    let offset = locals.alloc(1);
    instrs.push(Instruction::LocalGet(locals.reg_map[&frame].value_idx));
    let frame_width = unit.register_map[&frame].width();
    if frame_width < 64 {
        instrs.push(Instruction::I64Const(((1u64 << frame_width) - 1) as i64));
        instrs.push(Instruction::I64And);
    }
    instrs.push(Instruction::LocalSet(offset));
    match stack.size.checked_sub(callee.size) {
        Some(limit) => {
            instrs.push(Instruction::LocalGet(offset));
            instrs.push(Instruction::I64Const(limit as i64));
            instrs.push(Instruction::I64GtU);
        }
        None => instrs.push(Instruction::I32Const(1)),
    }
    instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
    instrs.push(Instruction::Unreachable);
    instrs.push(Instruction::End);

    let depth = locals.alloc(1);
    let memarg = wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    };
    instrs.push(Instruction::I32Const(stack.depth_offset as i32));
    instrs.push(Instruction::I64Load(memarg));
    instrs.push(Instruction::LocalTee(depth));
    instrs.push(Instruction::I64Const(celox_sir::MAX_CALL_DEPTH as i64));
    instrs.push(Instruction::I64GeU);
    instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
    instrs.push(Instruction::Unreachable);
    instrs.push(Instruction::End);
    instrs.push(Instruction::I32Const(stack.depth_offset as i32));
    instrs.push(Instruction::LocalGet(depth));
    instrs.push(Instruction::I64Const(1));
    instrs.push(Instruction::I64Add);
    instrs.push(Instruction::I64Store(memarg));

    // Slot offsets are relative to the frame, `stack.base_offset + offset`.
    let frame_base = Base::I64(offset);
    let in_frame = |slot: &FrameSlot<RegionedAbsoluteAddr>| FrameSlot {
        offset: stack.base_offset + slot.offset,
        ..slot.clone()
    };
    for &(position, arg) in args {
        let slot = in_frame(&callee.slots[position]);
        let src = locals.reg_map[&arg].clone();
        store_slot(
            &src, frame_base, &slot, 0, slot.width, four_state, locals, instrs,
        );
    }

    emit_address(frame_base, stack.base_offset, instrs);
    instrs.push(Instruction::Call(calls.first_index + function.0));
    let status = locals.alloc(1);
    instrs.push(Instruction::LocalSet(status));
    // The callee left the depth as it found it, also when it failed.
    instrs.push(Instruction::I32Const(stack.depth_offset as i32));
    instrs.push(Instruction::LocalGet(depth));
    instrs.push(Instruction::I64Store(memarg));
    instrs.push(Instruction::LocalGet(status));
    instrs.push(Instruction::I64Const(0));
    instrs.push(Instruction::I64Ne);
    instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
    instrs.push(Instruction::LocalGet(status));
    instrs.push(Instruction::Return);
    instrs.push(Instruction::End);

    let Some(dst) = dst else {
        return;
    };
    let dst = locals.reg_map[&dst].clone();
    if let [position] = outputs {
        let slot = in_frame(&callee.slots[*position]);
        load_slot(&dst, frame_base, &slot, 0, slot.width, four_state, instrs);
        return;
    }
    // The concatenation of several slots, the first in the least significant
    // bits: each is loaded whole, with no bits set above its width, and ored
    // into place.
    let planes = |local: &RegLocal| {
        std::iter::once(local.value_idx).chain(local.mask_idx.filter(|_| four_state))
    };
    for plane in planes(&dst) {
        for c in 0..dst.num_chunks {
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(plane + c as u32));
        }
    }
    let mut shift = 0;
    for &position in outputs {
        let slot = in_frame(&callee.slots[position]);
        let chunks = num_i64_chunks(slot.width);
        let part = RegLocal {
            value_idx: locals.alloc(chunks),
            num_chunks: chunks,
            mask_idx: four_state.then(|| locals.alloc(chunks)),
        };
        load_slot(&part, frame_base, &slot, 0, slot.width, four_state, instrs);
        for (into, from) in planes(&dst).zip(planes(&part)) {
            for c in 0..chunks {
                let bit = shift + c * 64;
                let (word, offset) = (bit / 64, bit % 64);
                let from = from + c as u32;
                if word < dst.num_chunks {
                    instrs.push(Instruction::LocalGet(into + word as u32));
                    instrs.push(Instruction::LocalGet(from));
                    if offset != 0 {
                        instrs.push(Instruction::I64Const(offset as i64));
                        instrs.push(Instruction::I64Shl);
                    }
                    instrs.push(Instruction::I64Or);
                    instrs.push(Instruction::LocalSet(into + word as u32));
                }
                if offset != 0 && word + 1 < dst.num_chunks {
                    instrs.push(Instruction::LocalGet(into + word as u32 + 1));
                    instrs.push(Instruction::LocalGet(from));
                    instrs.push(Instruction::I64Const((64 - offset) as i64));
                    instrs.push(Instruction::I64ShrU);
                    instrs.push(Instruction::I64Or);
                    instrs.push(Instruction::LocalSet(into + word as u32 + 1));
                }
            }
        }
        shift += slot.width;
    }
}

/// Call an extern function through its host import.
///
/// Every argument and the result is an i64 holding the C integer: a `Bit`
/// register is sign- or zero-extended, and a one-bit `Logic` register is an
/// `svLogic` (`value | mask << 1`).
pub(super) fn compile_extern_call(
    dst: Option<RegisterId>,
    function_index: u32,
    args: &[RegisterId],
    unit: &ExecutionUnit<RegionedAbsoluteAddr>,
    locals: &LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    for arg in args {
        let local = &locals.reg_map[arg];
        instrs.push(Instruction::LocalGet(local.value_idx));
        let ty = ExternValue::of_verified(&unit.register_map[arg]);
        match ty.sign_extended_width() {
            Some(8) => instrs.push(Instruction::I64Extend8S),
            Some(16) => instrs.push(Instruction::I64Extend16S),
            Some(32) => instrs.push(Instruction::I64Extend32S),
            Some(width) => unreachable!("a {width}-bit extern call argument is not a C integer"),
            None => {}
        }
        if let (ExternValue::Logic, Some(mask)) = (ty, local.mask_idx) {
            instrs.push(Instruction::LocalGet(mask));
            instrs.push(Instruction::I64Const(SV_LOGIC_MASK_SHIFT.into()));
            instrs.push(Instruction::I64Shl);
            instrs.push(Instruction::I64Or);
        }
    }
    instrs.push(Instruction::Call(function_index));
    let Some(dst) = dst else {
        instrs.push(Instruction::Drop);
        return;
    };
    let local = &locals.reg_map[&dst];
    let ty = ExternValue::of_verified(&unit.register_map[&dst]);
    if let Some(mask) = local.mask_idx {
        match ty {
            ExternValue::Logic => {
                instrs.push(Instruction::LocalTee(local.value_idx));
                instrs.push(Instruction::I64Const(SV_LOGIC_MASK_SHIFT.into()));
                instrs.push(Instruction::I64ShrU);
                instrs.push(Instruction::I64Const(1));
                instrs.push(Instruction::I64And);
                instrs.push(Instruction::LocalSet(mask));
                instrs.push(Instruction::LocalGet(local.value_idx));
            }
            ExternValue::Integer { .. } => {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(mask));
            }
        }
    }
    let value_mask = ty.result_value_mask();
    if value_mask != u64::MAX {
        instrs.push(Instruction::I64Const(value_mask as i64));
        instrs.push(Instruction::I64And);
    }
    instrs.push(Instruction::LocalSet(local.value_idx));
}
