use super::*;

fn execute_variable_shift_boundaries(use_bmi2: bool) {
    let mut vregs = VRegAllocator::new();
    let lhs = vregs.alloc();
    let count = vregs.alloc();
    let shl = vregs.alloc();
    let shr = vregs.alloc();
    let sar = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 5]);
    func.target_features = X86Features::for_test(use_bmi2);

    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Load {
        dst: lhs,
        base: BaseReg::SimState,
        offset: 0,
        size: OpSize::S64,
    });
    block.push(MInst::Load {
        dst: count,
        base: BaseReg::SimState,
        offset: 8,
        size: OpSize::S64,
    });
    block.push(MInst::Shl {
        dst: shl,
        lhs,
        rhs: count,
    });
    block.push(MInst::Shr {
        dst: shr,
        lhs,
        rhs: count,
    });
    block.push(MInst::Sar {
        dst: sar,
        lhs,
        rhs: count,
    });
    for (offset, src) in [(16, shl), (24, shr), (32, sar)] {
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset,
            src,
            size: OpSize::S64,
        });
    }
    block.push(MInst::Return);
    func.push_block(block);

    mir_legalize::legalize(&mut func);
    mir_opt::optimize(&mut func);
    assert_eq!(
        func.blocks
            .iter()
            .flat_map(|block| &block.insts)
            .filter(|inst| matches!(inst, MInst::CmpImmSelect { imm: 64, .. }))
            .count(),
        3
    );
    let allocation = regalloc::run_regalloc(&mut func).unwrap();
    let emitted = emit(&func, &allocation.assignment, allocation.spill_frame_size).unwrap();
    assert_eq!(
        emitted.required_image_features & IMAGE_FEATURE_BMI2,
        if use_bmi2 { IMAGE_FEATURE_BMI2 } else { 0 }
    );
    let jit = JitCode::new(&emitted.code).unwrap();
    let lhs_value = 0x8000_0000_0000_0001u64;

    for count_value in [63u64, 64, 65, 127, 128, 129] {
        let mut state = [0u8; 40];
        state[0..8].copy_from_slice(&lhs_value.to_le_bytes());
        state[8..16].copy_from_slice(&count_value.to_le_bytes());
        assert_eq!(unsafe { jit.call(&mut state) }, 0);

        let actual_shl = u64::from_le_bytes(state[16..24].try_into().unwrap());
        let actual_shr = u64::from_le_bytes(state[24..32].try_into().unwrap());
        let actual_sar = u64::from_le_bytes(state[32..40].try_into().unwrap());
        let expected_shl = if count_value >= 64 {
            0
        } else {
            lhs_value << count_value
        };
        let expected_shr = if count_value >= 64 {
            0
        } else {
            lhs_value >> count_value
        };
        let expected_sar = if count_value >= 64 {
            u64::MAX
        } else {
            ((lhs_value as i64) >> count_value) as u64
        };
        assert_eq!(actual_shl, expected_shl, "shl count={count_value}");
        assert_eq!(actual_shr, expected_shr, "shr count={count_value}");
        assert_eq!(actual_sar, expected_sar, "sar count={count_value}");
    }
}

fn decode_shift(
    op: ShiftOp,
    encoding: VariableShiftEncoding,
    dst: PhysReg,
    lhs: PhysReg,
    rhs: PhysReg,
) -> Vec<Instruction> {
    let mut assignment = AssignmentMap::default();
    assignment.set(VReg(0), dst);
    assignment.set(VReg(1), lhs);
    assignment.set(VReg(2), rhs);
    let mut asm = CodeAssembler::new(64).unwrap();
    emit_shift(
        &mut asm,
        &assignment,
        VReg(0),
        VReg(1),
        VReg(2),
        op,
        encoding,
    )
    .unwrap();
    let code = asm.assemble(0).unwrap();
    let mut decoder = Decoder::new(64, &code, DecoderOptions::NONE);
    let mut instructions = Vec::new();
    while decoder.can_decode() {
        instructions.push(decoder.decode());
    }
    instructions
}

#[test]
fn bmi2_shifts_use_three_arbitrary_register_operands() {
    for (op, mnemonic) in [
        (ShiftOp::Shr, Mnemonic::Shrx),
        (ShiftOp::Shl, Mnemonic::Shlx),
        (ShiftOp::Sar, Mnemonic::Sarx),
    ] {
        let instructions = decode_shift(
            op,
            VariableShiftEncoding::Bmi2,
            PhysReg::R8,
            PhysReg::R9,
            PhysReg::R10,
        );
        assert_eq!(instructions.len(), 1, "{instructions:?}");
        assert_eq!(instructions[0].mnemonic(), mnemonic);
        assert_eq!(instructions[0].op0_register(), Register::R8);
        assert_eq!(instructions[0].op1_register(), Register::R9);
        assert_eq!(instructions[0].op2_register(), Register::R10);
    }
}

