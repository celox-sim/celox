use super::*;

#[test]
fn dominators_do_not_depend_on_block_storage_order() {
    // Storage order is entry, join, left, right; reverse postorder is
    // entry, right, left, join.
    let preds = vec![vec![], vec![2, 3], vec![0], vec![0]];
    let succs = vec![vec![2, 3], vec![], vec![1], vec![1]];
    assert_eq!(
        compute_dominators(4, &preds, &succs),
        vec![None, Some(0), Some(0), Some(0)]
    );
}

#[test]
fn memcopy_destination_invalidates_global_load_gvn() {
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S64,
            },
            MInst::MemCopy {
                src_offset: 64,
                dst_offset: 16,
                byte_len: 8,
            },
            MInst::Load {
                dst: VReg(1),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 96,
                src: VReg(1),
                size: OpSize::S64,
            },
            MInst::Return,
        ],
        2,
    );

    global_gvn(&mut func);

    assert!(matches!(
        func.blocks[0].insts[2],
        MInst::Load {
            dst: VReg(1),
            base: BaseReg::SimState,
            offset: 16,
            size: OpSize::S64,
        }
    ));
}

#[test]
fn global_gvn_handles_deep_dominators_on_a_small_stack() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            const BLOCKS: u32 = 10_000;
            let mut func = make_func(Vec::new(), BLOCKS + 1);
            func.blocks.clear();
            for index in 0..BLOCKS {
                let mut block = MBlock::new(BlockId(index));
                if index == 0 {
                    block.push(MInst::LoadImm { dst: VReg(0), value: 1 });
                }
                block.push(MInst::AddImm { dst: VReg(index + 1), src: VReg(0), imm: index as i32 });
                block.push(if index + 1 < BLOCKS {
                    MInst::Jump { target: BlockId(index + 1) }
                } else { MInst::Return });
                func.blocks.push(block);
            }
            global_gvn(&mut func);
            assert_eq!(func.blocks.len(), BLOCKS as usize);
            for (index, block) in func.blocks.iter().enumerate() {
                assert!(block.insts.iter().any(|inst| matches!(inst, MInst::AddImm { dst, .. } if *dst == VReg(index as u32 + 1))));
            }
        })
        .unwrap().join().unwrap();
}

#[test]
fn global_gvn_numbers_values_instead_of_raw_vregs() {
    let mut func = make_func(
        vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 7,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 7,
            },
            MInst::Add {
                dst: VReg(2),
                lhs: VReg(0),
                rhs: VReg(0),
            },
            // v1 and v0 have the same value number.  This expression must
            // therefore match v2 in this GVN invocation, without relying
            // on a later copy-propagation pass.
            MInst::Add {
                dst: VReg(3),
                lhs: VReg(1),
                rhs: VReg(0),
            },
            // Keep the first leader naturally live at the repeated
            // expression, so reusing it does not lengthen its lifetime.
            MInst::Store {
                base: BaseReg::SimState,
                offset: 8,
                src: VReg(2),
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 0,
                src: VReg(3),
                size: OpSize::S64,
            },
            MInst::Return,
        ],
        4,
    );

    global_gvn(&mut func);

    assert_eq!(
        func.blocks[0].insts[1],
        MInst::Mov {
            dst: VReg(1),
            src: VReg(0),
        }
    );
    assert_eq!(
        func.blocks[0].insts[3],
        MInst::Mov {
            dst: VReg(3),
            src: VReg(2),
        }
    );
}

#[test]
fn global_gvn_recomputes_a_dead_same_block_leader() {
    let mut func = make_func(
        vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 3,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 5,
            },
            MInst::Add {
                dst: VReg(2),
                lhs: VReg(0),
                rhs: VReg(1),
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 0,
                src: VReg(0),
                size: OpSize::S64,
            },
            MInst::Add {
                dst: VReg(3),
                lhs: VReg(0),
                rhs: VReg(1),
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 8,
                src: VReg(3),
                size: OpSize::S64,
            },
            MInst::Return,
        ],
        4,
    );

    global_gvn(&mut func);

    assert!(matches!(
        func.blocks[0].insts[4],
        MInst::Add { dst: VReg(3), .. }
    ));
}

