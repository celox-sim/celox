use super::*;

#[test]
fn native_tick_loop_reenters_the_allocated_body_without_reentering_the_abi_boundary() {
    let mut vregs = VRegAllocator::new();
    let current = vregs.alloc();
    let one = vregs.alloc();
    let next = vregs.alloc();
    let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Load {
        dst: current,
        base: BaseReg::SimState,
        offset: 64,
        size: OpSize::S64,
    });
    block.push(MInst::LoadImm { dst: one, value: 1 });
    block.push(MInst::Add {
        dst: next,
        lhs: current,
        rhs: one,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 64,
        src: next,
        size: OpSize::S64,
    });
    block.push(MInst::Return);
    function.push_block(block);

    let allocation = regalloc::run_regalloc(&mut function).unwrap();
    let plan = SsaDestructionPlan::build(&function, &allocation.assignment).unwrap();
    let emitted = emit_planned(
        &function,
        &allocation.assignment,
        allocation.spill_frame_size,
        4096,
        &plan,
        true,
        false,
    )
    .unwrap();
    let mut decoder = Decoder::new(64, &emitted.code, DecoderOptions::NONE);
    while decoder.can_decode() {
        let instruction = decoder.decode();
        assert_ne!(
            instruction.memory_base(),
            Register::RSP,
            "native tick loop must not address through RSP: {instruction}"
        );
        for operand in 0..instruction.op_count() {
            assert_ne!(
                instruction.op_register(operand),
                Register::RSP,
                "native tick loop must not borrow RSP: {instruction}"
            );
        }
    }
    let jit = JitCode::new(&emitted.code).unwrap();
    let mut memory = vec![0u64; emitted.required_state_size as usize / 8 + 1];
    let event_sequence = 0u64;
    memory[STATE_HEADER_RUNTIME_EVENT_ADDR_OFFSET / 8] = (&event_sequence as *const u64) as u64;
    memory[STATE_HEADER_NATIVE_LOOP_REMAINING_OFFSET / 8] = 5;

    let result = unsafe { (jit.fn_ptr)(memory.as_mut_ptr().cast()) };

    assert_eq!(result, 0);
    assert_eq!(memory[64 / 8], 5);
    assert_eq!(memory[STATE_HEADER_NATIVE_LOOP_REMAINING_OFFSET / 8], 0);
}

#[test]
fn repeated_qword_spill_accesses_use_an_unallocated_xmm_register() {
    let mut vregs = VRegAllocator::new();
    let seed = vregs.alloc();
    let first_load = vregs.alloc();
    let first_increment = vregs.alloc();
    let second_load = vregs.alloc();
    let second_increment = vregs.alloc();
    let final_load = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 6]);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::LoadImm {
        dst: seed,
        value: 7,
    });
    block.push(MInst::Store {
        base: BaseReg::StackFrame,
        offset: 0,
        src: seed,
        size: OpSize::S64,
    });
    block.push(MInst::Load {
        dst: first_load,
        base: BaseReg::StackFrame,
        offset: 0,
        size: OpSize::S64,
    });
    block.push(MInst::AddImm {
        dst: first_increment,
        src: first_load,
        imm: 1,
    });
    block.push(MInst::Store {
        base: BaseReg::StackFrame,
        offset: 0,
        src: first_increment,
        size: OpSize::S64,
    });
    block.push(MInst::Load {
        dst: second_load,
        base: BaseReg::StackFrame,
        offset: 0,
        size: OpSize::S64,
    });
    block.push(MInst::AddImm {
        dst: second_increment,
        src: second_load,
        imm: 1,
    });
    block.push(MInst::Store {
        base: BaseReg::StackFrame,
        offset: 0,
        src: second_increment,
        size: OpSize::S64,
    });
    block.push(MInst::Load {
        dst: final_load,
        base: BaseReg::StackFrame,
        offset: 0,
        size: OpSize::S64,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 0,
        src: final_load,
        size: OpSize::S64,
    });
    block.push(MInst::Return);
    func.blocks.push(block);
    func.verify();

    let mut assignment = AssignmentMap::default();
    for (value, register) in [
        (seed, PhysReg::RAX),
        (first_load, PhysReg::RBX),
        (first_increment, PhysReg::RCX),
        (second_load, PhysReg::RDX),
        (second_increment, PhysReg::RSI),
        (final_load, PhysReg::R8),
    ] {
        assignment.set(value, register);
    }

    let emitted = emit(&func, &assignment, 8).unwrap();
    let mut decoder = Decoder::new(64, &emitted.code, DecoderOptions::NONE);
    let mut uses_xmm_cache = false;
    while decoder.can_decode() {
        let instruction = decoder.decode();
        uses_xmm_cache |= [instruction.op0_register(), instruction.op1_register()]
            .into_iter()
            .any(|register| {
                (Register::XMM0 as u32..=Register::XMM14 as u32).contains(&(register as u32))
            });
    }
    assert!(uses_xmm_cache);

    let jit = JitCode::new(&emitted.code).unwrap();
    let mut arena = [0u8; 128];
    assert_eq!(unsafe { jit.call(&mut arena) }, 0);
    assert_eq!(u64::from_le_bytes(arena[..8].try_into().unwrap()), 9);
}

