use super::*;

#[test]
fn traced_disassembly_labels_exact_basic_block_offsets() {
    let text =
        disassemble_with_block_offsets(&[0x90, 0xc3], 0x1000, &[(BlockId(9), 1), (BlockId(3), 0)]);

    assert_eq!(text, "bb3:\n  0x00001000  nop\nbb9:\n  0x00001001  ret\n");
    assert_eq!(
        disassemble(&[0x90, 0xc3], 0x1000),
        "  0x00001000  nop\n  0x00001001  ret\n"
    );
}

#[test]
fn emission_layout_pulls_a_backedge_chain_next_to_its_latch() {
    let mut vregs = VRegAllocator::new();
    let outer_condition = vregs.alloc();
    let loop_condition = vregs.alloc();
    let edge_value = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);

    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::Jump { target: BlockId(1) });
    let mut header = MBlock::new(BlockId(1));
    header.push(MInst::Branch {
        cond: outer_condition,
        true_bb: BlockId(2),
        false_bb: BlockId(3),
    });
    let mut latch = MBlock::new(BlockId(2));
    latch.push(MInst::Branch {
        cond: loop_condition,
        true_bb: BlockId(4),
        false_bb: BlockId(3),
    });
    let mut exit = MBlock::new(BlockId(3));
    exit.push(MInst::Return);
    let mut first_edge = MBlock::new(BlockId(4));
    first_edge.push(MInst::Jump { target: BlockId(5) });
    let mut second_edge = MBlock::new(BlockId(5));
    second_edge.push(MInst::Mov {
        dst: edge_value,
        src: outer_condition,
    });
    second_edge.push(MInst::Jump { target: BlockId(1) });
    func.blocks = vec![entry, header, latch, exit, first_edge, second_edge];

    let order = emission_block_order(&func)
        .into_iter()
        .map(|index| func.blocks[index].id)
        .collect::<Vec<_>>();

    assert_eq!(
        order,
        vec![
            BlockId(0),
            BlockId(1),
            BlockId(2),
            BlockId(4),
            BlockId(5),
            BlockId(3),
        ]
    );
}

#[test]
fn branch_inverts_when_true_successor_is_the_physical_fallthrough() {
    let mut vregs = VRegAllocator::new();
    let condition = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient()]);
    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm {
        dst: condition,
        value: 1,
    });
    entry.push(MInst::Branch {
        cond: condition,
        true_bb: BlockId(1),
        false_bb: BlockId(2),
    });
    let mut true_block = MBlock::new(BlockId(1));
    true_block.push(MInst::Return);
    let mut false_block = MBlock::new(BlockId(2));
    false_block.push(MInst::Return);
    func.blocks = vec![entry, true_block, false_block];

    let mut assignment = AssignmentMap::default();
    assignment.set(condition, PhysReg::RAX);
    let emitted = emit(&func, &assignment, 0).unwrap();
    let mut decoder = Decoder::new(64, &emitted.code, DecoderOptions::NONE);
    let mut conditional_branches = Vec::new();
    while decoder.can_decode() {
        let instruction = decoder.decode();
        if matches!(instruction.mnemonic(), Mnemonic::Je | Mnemonic::Jne) {
            conditional_branches.push(instruction.mnemonic());
        }
    }

    assert_eq!(conditional_branches, vec![Mnemonic::Je]);
}

#[test]
fn dense_jump_table_executes_every_masked_index() {
    let mut vregs = VRegAllocator::new();
    let loaded = vregs.alloc();
    let index = vregs.alloc();
    let table_base = vregs.alloc();
    let target = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 4]);

    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::Load {
        dst: loaded,
        base: BaseReg::SimState,
        offset: 0,
        size: OpSize::S8,
    });
    entry.push(MInst::AndImm32 {
        dst: index,
        src: loaded,
        imm: 3,
    });
    entry.push(MInst::Scratch { dst: table_base });
    entry.push(MInst::Scratch { dst: target });
    entry.push(MInst::JumpTable {
        index,
        table_base,
        target,
        targets: (1..=4).map(BlockId).collect(),
    });
    func.blocks.push(entry);
    for code in 1..=4 {
        let mut arm = MBlock::new(BlockId(code));
        arm.push(MInst::ReturnError {
            code: i64::from(code),
        });
        func.blocks.push(arm);
    }
    func.verify();

    let mut assignment = AssignmentMap::default();
    assignment.set(loaded, PhysReg::RAX);
    assignment.set(index, PhysReg::RCX);
    assignment.set(table_base, PhysReg::RDX);
    assignment.set(target, PhysReg::RBX);
    let emitted = emit(&func, &assignment, 0).unwrap();
    let jit = JitCode::new(&emitted.code).unwrap();
    for index in 0..4u8 {
        let mut state = [0xfcu8 | index];
        assert_eq!(unsafe { jit.call(&mut state) }, i64::from(index) + 1);
    }
}

