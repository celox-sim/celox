//! Physical register mapping and memory operand addressing.

use super::*;

// ────────────────────────────────────────────────────────────────
// PhysReg → iced-x86 register mapping
// ────────────────────────────────────────────────────────────────

pub(super) fn preg_to_reg64(preg: PhysReg) -> AsmRegister64 {
    match preg {
        PhysReg::RAX => rax,
        PhysReg::RCX => rcx,
        PhysReg::RDX => rdx,
        PhysReg::RBX => rbx,
        PhysReg::RBP => rbp,
        PhysReg::RSI => rsi,
        PhysReg::RDI => rdi,
        PhysReg::R8 => r8,
        PhysReg::R9 => r9,
        PhysReg::R10 => r10,
        PhysReg::R11 => r11,
        PhysReg::R12 => r12,
        PhysReg::R13 => r13,
        PhysReg::R14 => r14,
        PhysReg::R15 => r15,
    }
}

pub(super) fn x86_vec_to_xmm(register: X86PhysVec) -> AsmRegisterXmm {
    match register.0 {
        0 => xmm0,
        1 => xmm1,
        2 => xmm2,
        3 => xmm3,
        4 => xmm4,
        5 => xmm5,
        6 => xmm6,
        7 => xmm7,
        8 => xmm8,
        9 => xmm9,
        10 => xmm10,
        11 => xmm11,
        12 => xmm12,
        13 => xmm13,
        14 => xmm14,
        15 => xmm15,
        other => panic!("invalid XMM register index {other}"),
    }
}

pub(super) fn preg_to_reg32(preg: PhysReg) -> AsmRegister32 {
    match preg {
        PhysReg::RAX => eax,
        PhysReg::RCX => ecx,
        PhysReg::RDX => edx,
        PhysReg::RBX => ebx,
        PhysReg::RBP => ebp,
        PhysReg::RSI => esi,
        PhysReg::RDI => edi,
        PhysReg::R8 => r8d,
        PhysReg::R9 => r9d,
        PhysReg::R10 => r10d,
        PhysReg::R11 => r11d,
        PhysReg::R12 => r12d,
        PhysReg::R13 => r13d,
        PhysReg::R14 => r14d,
        PhysReg::R15 => r15d,
    }
}

pub(super) fn preg_to_reg16(preg: PhysReg) -> AsmRegister16 {
    match preg {
        PhysReg::RAX => ax,
        PhysReg::RCX => cx,
        PhysReg::RDX => dx,
        PhysReg::RBX => bx,
        PhysReg::RBP => bp,
        PhysReg::RSI => si,
        PhysReg::RDI => di,
        PhysReg::R8 => r8w,
        PhysReg::R9 => r9w,
        PhysReg::R10 => r10w,
        PhysReg::R11 => r11w,
        PhysReg::R12 => r12w,
        PhysReg::R13 => r13w,
        PhysReg::R14 => r14w,
        PhysReg::R15 => r15w,
    }
}

pub(super) fn preg_to_reg8(preg: PhysReg) -> AsmRegister8 {
    match preg {
        PhysReg::RAX => al,
        PhysReg::RCX => cl,
        PhysReg::RDX => dl,
        PhysReg::RBX => bl,
        PhysReg::RBP => bpl,
        PhysReg::RSI => sil,
        PhysReg::RDI => dil,
        PhysReg::R8 => r8b,
        PhysReg::R9 => r9b,
        PhysReg::R10 => r10b,
        PhysReg::R11 => r11b,
        PhysReg::R12 => r12b,
        PhysReg::R13 => r13b,
        PhysReg::R14 => r14b,
        PhysReg::R15 => r15b,
    }
}

// ────────────────────────────────────────────────────────────────
// Helper: resolve VReg to physical register
// ────────────────────────────────────────────────────────────────

pub(super) fn resolve(assignment: &AssignmentMap, vreg: VReg) -> PhysReg {
    assignment
        .get(vreg)
        .unwrap_or_else(|| panic!("VReg {vreg} has no physical register assignment"))
}

fn state_base_strategy() -> StateBaseStrategy {
    ACTIVE_STATE_BASE.with(Cell::get)
}

fn physical_offset(base: BaseReg, offset: i32) -> i32 {
    match base {
        BaseReg::SimState => offset,
        BaseReg::StackFrame => ACTIVE_SPILL_BASE.with(|base| {
            base.get()
                .checked_add(offset)
                .expect("verified stack offset fits native arena displacement")
        }),
    }
}

pub(super) fn mem_operand(base: BaseReg, offset: i32) -> AsmMemoryOperand {
    let offset = physical_offset(base, offset);
    match state_base_strategy() {
        StateBaseStrategy::Fs => ptr(offset).fs(),
        StateBaseStrategy::Gs => ptr(offset).gs(),
        StateBaseStrategy::R15 => r15 + offset,
    }
}

fn scratch_offset(slot: usize) -> i32 {
    ACTIVE_SCRATCH_BASE.with(|base| {
        base.get()
            .checked_add(i32::try_from(slot * 8).expect("scratch slot offset"))
            .expect("scratch slot displacement")
    })
}

pub(super) fn scratch_operand(slot: usize) -> AsmMemoryOperand {
    let offset = scratch_offset(slot);
    match state_base_strategy() {
        StateBaseStrategy::Fs => ptr(offset).fs(),
        StateBaseStrategy::Gs => ptr(offset).gs(),
        StateBaseStrategy::R15 => r15 + offset,
    }
}

pub(super) fn mem_operand_indexed(
    base: BaseReg,
    offset: i32,
    index: AsmRegister64,
    scale: u8,
) -> AsmMemoryOperand {
    let offset = physical_offset(base, offset);
    match (state_base_strategy(), scale) {
        (StateBaseStrategy::Fs, 1) => ptr(index + offset).fs(),
        (StateBaseStrategy::Fs, 2) => ptr(index * 2 + offset).fs(),
        (StateBaseStrategy::Fs, 4) => ptr(index * 4 + offset).fs(),
        (StateBaseStrategy::Fs, 8) => ptr(index * 8 + offset).fs(),
        (StateBaseStrategy::Gs, 1) => ptr(index + offset).gs(),
        (StateBaseStrategy::Gs, 2) => ptr(index * 2 + offset).gs(),
        (StateBaseStrategy::Gs, 4) => ptr(index * 4 + offset).gs(),
        (StateBaseStrategy::Gs, 8) => ptr(index * 8 + offset).gs(),
        (StateBaseStrategy::R15, 1) => r15 + index + offset,
        (StateBaseStrategy::R15, 2) => r15 + index * 2 + offset,
        (StateBaseStrategy::R15, 4) => r15 + index * 4 + offset,
        (StateBaseStrategy::R15, 8) => r15 + index * 8 + offset,
        _ => unreachable!("invalid indexed-memory scale {scale}"),
    }
}

pub(super) fn emit_state_base(
    asm: &mut CodeAssembler,
    destination: AsmRegister64,
) -> Result<(), IcedError> {
    match state_base_strategy() {
        StateBaseStrategy::Fs => asm.rdfsbase(destination),
        StateBaseStrategy::Gs => asm.rdgsbase(destination),
        StateBaseStrategy::R15 => asm.mov(destination, r15),
    }
}

pub(super) fn mem_operand_ptr(ptr: AsmRegister64, offset: i32) -> AsmMemoryOperand {
    ptr + offset
}

pub(super) fn mem_operand_ptr_indexed(
    ptr: AsmRegister64,
    offset: i32,
    index: AsmRegister64,
) -> AsmMemoryOperand {
    ptr + index + offset
}
