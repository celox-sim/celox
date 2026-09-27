use super::*;

fn byte_load_pack(with_write_barrier: bool) -> MFunction {
    let mut insts = (0..8)
        .map(|byte| MInst::Load {
            dst: VReg(byte),
            base: BaseReg::SimState,
            offset: 32 + byte as i32,
            size: OpSize::S8,
        })
        .collect::<Vec<_>>();
    if with_write_barrier {
        insts.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 200,
            src: VReg(0),
            size: OpSize::S8,
        });
    }
    let mut accumulated = VReg(0);
    for byte in 1..8 {
        let shifted = VReg(7 + byte);
        insts.push(MInst::ShlImm {
            dst: shifted,
            src: VReg(byte),
            imm: byte as u8 * 8,
        });
        let merged = VReg(14 + byte);
        insts.push(MInst::Or {
            dst: merged,
            lhs: accumulated,
            rhs: shifted,
        });
        accumulated = merged;
    }
    insts.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 100,
        src: accumulated,
        size: OpSize::S64,
    });
    make_func(insts, 22)
}

#[test]
fn folds_contiguous_single_use_state_copies() {
    let mut insts = Vec::new();
    for index in 0..8 {
        insts.push(MInst::Load {
            dst: VReg(index),
            base: BaseReg::SimState,
            offset: 64 + index as i32 * 8,
            size: OpSize::S64,
        });
        insts.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 256 + index as i32 * 8,
            src: VReg(index),
            size: OpSize::S64,
        });
    }
    insts.push(MInst::Return);
    let mut func = make_func(insts, 8);

    fold_contiguous_memory_copies(&mut func);

    assert_eq!(
        func.blocks[0].insts,
        vec![
            MInst::MemCopy {
                src_offset: 64,
                dst_offset: 256,
                byte_len: 64,
            },
            MInst::Return,
        ]
    );
}

#[test]
fn does_not_fold_state_copy_when_loaded_value_has_another_use() {
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 64,
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 256,
                src: VReg(0),
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 512,
                src: VReg(0),
                size: OpSize::S64,
            },
            MInst::Load {
                dst: VReg(1),
                base: BaseReg::SimState,
                offset: 72,
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 264,
                src: VReg(1),
                size: OpSize::S64,
            },
            MInst::Return,
        ],
        2,
    );

    fold_contiguous_memory_copies(&mut func);

    assert!(
        !func.blocks[0]
            .insts
            .iter()
            .any(|inst| matches!(inst, MInst::MemCopy { .. }))
    );
}

#[test]
fn folds_little_endian_byte_load_pack_into_one_native_load() {
    let mut func = byte_load_pack(false);
    fold_contiguous_load_packs(&mut func);
    dead_code_eliminate(&mut func);

    assert!(matches!(
        func.blocks[0].insts.as_slice(),
        [
            MInst::Load {
                dst: VReg(21),
                base: BaseReg::SimState,
                offset: 32,
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 100,
                src: VReg(21),
                size: OpSize::S64,
            }
        ]
    ));
}

#[test]
fn contiguous_load_pack_does_not_cross_a_memory_write() {
    let mut func = byte_load_pack(true);
    fold_contiguous_load_packs(&mut func);
    dead_code_eliminate(&mut func);

    assert!(!func.blocks[0].insts.iter().any(|instruction| matches!(
        instruction,
        MInst::Load {
            offset: 32,
            size: OpSize::S64,
            ..
        }
    )));
    assert!(
        func.blocks[0]
            .insts
            .iter()
            .any(|instruction| matches!(instruction, MInst::Or { dst: VReg(21), .. }))
    );
}

