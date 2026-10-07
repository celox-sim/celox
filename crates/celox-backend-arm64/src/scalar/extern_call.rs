//! Calls of extern functions through AAPCS64.
//!
//! The register allocator does not model clobbers, so a call preserves every
//! caller-saved register itself: x0-x17 and q0-q31 are saved below SP, the
//! arguments are read back from that save area into the argument registers,
//! and everything except the result register is restored after the call.
//! x19-x29 are callee-saved, so allocated values and the cached state-page
//! and spill bases survive the call.

#![allow(clippy::useless_conversion)]

use super::*;

/// Caller-saved general-purpose registers preserved around a call.
const SAVED_GPRS: u8 = 18;
/// Integer arguments passed in registers.
const REGISTER_ARGS: usize = 8;

pub(super) fn emit_call_extern(
    ops: &mut VecAssembler<Aarch64Relocation>,
    assignment: &Assignment<VReg>,
    dst: Option<VReg>,
    func: u32,
    args: &[VReg],
) -> Result<(), EmitError> {
    let stack_args = args.len().saturating_sub(REGISTER_ARGS);
    // Outgoing stack arguments sit at SP, below the register save area.
    let gpr_save = (stack_args * 8 + 15) & !15;
    let simd_save = gpr_save + usize::from(SAVED_GPRS) * 8;
    let frame = simd_save + 32 * 16;
    let frame_u32 = u32::try_from(frame).expect("extern call frame fits an immediate");
    dynasm!(ops ; .arch aarch64 ; sub sp, sp, frame_u32);
    for first in (0..SAVED_GPRS).step_by(2) {
        let offset = (gpr_save + usize::from(first) * 8) as i32;
        dynasm!(ops ; .arch aarch64 ; stp X(first), X(first + 1), [sp, offset]);
    }
    for first in (0..32u8).step_by(2) {
        let offset = (simd_save + usize::from(first) * 16) as i32;
        dynasm!(ops ; .arch aarch64 ; stp Q(first), Q(first + 1), [sp, offset]);
    }

    // Read an argument into `target`: a caller-saved register from its save
    // slot, a callee-saved one directly.
    let argument = |ops: &mut VecAssembler<Aarch64Relocation>, target: u8, value: VReg| {
        let register = resolve(assignment, value)?;
        if register < SAVED_GPRS {
            emit_load_at(
                ops,
                target,
                31,
                (gpr_save + usize::from(register) * 8) as i64,
                OpSize::S64,
            );
        } else {
            dynasm!(ops ; .arch aarch64 ; mov X(target), X(register));
        }
        Ok::<(), EmitError>(())
    };
    for (index, &value) in args.iter().enumerate().skip(REGISTER_ARGS) {
        argument(ops, SCRATCH0, value)?;
        emit_store_at(
            ops,
            SCRATCH0,
            31,
            ((index - REGISTER_ARGS) * 8) as i64,
            OpSize::S64,
        );
    }
    for (index, &value) in args.iter().enumerate().take(REGISTER_ARGS) {
        argument(ops, index as u8, value)?;
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
    emit_load_at(ops, SCRATCH1, SCRATCH1, i64::from(func) * 8, OpSize::S64);
    dynasm!(ops ; .arch aarch64 ; blr x17);

    if let Some(dst) = dst {
        let register = resolve(assignment, dst)?;
        if register < SAVED_GPRS {
            emit_store_at(
                ops,
                0,
                31,
                (gpr_save + usize::from(register) * 8) as i64,
                OpSize::S64,
            );
        } else {
            dynasm!(ops ; .arch aarch64 ; mov X(register), x0);
        }
    }
    for first in (0..32u8).step_by(2) {
        let offset = (simd_save + usize::from(first) * 16) as i32;
        dynasm!(ops ; .arch aarch64 ; ldp Q(first), Q(first + 1), [sp, offset]);
    }
    for first in (0..SAVED_GPRS).step_by(2) {
        let offset = (gpr_save + usize::from(first) * 8) as i32;
        dynasm!(ops ; .arch aarch64 ; ldp X(first), X(first + 1), [sp, offset]);
    }
    dynasm!(ops ; .arch aarch64 ; add sp, sp, frame_u32);
    Ok(())
}
