use super::*;

#[test]
fn post_regalloc_peephole_folds_adjacent_single_use_cmp() {
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
            MInst::Cmp {
                dst: VReg(2),
                lhs: VReg(0),
                rhs: VReg(1),
                kind: CmpKind::Ne,
            },
            MInst::Return,
        ],
        3,
    );

    post_regalloc_peephole(&mut func, &AssignmentMap::default());

    assert!(matches!(
        func.blocks[0].insts[1],
        MInst::CmpImm {
            lhs: VReg(0),
            imm: 0,
            kind: CmpKind::Ne,
            ..
        }
    ));
    assert_eq!(func.blocks[0].insts.len(), 3);
}

#[test]
fn post_regalloc_peephole_folds_width_normalization_into_unsigned_load() {
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 1,
                size: OpSize::S8,
            },
            MInst::AndImm32 {
                dst: VReg(1),
                src: VReg(0),
                imm: 0xff,
            },
            MInst::Load {
                dst: VReg(2),
                base: BaseReg::SimState,
                offset: 2,
                size: OpSize::S16,
            },
            MInst::AndImm32 {
                dst: VReg(3),
                src: VReg(2),
                imm: 0xffff,
            },
            MInst::Load {
                dst: VReg(4),
                base: BaseReg::SimState,
                offset: 4,
                size: OpSize::S32,
            },
            MInst::Mov32 {
                dst: VReg(5),
                src: VReg(4),
            },
            MInst::Load {
                dst: VReg(6),
                base: BaseReg::SimState,
                offset: 8,
                size: OpSize::S64,
            },
            MInst::Mov {
                dst: VReg(7),
                src: VReg(6),
            },
            MInst::Return,
        ],
        8,
    );

    post_regalloc_peephole(&mut func, &AssignmentMap::default());

    let expected = [
        (VReg(1), 1, OpSize::S8),
        (VReg(3), 2, OpSize::S16),
        (VReg(5), 4, OpSize::S32),
        (VReg(7), 8, OpSize::S64),
    ];
    assert_eq!(func.blocks[0].insts.len(), expected.len() + 1);
    for (inst, (dst, offset, size)) in func.blocks[0].insts.iter().zip(expected) {
        assert!(matches!(
            inst,
            MInst::Load {
                dst: actual_dst,
                base: BaseReg::SimState,
                offset: actual_offset,
                size: actual_size,
            } if *actual_dst == dst && *actual_offset == offset && *actual_size == size
        ));
    }
}

#[test]
fn post_regalloc_peephole_drops_load_clobbered_by_same_register_constant() {
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 0x78,
                size: OpSize::S64,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 0x5555_5555_5555_5555,
            },
            MInst::And {
                dst: VReg(2),
                lhs: VReg(0),
                rhs: VReg(1),
            },
            MInst::Or {
                dst: VReg(2),
                lhs: VReg(2),
                rhs: VReg(1),
            },
            MInst::Return,
        ],
        3,
    );
    let mut assignment = AssignmentMap::default();
    assignment.set(VReg(0), PhysReg::R8);
    assignment.set(VReg(1), PhysReg::R8);
    assignment.set(VReg(2), PhysReg::R9);

    post_regalloc_peephole(&mut func, &assignment);

    assert_eq!(func.blocks[0].insts.len(), 4);
    assert!(matches!(
        func.blocks[0].insts[0],
        MInst::LoadImm {
            dst: VReg(1),
            value: 0x5555_5555_5555_5555,
        }
    ));
    assert!(matches!(
        func.blocks[0].insts[1],
        MInst::And {
            dst: VReg(2),
            lhs: VReg(1),
            rhs: VReg(1),
        }
    ));
    assert!(matches!(
        func.blocks[0].insts[2],
        MInst::Or {
            dst: VReg(2),
            lhs: VReg(2),
            rhs: VReg(1),
        }
    ));
}

