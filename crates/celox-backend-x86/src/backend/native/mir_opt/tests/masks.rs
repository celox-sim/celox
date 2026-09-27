use super::*;

#[test]
fn algebraic_simplify_sinks_repeated_leaf_masks_to_and_root() {
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S8,
            },
            MInst::Load {
                dst: VReg(1),
                base: BaseReg::SimState,
                offset: 1,
                size: OpSize::S8,
            },
            MInst::Load {
                dst: VReg(2),
                base: BaseReg::SimState,
                offset: 2,
                size: OpSize::S8,
            },
            MInst::AndImm32 {
                dst: VReg(3),
                src: VReg(0),
                imm: 1,
            },
            MInst::AndImm32 {
                dst: VReg(4),
                src: VReg(1),
                imm: 1,
            },
            MInst::AndImm32 {
                dst: VReg(5),
                src: VReg(2),
                imm: 1,
            },
            MInst::And {
                dst: VReg(6),
                lhs: VReg(3),
                rhs: VReg(4),
            },
            MInst::And {
                dst: VReg(7),
                lhs: VReg(6),
                rhs: VReg(5),
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 8,
                src: VReg(7),
                size: OpSize::S8,
            },
            MInst::Return,
        ],
        8,
    );

    algebraic_simplify(&mut func);

    assert_eq!(
        func.blocks[0]
            .insts
            .iter()
            .filter(|instruction| matches!(
                instruction,
                MInst::AndImm { .. } | MInst::AndImm32 { .. }
            ))
            .count(),
        1
    );
    assert_eq!(
        func.blocks[0]
            .insts
            .iter()
            .filter(|instruction| matches!(instruction, MInst::And { .. } | MInst::And32 { .. }))
            .count(),
        2
    );
    assert!(func.blocks[0].insts.iter().any(|instruction| matches!(
        instruction,
        MInst::AndImm32 {
            dst: VReg(7),
            imm: 1,
            ..
        }
    )));
    func.verify();
}

#[test]
fn algebraic_simplify_combines_different_and_masks() {
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S8,
            },
            MInst::Load {
                dst: VReg(1),
                base: BaseReg::SimState,
                offset: 1,
                size: OpSize::S8,
            },
            MInst::AndImm32 {
                dst: VReg(2),
                src: VReg(0),
                imm: 0xf,
            },
            MInst::AndImm32 {
                dst: VReg(3),
                src: VReg(1),
                imm: 3,
            },
            MInst::And {
                dst: VReg(4),
                lhs: VReg(2),
                rhs: VReg(3),
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 8,
                src: VReg(4),
                size: OpSize::S8,
            },
            MInst::Return,
        ],
        5,
    );

    algebraic_simplify(&mut func);

    assert!(func.blocks[0].insts.iter().any(|instruction| matches!(
        instruction,
        MInst::AndImm32 {
            dst: VReg(4),
            imm: 3,
            ..
        }
    )));
    assert_eq!(
        func.blocks[0]
            .insts
            .iter()
            .filter(|instruction| matches!(
                instruction,
                MInst::AndImm { .. } | MInst::AndImm32 { .. }
            ))
            .count(),
        1
    );
    func.verify();
}

#[test]
fn algebraic_simplify_combines_serial_masks_across_widths() {
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
            },
            MInst::AndImm {
                dst: VReg(1),
                src: VReg(0),
                imm: !0x3f,
            },
            MInst::AndImm32 {
                dst: VReg(2),
                src: VReg(1),
                imm: 0x3f,
            },
            MInst::AndImm32 {
                dst: VReg(3),
                src: VReg(0),
                imm: 0x00ff_ffff,
            },
            MInst::AndImm {
                dst: VReg(4),
                src: VReg(3),
                imm: 0xffff_ffff_ff00_ffff,
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
                src: VReg(4),
                size: OpSize::S64,
            },
            MInst::Return,
        ],
        5,
    );

    algebraic_simplify(&mut func);

    assert!(func.blocks[0].insts.iter().any(|instruction| matches!(
        instruction,
        MInst::LoadImm {
            dst: VReg(2),
            value: 0
        }
    )));
    assert!(func.blocks[0].insts.iter().any(|instruction| matches!(
        instruction,
        MInst::AndImm32 {
            dst: VReg(4),
            src: VReg(0),
            imm: 0x0000_ffff,
        }
    )));
    func.verify();
}