#[test]
fn legacy_shift_uses_cl_after_copying_the_lhs() {
    let instructions = decode_shift(
        ShiftOp::Shl,
        VariableShiftEncoding::LegacyCl,
        PhysReg::R8,
        PhysReg::R9,
        PhysReg::RCX,
    );

    assert_eq!(
        instructions
            .iter()
            .map(Instruction::mnemonic)
            .collect::<Vec<_>>(),
        vec![Mnemonic::Mov, Mnemonic::Shl]
    );
    assert_eq!(instructions[1].op0_register(), Register::R8);
    assert_eq!(instructions[1].op1_register(), Register::CL);
}

#[test]
fn legacy_shift_with_rcx_destination_uses_an_r15_arena_copy() {
    let instructions = decode_shift(
        ShiftOp::Shl,
        VariableShiftEncoding::LegacyCl,
        PhysReg::RCX,
        PhysReg::R8,
        PhysReg::RCX,
    );

    assert_eq!(
        instructions
            .iter()
            .map(Instruction::mnemonic)
            .collect::<Vec<_>>(),
        vec![Mnemonic::Mov, Mnemonic::Shl, Mnemonic::Mov]
    );
    assert_eq!(instructions[0].op1_register(), Register::R8);
    assert_eq!(instructions[1].memory_base(), Register::R15);
    assert_eq!(instructions[1].segment_prefix(), Register::None);
    assert_eq!(instructions[1].op1_register(), Register::CL);
    assert_eq!(instructions[2].op0_register(), Register::RCX);
}

#[test]
fn legacy_variable_shifts_do_not_wrap_large_counts() {
    execute_variable_shift_boundaries(false);
}

#[test]
fn bmi2_variable_shifts_do_not_wrap_large_counts() {
    if !std::is_x86_feature_detected!("bmi2") {
        return;
    }
    execute_variable_shift_boundaries(true);
}

#[test]
fn narrow_immediate_shifts_do_not_use_x86_count_masking() {
    let mut vregs = VRegAllocator::new();
    let src = vregs.alloc();
    let mut results = Vec::new();
    for imm in [31u8, 32, 33, 63] {
        results.push((imm, vregs.alloc(), vregs.alloc()));
    }
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 1 + results.len() * 2]);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Load {
        dst: src,
        base: BaseReg::SimState,
        offset: 0,
        size: OpSize::S32,
    });
    for (index, (imm, shr, shl)) in results.iter().copied().enumerate() {
        block.push(MInst::ShrImm { dst: shr, src, imm });
        block.push(MInst::ShlImm { dst: shl, src, imm });
        for (column, result) in [shr, shl].into_iter().enumerate() {
            block.push(MInst::Store {
                base: BaseReg::SimState,
                offset: (8 + (index * 16 + column * 8)) as i32,
                src: result,
                size: OpSize::S64,
            });
        }
    }
    block.push(MInst::Return);
    func.push_block(block);

    mir_legalize::legalize(&mut func);
    mir_opt::optimize(&mut func);
    let allocation = regalloc::run_regalloc(&mut func).unwrap();
    let emitted = emit(&func, &allocation.assignment, allocation.spill_frame_size).unwrap();
    let jit = JitCode::new(&emitted.code).unwrap();
    let value = 9u64;
    let mut state = [0u8; 72];
    state[0..8].copy_from_slice(&value.to_le_bytes());
    assert_eq!(unsafe { jit.call(&mut state) }, 0);

    for (index, (imm, _, _)) in results.iter().copied().enumerate() {
        let shr_offset = 8 + index * 16;
        let shl_offset = shr_offset + 8;
        let actual_shr = u64::from_le_bytes(state[shr_offset..shr_offset + 8].try_into().unwrap());
        let actual_shl = u64::from_le_bytes(state[shl_offset..shl_offset + 8].try_into().unwrap());
        assert_eq!(actual_shr, value >> imm, "shr immediate {imm}");
        assert_eq!(actual_shl, value << imm, "shl immediate {imm}");
    }
}

#[test]
fn legacy_rcx_destination_executes_without_clobbering_live_lhs() {
    let mut vregs = VRegAllocator::new();
    let lhs = vregs.alloc();
    let count = vregs.alloc();
    let result = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);
    func.target_features = X86Features::for_test(false);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::LoadImm { dst: lhs, value: 5 });
    block.push(MInst::LoadImm {
        dst: count,
        value: 3,
    });
    block.push(MInst::Shl {
        dst: result,
        lhs,
        rhs: count,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 0,
        src: result,
        size: OpSize::S64,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 8,
        src: lhs,
        size: OpSize::S64,
    });
    block.push(MInst::Return);
    func.push_block(block);

    let mut assignment = AssignmentMap::default();
    assignment.set(lhs, PhysReg::R8);
    assignment.set(count, PhysReg::RCX);
    assignment.set(result, PhysReg::RCX);
    let emitted = emit(&func, &assignment, 0).unwrap();
    let jit = JitCode::new(&emitted.code).unwrap();
    let mut state = [0u8; 16];

    assert_eq!(unsafe { jit.call(&mut state) }, 0);
    assert_eq!(u64::from_le_bytes(state[0..8].try_into().unwrap()), 40);
    assert_eq!(u64::from_le_bytes(state[8..16].try_into().unwrap()), 5);
}
