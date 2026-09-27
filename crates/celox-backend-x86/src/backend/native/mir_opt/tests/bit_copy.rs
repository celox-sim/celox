use super::*;

#[test]
fn folds_complete_bit_partition_reconstruction_to_original_word() {
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
            MInst::AndImm32 {
                dst: VReg(2),
                src: VReg(1),
                imm: 0x1,
            },
            MInst::ShrImm {
                dst: VReg(3),
                src: VReg(1),
                imm: 1,
            },
            MInst::AndImm32 {
                dst: VReg(4),
                src: VReg(3),
                imm: 0x7f,
            },
            MInst::ShlImm {
                dst: VReg(5),
                src: VReg(4),
                imm: 1,
            },
            MInst::Or {
                dst: VReg(6),
                lhs: VReg(2),
                rhs: VReg(5),
            },
            MInst::ShrImm {
                dst: VReg(7),
                src: VReg(1),
                imm: 8,
            },
            MInst::AndImm32 {
                dst: VReg(8),
                src: VReg(7),
                imm: 0xff,
            },
            MInst::ShlImm {
                dst: VReg(9),
                src: VReg(8),
                imm: 8,
            },
            MInst::Or {
                dst: VReg(10),
                lhs: VReg(6),
                rhs: VReg(9),
            },
            MInst::ShrImm {
                dst: VReg(11),
                src: VReg(1),
                imm: 16,
            },
            MInst::ShlImm {
                dst: VReg(12),
                src: VReg(11),
                imm: 16,
            },
            MInst::Or {
                dst: VReg(13),
                lhs: VReg(10),
                rhs: VReg(12),
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 8,
                src: VReg(13),
                size: OpSize::S32,
            },
            MInst::Return,
        ],
        14,
    );

    fold_reconstructed_bit_partitions(&mut func);

    assert!(matches!(
        func.blocks[0].insts[13],
        MInst::Mov32 {
            dst: VReg(13),
            src: VReg(0)
        }
    ));
}

#[test]
fn bit_partition_reconstruction_rejects_mixed_sources() {
    let mut func = make_func(
        vec![
            MInst::AndImm32 {
                dst: VReg(2),
                src: VReg(0),
                imm: 0xff,
            },
            MInst::AndImm32 {
                dst: VReg(3),
                src: VReg(1),
                imm: 0xff00,
            },
            MInst::Or {
                dst: VReg(4),
                lhs: VReg(2),
                rhs: VReg(3),
            },
            MInst::Return,
        ],
        5,
    );

    fold_reconstructed_bit_partitions(&mut func);

    assert!(matches!(func.blocks[0].insts[2], MInst::Or { .. }));
}

#[test]
fn bit_partition_reconstruction_rejects_relocated_bits() {
    let mut func = make_func(
        vec![
            MInst::AndImm32 {
                dst: VReg(1),
                src: VReg(0),
                imm: 0xff,
            },
            MInst::ShlImm {
                dst: VReg(2),
                src: VReg(1),
                imm: 8,
            },
            MInst::AndImm32 {
                dst: VReg(3),
                src: VReg(0),
                imm: 0xff00,
            },
            MInst::Or {
                dst: VReg(4),
                lhs: VReg(2),
                rhs: VReg(3),
            },
            MInst::Return,
        ],
        5,
    );

    fold_reconstructed_bit_partitions(&mut func);

    assert!(matches!(func.blocks[0].insts[3], MInst::Or { .. }));
}

