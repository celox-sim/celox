//! Calls of extern functions through AAPCS64.
//!
//! A C function may change x0-x18 and all SIMD registers except the low
//! halves of d8-d15. The allocator steers values live across a call into
//! x19-x27, which the callee preserves along with the cached state-page and
//! spill bases. A call saves below SP only what it must: the state pointer in
//! x0, the caller-saved registers that still hold values live across the call,
//! and the SIMD registers a tick loop pins for its whole body.

use super::*;

/// Integer arguments passed in registers.
const REGISTER_ARGS: usize = 8;

/// Registers each extern call saves, keyed by block and instruction index:
/// x0 and the caller-saved registers holding values live across the call.
pub(super) fn call_saves(
    function: &MFunction,
    assignment: &Assignment<VReg>,
) -> Result<HashMap<(BlockId, usize), Vec<u8>>, EmitError> {
    let has_calls = function
        .blocks
        .iter()
        .flat_map(|block| &block.insts)
        .any(|instruction| matches!(instruction, MInst::CallExtern { .. }));
    if !has_calls {
        return Ok(HashMap::default());
    }
    let calls = crate::regalloc::allocated_values_live_across_calls(function)
        .map_err(|error| EmitError::Lowering(error.to_string()))?;
    calls
        .into_iter()
        .map(|(call, values)| {
            let mut registers = vec![STATE_REG];
            for value in values {
                let register = resolve(assignment, value)?;
                if register < 19 {
                    registers.push(register);
                }
            }
            registers.sort_unstable();
            registers.dedup();
            Ok((call, registers))
        })
        .collect()
}

