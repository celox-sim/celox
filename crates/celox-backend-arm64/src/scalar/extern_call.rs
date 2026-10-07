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
) -> Result<(), EmitError> {
    let register_args = args.len().min(REGISTER_ARGS);
    // Outgoing stack arguments sit at SP, followed by the staged register
    // arguments and the save areas.
    let staging = args.len().saturating_sub(REGISTER_ARGS) * 8;
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
        let offset = if index < REGISTER_ARGS {
            staging + index * 8
        } else {
            (index - REGISTER_ARGS) * 8
        };
        emit_store_at(
            ops,
            resolve(assignment, value)?,
            31,
            offset as i64,
            OpSize::S64,
        );
    }
    for index in 0..register_args {
        emit_load_at(
            ops,
            index as u8,
            31,
            (staging + index * 8) as i64,
            OpSize::S64,
        );
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
