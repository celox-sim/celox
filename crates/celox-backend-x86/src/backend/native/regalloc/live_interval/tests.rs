use super::*;
use crate::native::mir::{
    BaseReg, MBlock, MFunction, MInst, OpSize, PhiNode, SpillDesc, VRegAllocator,
};

fn function(value_count: u32, blocks: Vec<MBlock>) -> MFunction {
    let mut values = VRegAllocator::new();
    for _ in 0..value_count {
        values.alloc();
    }
    let mut function = MFunction::new(values, vec![SpillDesc::transient(); value_count as usize]);
    function.blocks = blocks;
    function
}

fn normalize(function: &mut MFunction) -> NormalizedCfg {
    super::super::cfg::normalize(function).unwrap()
}

#[test]
fn arithmetic_slots_cover_large_blocks_and_reject_overflow() {
    for count in [0, 1, 2, 100_000_000] {
        let slots = slots_for_block(BlockId(0), count).unwrap();
        assert_eq!(slots.exit, SlotIndex(count as u64 * 3 + 2));
        assert_eq!(slots.instruction_use(count), None);
        assert_eq!(slots.instruction_def(count), None);
        if count != 0 {
            assert_eq!(
                slots.instruction_use(count - 1),
                Some(SlotIndex((count as u64 - 1) * 3 + 2))
            );
            assert_eq!(
                slots.instruction_clobber(count - 1),
                Some(SlotIndex((count as u64 - 1) * 3 + 3))
            );
            assert_eq!(
                slots.instruction_def(count - 1),
                Some(SlotIndex((count as u64 - 1) * 3 + 4))
            );
        }
    }
    if let Ok(count) = usize::try_from(u64::MAX / 3 + 1) {
        assert_eq!(
            slots_for_block(BlockId(0), count).unwrap_err().rule,
            "LIVE_INTERVAL.SLOT_RANGE"
        );
    }
}

#[test]
fn instruction_use_and_definition_slots_allow_last_use_register_reuse() {
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::LoadImm {
        dst: VReg(0),
        value: 7,
    });
    block.push(MInst::Mov {
        dst: VReg(1),
        src: VReg(0),
    });
    block.push(MInst::Return);
    let mut function = function(2, vec![block]);
    let cfg = normalize(&mut function);
    let intervals = analyze_program(&function, &cfg).unwrap();
    let source = intervals.intervals[0].as_ref().unwrap();
    let destination = intervals.intervals[1].as_ref().unwrap();
    assert!(!source.interferes(destination));
    assert_eq!(
        source.segments[0].end,
        intervals.block_slots[0].instruction_clobber(1).unwrap()
    );
    assert!(
        intervals.block_slots[0].instruction_clobber(1).unwrap()
            < intervals.block_slots[0].instruction_def(1).unwrap()
    );
    assert_eq!(
        destination.segments[0].start,
        intervals.block_slots[0].instruction_def(1).unwrap()
    );
}

#[test]
fn unused_phi_has_no_machine_range_but_unused_instruction_definition_does() {
    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm {
        dst: VReg(0),
        value: 7,
    });
    entry.push(MInst::Jump { target: BlockId(1) });
    let mut exit = MBlock::new(BlockId(1));
    exit.phis.push(PhiNode {
        dst: VReg(1),
        sources: vec![(BlockId(0), VReg(0))],
    });
    exit.push(MInst::LoadImm {
        dst: VReg(2),
        value: 11,
    });
    exit.push(MInst::Return);
    let mut function = function(3, vec![entry, exit]);
    let cfg = normalize(&mut function);

    let intervals = analyze_program(&function, &cfg).unwrap();
    intervals.verify(&function, &cfg).unwrap();

    assert!(intervals.intervals[0].is_some());
    assert!(intervals.intervals[1].is_none());
    let instruction = intervals.intervals[2].as_ref().unwrap();
    assert!(instruction.uses.is_empty());
    assert!(matches!(
        instruction.definition,
        DefinitionSite::Instruction { .. }
    ));
}