#[test]
fn global_gvn_reuses_a_dead_same_block_rematerializable_leader() {
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
            },
            MInst::ShrImm {
                dst: VReg(1),
                src: VReg(0),
                imm: 3,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 16,
                src: VReg(1),
                size: OpSize::S64,
            },
            MInst::ShrImm {
                dst: VReg(2),
                src: VReg(0),
                imm: 3,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 24,
                src: VReg(2),
                size: OpSize::S64,
            },
            MInst::Return,
        ],
        3,
    );

    global_gvn(&mut func);

    assert_eq!(
        func.blocks[0].insts[3],
        MInst::Mov {
            dst: VReg(2),
            src: VReg(1),
        }
    );
}

#[test]
fn global_gvn_reuses_a_dead_same_block_versioned_load_leader() {
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 16,
                src: VReg(0),
                size: OpSize::S64,
            },
            MInst::Load {
                dst: VReg(1),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 24,
                src: VReg(1),
                size: OpSize::S64,
            },
            MInst::Return,
        ],
        2,
    );

    global_gvn(&mut func);

    assert_eq!(
        func.blocks[0].insts[2],
        MInst::Mov {
            dst: VReg(1),
            src: VReg(0),
        }
    );
}

#[test]
fn global_gvn_does_not_extend_a_leader_only_for_cross_block_cse() {
    let mut vregs = VRegAllocator::new();
    for _ in 0..4 {
        vregs.alloc();
    }
    let spill_descs = (0..4).map(|_| SpillDesc::transient()).collect();
    let mut func = MFunction::new(vregs, spill_descs);

    let mut entry = MBlock::new(BlockId(0));
    entry.insts = vec![
        MInst::LoadImm {
            dst: VReg(0),
            value: 3,
        },
        MInst::LoadImm {
            dst: VReg(1),
            value: 5,
        },
        MInst::Add {
            dst: VReg(2),
            lhs: VReg(0),
            rhs: VReg(1),
        },
        MInst::Jump { target: BlockId(1) },
    ];
    func.push_block(entry);

    let mut successor = MBlock::new(BlockId(1));
    successor.insts = vec![
        MInst::Add {
            dst: VReg(3),
            lhs: VReg(0),
            rhs: VReg(1),
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 0,
            src: VReg(3),
            size: OpSize::S64,
        },
        MInst::Return,
    ];
    func.push_block(successor);

    global_gvn(&mut func);

    assert!(matches!(
        func.blocks[1].insts[0],
        MInst::Add { dst: VReg(3), .. }
    ));
}

#[test]
fn global_gvn_does_not_extend_state_load_leader_across_blocks() {
    let mut vregs = VRegAllocator::new();
    for _ in 0..2 {
        vregs.alloc();
    }
    let spill_descs = (0..2).map(|_| SpillDesc::transient()).collect();
    let mut func = MFunction::new(vregs, spill_descs);

    let mut entry = MBlock::new(BlockId(0));
    entry.insts = vec![
        MInst::Load {
            dst: VReg(0),
            base: BaseReg::SimState,
            offset: 16,
            size: OpSize::S64,
        },
        MInst::Jump { target: BlockId(1) },
    ];
    func.push_block(entry);

    let mut successor = MBlock::new(BlockId(1));
    successor.insts = vec![
        MInst::Load {
            dst: VReg(1),
            base: BaseReg::SimState,
            offset: 16,
            size: OpSize::S64,
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 80,
            src: VReg(1),
            size: OpSize::S64,
        },
        MInst::Return,
    ];
    func.push_block(successor);

    global_gvn(&mut func);

    assert!(matches!(
        func.blocks[1].insts[0],
        MInst::Load { dst: VReg(1), .. }
    ));
}