#[test]
fn relocated_bit_copy_groups_fold_private_runs_and_keep_shared_terms() {
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
            },
            MInst::Load {
                dst: VReg(1),
                base: BaseReg::SimState,
                offset: 8,
                size: OpSize::S64,
            },
            MInst::Load {
                dst: VReg(2),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S64,
            },
            MInst::ShrImm {
                dst: VReg(3),
                src: VReg(0),
                imm: 42,
            },
            MInst::AndImm32 {
                dst: VReg(4),
                src: VReg(3),
                imm: 1,
            },
            MInst::ShlImm {
                dst: VReg(5),
                src: VReg(4),
                imm: 1,
            },
            MInst::ShrImm {
                dst: VReg(6),
                src: VReg(0),
                imm: 43,
            },
            MInst::AndImm32 {
                dst: VReg(7),
                src: VReg(6),
                imm: 1,
            },
            MInst::ShlImm {
                dst: VReg(8),
                src: VReg(7),
                imm: 2,
            },
            MInst::ShrImm {
                dst: VReg(9),
                src: VReg(0),
                imm: 44,
            },
            MInst::AndImm32 {
                dst: VReg(10),
                src: VReg(9),
                imm: 1,
            },
            MInst::ShlImm {
                dst: VReg(11),
                src: VReg(10),
                imm: 3,
            },
            MInst::AndImm32 {
                dst: VReg(12),
                src: VReg(2),
                imm: 0x100,
            },
            MInst::Or {
                dst: VReg(13),
                lhs: VReg(1),
                rhs: VReg(5),
            },
            MInst::Or {
                dst: VReg(14),
                lhs: VReg(13),
                rhs: VReg(8),
            },
            MInst::Or {
                dst: VReg(15),
                lhs: VReg(14),
                rhs: VReg(11),
            },
            MInst::Or {
                dst: VReg(16),
                lhs: VReg(15),
                rhs: VReg(12),
            },
            // The third projection is intentionally shared.  It must stay
            // as an existing term instead of being bypassed into the run.
            MInst::Store {
                base: BaseReg::SimState,
                offset: 24,
                src: VReg(11),
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 32,
                src: VReg(16),
                size: OpSize::S64,
            },
            MInst::Return,
        ],
        17,
    );

    fold_relocated_bit_copy_groups(&mut func);
    dead_code_eliminate(&mut func);

    assert!(func.blocks[0].insts.iter().any(|instruction| matches!(
        instruction,
        MInst::ShrImm {
            src: VReg(0),
            imm: 41,
            ..
        }
    )));
    assert!(
        func.blocks[0]
            .insts
            .iter()
            .any(|instruction| matches!(instruction, MInst::AndImm32 { imm: 0xe, .. }))
    );
    assert!(!func.blocks[0].insts.iter().any(|instruction| matches!(
        instruction,
        MInst::ShrImm {
            src: VReg(0),
            imm: 42 | 43,
            ..
        }
    )));
    assert!(func.blocks[0].insts.iter().any(|instruction| matches!(
        instruction,
        MInst::ShrImm {
            dst: VReg(9),
            src: VReg(0),
            imm: 44,
        }
    )));
    assert_eq!(
        func.blocks[0]
            .insts
            .iter()
            .filter(|instruction| instruction.def() == Some(VReg(16)))
            .count(),
        1
    );
    func.verify();
}

#[test]
fn relocated_bit_copy_groups_preserve_a_projection_used_on_a_phi_edge() {
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
                imm: 42,
            },
            MInst::AndImm32 {
                dst: VReg(2),
                src: VReg(1),
                imm: 1,
            },
            MInst::ShlImm {
                dst: VReg(3),
                src: VReg(2),
                imm: 1,
            },
            MInst::ShrImm {
                dst: VReg(4),
                src: VReg(0),
                imm: 43,
            },
            MInst::AndImm32 {
                dst: VReg(5),
                src: VReg(4),
                imm: 1,
            },
            MInst::ShlImm {
                dst: VReg(6),
                src: VReg(5),
                imm: 2,
            },
            MInst::ShrImm {
                dst: VReg(7),
                src: VReg(0),
                imm: 44,
            },
            MInst::AndImm32 {
                dst: VReg(8),
                src: VReg(7),
                imm: 1,
            },
            MInst::ShlImm {
                dst: VReg(9),
                src: VReg(8),
                imm: 3,
            },
            MInst::Or {
                dst: VReg(10),
                lhs: VReg(3),
                rhs: VReg(6),
            },
            MInst::Or {
                dst: VReg(11),
                lhs: VReg(10),
                rhs: VReg(9),
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 8,
                src: VReg(11),
                size: OpSize::S64,
            },
            MInst::Jump { target: BlockId(1) },
        ],
        13,
    );
    let mut join = MBlock::new(BlockId(1));
    join.phis.push(PhiNode {
        dst: VReg(12),
        sources: vec![(BlockId(0), VReg(9))],
    });
    join.insts = vec![
        MInst::Store {
            base: BaseReg::SimState,
            offset: 16,
            src: VReg(12),
            size: OpSize::S64,
        },
        MInst::Return,
    ];
    func.push_block(join);

    fold_relocated_bit_copy_groups(&mut func);
    dead_code_eliminate(&mut func);

    assert!(
        func.blocks[0]
            .insts
            .iter()
            .any(|instruction| instruction.def() == Some(VReg(9)))
    );
    assert_eq!(func.blocks[1].phis[0].sources, vec![(BlockId(0), VReg(9))]);
    func.verify();
}