#[test]
fn post_regalloc_peephole_keeps_load_when_constant_uses_other_register() {
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 0x78,
                size: OpSize::S64,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 7,
            },
            MInst::And {
                dst: VReg(2),
                lhs: VReg(0),
                rhs: VReg(1),
            },
            MInst::Or {
                dst: VReg(2),
                lhs: VReg(2),
                rhs: VReg(1),
            },
            MInst::Return,
        ],
        3,
    );
    let mut assignment = AssignmentMap::default();
    assignment.set(VReg(0), PhysReg::R8);
    assignment.set(VReg(1), PhysReg::R9);
    assignment.set(VReg(2), PhysReg::R10);

    post_regalloc_peephole(&mut func, &assignment);

    assert_eq!(func.blocks[0].insts.len(), 5);
    assert!(matches!(
        func.blocks[0].insts[0],
        MInst::Load { dst: VReg(0), .. }
    ));
    assert!(matches!(
        func.blocks[0].insts[2],
        MInst::And {
            lhs: VReg(0),
            rhs: VReg(1),
            ..
        }
    ));
}

#[test]
fn post_regalloc_peephole_keeps_load_whose_value_is_used_before_the_constant() {
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 0x78,
                size: OpSize::S64,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 7,
            },
            MInst::And {
                dst: VReg(2),
                lhs: VReg(0),
                rhs: VReg(1),
            },
            MInst::Return,
        ],
        3,
    );
    let mut assignment = AssignmentMap::default();
    // Same physical register, but the loaded value feeds an instruction
    // between the load and the rematerialized constant.
    assignment.set(VReg(0), PhysReg::R8);
    assignment.set(VReg(1), PhysReg::R8);
    assignment.set(VReg(2), PhysReg::R9);
    func.blocks[0].insts.swap(1, 2);

    post_regalloc_peephole(&mut func, &assignment);

    assert_eq!(func.blocks[0].insts.len(), 4);
    assert!(matches!(func.blocks[0].insts[0], MInst::Load { .. }));
}

#[test]
fn post_regalloc_peephole_keeps_load_with_cross_block_or_phi_uses() {
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
            offset: 0x78,
            size: OpSize::S64,
        },
        MInst::LoadImm {
            dst: VReg(1),
            value: 7,
        },
    ];
    func.push_block(entry);
    let mut exit = MBlock::new(BlockId(1));
    exit.phis.push(PhiNode {
        dst: VReg(2),
        sources: vec![(BlockId(0), VReg(0))],
    });
    func.push_block(exit);

    let mut assignment = AssignmentMap::default();
    assignment.set(VReg(0), PhysReg::R8);
    assignment.set(VReg(1), PhysReg::R8);
    assignment.set(VReg(2), PhysReg::R9);

    post_regalloc_peephole(&mut func, &assignment);

    assert!(matches!(func.blocks[0].insts[0], MInst::Load { .. }));
    assert_eq!(func.blocks[1].phis[0].sources, vec![(BlockId(0), VReg(0))],);
}

#[test]
fn late_state_load_cse_repairs_copy_folding_without_changing_assignment() {
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
                offset: 0,
                size: OpSize::S64,
            },
            MInst::Load {
                dst: VReg(2),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 8,
                src: VReg(0),
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 16,
                src: VReg(1),
                size: OpSize::S64,
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
    let mut assignment = AssignmentMap::default();
    assignment.set(VReg(0), PhysReg::R10);
    assignment.set(VReg(1), PhysReg::R9);
    assignment.set(VReg(2), PhysReg::R8);

    assert_eq!(post_regalloc_direct_load_cse(&mut func, &assignment), 2);
    post_regalloc_peephole(&mut func, &AssignmentMap::default());
    post_regalloc_cleanup(&mut func);
    post_regalloc_direct_load_cse(&mut func, &assignment);

    assert_eq!(
        func.blocks[0]
            .insts
            .iter()
            .filter(|instruction| matches!(instruction, MInst::Load { .. }))
            .count(),
        1
    );
    assert_eq!(
        func.blocks[0]
            .insts
            .iter()
            .filter(|instruction| matches!(instruction, MInst::Mov { .. }))
            .count(),
        2
    );
    assert!(
        func.blocks[0].insts.iter().all(|instruction| {
            !matches!(instruction, MInst::Mov { src, .. } if *src != VReg(0))
        })
    );
    crate::native::regalloc::verify_assignment(&func, &assignment).unwrap();
}