#[test]
fn global_gvn_reuses_state_load_leader_that_is_already_live() {
    let mut vregs = VRegAllocator::new();
    for _ in 0..2 {
        vregs.alloc();
    }
    let spill_descs = (0..2).map(|_| SpillDesc::transient()).collect();
    let mut func = MFunction::new(vregs, spill_descs);

    let mut entry = MBlock::new(BlockId(0));
    entry.insts = vec![
        MInst::Load {
            dst: VReg(0),
            base: BaseReg::SimState,
            offset: 16,
            size: OpSize::S64,
        },
        MInst::Jump { target: BlockId(1) },
    ];
    func.push_block(entry);

    let mut successor = MBlock::new(BlockId(1));
    successor.insts = vec![
        MInst::Load {
            dst: VReg(1),
            base: BaseReg::SimState,
            offset: 16,
            size: OpSize::S64,
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 80,
            src: VReg(0),
            size: OpSize::S64,
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 88,
            src: VReg(1),
            size: OpSize::S64,
        },
        MInst::Return,
    ];
    func.push_block(successor);

    global_gvn(&mut func);

    assert_eq!(
        func.blocks[1].insts[0],
        MInst::Mov {
            dst: VReg(1),
            src: VReg(0),
        }
    );
}

#[test]
fn global_gvn_reuses_a_cross_block_leader_that_is_already_live() {
    let mut vregs = VRegAllocator::new();
    for _ in 0..4 {
        vregs.alloc();
    }
    let spill_descs = (0..4).map(|_| SpillDesc::transient()).collect();
    let mut func = MFunction::new(vregs, spill_descs);

    let mut entry = MBlock::new(BlockId(0));
    entry.insts = vec![
        MInst::LoadImm {
            dst: VReg(0),
            value: 3,
        },
        MInst::LoadImm {
            dst: VReg(1),
            value: 5,
        },
        MInst::Add {
            dst: VReg(2),
            lhs: VReg(0),
            rhs: VReg(1),
        },
        MInst::Jump { target: BlockId(1) },
    ];
    func.push_block(entry);

    let mut successor = MBlock::new(BlockId(1));
    successor.insts = vec![
        MInst::Add {
            dst: VReg(3),
            lhs: VReg(0),
            rhs: VReg(1),
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 0,
            src: VReg(2),
            size: OpSize::S64,
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 8,
            src: VReg(3),
            size: OpSize::S64,
        },
        MInst::Return,
    ];
    func.push_block(successor);

    global_gvn(&mut func);

    assert_eq!(
        func.blocks[1].insts[0],
        MInst::Mov {
            dst: VReg(3),
            src: VReg(2),
        }
    );
}

#[test]
fn global_gvn_does_not_reuse_a_sibling_expression() {
    let mut vregs = VRegAllocator::new();
    for _ in 0..3 {
        vregs.alloc();
    }
    let spill_descs = (0..3).map(|_| SpillDesc::transient()).collect();
    let mut func = MFunction::new(vregs, spill_descs);

    let mut entry = MBlock::new(BlockId(0));
    entry.insts = vec![
        MInst::LoadImm {
            dst: VReg(0),
            value: 1,
        },
        MInst::Branch {
            cond: VReg(0),
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        },
    ];
    func.push_block(entry);

    let mut left = MBlock::new(BlockId(1));
    left.insts = vec![
        MInst::Add {
            dst: VReg(1),
            lhs: VReg(0),
            rhs: VReg(0),
        },
        MInst::Return,
    ];
    func.push_block(left);

    let mut right = MBlock::new(BlockId(2));
    right.insts = vec![
        MInst::Add {
            dst: VReg(2),
            lhs: VReg(0),
            rhs: VReg(0),
        },
        MInst::Return,
    ];
    func.push_block(right);

    global_gvn(&mut func);

    assert!(matches!(func.blocks[1].insts[0], MInst::Add { .. }));
    assert!(matches!(func.blocks[2].insts[0], MInst::Add { .. }));
}

#[test]
fn global_gvn_does_not_reuse_bsr_with_unspecified_zero_result() {
    let mut func = make_func(
        vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 0,
            },
            MInst::Bsr {
                dst: VReg(1),
                src: VReg(0),
            },
            MInst::Bsr {
                dst: VReg(2),
                src: VReg(0),
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 0,
                src: VReg(2),
                size: OpSize::S64,
            },
            MInst::Return,
        ],
        3,
    );

    global_gvn(&mut func);

    assert!(matches!(func.blocks[0].insts[1], MInst::Bsr { .. }));
    assert!(matches!(func.blocks[0].insts[2], MInst::Bsr { .. }));
}