#[test]
fn full_word_masked_merge_collapses_to_a_direct_store() {
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
            MInst::AndImm32 {
                dst: VReg(2),
                src: VReg(1),
                imm: 0,
            },
            MInst::Or {
                dst: VReg(3),
                lhs: VReg(2),
                rhs: VReg(0),
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

    optimize(&mut func);

    assert!(func.blocks[0].insts.iter().any(|inst| matches!(
        inst,
        MInst::Load {
            offset: 0,
            size: OpSize::S64,
            ..
        }
    )));
    assert!(!func.blocks[0].insts.iter().any(|inst| matches!(
        inst,
        MInst::Load {
            offset: 8,
            size: OpSize::S64,
            ..
        }
    )));
    assert!(func.blocks[0].insts.iter().any(|inst| matches!(
        inst,
        MInst::Store {
            offset: 8,
            src: VReg(0),
            size: OpSize::S64,
            ..
        }
    )));
    assert!(!func.blocks[0].insts.iter().any(|inst| matches!(
        inst,
        MInst::And { .. }
            | MInst::And32 { .. }
            | MInst::AndImm { .. }
            | MInst::AndImm32 { .. }
            | MInst::Or { .. }
            | MInst::Or32 { .. }
            | MInst::OrImm { .. }
    )));
}

#[test]
fn folded_indexed_load_executes_the_original_address_and_keeps_its_alias_range() {
    use crate::native::{emit, jit_mem::JitCode, regalloc};
    let alias_range = MemoryAliasRange::new(32, 160);
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
            },
            MInst::ShlImm {
                dst: VReg(1),
                src: VReg(0),
                imm: 2,
            },
            MInst::AddImm {
                dst: VReg(2),
                src: VReg(1),
                imm: 3,
            },
            MInst::LoadIndexed {
                dst: VReg(3),
                base: BaseReg::SimState,
                offset: 32,
                index: VReg(2),
                scale: 1,
                size: OpSize::S64,
                alias_range,
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
    fold_indexed_load_addresses(&mut func);
    assert!(
        matches!(func.blocks[0].insts[3], MInst::LoadIndexed { offset: 35, index: VReg(0), scale: 4, alias_range: range, .. } if range == alias_range)
    );
    dead_code_eliminate(&mut func);
    let allocation = regalloc::run_regalloc(&mut func).unwrap();
    let emitted = emit::emit(&func, &allocation.assignment, allocation.spill_frame_size).unwrap();
    let jit = JitCode::new(&emitted.code).unwrap();
    for index in 0..32u64 {
        let mut state = [0u8; 192];
        for (offset, byte) in state.iter_mut().enumerate().skip(32) {
            *byte = offset as u8 ^ 0x5a;
        }
        state[..8].copy_from_slice(&index.to_le_bytes());
        let address = 32 + (index as usize * 4 + 3);
        let expected = u64::from_le_bytes(state[address..address + 8].try_into().unwrap());
        assert_eq!(unsafe { jit.call(&mut state) }, 0);
        assert_eq!(
            u64::from_le_bytes(state[8..16].try_into().unwrap()),
            expected
        );
    }
}

#[test]
fn indexed_load_displacement_overflow_keeps_the_full_width_addition() {
    for (offset, displacement, scale) in [(i32::MAX, 1, 1), (i32::MIN, -1, 1), (0, i32::MAX, 8)] {
        let mut func = make_func(
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 0,
                    size: OpSize::S64,
                },
                MInst::AddImm {
                    dst: VReg(1),
                    src: VReg(0),
                    imm: displacement,
                },
                MInst::LoadIndexed {
                    dst: VReg(2),
                    base: BaseReg::SimState,
                    offset,
                    index: VReg(1),
                    scale,
                    size: OpSize::S64,
                    alias_range: None,
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
        fold_indexed_load_addresses(&mut func);
        assert!(
            matches!(func.blocks[0].insts[2], MInst::LoadIndexed { index: VReg(1), offset: original, .. } if original == offset)
        );
    }
}

#[test]
fn forwards_exact_store_to_load_in_block() {
    let mut func = make_func(
        vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 0x55,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 16,
                src: VReg(0),
                size: OpSize::S8,
            },
            MInst::Load {
                dst: VReg(1),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S8,
            },
            MInst::LoadImm {
                dst: VReg(2),
                value: 0x56,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 24,
                src: VReg(2),
                size: OpSize::S8,
            },
            MInst::Return,
        ],
        3,
    );

    optimize(&mut func);

    let insts = &func.blocks[0].insts;
    assert!(
        !insts.iter().any(|inst| matches!(inst, MInst::Load { .. })),
        "{insts:#?}"
    );
    assert!(
        insts.iter().any(|inst| matches!(
            inst,
            MInst::LoadImm {
                dst: VReg(2),
                value: 0x56,
            }
        )),
        "{insts:#?}"
    );
    assert!(
        insts.iter().any(|inst| matches!(
            inst,
            MInst::Store {
                base: BaseReg::SimState,
                offset: 24,
                src: VReg(2),
                size: OpSize::S8,
            }
        )),
        "{insts:#?}"
    );
    assert!(!insts.iter().any(|inst| matches!(
        inst,
        MInst::Load {
            dst: VReg(1),
            base: BaseReg::SimState,
            offset: 16,
            size: OpSize::S8,
        }
    )));
}