#[test]
fn late_state_load_cse_prefers_value_already_in_destination_register() {
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
                offset: 8,
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
                offset: 16,
                src: VReg(1),
                size: OpSize::S64,
            },
            MInst::Load {
                dst: VReg(2),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
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
    let mut assignment = AssignmentMap::default();
    assignment.set(VReg(0), PhysReg::R9);
    assignment.set(VReg(1), PhysReg::R10);
    assignment.set(VReg(2), PhysReg::R10);

    assert_eq!(post_regalloc_direct_load_cse(&mut func, &assignment), 2);
    assert!(matches!(
        func.blocks[0].insts[2],
        MInst::Mov {
            dst: VReg(1),
            src: VReg(0)
        }
    ));
    assert!(matches!(
        func.blocks[0].insts[4],
        MInst::Mov {
            dst: VReg(2),
            src: VReg(1)
        }
    ));
    crate::native::regalloc::verify_assignment(&func, &assignment).unwrap();
}

#[test]
fn late_direct_load_cse_reuses_stack_home_until_overlapping_store() {
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::StackFrame,
                offset: 0,
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 0,
                src: VReg(0),
                size: OpSize::S64,
            },
            MInst::Load {
                dst: VReg(1),
                base: BaseReg::StackFrame,
                offset: 0,
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::StackFrame,
                offset: 0,
                src: VReg(1),
                size: OpSize::S64,
            },
            MInst::Load {
                dst: VReg(2),
                base: BaseReg::StackFrame,
                offset: 0,
                size: OpSize::S64,
            },
            MInst::Return,
        ],
        3,
    );
    let mut assignment = AssignmentMap::default();
    assignment.set(VReg(0), PhysReg::R9);
    assignment.set(VReg(1), PhysReg::R10);
    assignment.set(VReg(2), PhysReg::R8);

    assert_eq!(post_regalloc_direct_load_cse(&mut func, &assignment), 1);
    assert!(matches!(
        func.blocks[0].insts[2],
        MInst::Mov {
            dst: VReg(1),
            src: VReg(0)
        }
    ));
    assert!(matches!(
        func.blocks[0].insts[4],
        MInst::Load {
            dst: VReg(2),
            base: BaseReg::StackFrame,
            offset: 0,
            size: OpSize::S64
        }
    ));
    crate::native::regalloc::verify_assignment(&func, &assignment).unwrap();
}

#[test]
fn post_regalloc_peephole_keeps_a_multi_use_loaded_value() {
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 1,
                size: OpSize::S8,
            },
            MInst::AndImm32 {
                dst: VReg(1),
                src: VReg(0),
                imm: 0xff,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 2,
                src: VReg(0),
                size: OpSize::S8,
            },
            MInst::Return,
        ],
        2,
    );

    post_regalloc_peephole(&mut func, &AssignmentMap::default());

    assert!(matches!(
        func.blocks[0].insts.as_slice(),
        [
            MInst::Load { dst: VReg(0), .. },
            MInst::AndImm32 {
                dst: VReg(1),
                src: VReg(0),
                ..
            },
            MInst::Store { src: VReg(0), .. },
            MInst::Return
        ]
    ));
}