#[test]
fn global_gvn_invalidates_loads_at_exact_byte_boundaries() {
    let mut overlapping = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 23,
                src: VReg(0),
                size: OpSize::S8,
            },
            MInst::Load {
                dst: VReg(1),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 32,
                src: VReg(0),
                size: OpSize::S64,
            },
            MInst::Return,
        ],
        2,
    );
    global_gvn(&mut overlapping);
    assert!(matches!(overlapping.blocks[0].insts[2], MInst::Load { .. }));

    let mut adjacent = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 24,
                src: VReg(0),
                size: OpSize::S8,
            },
            MInst::Load {
                dst: VReg(1),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 32,
                src: VReg(0),
                size: OpSize::S64,
            },
            MInst::Return,
        ],
        2,
    );
    global_gvn(&mut adjacent);
    assert_eq!(
        adjacent.blocks[0].insts[2],
        MInst::Mov {
            dst: VReg(1),
            src: VReg(0),
        }
    );
}

#[test]
fn memcopy_preserves_nonoverlapping_global_load_gvn() {
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 80,
                size: OpSize::S64,
            },
            MInst::MemCopy {
                src_offset: 64,
                dst_offset: 16,
                byte_len: 8,
            },
            MInst::Load {
                dst: VReg(1),
                base: BaseReg::SimState,
                offset: 80,
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 96,
                src: VReg(1),
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 104,
                src: VReg(0),
                size: OpSize::S64,
            },
            MInst::Return,
        ],
        2,
    );

    global_gvn(&mut func);

    assert!(matches!(
        func.blocks[0].insts[2],
        MInst::Mov {
            dst: VReg(1),
            src: VReg(0),
        }
    ));
}

#[test]
fn gvn_write_ordinals_follow_layout_even_when_dominance_order_differs() {
    let mut vregs = VRegAllocator::new();
    for _ in 0..4 {
        vregs.alloc();
    }
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 4]);
    let load = |dst| MInst::Load {
        dst: VReg(dst),
        base: BaseReg::SimState,
        offset: 16,
        size: OpSize::S64,
    };
    let store = |offset, size| MInst::Store {
        base: BaseReg::SimState,
        offset,
        src: VReg(0),
        size,
    };
    let mut entry = MBlock::new(BlockId(0));
    // An untracked write still consumes an ordinal.
    entry.insts = vec![store(128, OpSize::S64), MInst::Jump { target: BlockId(2) }];
    let mut exit = MBlock::new(BlockId(1));
    exit.insts = vec![load(1), store(16, OpSize::S8), load(2), MInst::Return];
    let mut middle = MBlock::new(BlockId(2));
    middle.insts = vec![
        store(16, OpSize::S64),
        load(3),
        MInst::Jump { target: BlockId(1) },
    ];
    func.push_block(entry);
    func.push_block(exit);
    func.push_block(middle);
    let versions = compute_gvn_load_versions(
        &func,
        &[vec![], vec![2], vec![0]],
        &[Some(0), Some(2), Some(0)],
    )
    .unwrap();
    for location in [(2, 1), (1, 0)] {
        let version = &versions[&location];
        for (index, actual) in version.bytes.iter().enumerate() {
            assert_eq!(
                *actual,
                GvnMemoryVersion::Write {
                    ordinal: 2,
                    variable: GvnMemoryVariable::Byte(BaseReg::SimState, 16 + index as i64),
                }
            );
        }
    }
    let version = &versions[&(1, 2)];
    for (index, actual) in version.bytes.iter().enumerate() {
        assert_eq!(
            *actual,
            GvnMemoryVersion::Write {
                ordinal: if index == 0 { 1 } else { 2 },
                variable: GvnMemoryVariable::Byte(BaseReg::SimState, 16 + index as i64),
            }
        );
    }
}