pub(super) fn emit_call_extern(
    ops: &mut VecAssembler<Aarch64Relocation>,
    assignment: &Assignment<VReg>,
    dst: Option<VReg>,
    func: u32,
    args: &[VReg],
    saved: &[u8],
    pinned_fp: &[u8],
    call_stack: Option<celox_state_layout::CallStackLayout>,
) -> Result<(), EmitError> {
    // The heap helper takes the state pointer before the arguments, so C
    // argument `position` is `args[position - leading]`. So does a function
    // unit, whose one argument is its frame's offset into the call stack.
    let heap = func == celox_sir::extern_abi::HEAP_HELPER_ID;
    let function = celox_sir::extern_abi::native_function_slot(func);
    let leading = usize::from(heap || function.is_some());
    let positions = args.len() + leading;
    let register_args = positions.min(REGISTER_ARGS);
    // Outgoing stack arguments sit at SP, followed by the staged register
    // arguments and the save areas.
    let staging = positions.saturating_sub(REGISTER_ARGS) * 8;
    let gpr_save = staging + register_args * 8;
    let fp_save = gpr_save + saved.len() * 8;
    let frame = (fp_save + pinned_fp.len() * 8 + 15) & !15;
    let frame_u32 = u32::try_from(frame).expect("extern call frame fits an immediate");
    dynasm!(ops ; .arch aarch64 ; sub sp, sp, frame_u32);
    for (index, &register) in saved.iter().enumerate() {
        emit_store_at(
            ops,
            register,
            31,
            (gpr_save + index * 8) as i64,
            OpSize::S64,
        );
    }
    for (index, &register) in pinned_fp.iter().enumerate() {
        let offset = u32::try_from(fp_save + index * 8).expect("extern call frame offset");
        dynasm!(ops ; .arch aarch64 ; str D(register), [sp, offset]);
    }

    // Write every argument before overwriting any argument register, so
    // the moves into x0-x7 cannot clobber a source.
    for (index, &value) in args.iter().enumerate() {
        let position = index + leading;
        let offset = if position < REGISTER_ARGS {
            staging + position * 8
        } else {
            (position - REGISTER_ARGS) * 8
        };
        emit_store_at(
            ops,
            resolve(assignment, value)?,
            31,
            offset as i64,
            OpSize::S64,
        );
    }
    for index in leading..register_args {
        emit_load_at(
            ops,
            index as u8,
            31,
            (staging + index * 8) as i64,
            OpSize::S64,
        );
    }
    if heap {
        // The state pointer is the saved x0.
        emit_load_at(ops, 0, 31, gpr_save as i64, OpSize::S64);
    }
    let stack = function
        .map(|_| call_stack.ok_or(EmitError::Unsupported("a call without a call stack")))
        .transpose()?;
    if let Some(stack) = stack {
        // A function unit takes its frame's address; x0 still holds the
        // state pointer.
        dynasm!(ops ; .arch aarch64 ; add x1, x0, x1);
        emit_address_to(ops, 1, 1, stack.base_offset as i64);
    }

    // The state pointer is the saved x0.
    emit_load_at(ops, SCRATCH1, 31, gpr_save as i64, OpSize::S64);
    emit_load_at(
        ops,
        SCRATCH1,
        SCRATCH1,
        celox_state_layout::STATE_HEADER_EXTERN_FUNCTIONS_ADDR_OFFSET as i64,
        OpSize::S64,
    );
    let offset = if func == celox_sir::float::FLOAT_HELPER_ID {
        -8
    } else if heap {
        -16
    } else {
        i64::from(function.unwrap_or(func)) * 8
    };
    emit_load_at(ops, SCRATCH1, SCRATCH1, offset, OpSize::S64);
    dynasm!(ops ; .arch aarch64 ; blr x17);
    if let Some(stack) = stack {
        // Return to the depth before the guard, whatever the callee's
        // status, and keep the status in x16 while x0 is restored.
        emit_load_at(ops, SCRATCH0, 31, gpr_save as i64, OpSize::S64);
        emit_address_to(ops, SCRATCH0, SCRATCH0, stack.depth_offset as i64);
        dynasm!(ops
            ; .arch aarch64
            ; ldr x17, [x16]
            ; sub x17, x17, #1
            ; str x17, [x16]
            ; mov x16, x0
        );
    }

    // The result interferes with every value live across the call, so the
    // restores below never overwrite it.
    if let Some(dst) = dst {
        let register = resolve(assignment, dst)?;
        dynasm!(ops ; .arch aarch64 ; mov X(register), x0);
    }
    for (index, &register) in pinned_fp.iter().enumerate() {
        let offset = u32::try_from(fp_save + index * 8).expect("extern call frame offset");
        dynasm!(ops ; .arch aarch64 ; ldr D(register), [sp, offset]);
    }
    for (index, &register) in saved.iter().enumerate() {
        emit_load_at(
            ops,
            register,
            31,
            (gpr_save + index * 8) as i64,
            OpSize::S64,
        );
    }
    dynasm!(ops ; .arch aarch64 ; add sp, sp, frame_u32);
    Ok(())
}

/// Guard the call of a function unit whose frame is `frame_size` bytes: trap
/// at `trap` when the frame, whose offset into the call stack is in register
/// `frame`, does not fit in the stack or the calls already nest
/// `MAX_CALL_DEPTH` deep, and otherwise enter one call deeper. It changes
/// only the scratch registers and x30, which the prologue saved. The trap
/// is reached by unconditional branches, whose range covers any function.
pub(super) fn emit_call_guard(
    ops: &mut VecAssembler<Aarch64Relocation>,
    frame: u8,
    frame_size: usize,
    stack: celox_state_layout::CallStackLayout,
    trap: DynamicLabel,
) {
    match stack.size.checked_sub(frame_size) {
        Some(limit) => {
            let fits = ops.new_dynamic_label();
            emit_load_imm(ops, SCRATCH1, limit as u64);
            dynasm!(ops
                ; .arch aarch64
                ; cmp X(frame), x17
                ; b.ls =>fits
                ; b =>trap
                ; =>fits
            );
        }
        None => dynasm!(ops ; .arch aarch64 ; b =>trap),
    }
    let shallow = ops.new_dynamic_label();
    emit_address(ops, STATE_REG, stack.depth_offset as i64);
    emit_load_imm(ops, 30, celox_sir::MAX_CALL_DEPTH as u64);
    dynasm!(ops
        ; .arch aarch64
        ; ldr x17, [x16]
        ; cmp x17, x30
        ; b.lo =>shallow
        ; b =>trap
        ; =>shallow
        ; add x17, x17, #1
        ; str x17, [x16]
    );
}
