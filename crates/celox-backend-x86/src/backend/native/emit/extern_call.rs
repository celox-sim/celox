//! Calls of extern functions through the platform C ABI.
//!
//! Generated code keeps no stack frame and, in segment-base mode, owns the FS
//! or GS base. A call hands the caller's segment base back to C code (which
//! may use it for thread-local storage), aligns the stack, and restores the
//! base afterwards.
//!
//! `CallExtern` clobbers the C caller-saved GPRs and RBX, which holds the state
//! pointer during the call, so the allocator keeps values live across the call
//! in the other callee-saved GPRs or their homes. No vector value or cached
//! spill slot lives across the call either; only the XMMs pinned for the whole
//! function (stashed GPRs, the segment base, the tick count) are saved, and
//! only where the C ABI does not preserve them. Generated code uses only
//! 128-bit vector instructions, which never leave the upper YMM halves dirty,
//! so the call needs no `vzeroupper`.

use super::operands::state_base_strategy;
use super::*;

/// Where the caller's FS/GS base is kept while generated code borrows it.
#[derive(Clone, Copy, Debug)]
pub(super) enum CallerSegment {
    /// The state base is R15; no segment base is borrowed.
    None,
    /// The low qword of XMM15.
    Xmm15,
    /// A qword at this state-relative arena offset.
    Arena(i32),
}

/// XMM registers a C function preserves: XMM6-XMM15 on Windows, none in the
/// System V ABI.
const C_PRESERVED_XMMS: u16 = if cfg!(target_os = "windows") {
    0xffc0
} else {
    0
};

/// Set up the extern calls of the function being emitted. Bit `n` of
/// `pinned_xmms` marks XMMn as holding a value for the whole function.
pub(super) fn set_call_area(
    asm: &mut EmitAssembler,
    call_area: Option<i32>,
    caller_segment: CallerSegment,
    pinned_xmms: u16,
) {
    asm.context.call_area = call_area;
    asm.context.caller_segment = caller_segment;
    asm.context.pinned_xmms = pinned_xmms;
}

/// The XMM registers a call must save: pinned ones the callee may change.
fn saved_xmms(context: &EmissionContext) -> impl Iterator<Item = usize> {
    let saved = context.pinned_xmms & !C_PRESERVED_XMMS;
    (0..16).filter(move |index| saved & (1 << index) != 0)
}

fn call_area(context: &EmissionContext) -> i32 {
    context
        .call_area
        .expect("a function with extern calls has an extern call area")
}

fn xmm_register(index: usize) -> AsmRegisterXmm {
    [
        xmm0, xmm1, xmm2, xmm3, xmm4, xmm5, xmm6, xmm7, xmm8, xmm9, xmm10, xmm11, xmm12, xmm13,
        xmm14, xmm15,
    ][index]
}

/// `[state + call area + 8 * slot]` for an argument or the result.
fn call_slot(context: &EmissionContext, slot: i32) -> AsmMemoryOperand {
    mem_operand(context, BaseReg::SimState, call_area(context) + 8 * slot)
}

pub(super) fn emit_extern_arg(
    asm: &mut EmitAssembler,
    index: u8,
    src: AsmRegister64,
) -> Result<(), IcedError> {
    let context = asm.context;
    asm.mov(qword_ptr(call_slot(&context, i32::from(index))), src)
}

pub(super) fn emit_extern_result(
    asm: &mut EmitAssembler,
    dst: AsmRegister64,
) -> Result<(), IcedError> {
    let context = asm.context;
    asm.mov(dst, qword_ptr(call_slot(&context, CALL_AREA_RESULT / 8)))
}

pub(super) fn emit_call_extern(
    asm: &mut EmitAssembler,
    func: u32,
    arg_count: u8,
) -> Result<(), IcedError> {
    let context = asm.context;
    let area = call_area(&context);
    let strategy = state_base_strategy(&context);

    // RBX is callee-saved in every C ABI and free here: it holds the state
    // pointer while the segment base belongs to C code.
    emit_state_base(asm, rbx)?;
    for index in saved_xmms(&context) {
        asm.movq(
            qword_ptr(rbx + (area + CALL_AREA_XMM_SAVE + 8 * index as i32)),
            xmm_register(index),
        )?;
    }
    match asm.context.caller_segment {
        CallerSegment::None => {}
        CallerSegment::Xmm15 => asm.movq(rax, xmm15)?,
        CallerSegment::Arena(offset) => asm.mov(rax, qword_ptr(rbx + offset))?,
    }
    match strategy {
        StateBaseStrategy::Fs => asm.wrfsbase(rax)?,
        StateBaseStrategy::Gs => asm.wrgsbase(rax)?,
        StateBaseStrategy::R15 => {}
    }

    let (registers, shadow): (&[AsmRegister64], i32) = if cfg!(target_os = "windows") {
        (&[rcx, rdx, r8, r9], 32)
    } else {
        (&[rdi, rsi, rdx, rcx, r8, r9], 0)
    };
    let arg_count = usize::from(arg_count);
    let stack_args = arg_count.saturating_sub(registers.len()) as i32;
    // Generated code is entered with RSP = 8 (mod 16) and never moves it.
    let frame = ((shadow + 8 * stack_args + 15) & !15) + 8;
    asm.sub(rsp, frame)?;
    for slot in registers.len()..arg_count {
        let slot = slot as i32;
        asm.mov(rax, qword_ptr(rbx + (area + 8 * slot)))?;
        asm.mov(
            qword_ptr(rsp + (shadow + 8 * (slot - registers.len() as i32))),
            rax,
        )?;
    }
    for (slot, &register) in registers.iter().enumerate().take(arg_count) {
        asm.mov(register, qword_ptr(rbx + (area + 8 * slot as i32)))?;
    }
    asm.mov(
        rax,
        qword_ptr(rbx + celox_state_layout::STATE_HEADER_EXTERN_FUNCTIONS_ADDR_OFFSET as i32),
    )?;
    let offset = if func == celox_sir::float::FLOAT_HELPER_ID {
        -8
    } else {
        i32::try_from(u64::from(func) * 8).expect("extern function index")
    };
    asm.mov(rax, qword_ptr(rax + offset))?;
    asm.call(rax)?;
    asm.add(rsp, frame)?;
    asm.mov(qword_ptr(rbx + (area + CALL_AREA_RESULT)), rax)?;

    match strategy {
        StateBaseStrategy::Fs => asm.wrfsbase(rbx)?,
        StateBaseStrategy::Gs => asm.wrgsbase(rbx)?,
        StateBaseStrategy::R15 => {}
    }
    for index in saved_xmms(&context) {
        asm.movq(
            xmm_register(index),
            qword_ptr(rbx + (area + CALL_AREA_XMM_SAVE + 8 * index as i32)),
        )?;
    }
    Ok(())
}