#[test]
fn memory_branch_predicate_compares_in_place_for_every_width() {
    for size in [OpSize::S8, OpSize::S16, OpSize::S32, OpSize::S64] {
        let mut func = MFunction::new(VRegAllocator::new(), Vec::new());
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::BranchPred {
            predicate: BranchPredicate::MemoryNonZero {
                base: BaseReg::SimState,
                offset: 8,
                size,
            },
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });
        let mut true_block = MBlock::new(BlockId(1));
        true_block.push(MInst::ReturnError { code: 7 });
        let mut false_block = MBlock::new(BlockId(2));
        false_block.push(MInst::ReturnError { code: 9 });
        func.blocks = vec![entry, true_block, false_block];
        func.verify();

        let emitted = emit(&func, &AssignmentMap::default(), 0).unwrap();
        let mut decoder = Decoder::new(64, &emitted.code, DecoderOptions::NONE);
        let mut mnemonics = Vec::new();
        while decoder.can_decode() {
            mnemonics.push(decoder.decode().mnemonic());
        }
        assert!(
            mnemonics.contains(&Mnemonic::Cmp),
            "{size:?}: {mnemonics:?}"
        );
        assert!(
            !mnemonics.contains(&Mnemonic::Movzx),
            "{size:?}: {mnemonics:?}"
        );

        let jit = JitCode::new(&emitted.code).unwrap();
        let mut state = [0u8; 16];
        assert_eq!(unsafe { jit.call(&mut state) }, 9);
        state[8] = 1;
        assert_eq!(unsafe { jit.call(&mut state) }, 7);
    }
}

#[test]
fn materialized_compare_branch_preserves_condition_used_after_the_branch() {
    let mut vregs = VRegAllocator::new();
    let zero = vregs.alloc();
    let alternative = vregs.alloc();
    let condition = vregs.alloc();
    let merged = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 4]);

    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm {
        dst: zero,
        value: 0,
    });
    entry.push(MInst::LoadImm {
        dst: alternative,
        value: 7,
    });
    entry.push(MInst::CmpImm {
        dst: condition,
        lhs: zero,
        imm: 0,
        kind: CmpKind::Eq,
    });
    entry.push(MInst::Branch {
        cond: condition,
        true_bb: BlockId(1),
        false_bb: BlockId(2),
    });

    let mut true_block = MBlock::new(BlockId(1));
    true_block.push(MInst::Jump { target: BlockId(3) });
    let mut false_block = MBlock::new(BlockId(2));
    false_block.push(MInst::Jump { target: BlockId(3) });

    let mut join = MBlock::new(BlockId(3));
    join.phis.push(PhiNode {
        dst: merged,
        sources: vec![(BlockId(1), condition), (BlockId(2), alternative)],
    });
    join.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 0,
        src: merged,
        size: OpSize::S64,
    });
    join.push(MInst::Return);
    func.blocks = vec![entry, true_block, false_block, join];
    func.verify();

    let allocation = regalloc::run_regalloc(&mut func).unwrap();
    let emitted = emit(&func, &allocation.assignment, allocation.spill_frame_size).unwrap();
    let jit = JitCode::new(&emitted.code).unwrap();
    let mut state = [0u8; 8];

    assert_eq!(unsafe { jit.call(&mut state) }, 0);
    assert_eq!(u64::from_le_bytes(state), 1);
}