#[test]
fn optimize_combines_serial_masks_exposed_by_immediate_lowering() {
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: !0x3f,
            },
            MInst::And {
                dst: VReg(2),
                lhs: VReg(0),
                rhs: VReg(1),
            },
            MInst::LoadImm {
                dst: VReg(3),
                value: 0x3f,
            },
            MInst::And32 {
                dst: VReg(4),
                lhs: VReg(2),
                rhs: VReg(3),
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 8,
                src: VReg(4),
                size: OpSize::S64,
            },
            MInst::Return,
        ],
        64,
    );

    optimize(&mut func);

    assert!(func.blocks[0].insts.iter().any(|instruction| matches!(
        instruction,
        MInst::LoadImm {
            dst: VReg(4),
            value: 0
        }
    )));
    assert!(
        !func.blocks[0].insts.iter().any(|instruction| matches!(
            instruction,
            MInst::AndImm { .. } | MInst::AndImm32 { .. }
        ))
    );
    func.verify();
}

#[test]
fn algebraic_simplify_keeps_shared_leaf_masks() {
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S8,
            },
            MInst::Load {
                dst: VReg(1),
                base: BaseReg::SimState,
                offset: 1,
                size: OpSize::S8,
            },
            MInst::Load {
                dst: VReg(2),
                base: BaseReg::SimState,
                offset: 2,
                size: OpSize::S8,
            },
            MInst::AndImm32 {
                dst: VReg(3),
                src: VReg(0),
                imm: 1,
            },
            MInst::AndImm32 {
                dst: VReg(4),
                src: VReg(1),
                imm: 1,
            },
            MInst::AndImm32 {
                dst: VReg(5),
                src: VReg(2),
                imm: 1,
            },
            MInst::And {
                dst: VReg(6),
                lhs: VReg(3),
                rhs: VReg(4),
            },
            MInst::And {
                dst: VReg(7),
                lhs: VReg(3),
                rhs: VReg(5),
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 8,
                src: VReg(6),
                size: OpSize::S8,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 9,
                src: VReg(7),
                size: OpSize::S8,
            },
            MInst::Return,
        ],
        8,
    );
    let original = func.blocks[0].insts.clone();

    algebraic_simplify(&mut func);

    assert_eq!(func.blocks[0].insts, original);
    func.verify();
}

#[test]
fn word32_algebraic_identities_keep_their_zero_extension() {
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 0,
            },
            MInst::LoadImm {
                dst: VReg(2),
                value: 1,
            },
            MInst::LoadImm {
                dst: VReg(3),
                value: u32::MAX as u64,
            },
            MInst::Add32 {
                dst: VReg(4),
                lhs: VReg(0),
                rhs: VReg(1),
            },
            MInst::Sub32 {
                dst: VReg(5),
                lhs: VReg(0),
                rhs: VReg(1),
            },
            MInst::Mul32 {
                dst: VReg(6),
                lhs: VReg(0),
                rhs: VReg(2),
            },
            MInst::And32 {
                dst: VReg(7),
                lhs: VReg(0),
                rhs: VReg(3),
            },
            MInst::Or32 {
                dst: VReg(8),
                lhs: VReg(0),
                rhs: VReg(1),
            },
            MInst::Xor32 {
                dst: VReg(9),
                lhs: VReg(0),
                rhs: VReg(1),
            },
            MInst::AndImm32 {
                dst: VReg(10),
                src: VReg(0),
                imm: u32::MAX,
            },
            MInst::Return,
        ],
        11,
    );

    algebraic_simplify(&mut func);

    for (index, dst) in (4..=10).enumerate() {
        assert!(
            matches!(
                func.blocks[0].insts[index + 4],
                MInst::Mov32 {
                    dst: actual_dst,
                    src: VReg(0)
                } if actual_dst == VReg(dst)
            ),
            "word32 identity at v{dst} lost its zero extension"
        );
    }
}

#[test]
fn redundant_mask_elimination_keeps_mask_after_subtraction() {
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S8,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 0,
            },
            MInst::Sub {
                dst: VReg(2),
                lhs: VReg(1),
                rhs: VReg(0),
            },
            MInst::AndImm {
                dst: VReg(3),
                src: VReg(2),
                imm: 0x1ff,
            },
            MInst::Return,
        ],
        4,
    );

    redundant_mask_eliminate(&mut func);

    // `0 - 1` is all ones, not a nine-bit result.  The mask is needed.
    assert!(matches!(
        func.blocks[0].insts[3],
        MInst::AndImm {
            dst: VReg(3),
            src: VReg(2),
            imm: 0x1ff,
        }
    ));
}

#[test]
fn redundant_mask_elimination_keeps_mask_after_unchecked_bsr() {
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
            },
            MInst::Bsr {
                dst: VReg(1),
                src: VReg(0),
            },
            MInst::AndImm {
                dst: VReg(2),
                src: VReg(1),
                imm: 0x3f,
            },
            MInst::Return,
        ],
        3,
    );

    redundant_mask_eliminate(&mut func);

    assert!(matches!(
        func.blocks[0].insts[2],
        MInst::AndImm {
            dst: VReg(2),
            src: VReg(1),
            imm: 0x3f,
        }
    ));
}