#[test]
fn native_tick_loop_uses_all_nonconflicting_xmm_spill_cache_registers() {
    let mut vregs = VRegAllocator::new();
    let value = vregs.alloc();
    let mut function = MFunction::new(vregs, vec![SpillDesc::transient()]);
    let mut block = MBlock::new(BlockId(0));
    for offset in (0..9).map(|slot| slot * 8) {
        for _ in 0..2 {
            block.push(MInst::Load {
                dst: value,
                base: BaseReg::StackFrame,
                offset,
                size: OpSize::S64,
            });
            block.push(MInst::Store {
                base: BaseReg::StackFrame,
                offset,
                src: value,
                size: OpSize::S64,
            });
        }
    }
    block.push(MInst::Return);
    function.push_block(block);
    let plan = SsaDestructionPlan::default();
    let assignment = AssignmentMap::default();

    let standalone = select_spill_register_cache(&function, &plan, &assignment, false);
    let tick_loop = select_spill_register_cache(&function, &plan, &assignment, true);

    for offset in (0..9).map(|slot| slot * 8) {
        assert!(standalone.register(offset).is_some());
    }
    for offset in (0..9).map(|slot| slot * 8) {
        assert!(tick_loop.register(offset).is_some());
    }
}

#[test]
fn fsgsbase_target_uses_gs_state_addressing_without_reserving_r15() {
    let mut vregs = VRegAllocator::new();
    let value = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient()]);
    func.target_features = X86Features::for_test_with_state_base(false, StateBaseStrategy::Gs);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Load {
        dst: value,
        base: BaseReg::SimState,
        offset: 0,
        size: OpSize::S64,
    });
    block.push(MInst::Return);
    func.push_block(block);
    let mut assignment = AssignmentMap::default();
    assignment.set(value, PhysReg::R15);

    let emitted = emit(&func, &assignment, 0).unwrap();
    let mut decoder = Decoder::new(64, &emitted.code[..emitted.text_size], DecoderOptions::NONE);
    let mut instructions = Vec::new();
    while decoder.can_decode() {
        instructions.push(decoder.decode());
    }

    assert!(
        instructions
            .iter()
            .any(|inst| inst.mnemonic() == Mnemonic::Rdgsbase)
    );
    assert!(instructions.iter().any(|inst| {
        inst.segment_prefix() == Register::GS && inst.memory_base() == Register::None
    }));
    assert!(
        instructions
            .iter()
            .any(|inst| inst.mnemonic() == Mnemonic::Wrgsbase)
    );
}