#[test]
fn post_regalloc_cleanup_removes_dead_remats_and_equal_select_predicates() {
    let mut func = make_func(
        vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 0x100,
            },
            MInst::Load {
                dst: VReg(1),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
            },
            MInst::Load {
                dst: VReg(2),
                base: BaseReg::SimState,
                offset: 8,
                size: OpSize::S64,
            },
            MInst::Select {
                dst: VReg(3),
                cond: VReg(2),
                true_val: VReg(1),
                false_val: VReg(1),
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

    post_regalloc_peephole(&mut func, &AssignmentMap::default());
    post_regalloc_cleanup(&mut func);

    assert!(matches!(
        func.blocks[0].insts.as_slice(),
        [
            MInst::Load {
                dst: VReg(1),
                offset: 0,
                ..
            },
            MInst::Mov {
                dst: VReg(3),
                src: VReg(1)
            },
            MInst::Store {
                src: VReg(3),
                offset: 16,
                ..
            },
            MInst::Return
        ]
    ));
}

#[test]
fn post_regalloc_peephole_folds_unsigned_load_copies_at_every_machine_width() {
    let mut instructions = Vec::new();
    for (index, size) in [OpSize::S8, OpSize::S16, OpSize::S32, OpSize::S64]
        .into_iter()
        .enumerate()
    {
        let loaded = VReg((index * 2) as u32);
        let destination = VReg((index * 2 + 1) as u32);
        instructions.push(MInst::Load {
            dst: loaded,
            base: BaseReg::SimState,
            offset: index as i32 * 8,
            size,
        });
        instructions.push(MInst::Mov {
            dst: destination,
            src: loaded,
        });
    }
    instructions.push(MInst::Return);
    let mut func = make_func(instructions, 8);

    post_regalloc_peephole(&mut func, &AssignmentMap::default());

    assert_eq!(func.blocks[0].insts.len(), 5);
    for (index, (inst, size)) in func.blocks[0]
        .insts
        .iter()
        .zip([OpSize::S8, OpSize::S16, OpSize::S32, OpSize::S64])
        .enumerate()
    {
        assert!(matches!(
            inst,
            MInst::Load {
                dst,
                base: BaseReg::SimState,
                offset,
                size: actual_size,
            } if *dst == VReg((index * 2 + 1) as u32)
                && *offset == index as i32 * 8
                && *actual_size == size
        ));
    }
}

#[test]
fn post_regalloc_peephole_keeps_multi_use_constant() {
    let mut func = make_func(
        vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 1,
            },
            MInst::Add {
                dst: VReg(1),
                lhs: VReg(2),
                rhs: VReg(0),
            },
            MInst::Or {
                dst: VReg(3),
                lhs: VReg(1),
                rhs: VReg(0),
            },
            MInst::Return,
        ],
        4,
    );

    post_regalloc_peephole(&mut func, &AssignmentMap::default());

    assert!(matches!(func.blocks[0].insts[0], MInst::LoadImm { .. }));
    assert_eq!(func.blocks[0].insts.len(), 4);
}

#[test]
fn post_regalloc_peephole_folds_nearby_single_use_imm() {
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
                value: 7,
            },
            MInst::Store {
                base: BaseReg::StackFrame,
                offset: 0,
                src: VReg(0),
                size: OpSize::S64,
            },
            MInst::ShrImm {
                dst: VReg(2),
                src: VReg(0),
                imm: 3,
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
                size: OpSize::S8,
            },
            MInst::Return,
        ],
        4,
    );

    post_regalloc_peephole(&mut func, &AssignmentMap::default());

    assert!(
        !func.blocks[0]
            .insts
            .iter()
            .any(|inst| matches!(inst, MInst::LoadImm { dst: VReg(1), .. })),
        "{:#?}",
        func.blocks[0].insts
    );
    assert!(func.blocks[0].insts.iter().any(|inst| matches!(
        inst,
        MInst::AndImm {
            dst: VReg(3),
            src: VReg(2),
            imm: 7
        }
    )));
}

#[test]
fn post_regalloc_peephole_folds_adjacent_alu_immediates() {
    let mut func = make_func(
        vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 5,
            },
            MInst::Add {
                dst: VReg(1),
                lhs: VReg(0),
                rhs: VReg(2),
            },
            MInst::LoadImm {
                dst: VReg(3),
                value: 0xffff_ffff,
            },
            MInst::And {
                dst: VReg(4),
                lhs: VReg(5),
                rhs: VReg(3),
            },
            MInst::LoadImm {
                dst: VReg(6),
                value: 31,
            },
            MInst::Shr {
                dst: VReg(7),
                lhs: VReg(8),
                rhs: VReg(6),
            },
            MInst::Return,
        ],
        9,
    );

    post_regalloc_peephole(&mut func, &AssignmentMap::default());

    assert!(matches!(
        func.blocks[0].insts[0],
        MInst::AddImm {
            dst: VReg(1),
            src: VReg(2),
            imm: 5,
        }
    ));
    assert!(matches!(
        func.blocks[0].insts[1],
        MInst::AndImm {
            dst: VReg(4),
            src: VReg(5),
            imm: 0xffff_ffff,
        }
    ));
    assert!(matches!(
        func.blocks[0].insts[2],
        MInst::ShrImm {
            dst: VReg(7),
            src: VReg(8),
            imm: 31,
        }
    ));
    assert_eq!(func.blocks[0].insts.len(), 4);
}