#[test]
fn preserves_overlapping_store_when_forwarding_later_load() {
    use crate::native::{emit, jit_mem::JitCode, regalloc};
    let mut func = make_func(
        vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 0x1122,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 0x33,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 16,
                src: VReg(0),
                size: OpSize::S16,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 17,
                src: VReg(1),
                size: OpSize::S8,
            },
            MInst::Load {
                dst: VReg(2),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S16,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 32,
                src: VReg(2),
                size: OpSize::S16,
            },
            MInst::Return,
        ],
        3,
    );

    optimize(&mut func);

    let allocation = regalloc::run_regalloc(&mut func).unwrap();
    let emitted = emit::emit(&func, &allocation.assignment, allocation.spill_frame_size).unwrap();
    let jit = JitCode::new(&emitted.code).unwrap();
    let mut state = vec![0xa5u8; emitted.required_state_size.max(48) as usize];
    assert_eq!(unsafe { jit.call(&mut state) }, 0);
    // The later byte replaces the high byte of 0x1122. Reusing that old
    // whole value would lose the overlapping store.
    assert_eq!(&state[16..18], &[0x22, 0x33]);
    assert_eq!(&state[32..34], &[0x22, 0x33]);
    assert_eq!(state[18], 0xa5);
    assert_eq!(state[34], 0xa5);
}

#[test]
fn eliminates_redundant_same_slot_store() {
    let mut func = make_func(
        vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 1,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 16,
                src: VReg(0),
                size: OpSize::S8,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 16,
                src: VReg(0),
                size: OpSize::S8,
            },
            MInst::Return,
        ],
        1,
    );

    optimize(&mut func);

    let store_count = func.blocks[0]
        .insts
        .iter()
        .filter(|inst| {
            matches!(
                inst,
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 16,
                    src: VReg(0),
                    size: OpSize::S8,
                }
            )
        })
        .count();
    assert_eq!(store_count, 1, "{:#?}", func.blocks[0].insts);
}

#[test]
fn eliminates_dead_store_overwritten_before_any_load() {
    let mut func = make_func(
        vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 1,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 16,
                src: VReg(0),
                size: OpSize::S8,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 2,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 16,
                src: VReg(1),
                size: OpSize::S8,
            },
            MInst::Return,
        ],
        2,
    );

    optimize(&mut func);

    assert!(
        !func.blocks[0].insts.iter().any(|inst| matches!(
            inst,
            MInst::Store {
                base: BaseReg::SimState,
                offset: 16,
                src: VReg(0),
                size: OpSize::S8,
            }
        )),
        "{:#?}",
        func.blocks[0].insts
    );
    assert!(func.blocks[0].insts.iter().any(|inst| matches!(
        inst,
        MInst::Store {
            base: BaseReg::SimState,
            offset: 16,
            src: VReg(1),
            size: OpSize::S8,
        }
    )));
}

