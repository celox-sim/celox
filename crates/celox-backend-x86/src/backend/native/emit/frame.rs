//! Function prologue and epilogue: saved registers, the state base and the
//! tick-loop count.

use super::*;

/// Emit the prologue: save the registers the function must preserve, install
/// the state base and, in a tick loop, load the tick count.
pub(super) fn emit_prologue(
    asm: &mut EmitAssembler,
    arena: &NativeArenaLayout,
    context: EmissionContext,
    state_base: StateBaseStrategy,
    stack_arena: i32,
    tick_loop: bool,
    check_runtime_events: bool,
) -> Result<(), IcedError> {
    if let Some(save_offset) = arena.loop_xmm15_save {
        asm.movdqu(xmmword_ptr(rdi + save_offset), xmm15)?;
    }
    // The GPR allocator does not own vector registers. Preserve the used
    // callee-saved GPRs outside that file. GS mode additionally preserves
    // the caller's segment base; fallback mode borrows the saved R15.
    match (tick_loop, state_base) {
        (true, StateBaseStrategy::Fs) => {
            asm.rdfsbase(rax)?;
            asm.mov(
                qword_ptr(rdi + arena.loop_segment_save.expect("loop segment save")),
                rax,
            )?;
        }
        (true, StateBaseStrategy::Gs) => {
            asm.rdgsbase(rax)?;
            asm.mov(
                qword_ptr(rdi + arena.loop_segment_save.expect("loop segment save")),
                rax,
            )?;
        }
        (true, StateBaseStrategy::R15) => {}
        (false, StateBaseStrategy::Fs) => {
            asm.rdfsbase(rax)?;
            asm.movq(xmm15, rax)?;
        }
        (false, StateBaseStrategy::Gs) => {
            asm.rdgsbase(rax)?;
            asm.movq(xmm15, rax)?;
        }
        (false, StateBaseStrategy::R15) => {}
    }
    if let Some(base) = arena.loop_gpr_save_base {
        for (index, &reg) in arena.callee_saved.iter().enumerate() {
            asm.mov(
                qword_ptr(rdi + base + i32::try_from(index * 8).unwrap()),
                preg_to_reg64(reg),
            )?;
        }
    } else {
        for (index, &reg) in arena.callee_saved.iter().enumerate() {
            asm.movq(saved_gpr_xmm(index), preg_to_reg64(reg))?;
        }
    }
    match state_base {
        StateBaseStrategy::Fs => asm.wrfsbase(rdi)?,
        StateBaseStrategy::Gs => asm.wrgsbase(rdi)?,
        StateBaseStrategy::R15 => asm.mov(r15, rdi)?,
    }
    if let Some((_, register)) = context.frame {
        // A function unit takes its frame's address after the state
        // pointer. RSP is a multiple of 16 below the arena.
        asm.mov(register, rsi)?;
        asm.sub(rsp, stack_arena)?;
    }
    if tick_loop {
        let mut count_ready = asm.create_label();
        asm.mov(
            rax,
            qword_ptr(mem_operand(
                &context,
                BaseReg::SimState,
                STATE_HEADER_NATIVE_LOOP_REMAINING_OFFSET as i32,
            )),
        )?;
        asm.test(rax, rax)?;
        asm.jne(count_ready)?;
        asm.mov(eax, 1u32)?;
        asm.set_label(&mut count_ready)?;
        // XMM15 is invisible to GPR allocation. Its low qword carries the
        // tick count.
        asm.movq(xmm15, rax)?;
        // Keep the internal count-ready label distinct from the first MIR
        // block label when runtime-event initialization is absent.
        asm.nop()?;

        if check_runtime_events {
            asm.mov(
                rax,
                qword_ptr(mem_operand(
                    &context,
                    BaseReg::SimState,
                    STATE_HEADER_RUNTIME_EVENT_ADDR_OFFSET as i32,
                )),
            )?;
            asm.mov(rax, qword_ptr(rax))?;
            asm.mov(
                qword_ptr(mem_operand(
                    &context,
                    BaseReg::SimState,
                    STATE_HEADER_NATIVE_LOOP_EVENT_SEQ_OFFSET as i32,
                )),
                rax,
            )?;
        }
    }
    Ok(())
}

/// Emit the epilogue: bind the tick-loop success exit and the common exit,
/// restore what the prologue saved, and return.
pub(super) fn emit_epilogue(
    asm: &mut EmitAssembler,
    arena: &NativeArenaLayout,
    context: EmissionContext,
    state_base: StateBaseStrategy,
    stack_arena: i32,
    tick_loop: bool,
    tick_loop_success_label: &mut Option<CodeLabel>,
    epilogue_label: &mut CodeLabel,
) -> Result<(), IcedError> {
    if let Some(label) = tick_loop_success_label {
        asm.set_label(label)?;
        asm.xor(eax, eax)?;
    }
    asm.set_label(epilogue_label)?;
    if tick_loop {
        asm.movq(r10, xmm15)?;
        asm.mov(
            qword_ptr(mem_operand(
                &context,
                BaseReg::SimState,
                STATE_HEADER_NATIVE_LOOP_REMAINING_OFFSET as i32,
            )),
            r10,
        )?;
    }
    if let Some(save_offset) = arena.loop_xmm15_save {
        asm.movdqu(
            xmm15,
            xmmword_ptr(mem_operand(&context, BaseReg::SimState, save_offset)),
        )?;
    }
    if let Some(base) = arena.loop_gpr_save_base {
        for (index, &reg) in arena.callee_saved.iter().enumerate() {
            asm.mov(
                preg_to_reg64(reg),
                qword_ptr(mem_operand(
                    &context,
                    BaseReg::SimState,
                    base + i32::try_from(index * 8).unwrap(),
                )),
            )?;
        }
    } else {
        for (index, &reg) in arena.callee_saved.iter().enumerate().rev() {
            asm.movq(preg_to_reg64(reg), saved_gpr_xmm(index))?;
        }
    }
    match (tick_loop, state_base) {
        (true, StateBaseStrategy::Fs) => {
            asm.mov(
                r11,
                qword_ptr(mem_operand(
                    &context,
                    BaseReg::SimState,
                    arena.loop_segment_save.expect("loop segment save"),
                )),
            )?;
            asm.wrfsbase(r11)?;
        }
        (true, StateBaseStrategy::Gs) => {
            asm.mov(
                r11,
                qword_ptr(mem_operand(
                    &context,
                    BaseReg::SimState,
                    arena.loop_segment_save.expect("loop segment save"),
                )),
            )?;
            asm.wrgsbase(r11)?;
        }
        (true, StateBaseStrategy::R15) => {}
        (false, StateBaseStrategy::Fs) => {
            asm.movq(r11, xmm15)?;
            asm.wrfsbase(r11)?;
        }
        (false, StateBaseStrategy::Gs) => {
            asm.movq(r11, xmm15)?;
            asm.wrgsbase(r11)?;
        }
        (false, StateBaseStrategy::R15) => {}
    }
    if context.frame.is_some() {
        asm.add(rsp, stack_arena)?;
    }
    asm.ret()?;
    Ok(())
}
