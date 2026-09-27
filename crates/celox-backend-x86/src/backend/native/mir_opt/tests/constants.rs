use super::*;

#[test]
fn multiply_immediates_preserve_full_and_low_word_constant_semantics() {
    for word32 in [false, true] {
        for constant_on_left in [false, true] {
            for value in [
                0,
                3,
                30,
                i32::MAX as u64,
                0x8000_0000,
                0xffff_ffff,
                i32::MIN as u64,
                u64::MAX,
                0x1234_5678_89ab_cdef,
            ] {
                let (lhs, rhs) = if constant_on_left {
                    (VReg(0), VReg(1))
                } else {
                    (VReg(1), VReg(0))
                };
                let inst = if word32 {
                    MInst::Mul32 {
                        dst: VReg(2),
                        lhs,
                        rhs,
                    }
                } else {
                    MInst::Mul {
                        dst: VReg(2),
                        lhs,
                        rhs,
                    }
                };
                let folded = fold_imm_use(&inst, VReg(0), value);
                if !word32 && (value as i32 as u64) != value {
                    assert!(
                        folded.is_none(),
                        "unencodable full-word constant {value:#x}"
                    );
                    continue;
                }
                let folded = folded.unwrap();
                assert_eq!(
                    folded.uses().iter().copied().collect::<Vec<_>>(),
                    vec![VReg(1)]
                );
                let imm = match folded {
                    MInst::MulImm {
                        dst: VReg(2),
                        src: VReg(1),
                        imm,
                    } if !word32 => imm,
                    MInst::MulImm32 {
                        dst: VReg(2),
                        src: VReg(1),
                        imm,
                    } if word32 => imm,
                    other => panic!("incorrect immediate form: {other:?}"),
                };
                for input in [0u64, 1, 0x8000_0000, 1 << 32, 1 << 63, u64::MAX] {
                    let expected = input.wrapping_mul(value);
                    let actual = input.wrapping_mul(imm as u64);
                    assert_eq!(
                        if word32 { actual as u32 as u64 } else { actual },
                        if word32 {
                            expected as u32 as u64
                        } else {
                            expected
                        }
                    );
                }
            }
        }
    }
}

#[test]
fn word32_constant_fold_truncates_inputs_and_zero_extends_results() {
    let mut func = make_func(
        vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: u64::MAX,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 2,
            },
            MInst::Add32 {
                dst: VReg(2),
                lhs: VReg(0),
                rhs: VReg(1),
            },
            MInst::Sub32 {
                dst: VReg(3),
                lhs: VReg(0),
                rhs: VReg(1),
            },
            MInst::Mul32 {
                dst: VReg(4),
                lhs: VReg(0),
                rhs: VReg(1),
            },
            MInst::And32 {
                dst: VReg(5),
                lhs: VReg(0),
                rhs: VReg(1),
            },
            MInst::Or32 {
                dst: VReg(6),
                lhs: VReg(0),
                rhs: VReg(1),
            },
            MInst::Xor32 {
                dst: VReg(7),
                lhs: VReg(0),
                rhs: VReg(1),
            },
            MInst::AndImm32 {
                dst: VReg(8),
                src: VReg(0),
                imm: 0x8000_0000,
            },
            MInst::Return,
        ],
        9,
    );

    constant_fold(&mut func);

    for (index, expected) in [
        1,
        0xffff_fffd,
        0xffff_fffe,
        2,
        0xffff_ffff,
        0xffff_fffd,
        0x8000_0000,
    ]
    .into_iter()
    .enumerate()
    {
        let dst = VReg(index as u32 + 2);
        assert!(
            matches!(
                func.blocks[0].insts[index + 2],
                MInst::LoadImm {
                    dst: actual_dst,
                    value
                } if actual_dst == dst && value == expected
            ),
            "word32 constant fold for {dst} produced the wrong value"
        );
    }
}

#[test]
fn folded_constants_are_marked_rematerializable() {
    let mut func = make_func(
        vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 3,
            },
            MInst::ShlImm {
                dst: VReg(1),
                src: VReg(0),
                imm: 2,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 0,
                src: VReg(1),
                size: OpSize::S64,
            },
            MInst::Return,
        ],
        2,
    );

    optimize(&mut func);

    let constant = func.blocks[0]
        .insts
        .iter()
        .find_map(|inst| match inst {
            MInst::LoadImm { dst, value: 12 } => Some(*dst),
            _ => None,
        })
        .expect("the shift of a known constant must fold");
    assert!(matches!(
        func.spill_desc(constant).map(|desc| &desc.kind),
        Some(SpillKind::Remat { value: 12 })
    ));
}