#[test]
fn keeps_store_before_unknown_memory_access() {
    let mut func = make_func(
        vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 1,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 16,
                src: VReg(0),
                size: OpSize::S8,
            },
            MInst::Load {
                dst: VReg(1),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
            },
            MInst::LoadIndexed {
                dst: VReg(2),
                base: BaseReg::SimState,
                offset: 0,
                index: VReg(1),
                scale: 1,
                size: OpSize::S8,
                alias_range: None,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 24,
                src: VReg(2),
                size: OpSize::S8,
            },
            MInst::LoadImm {
                dst: VReg(3),
                value: 2,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 16,
                src: VReg(3),
                size: OpSize::S8,
            },
            MInst::Return,
        ],
        4,
    );

    optimize(&mut func);

    assert!(func.blocks[0].insts.iter().any(|inst| matches!(
        inst,
        MInst::Store {
            base: BaseReg::SimState,
            offset: 16,
            src: VReg(0),
            size: OpSize::S8,
        }
    )));
    assert!(func.blocks[0].insts.iter().any(|inst| matches!(
        inst,
        MInst::Store {
            base: BaseReg::SimState,
            offset: 16,
            src: VReg(3),
            size: OpSize::S8,
        }
    )));
}

#[test]
fn constant_index_folding_preserves_scale_and_rejects_displacement_overflow() {
    for scale in [1, 2, 4, 8] {
        let instruction = MInst::LoadIndexed {
            dst: VReg(1),
            base: BaseReg::SimState,
            offset: 64,
            index: VReg(0),
            scale,
            size: OpSize::S64,
            alias_range: MemoryAliasRange::new(0, 128),
        };
        for index in [-2i32, 0, 3] {
            assert!(matches!(fold_imm_use(&instruction, VReg(0), index as u64),
                Some(MInst::Load { offset, .. }) if offset == 64 + index * i32::from(scale)));
        }
        assert!(fold_imm_use(&instruction, VReg(0), i32::MAX as u64).is_none());
    }
}

#[test]
fn forwards_partial_load_from_recent_store() {
    let mut func = make_func(
        vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 0x3412,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 16,
                src: VReg(0),
                size: OpSize::S16,
            },
            MInst::Load {
                dst: VReg(1),
                base: BaseReg::SimState,
                offset: 17,
                size: OpSize::S8,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 24,
                src: VReg(1),
                size: OpSize::S8,
            },
            MInst::Return,
        ],
        2,
    );

    optimize(&mut func);

    let insts = &func.blocks[0].insts;
    assert!(
        !insts.iter().any(|inst| matches!(
            inst,
            MInst::Load {
                dst: VReg(1),
                base: BaseReg::SimState,
                offset: 17,
                size: OpSize::S8,
            }
        )),
        "{insts:#?}"
    );
    assert!(
        insts.iter().any(|inst| matches!(
            inst,
            MInst::LoadImm {
                dst: _,
                value: 0x34,
            }
        )),
        "{insts:#?}"
    );
}

#[test]
fn sink_loads_keeps_each_definition_before_its_use() {
    let mut func = make_func(
        vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 10,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 20,
            },
            MInst::LoadImm {
                dst: VReg(2),
                value: 30,
            },
            MInst::LoadImm {
                dst: VReg(3),
                value: 40,
            },
            MInst::LoadImm {
                dst: VReg(4),
                value: 50,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 0,
                src: VReg(0),
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 8,
                src: VReg(1),
                size: OpSize::S64,
            },
            MInst::Return,
        ],
        5,
    );

    sink_loads(&mut func);

    assert_eq!(func.verify_result(), Ok(()));
}

