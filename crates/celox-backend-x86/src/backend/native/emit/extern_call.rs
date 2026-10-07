//! Calls of extern functions through the platform C ABI.
//!
//! Generated code keeps no stack frame and owns the vector registers and, in
//! segment-base mode, the FS or GS base. A call therefore saves every XMM
//! register, hands the caller's segment base back to C code (which may use it
//! for thread-local storage), aligns the stack, and restores all of this
//! afterwards. The allocator has already moved every live GPR value to its
//! home, because `CallExtern` clobbers all allocatable registers.

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

thread_local! {
    static ACTIVE_CALL_AREA: Cell<Option<i32>> = const { Cell::new(None) };
    static ACTIVE_CALLER_SEGMENT: Cell<CallerSegment> = const { Cell::new(CallerSegment::None) };
}

pub(super) fn set_active_call_area(call_area: Option<i32>, caller_segment: CallerSegment) {
    ACTIVE_CALL_AREA.with(|area| area.set(call_area));
    ACTIVE_CALLER_SEGMENT.with(|segment| segment.set(caller_segment));
}

fn call_area() -> i32 {
    ACTIVE_CALL_AREA
        .with(Cell::get)
        .expect("a function with extern calls has an extern call area")
}

fn xmm_register(index: usize) -> AsmRegisterXmm {
    [
        xmm0, xmm1, xmm2, xmm3, xmm4, xmm5, xmm6, xmm7, xmm8, xmm9, xmm10, xmm11, xmm12, xmm13,
        xmm14, xmm15,
    ][index]
}

/// `[state + call area + 8 * slot]` for an argument or the result.
fn call_slot(slot: i32) -> AsmMemoryOperand {
    mem_operand(BaseReg::SimState, call_area() + 8 * slot)
}

pub(super) fn emit_extern_arg(
    asm: &mut CodeAssembler,
    index: u8,
    src: AsmRegister64,
) -> Result<(), IcedError> {
    asm.mov(qword_ptr(call_slot(i32::from(index))), src)
}

pub(super) fn emit_extern_result(
    asm: &mut CodeAssembler,
    dst: AsmRegister64,
) -> Result<(), IcedError> {
    asm.mov(dst, qword_ptr(call_slot(CALL_AREA_RESULT / 8)))
}

pub(super) fn emit_call_extern(
    asm: &mut CodeAssembler,
    func: u32,
    arg_count: u8,
) -> Result<(), IcedError> {
    let area = call_area();
    let strategy = state_base_strategy();

    // RBX is callee-saved in every C ABI and free here: it holds the state
    // pointer while the segment base belongs to C code.
    emit_state_base(asm, rbx)?;
    for index in 0..16 {
        asm.movdqu(
            xmmword_ptr(rbx + (area + CALL_AREA_XMM_SAVE + 16 * index as i32)),
            xmm_register(index),
        )?;
    }
    match ACTIVE_CALLER_SEGMENT.with(Cell::get) {
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
    asm.mov(
        rax,
        qword_ptr(rax + i32::try_from(u64::from(func) * 8).expect("extern function index")),
    )?;
    asm.call(rax)?;
    asm.add(rsp, frame)?;
    asm.mov(qword_ptr(rbx + (area + CALL_AREA_RESULT)), rax)?;

    match strategy {
        StateBaseStrategy::Fs => asm.wrfsbase(rbx)?,
        StateBaseStrategy::Gs => asm.wrgsbase(rbx)?,
        StateBaseStrategy::R15 => {}
    }
    for index in 0..16 {
        asm.movdqu(
            xmm_register(index),
            xmmword_ptr(rbx + (area + CALL_AREA_XMM_SAVE + 16 * index as i32)),
        )?;
    }
    Ok(())
}
