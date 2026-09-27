use super::*;

#[test]
fn vreg_compaction_removes_dead_ids_and_rewrites_phi_and_spill_metadata() {
    let mut vregs = VRegAllocator::new();
    for _ in 0..8 {
        vregs.alloc();
    }
    let mut spill_descs = vec![SpillDesc::transient(); 8];
    spill_descs[6] = SpillDesc::transient().with_state_insert(VReg(4), 0, 8);
    spill_descs[7] = SpillDesc::transient().with_state_insert(VReg(5), 0, 8);
    let mut function = MFunction::new(vregs, spill_descs);
    function.push_block(MBlock {
        id: BlockId(0),
        phis: Vec::new(),
        insts: vec![
            MInst::Load {
                dst: VReg(2),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
            },
            MInst::LoadImm {
                dst: VReg(5),
                value: 1,
            },
            MInst::Jump { target: BlockId(1) },
        ],
    });
    function.push_block(MBlock {
        id: BlockId(1),
        phis: vec![PhiNode {
            dst: VReg(6),
            sources: vec![(BlockId(0), VReg(2))],
        }],
        insts: vec![
            MInst::Add {
                dst: VReg(7),
                lhs: VReg(6),
                rhs: VReg(5),
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 8,
                src: VReg(7),
                size: OpSize::S64,
            },
            MInst::Return,
        ],
    });

    assert_eq!(
        compact_vregs(&mut function),
        VRegCompaction {
            before: 8,
            after: 4,
        }
    );
    assert_eq!(function.vregs.count(), 4);
    assert_eq!(function.spill_descs.len(), 4);
    assert_eq!(function.blocks[1].phis[0].dst, VReg(2));
    assert_eq!(function.blocks[1].phis[0].sources[0].1, VReg(0));
    assert!(matches!(
        function.blocks[1].insts[0],
        MInst::Add {
            dst: VReg(3),
            lhs: VReg(2),
            rhs: VReg(1),
        }
    ));
    assert_eq!(
        function.spill_descs[3]
            .state_insert
            .expect("live provenance")
            .value,
        VReg(1)
    );
    assert!(
        function.spill_descs[2].state_insert.is_none(),
        "provenance for a DCE-removed definition must not survive"
    );
    function.verify();
}

#[test]
fn copy_propagates_only_redundant_word32_snapshots() {
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
            },
            MInst::Mov32 {
                dst: VReg(1),
                src: VReg(0),
            },
            MInst::Mov32 {
                dst: VReg(2),
                src: VReg(1),
            },
            MInst::Mov32 {
                dst: VReg(3),
                src: VReg(1),
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 8,
                src: VReg(2),
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 16,
                src: VReg(3),
                size: OpSize::S64,
            },
            MInst::Return,
        ],
        4,
    );

    copy_propagate(&mut func);

    assert_eq!(
        func.blocks[0]
            .insts
            .iter()
            .filter(|inst| matches!(inst, MInst::Mov32 { .. }))
            .count(),
        1,
        "the first Mov32 is a real 64-to-32 truncation"
    );
    let stored = func.blocks[0]
        .insts
        .iter()
        .filter_map(|inst| match inst {
            MInst::Store { src, .. } => Some(*src),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(stored, vec![VReg(1), VReg(1)]);
}

#[test]
fn dead_code_elimination_removes_unused_phi_chains() {
    let mut func = make_func(
        vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 1,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 2,
            },
            MInst::Return,
        ],
        4,
    );
    func.blocks[0].phis.extend([
        PhiNode {
            dst: VReg(2),
            sources: vec![(BlockId(1), VReg(0)), (BlockId(2), VReg(1))],
        },
        PhiNode {
            dst: VReg(3),
            sources: vec![(BlockId(1), VReg(2)), (BlockId(2), VReg(2))],
        },
    ]);

    dead_code_eliminate(&mut func);

    assert!(func.blocks[0].phis.is_empty());
    assert!(matches!(func.blocks[0].insts.as_slice(), [MInst::Return]));
}

#[test]
fn simplify_cfg_does_not_collapse_distinct_phi_edges() {
    let mut func = make_func(Vec::new(), 3);
    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm {
        dst: VReg(0),
        value: 1,
    });
    entry.push(MInst::LoadImm {
        dst: VReg(1),
        value: 10,
    });
    entry.push(MInst::LoadImm {
        dst: VReg(2),
        value: 20,
    });
    entry.push(MInst::Branch {
        cond: VReg(0),
        true_bb: BlockId(1),
        false_bb: BlockId(2),
    });
    let mut left = MBlock::new(BlockId(1));
    left.push(MInst::Jump { target: BlockId(3) });
    let mut right = MBlock::new(BlockId(2));
    right.push(MInst::Jump { target: BlockId(3) });
    let mut merge = MBlock::new(BlockId(3));
    merge.phis.push(PhiNode {
        dst: VReg(3),
        sources: vec![(BlockId(1), VReg(1)), (BlockId(2), VReg(2))],
    });
    merge.push(MInst::Return);
    func.vregs.alloc();
    func.spill_descs.push(SpillDesc::transient());
    func.blocks = vec![entry, left, right, merge];

    simplify_cfg(&mut func);

    assert_eq!(func.verify_result(), Ok(()));
    assert_eq!(func.blocks.len(), 4);
}

#[test]
fn simplify_cfg_folds_a_jump_table_whose_redirected_targets_are_equal() {
    let mut func = make_func(Vec::new(), 3);
    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm {
        dst: VReg(0),
        value: 7,
    });
    entry.push(MInst::LoadImm {
        dst: VReg(1),
        value: 0,
    });
    entry.push(MInst::LoadImm {
        dst: VReg(2),
        value: 0,
    });
    entry.push(MInst::JumpTable {
        index: VReg(0),
        table_base: VReg(1),
        target: VReg(2),
        targets: vec![BlockId(1), BlockId(2)].into_boxed_slice(),
    });
    let mut case_a = MBlock::new(BlockId(1));
    case_a.push(MInst::Jump { target: BlockId(3) });
    let mut case_b = MBlock::new(BlockId(2));
    case_b.push(MInst::Jump { target: BlockId(3) });
    let mut target = MBlock::new(BlockId(3));
    target.push(MInst::Return);
    func.blocks = vec![entry, case_a, case_b, target];

    simplify_cfg(&mut func);
    dead_code_eliminate(&mut func);

    assert_eq!(func.verify_result(), Ok(()));
    assert!(matches!(
        func.blocks[0].insts.as_slice(),
        [MInst::Jump { target: BlockId(3) }]
    ));
    assert_eq!(func.blocks.len(), 2);
}

#[test]
fn simplify_cfg_folds_an_equal_target_branch_without_redirects() {
    let mut func = make_func(
        vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 1,
            },
            MInst::Branch {
                cond: VReg(0),
                true_bb: BlockId(1),
                false_bb: BlockId(1),
            },
        ],
        1,
    );
    let mut target = MBlock::new(BlockId(1));
    target.push(MInst::Return);
    func.blocks.push(target);

    simplify_cfg(&mut func);
    dead_code_eliminate(&mut func);

    assert_eq!(func.verify_result(), Ok(()));
    assert!(matches!(
        func.blocks[0].insts.as_slice(),
        [MInst::Jump { target: BlockId(1) }]
    ));
}