#[test]
fn global_gvn_restores_load_scope_for_sibling_subtrees() {
    let mut vregs = VRegAllocator::new();
    for _ in 0..4 {
        vregs.alloc();
    }
    let spill_descs = (0..4).map(|_| SpillDesc::transient()).collect();
    let mut func = MFunction::new(vregs, spill_descs);

    let mut entry = MBlock::new(BlockId(0));
    entry.insts = vec![
        MInst::Load {
            dst: VReg(0),
            base: BaseReg::SimState,
            offset: 16,
            size: OpSize::S64,
        },
        MInst::LoadImm {
            dst: VReg(1),
            value: 1,
        },
        MInst::Branch {
            cond: VReg(1),
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        },
    ];
    func.push_block(entry);

    let mut writing_child = MBlock::new(BlockId(1));
    writing_child.insts = vec![
        MInst::Store {
            base: BaseReg::SimState,
            offset: 16,
            src: VReg(1),
            size: OpSize::S64,
        },
        MInst::Load {
            dst: VReg(2),
            base: BaseReg::SimState,
            offset: 16,
            size: OpSize::S64,
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 80,
            src: VReg(2),
            size: OpSize::S64,
        },
        MInst::Return,
    ];
    func.push_block(writing_child);

    let mut sibling = MBlock::new(BlockId(2));
    sibling.insts = vec![
        MInst::Load {
            dst: VReg(3),
            base: BaseReg::SimState,
            offset: 16,
            size: OpSize::S64,
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 88,
            src: VReg(3),
            size: OpSize::S64,
        },
        // Keep the entry load independently live in this child. This
        // isolates the scoped-memory assertion from GVN's rule against
        // extending a leader solely for cross-block CSE.
        MInst::Store {
            base: BaseReg::SimState,
            offset: 96,
            src: VReg(0),
            size: OpSize::S64,
        },
        MInst::Return,
    ];
    func.push_block(sibling);

    global_gvn(&mut func);

    assert!(matches!(func.blocks[1].insts[1], MInst::Load { .. }));
    assert!(matches!(
        func.blocks[2].insts[0],
        MInst::Mov {
            dst: VReg(3),
            src: VReg(0),
        }
    ));
}

#[test]
fn global_gvn_does_not_reuse_load_across_a_joining_write() {
    let mut vregs = VRegAllocator::new();
    for _ in 0..4 {
        vregs.alloc();
    }
    let spill_descs = (0..4).map(|_| SpillDesc::transient()).collect();
    let mut func = MFunction::new(vregs, spill_descs);

    let mut entry = MBlock::new(BlockId(0));
    entry.insts = vec![
        MInst::Load {
            dst: VReg(0),
            base: BaseReg::SimState,
            offset: 16,
            size: OpSize::S64,
        },
        MInst::LoadImm {
            dst: VReg(1),
            value: 1,
        },
        MInst::LoadImm {
            dst: VReg(2),
            value: 9,
        },
        MInst::Branch {
            cond: VReg(1),
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        },
    ];
    func.push_block(entry);

    let mut writing_arm = MBlock::new(BlockId(1));
    writing_arm.insts = vec![
        MInst::Store {
            base: BaseReg::SimState,
            offset: 16,
            src: VReg(2),
            size: OpSize::S64,
        },
        MInst::Jump { target: BlockId(3) },
    ];
    func.push_block(writing_arm);

    let mut unchanged_arm = MBlock::new(BlockId(2));
    unchanged_arm.insts = vec![MInst::Jump { target: BlockId(3) }];
    func.push_block(unchanged_arm);

    let mut join = MBlock::new(BlockId(3));
    join.insts = vec![
        MInst::Load {
            dst: VReg(3),
            base: BaseReg::SimState,
            offset: 16,
            size: OpSize::S64,
        },
        // Keep the entry value live at the repeated load. The memory
        // version, not register liveness, must reject this replacement.
        MInst::Store {
            base: BaseReg::SimState,
            offset: 80,
            src: VReg(0),
            size: OpSize::S64,
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 88,
            src: VReg(3),
            size: OpSize::S64,
        },
        MInst::Return,
    ];
    func.push_block(join);

    global_gvn(&mut func);

    assert!(matches!(func.blocks[3].insts[0], MInst::Load { .. }));
}