#[test]
fn redundant_word32_register_mask_is_eliminated() {
    let mask = 0x3fff_ffff;
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
            },
            MInst::AndImm32 {
                dst: VReg(1),
                src: VReg(0),
                imm: mask,
            },
            MInst::LoadImm {
                dst: VReg(2),
                value: u64::from(mask),
            },
            MInst::And32 {
                dst: VReg(3),
                lhs: VReg(1),
                rhs: VReg(2),
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

    redundant_mask_eliminate(&mut func);

    assert!(matches!(
        func.blocks[0].insts[3],
        MInst::Mov {
            dst: VReg(3),
            src: VReg(1),
        }
    ));
}

#[test]
fn redundant_word32_mask_preserves_required_zero_extension() {
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S8,
            },
            MInst::ShlImm {
                dst: VReg(1),
                src: VReg(0),
                imm: 40,
            },
            MInst::AndImm32 {
                dst: VReg(2),
                src: VReg(1),
                imm: 0xff,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 8,
                src: VReg(2),
                size: OpSize::S64,
            },
            MInst::Return,
        ],
        3,
    );

    redundant_mask_eliminate(&mut func);

    assert!(matches!(
        func.blocks[0].insts[2],
        MInst::Mov32 {
            dst: VReg(2),
            src: VReg(1),
        }
    ));
}

#[test]
fn redundant_mask_elimination_follows_phi_across_blocks() {
    let mut func = make_func(Vec::new(), 4);
    func.blocks.clear();

    let mut left = MBlock::new(BlockId(0));
    left.insts = vec![
        MInst::Load {
            dst: VReg(0),
            base: BaseReg::SimState,
            offset: 0,
            size: OpSize::S8,
        },
        MInst::Jump { target: BlockId(2) },
    ];
    let mut right = MBlock::new(BlockId(1));
    right.insts = vec![
        MInst::Load {
            dst: VReg(1),
            base: BaseReg::SimState,
            offset: 8,
            size: OpSize::S16,
        },
        MInst::Jump { target: BlockId(2) },
    ];
    let mut join = MBlock::new(BlockId(2));
    join.phis.push(PhiNode {
        dst: VReg(2),
        sources: vec![(BlockId(0), VReg(0)), (BlockId(1), VReg(1))],
    });
    join.insts = vec![
        MInst::AndImm {
            dst: VReg(3),
            src: VReg(2),
            imm: u64::from(u16::MAX),
        },
        MInst::Return,
    ];
    func.push_block(left);
    func.push_block(right);
    func.push_block(join);

    redundant_mask_eliminate(&mut func);

    assert!(matches!(
        func.blocks[2].insts[0],
        MInst::Mov {
            dst: VReg(3),
            src: VReg(2),
        }
    ));
}

#[test]
fn redundant_mask_elimination_keeps_mask_for_wide_phi_arm() {
    let mut func = make_func(Vec::new(), 4);
    func.blocks.clear();

    let mut left = MBlock::new(BlockId(0));
    left.insts = vec![
        MInst::Load {
            dst: VReg(0),
            base: BaseReg::SimState,
            offset: 0,
            size: OpSize::S8,
        },
        MInst::Jump { target: BlockId(2) },
    ];
    let mut right = MBlock::new(BlockId(1));
    right.insts = vec![
        MInst::Load {
            dst: VReg(1),
            base: BaseReg::SimState,
            offset: 8,
            size: OpSize::S64,
        },
        MInst::Jump { target: BlockId(2) },
    ];
    let mut join = MBlock::new(BlockId(2));
    join.phis.push(PhiNode {
        dst: VReg(2),
        sources: vec![(BlockId(0), VReg(0)), (BlockId(1), VReg(1))],
    });
    join.insts = vec![
        MInst::AndImm {
            dst: VReg(3),
            src: VReg(2),
            imm: u64::from(u16::MAX),
        },
        MInst::Return,
    ];
    func.push_block(left);
    func.push_block(right);
    func.push_block(join);

    redundant_mask_eliminate(&mut func);

    assert!(matches!(
        func.blocks[2].insts[0],
        MInst::AndImm {
            dst: VReg(3),
            src: VReg(2),
            imm,
        } if imm == u64::from(u16::MAX)
    ));
}

#[test]
fn repeated_large_register_mask_is_eliminated() {
    let mask = 0x00ff_00ff_00ff_00ff;
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: mask,
            },
            MInst::And {
                dst: VReg(2),
                lhs: VReg(0),
                rhs: VReg(1),
            },
            MInst::And {
                dst: VReg(3),
                lhs: VReg(2),
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

    redundant_mask_eliminate(&mut func);

    assert!(matches!(
        func.blocks[0].insts[3],
        MInst::Mov {
            dst: VReg(3),
            src: VReg(2),
        }
    ));
}