#[test]
fn folds_exact_narrow_load_and_store_into_direct_memory_and() {
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 37,
                size: OpSize::S8,
            },
            MInst::AndImm {
                dst: VReg(1),
                src: VReg(0),
                imm: 0xffff_ffff_ffff_fffcu64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 37,
                src: VReg(1),
                size: OpSize::S8,
            },
            MInst::Return,
        ],
        2,
    );

    algebraic_simplify(&mut func);
    dead_code_eliminate(&mut func);
    assert_eq!(fold_direct_immediate_stores(&mut func), 1);
    assert_eq!(
        func.blocks[0].insts,
        vec![
            MInst::AndStoreImm {
                base: BaseReg::SimState,
                offset: 37,
                size: OpSize::S8,
                imm: 0xfc,
            },
            MInst::Return,
        ]
    );
    assert_eq!(func.verify_result(), Ok(()));
}

#[test]
fn folds_encodable_qword_load_and_store_into_direct_memory_and() {
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 40,
                size: OpSize::S64,
            },
            MInst::AndImm32 {
                dst: VReg(1),
                src: VReg(0),
                imm: 0x3f,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 40,
                src: VReg(1),
                size: OpSize::S64,
            },
            MInst::Return,
        ],
        2,
    );

    assert_eq!(fold_direct_immediate_stores(&mut func), 1);
    assert_eq!(
        func.blocks[0].insts,
        vec![
            MInst::AndStoreImm {
                base: BaseReg::SimState,
                offset: 40,
                size: OpSize::S64,
                imm: 0x3f,
            },
            MInst::Return,
        ]
    );
    assert_eq!(func.verify_result(), Ok(()));
}

#[test]
fn qword_direct_memory_and_preserves_word32_zero_extension() {
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 40,
                size: OpSize::S64,
            },
            MInst::AndImm32 {
                dst: VReg(1),
                src: VReg(0),
                imm: 0xfc00_000f,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 40,
                src: VReg(1),
                size: OpSize::S64,
            },
            MInst::Return,
        ],
        2,
    );

    assert_eq!(fold_direct_immediate_stores(&mut func), 0);
}

#[test]
fn folds_clear_then_set_of_same_narrow_bits_into_direct_memory_or() {
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 37,
                size: OpSize::S8,
            },
            MInst::AndImm {
                dst: VReg(1),
                src: VReg(0),
                imm: !0x40,
            },
            MInst::OrImm {
                dst: VReg(2),
                src: VReg(1),
                imm: 0x40,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 37,
                src: VReg(2),
                size: OpSize::S8,
            },
            MInst::Return,
        ],
        3,
    );

    algebraic_simplify(&mut func);
    dead_code_eliminate(&mut func);
    assert_eq!(fold_direct_immediate_stores(&mut func), 1);
    assert_eq!(
        func.blocks[0].insts,
        vec![
            MInst::OrStoreImm {
                base: BaseReg::SimState,
                offset: 37,
                size: OpSize::S8,
                imm: 0x40,
            },
            MInst::Return,
        ]
    );
    assert_eq!(func.verify_result(), Ok(()));
}

#[test]
fn keeps_clear_then_set_when_the_or_does_not_restore_every_cleared_bit() {
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 37,
                size: OpSize::S8,
            },
            MInst::AndImm {
                dst: VReg(1),
                src: VReg(0),
                imm: !0x60,
            },
            MInst::OrImm {
                dst: VReg(2),
                src: VReg(1),
                imm: 0x40,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 37,
                src: VReg(2),
                size: OpSize::S8,
            },
            MInst::Return,
        ],
        3,
    );

    algebraic_simplify(&mut func);
    dead_code_eliminate(&mut func);
    assert_eq!(fold_direct_immediate_stores(&mut func), 0);
}

#[test]
fn direct_memory_and_requires_exclusive_ssa_temporaries() {
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 37,
                size: OpSize::S8,
            },
            MInst::AndImm {
                dst: VReg(1),
                src: VReg(0),
                imm: 0xfc,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 37,
                src: VReg(1),
                size: OpSize::S8,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 38,
                src: VReg(0),
                size: OpSize::S8,
            },
            MInst::Return,
        ],
        2,
    );

    assert_eq!(fold_direct_immediate_stores(&mut func), 0);
}