#[test]
fn global_gvn_does_not_reuse_load_across_a_loop_carried_write() {
    let mut vregs = VRegAllocator::new();
    for _ in 0..3 {
        vregs.alloc();
    }
    let spill_descs = (0..3).map(|_| SpillDesc::transient()).collect();
    let mut func = MFunction::new(vregs, spill_descs);

    let mut entry = MBlock::new(BlockId(0));
    entry.insts = vec![
        MInst::Load {
            dst: VReg(0),
            base: BaseReg::SimState,
            offset: 16,
            size: OpSize::S64,
        },
        MInst::LoadImm {
            dst: VReg(1),
            value: 1,
        },
        MInst::Jump { target: BlockId(1) },
    ];
    func.push_block(entry);

    let mut header = MBlock::new(BlockId(1));
    header.insts = vec![MInst::Branch {
        cond: VReg(1),
        true_bb: BlockId(2),
        false_bb: BlockId(3),
    }];
    func.push_block(header);

    let mut body = MBlock::new(BlockId(2));
    body.insts = vec![
        MInst::Store {
            base: BaseReg::SimState,
            offset: 16,
            src: VReg(1),
            size: OpSize::S64,
        },
        MInst::Jump { target: BlockId(1) },
    ];
    func.push_block(body);

    let mut exit = MBlock::new(BlockId(3));
    exit.insts = vec![
        MInst::Load {
            dst: VReg(2),
            base: BaseReg::SimState,
            offset: 16,
            size: OpSize::S64,
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 80,
            src: VReg(0),
            size: OpSize::S64,
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 88,
            src: VReg(2),
            size: OpSize::S64,
        },
        MInst::Return,
    ];
    func.push_block(exit);

    global_gvn(&mut func);

    assert!(matches!(func.blocks[3].insts[0], MInst::Load { .. }));
}

#[test]
fn global_gvn_sparse_mark_invalidates_only_its_metadata_ranges() {
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S64,
            },
            MInst::Load {
                dst: VReg(1),
                base: BaseReg::SimState,
                offset: 200,
                size: OpSize::S64,
            },
            MInst::SparseMarkActive {
                active_index: 3,
                active_bits_offset: 200,
                active_capacity: 16,
            },
            MInst::Load {
                dst: VReg(2),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S64,
            },
            MInst::Load {
                dst: VReg(3),
                base: BaseReg::SimState,
                offset: 200,
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 400,
                src: VReg(0),
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 408,
                src: VReg(1),
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 416,
                src: VReg(2),
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 424,
                src: VReg(3),
                size: OpSize::S64,
            },
            MInst::Return,
        ],
        4,
    );

    global_gvn(&mut func);

    assert_eq!(
        func.blocks[0].insts[3],
        MInst::Mov {
            dst: VReg(2),
            src: VReg(0),
        }
    );
    assert!(matches!(func.blocks[0].insts[4], MInst::Load { .. }));
}

#[test]
fn global_gvn_bounded_indexed_store_invalidates_only_its_alias_range() {
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S64,
            },
            MInst::Load {
                dst: VReg(1),
                base: BaseReg::SimState,
                offset: 128,
                size: OpSize::S64,
            },
            MInst::LoadImm {
                dst: VReg(2),
                value: 0,
            },
            MInst::LoadImm {
                dst: VReg(3),
                value: 1,
            },
            MInst::StoreIndexed {
                base: BaseReg::SimState,
                offset: 16,
                index: VReg(2),
                src: VReg(3),
                size: OpSize::S8,
                alias_range: MemoryAliasRange::new(16, 64),
            },
            MInst::Load {
                dst: VReg(4),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S64,
            },
            MInst::Load {
                dst: VReg(5),
                base: BaseReg::SimState,
                offset: 128,
                size: OpSize::S64,
            },
            // Keep the original nonoverlapping leader live independently
            // of the candidate CSE. The test is about the bounded alias
            // envelope, not permission to lengthen a state-load range.
            MInst::Store {
                base: BaseReg::SimState,
                offset: 400,
                src: VReg(1),
                size: OpSize::S64,
            },
            MInst::Return,
        ],
        6,
    );

    global_gvn(&mut func);

    assert!(matches!(func.blocks[0].insts[5], MInst::Load { .. }));
    assert_eq!(
        func.blocks[0].insts[6],
        MInst::Mov {
            dst: VReg(5),
            src: VReg(1),
        }
    );
}
