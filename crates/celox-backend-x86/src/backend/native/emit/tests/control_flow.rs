use super::*;

#[test]
fn error_returns_preserve_the_full_signed_status() {
    for code in [
        1,
        2000,
        -1,
        -3,
        -4294967296,
        i64::MIN,
        i64::from(u32::MAX) + 1,
    ] {
        let mut func = MFunction::new(VRegAllocator::new(), Vec::new());
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::ReturnError { code });
        func.blocks.push(entry);
        let emitted = emit(&func, &AssignmentMap::default(), 0).unwrap();
        let jit = JitCode::new(&emitted.code).unwrap();
        assert_eq!(unsafe { jit.call(&mut [0u8; 8]) }, code);
    }
}

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

#[test]
fn settle_guard_layout_clusters_clean_paths_and_preserves_dirty_paths() {
    // The same diamond shape appears when skipping a clean partition and
    // when skipping publication by an unchanged writer.
    for publication in [false, true] {
        let mut vregs = VRegAllocator::new();
        let a = vregs.alloc();
        let b = vregs.alloc();
        let value = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);
        func.settle_flags = Some(if publication { 8..24 } else { 0..2 });
        let mut first = MBlock::new(BlockId(0));
        first.push(MInst::Load {
            dst: a,
            base: BaseReg::SimState,
            offset: 0,
            size: OpSize::S8,
        });
        first.push(MInst::LoadImm {
            dst: value,
            value: 42,
        });
        first.push(MInst::Branch {
            cond: a,
            true_bb: BlockId(2),
            false_bb: BlockId(1),
        });
        let mut first_work = MBlock::new(BlockId(2));
        first_work.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 8,
            src: value,
            size: OpSize::S64,
        });
        first_work.push(MInst::Jump { target: BlockId(1) });
        let mut second = MBlock::new(BlockId(1));
        second.push(MInst::Load {
            dst: b,
            base: BaseReg::SimState,
            offset: 1,
            size: OpSize::S8,
        });
        second.push(MInst::Branch {
            cond: b,
            true_bb: BlockId(3),
            false_bb: BlockId(4),
        });
        let mut second_work = MBlock::new(BlockId(3));
        second_work.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 16,
            src: value,
            size: OpSize::S64,
        });
        second_work.push(MInst::Jump { target: BlockId(4) });
        let mut done = MBlock::new(BlockId(4));
        done.push(MInst::Return);
        func.blocks = vec![first, first_work, second, second_work, done];
        assert_eq!(
            emission_block_order(&func)
                .into_iter()
                .map(|i| func.blocks[i].id)
                .collect::<Vec<_>>(),
            vec![BlockId(0), BlockId(1), BlockId(4), BlockId(2), BlockId(3)]
        );
        let allocation = crate::native::regalloc::run_regalloc(&mut func).unwrap();
        let code = emit(&func, &allocation.assignment, allocation.spill_frame_size).unwrap();
        let jit = JitCode::new(&code.code).unwrap();
        for flags in 0..4 {
            let mut state = vec![0u8; (code.required_state_size as usize).max(24)];
            state[0] = flags & 1;
            state[1] = flags >> 1;
            assert_eq!(unsafe { jit.call(&mut state) }, 0);
            assert_eq!(
                u64::from_le_bytes(state[8..16].try_into().unwrap()),
                if flags & 1 != 0 { 42 } else { 0 }
            );
            assert_eq!(
                u64::from_le_bytes(state[16..24].try_into().unwrap()),
                if flags & 2 != 0 { 42 } else { 0 }
            );
        }
    }
}

#[test]
fn unchanged_outputs_fall_through_past_the_reader_publication_chain() {
    let mut regs = VRegAllocator::new();
    let [a, b, changed] = std::array::from_fn(|_| regs.alloc());
    let mut func = MFunction::new(regs, vec![SpillDesc::transient(); 3]);
    func.settle_flags = Some(8..10);
    let mut entry = MBlock::new(BlockId(0));
    for (dst, offset) in [(a, 0), (b, 1)] {
        entry.push(MInst::Load {
            dst,
            base: BaseReg::SimState,
            offset,
            size: OpSize::S8,
        });
    }
    entry.push(MInst::Or32 {
        dst: changed,
        lhs: a,
        rhs: b,
    });
    let exit = BlockId(5);
    entry.push(MInst::Branch {
        cond: changed,
        true_bb: BlockId(1),
        false_bb: exit,
    });
    func.blocks.push(entry);
    for (first, cond, offset, next) in [(1, a, 8, BlockId(3)), (3, b, 9, exit)] {
        let mut guard = MBlock::new(BlockId(first));
        let mut mark = MBlock::new(BlockId(first + 1));
        guard.push(MInst::Branch {
            cond,
            true_bb: mark.id,
            false_bb: next,
        });
        mark.push(MInst::StoreImm {
            base: BaseReg::SimState,
            offset,
            size: OpSize::S8,
            imm: 1,
        });
        mark.push(MInst::Jump { target: next });
        func.blocks.extend([guard, mark]);
    }
    let mut done = MBlock::new(exit);
    done.push(MInst::Return);
    func.blocks.push(done);

    let order = emission_block_order(&func);
    assert_eq!(func.blocks[order[0]].id, BlockId(0));
    assert_eq!(func.blocks[order[1]].id, exit);
    // Skipping just the first notification does not identify a whole
    // publication chain. Leave the ordinary block order as the tie-breaker.
    let mut single = func.clone();
    let Some(MInst::Branch { false_bb, .. }) = single.blocks[0].insts.last_mut() else {
        unreachable!();
    };
    *false_bb = BlockId(3);
    let natural = (0..single.blocks.len()).collect();
    let single_order = crate::native::emit::settle_layout::cluster_clean_guards(&single, natural);
    assert_eq!(single.blocks[single_order[1]].id, BlockId(1));

    let allocation = regalloc::run_regalloc(&mut func).unwrap();
    let code = emit(&func, &allocation.assignment, allocation.spill_frame_size).unwrap();
    let jit = JitCode::new(&code.code).unwrap();
    for bits in 0..16 {
        let mut state = vec![0u8; (code.required_state_size as usize).max(10)];
        let (a, b) = (bits & 1, (bits >> 1) & 1);
        let (pending_a, pending_b) = ((bits >> 2) & 1, (bits >> 3) & 1);
        state[..2].copy_from_slice(&[a, b]);
        state[8..10].copy_from_slice(&[pending_a, pending_b]);
        assert_eq!(unsafe { jit.call(&mut state) }, 0);
        assert_eq!(state[8..10], [a | pending_a, b | pending_b]);
    }
}