#[test]
fn lower_to_imm_forms_uses_sign_extended_immediates() {
    let mut func = make_func(
        vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: u64::MAX,
            },
            MInst::Add {
                dst: VReg(1),
                lhs: VReg(2),
                rhs: VReg(0),
            },
            MInst::LoadImm {
                dst: VReg(3),
                value: 0x8000_0000,
            },
            MInst::Sub {
                dst: VReg(4),
                lhs: VReg(5),
                rhs: VReg(3),
            },
            MInst::Return,
        ],
        6,
    );

    lower_to_imm_forms(&mut func);

    assert!(matches!(
        func.blocks[0].insts[1],
        MInst::AddImm {
            dst: VReg(1),
            src: VReg(2),
            imm: -1,
        }
    ));
    assert!(matches!(func.blocks[0].insts[3], MInst::Sub { .. }));
}

#[test]
fn lower_to_imm_forms_folds_multi_use_and_constants() {
    let mut func = make_func(
        vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 7,
            },
            MInst::And {
                dst: VReg(1),
                lhs: VReg(2),
                rhs: VReg(0),
            },
            MInst::And {
                dst: VReg(3),
                lhs: VReg(4),
                rhs: VReg(0),
            },
            MInst::Return,
        ],
        5,
    );

    lower_to_imm_forms(&mut func);

    assert!(matches!(
        func.blocks[0].insts[1],
        MInst::AndImm {
            dst: VReg(1),
            src: VReg(2),
            imm: 7,
        }
    ));
    assert!(matches!(
        func.blocks[0].insts[2],
        MInst::AndImm {
            dst: VReg(3),
            src: VReg(4),
            imm: 7,
        }
    ));
}

#[test]
fn lower_to_imm_forms_folds_word32_and_constant_low_word() {
    let mut func = make_func(
        vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 0xfeed_face_3fff_ffff,
            },
            MInst::And32 {
                dst: VReg(1),
                lhs: VReg(2),
                rhs: VReg(0),
            },
            MInst::Return,
        ],
        3,
    );

    lower_to_imm_forms(&mut func);

    assert!(matches!(
        func.blocks[0].insts[1],
        MInst::AndImm32 {
            dst: VReg(1),
            src: VReg(2),
            imm: 0x3fff_ffff,
        }
    ));
}

#[test]
fn lower_to_imm_forms_folds_constant_memory_indices() {
    let mut func = make_func(
        vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 24,
            },
            MInst::LoadIndexed {
                dst: VReg(1),
                base: BaseReg::SimState,
                offset: 16,
                index: VReg(0),
                scale: 1,
                size: OpSize::S64,
                alias_range: None,
            },
            MInst::StoreIndexed {
                base: BaseReg::SimState,
                offset: 32,
                index: VReg(0),
                src: VReg(2),
                size: OpSize::S64,
                alias_range: None,
            },
            MInst::Return,
        ],
        3,
    );

    lower_to_imm_forms(&mut func);

    assert!(matches!(
        func.blocks[0].insts[1],
        MInst::Load {
            dst: VReg(1),
            base: BaseReg::SimState,
            offset: 40,
            size: OpSize::S64,
        }
    ));
    assert!(matches!(
        func.blocks[0].insts[2],
        MInst::Store {
            base: BaseReg::SimState,
            offset: 56,
            src: VReg(2),
            size: OpSize::S64,
        }
    ));
}

#[test]
fn optimize_shares_repeated_immediate_index_calculation() {
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
                value: 3,
            },
            MInst::LoadImm {
                dst: VReg(2),
                value: 7,
            },
            MInst::Shr {
                dst: VReg(3),
                lhs: VReg(0),
                rhs: VReg(1),
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 16,
                src: VReg(3),
                size: OpSize::S64,
            },
            MInst::And32 {
                dst: VReg(4),
                lhs: VReg(0),
                rhs: VReg(2),
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 24,
                src: VReg(4),
                size: OpSize::S64,
            },
            MInst::Shr {
                dst: VReg(5),
                lhs: VReg(0),
                rhs: VReg(1),
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 32,
                src: VReg(5),
                size: OpSize::S64,
            },
            MInst::And32 {
                dst: VReg(6),
                lhs: VReg(0),
                rhs: VReg(2),
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 40,
                src: VReg(6),
                size: OpSize::S64,
            },
            MInst::Return,
        ],
        41,
    );

    optimize(&mut func);

    assert_eq!(
        func.blocks[0]
            .insts
            .iter()
            .filter(|inst| matches!(inst, MInst::ShrImm { imm: 3, .. }))
            .count(),
        1
    );
    let stored = func.blocks[0]
        .insts
        .iter()
        .filter_map(|inst| match inst {
            MInst::Store {
                base: BaseReg::SimState,
                offset: 16 | 32,
                src,
                size: OpSize::S64,
            } => Some(*src),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(stored.len(), 2);
    assert_eq!(stored[0], stored[1]);

    assert_eq!(
        func.blocks[0]
            .insts
            .iter()
            .filter(|inst| matches!(inst, MInst::AndImm32 { imm: 7, .. }))
            .count(),
        1
    );
    let stored = func.blocks[0]
        .insts
        .iter()
        .filter_map(|inst| match inst {
            MInst::Store {
                base: BaseReg::SimState,
                offset: 24 | 40,
                src,
                size: OpSize::S64,
            } => Some(*src),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(stored.len(), 2);
    assert_eq!(stored[0], stored[1]);
}