#[test]
fn block_local_slots_do_not_renumber_an_unchanged_successor() {
    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm {
        dst: VReg(0),
        value: 7,
    });
    entry.push(MInst::Jump { target: BlockId(1) });
    let mut exit = MBlock::new(BlockId(1));
    exit.push(MInst::Mov {
        dst: VReg(1),
        src: VReg(0),
    });
    exit.push(MInst::Return);
    let mut before = function(2, vec![entry.clone(), exit.clone()]);
    let before_cfg = normalize(&mut before);
    let before_intervals = analyze_program(&before, &before_cfg).unwrap();

    entry.insts.insert(
        1,
        MInst::Mov {
            dst: VReg(2),
            src: VReg(0),
        },
    );
    let mut after = function(3, vec![entry, exit]);
    let after_cfg = normalize(&mut after);
    let after_intervals = analyze_program(&after, &after_cfg).unwrap();

    assert_ne!(
        before_intervals.block_slots[0],
        after_intervals.block_slots[0]
    );
    assert_eq!(
        before_intervals.block_slots[1],
        after_intervals.block_slots[1]
    );
}

#[test]
fn diamond_arm_values_do_not_interfere_but_phi_sources_are_edge_live() {
    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm {
        dst: VReg(0),
        value: 1,
    });
    entry.push(MInst::Branch {
        cond: VReg(0),
        true_bb: BlockId(1),
        false_bb: BlockId(2),
    });
    let mut left = MBlock::new(BlockId(1));
    left.push(MInst::LoadImm {
        dst: VReg(1),
        value: 11,
    });
    left.push(MInst::Jump { target: BlockId(3) });
    let mut right = MBlock::new(BlockId(2));
    right.push(MInst::LoadImm {
        dst: VReg(2),
        value: 22,
    });
    right.push(MInst::Jump { target: BlockId(3) });
    let mut merge = MBlock::new(BlockId(3));
    merge.phis.push(PhiNode {
        dst: VReg(3),
        sources: vec![(BlockId(1), VReg(1)), (BlockId(2), VReg(2))],
    });
    merge.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 0,
        src: VReg(3),
        size: OpSize::S64,
    });
    merge.push(MInst::Return);
    let mut function = function(4, vec![entry, left, right, merge]);
    let cfg = normalize(&mut function);
    let intervals = analyze_program(&function, &cfg).unwrap();
    let left = intervals.intervals[1].as_ref().unwrap();
    let right = intervals.intervals[2].as_ref().unwrap();
    let left_block = cfg.block_index[&BlockId(1)];
    let right_block = cfg.block_index[&BlockId(2)];
    assert!(!left.interferes(right));
    assert!(left.covers(BlockId(1), intervals.block_slots[left_block].exit));
    assert!(right.covers(BlockId(2), intervals.block_slots[right_block].exit));
    assert!(matches!(left.uses.last(), Some(UseSite::PhiEdge { .. })));
    assert!(matches!(right.uses.last(), Some(UseSite::PhiEdge { .. })));
}

#[test]
fn sparse_liveness_matches_dataflow_across_branching_loops() {
    for pattern in 0..64_u32 {
        let mut blocks = Vec::new();
        for block in 0..8_u32 {
            let mut row = MBlock::new(BlockId(block));
            if block == 0 {
                for value in 0..7 {
                    row.push(MInst::LoadImm {
                        dst: VReg(value),
                        value: value as u64,
                    });
                }
            } else {
                row.push(MInst::Store {
                    base: BaseReg::SimState,
                    offset: 0,
                    src: VReg(1 + (pattern + block) % 6),
                    size: OpSize::S64,
                });
            }
            if block == 7 {
                row.push(MInst::Return);
            } else if block > 0 && pattern & (1 << (block - 1)) != 0 {
                row.push(MInst::Branch {
                    cond: VReg(0),
                    true_bb: BlockId(block + 1),
                    false_bb: BlockId(1 + (pattern % block)),
                });
            } else {
                row.push(MInst::Jump {
                    target: BlockId(block + 1),
                });
            }
            blocks.push(row);
        }
        let mut function = function(7, blocks);
        let cfg = normalize(&mut function);
        // Test builds compare every sparse result with the original
        // fixed-point solver before the independent interval verifier.
        analyze_program(&function, &cfg).unwrap();
    }
}