#[test]
fn eliminates_redundant_or_of_same_select_term() {
    let mut func = make_func(
        vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 0,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 2,
            },
            MInst::Load {
                dst: VReg(2),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S8,
            },
            MInst::Cmp {
                dst: VReg(3),
                lhs: VReg(2),
                rhs: VReg(0),
                kind: CmpKind::Ne,
            },
            MInst::Select {
                dst: VReg(4),
                cond: VReg(3),
                true_val: VReg(1),
                false_val: VReg(0),
            },
            MInst::Or {
                dst: VReg(5),
                lhs: VReg(2),
                rhs: VReg(4),
            },
            MInst::Mov {
                dst: VReg(6),
                src: VReg(3),
            },
            MInst::Select {
                dst: VReg(7),
                cond: VReg(6),
                true_val: VReg(1),
                false_val: VReg(0),
            },
            MInst::Or {
                dst: VReg(8),
                lhs: VReg(5),
                rhs: VReg(7),
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 24,
                src: VReg(8),
                size: OpSize::S8,
            },
            MInst::Return,
        ],
        9,
    );

    optimize(&mut func);

    assert!(
        !func.blocks[0]
            .insts
            .iter()
            .any(|inst| matches!(inst, MInst::Or { dst: VReg(8), .. })),
        "{:#?}",
        func.blocks[0].insts
    );
    assert!(func.blocks[0].insts.iter().any(|inst| matches!(
        inst,
        MInst::Store {
            base: BaseReg::SimState,
            offset: 24,
            src: VReg(5),
            size: OpSize::S8,
        }
    )));
}

#[test]
fn keeps_or_of_different_select_terms() {
    let mut func = make_func(
        vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 0,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 2,
            },
            MInst::Load {
                dst: VReg(2),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S8,
            },
            MInst::Load {
                dst: VReg(3),
                base: BaseReg::SimState,
                offset: 17,
                size: OpSize::S8,
            },
            MInst::Cmp {
                dst: VReg(4),
                lhs: VReg(2),
                rhs: VReg(0),
                kind: CmpKind::Ne,
            },
            MInst::Cmp {
                dst: VReg(5),
                lhs: VReg(3),
                rhs: VReg(0),
                kind: CmpKind::Ne,
            },
            MInst::Select {
                dst: VReg(6),
                cond: VReg(4),
                true_val: VReg(1),
                false_val: VReg(0),
            },
            MInst::Or {
                dst: VReg(7),
                lhs: VReg(2),
                rhs: VReg(6),
            },
            MInst::Select {
                dst: VReg(8),
                cond: VReg(5),
                true_val: VReg(1),
                false_val: VReg(0),
            },
            MInst::Or {
                dst: VReg(9),
                lhs: VReg(7),
                rhs: VReg(8),
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 24,
                src: VReg(9),
                size: OpSize::S8,
            },
            MInst::Return,
        ],
        10,
    );

    optimize(&mut func);

    assert!(
        func.blocks[0].insts.iter().any(|inst| matches!(
            inst,
            MInst::Or { dst: VReg(9), .. } | MInst::Or32 { dst: VReg(9), .. }
        )),
        "{:#?}",
        func.blocks[0].insts
    );
}