#[test]
fn post_regalloc_peephole_rejects_unsupported_immediates() {
    let mut func = make_func(
        vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: i32::MAX as u64 + 1,
            },
            MInst::Or {
                dst: VReg(1),
                lhs: VReg(2),
                rhs: VReg(0),
            },
            MInst::LoadImm {
                dst: VReg(3),
                value: 64,
            },
            MInst::Shl {
                dst: VReg(4),
                lhs: VReg(5),
                rhs: VReg(3),
            },
            MInst::Return,
        ],
        6,
    );

    post_regalloc_peephole(&mut func, &AssignmentMap::default());

    assert!(matches!(func.blocks[0].insts[0], MInst::LoadImm { .. }));
    assert!(matches!(func.blocks[0].insts[1], MInst::Or { .. }));
    assert!(matches!(func.blocks[0].insts[2], MInst::LoadImm { .. }));
    assert!(matches!(func.blocks[0].insts[3], MInst::Shl { .. }));
    assert_eq!(func.blocks[0].insts.len(), 5);
}

#[test]
fn post_regalloc_peephole_folds_sign_extended_immediates() {
    let mut func = make_func(
        vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: u64::MAX - 1,
            },
            MInst::And {
                dst: VReg(1),
                lhs: VReg(2),
                rhs: VReg(0),
            },
            MInst::LoadImm {
                dst: VReg(3),
                value: u64::MAX,
            },
            MInst::Sub {
                dst: VReg(4),
                lhs: VReg(5),
                rhs: VReg(3),
            },
            MInst::LoadImm {
                dst: VReg(6),
                value: u64::MAX,
            },
            MInst::Cmp {
                dst: VReg(7),
                lhs: VReg(8),
                rhs: VReg(6),
                kind: CmpKind::Eq,
            },
            MInst::Return,
        ],
        9,
    );

    post_regalloc_peephole(&mut func, &AssignmentMap::default());

    assert!(matches!(
        func.blocks[0].insts[0],
        MInst::AndImm {
            dst: VReg(1),
            src: VReg(2),
            imm: 0xffff_ffff_ffff_fffe,
        }
    ));
    assert!(matches!(
        func.blocks[0].insts[1],
        MInst::SubImm {
            dst: VReg(4),
            src: VReg(5),
            imm: -1,
        }
    ));
    assert!(matches!(
        func.blocks[0].insts[2],
        MInst::CmpImm {
            dst: VReg(7),
            lhs: VReg(8),
            imm: -1,
            kind: CmpKind::Eq,
        }
    ));
    assert_eq!(func.blocks[0].insts.len(), 4);
}

#[test]
fn post_regalloc_cleanup_threads_empty_edge_blocks() {
    let mut func = make_func(Vec::new(), 1);
    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm {
        dst: VReg(0),
        value: 1,
    });
    entry.push(MInst::BranchPred {
        predicate: BranchPredicate::CompareImm {
            lhs: VReg(0),
            imm: 0,
            kind: CmpKind::Ne,
        },
        true_bb: BlockId(1),
        false_bb: BlockId(2),
    });

    let mut true_edge = MBlock::new(BlockId(1));
    true_edge.push(MInst::Jump { target: BlockId(3) });

    let mut false_edge = MBlock::new(BlockId(2));
    false_edge.push(MInst::Jump { target: BlockId(4) });

    let mut true_target = MBlock::new(BlockId(3));
    true_target.push(MInst::Return);

    let mut false_target = MBlock::new(BlockId(4));
    false_target.push(MInst::Return);

    func.blocks = vec![entry, true_edge, false_edge, true_target, false_target];

    post_regalloc_cleanup(&mut func);

    assert_eq!(func.verify_result(), Ok(()));
    assert!(matches!(
        func.blocks[0].insts.last(),
        Some(MInst::BranchPred {
            true_bb: BlockId(3),
            false_bb: BlockId(4),
            ..
        })
    ));
    assert!(!func.blocks.iter().any(|block| block.id == BlockId(1)));
    assert!(!func.blocks.iter().any(|block| block.id == BlockId(2)));
}