#[test]
fn loop_carried_phi_source_is_live_on_the_backedge() {
    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm {
        dst: VReg(0),
        value: 0,
    });
    entry.push(MInst::Jump { target: BlockId(1) });
    let mut header = MBlock::new(BlockId(1));
    header.phis.push(PhiNode {
        dst: VReg(1),
        sources: vec![(BlockId(0), VReg(0)), (BlockId(2), VReg(2))],
    });
    header.push(MInst::Branch {
        cond: VReg(1),
        true_bb: BlockId(2),
        false_bb: BlockId(3),
    });
    let mut body = MBlock::new(BlockId(2));
    body.push(MInst::AddImm {
        dst: VReg(2),
        src: VReg(1),
        imm: 1,
    });
    body.push(MInst::Jump { target: BlockId(1) });
    let mut exit = MBlock::new(BlockId(3));
    exit.push(MInst::Return);
    let mut function = function(3, vec![entry, header, body, exit]);
    let cfg = normalize(&mut function);
    let intervals = analyze_program(&function, &cfg).unwrap();
    let backedge = intervals.intervals[2].as_ref().unwrap();
    let body_index = cfg.block_index[&BlockId(2)];
    assert!(backedge.covers(BlockId(2), intervals.block_slots[body_index].exit));
    intervals.verify(&function, &cfg).unwrap();
}

#[test]
fn independent_verifier_rejects_a_missing_edge_segment() {
    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm {
        dst: VReg(0),
        value: 9,
    });
    entry.push(MInst::Jump { target: BlockId(1) });
    let mut exit = MBlock::new(BlockId(1));
    exit.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 0,
        src: VReg(0),
        size: OpSize::S64,
    });
    exit.push(MInst::Return);
    let mut function = function(1, vec![entry, exit]);
    let cfg = normalize(&mut function);
    let mut intervals = analyze_program(&function, &cfg).unwrap();
    let entry = cfg.block_index[&BlockId(0)];
    let exit_slot = intervals.block_slots[entry].exit;
    let segment = intervals.intervals[0]
        .as_mut()
        .unwrap()
        .segments
        .iter_mut()
        .find(|segment| segment.block == BlockId(0))
        .unwrap();
    segment.end = exit_slot;
    let error = intervals.verify(&function, &cfg).unwrap_err();
    assert_eq!(error.rule, "LIVE_INTERVAL.DATAFLOW_EQUATION");
}

#[test]
fn long_cfg_keeps_one_sparse_segment_per_live_block() {
    const BLOCKS: usize = 4096;
    let mut blocks = Vec::with_capacity(BLOCKS);
    for index in 0..BLOCKS {
        let mut block = MBlock::new(BlockId(index as u32));
        if index == 0 {
            block.push(MInst::LoadImm {
                dst: VReg(0),
                value: 1,
            });
        }
        if index + 1 == BLOCKS {
            block.push(MInst::Store {
                base: BaseReg::SimState,
                offset: 0,
                src: VReg(0),
                size: OpSize::S64,
            });
            block.push(MInst::Return);
        } else {
            block.push(MInst::Jump {
                target: BlockId((index + 1) as u32),
            });
        }
        blocks.push(block);
    }
    let mut function = function(1, blocks);
    let cfg = normalize(&mut function);
    let intervals = analyze_program(&function, &cfg).unwrap();
    assert_eq!(
        intervals.intervals[0].as_ref().unwrap().segments.len(),
        BLOCKS
    );
}
