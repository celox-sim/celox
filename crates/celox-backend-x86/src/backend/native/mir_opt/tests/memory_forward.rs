use super::*;

/// Differential test: the offset-indexed `AvailableStores` must select
/// exactly the same covering slots as a brute-force scan over every
/// tracked slot (the previous linear-scan implementation).
#[test]
fn available_stores_covering_selection_matches_brute_force_scan() {
    fn naive_find_best_covering_value(
        available: &HashMap<MemorySlot, VReg>,
        base: BaseReg,
        offset: i32,
        size: OpSize,
    ) -> Option<(MemorySlot, VReg)> {
        let load_start = i64::from(offset);
        let load_end = load_start + i64::from(size.bytes());
        available
            .iter()
            .filter_map(|(slot, &src)| {
                if slot.base != base {
                    return None;
                }
                let value_start = i64::from(slot.offset);
                let value_end = value_start + i64::from(slot.size.bytes());
                (value_start <= load_start && load_end <= value_end).then_some((*slot, src))
            })
            .min_by_key(|(slot, src)| {
                (
                    slot.size.bytes(),
                    load_start - i64::from(slot.offset),
                    slot.offset,
                    src.0,
                )
            })
    }

    // Small xorshift PRNG keeps this deterministic without external deps.
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };

    let bases = [BaseReg::SimState, BaseReg::StackFrame];
    let sizes = [OpSize::S8, OpSize::S16, OpSize::S32, OpSize::S64];
    for _round in 0..400 {
        let mut indexed = AvailableStores::default();
        let mut naive: HashMap<MemorySlot, VReg> = HashMap::default();
        for step in 0..120 {
            match next() % 5 {
                0 | 1 => {
                    let base = bases[(next() % 2) as usize];
                    let offset = (next() % 64) as i32 - 8;
                    let size = sizes[(next() % 4) as usize];
                    let value = VReg((next() % 1_000) as u32);
                    indexed.insert(base, offset, size, value);
                    naive.insert(MemorySlot { base, offset, size }, value);
                }
                2 => {
                    let base = bases[(next() % 2) as usize];
                    let offset = (next() % 64) as i32 - 8;
                    let size = sizes[(next() % 4) as usize];
                    indexed.invalidate_slot(base, offset, size);
                    let Some((start, end)) = byte_range(offset, size.bytes() as usize) else {
                        naive.retain(|slot, _| slot.base != base);
                        continue;
                    };
                    naive.retain(|slot, _| {
                        if slot.base != base {
                            return true;
                        }
                        let slot_start = i64::from(slot.offset);
                        let slot_end = slot_start + i64::from(slot.size.bytes());
                        slot_end <= start || end <= slot_start
                    });
                }
                3 => {
                    let offset = (next() % 64) as i32 - 8;
                    let byte_len = (next() % 9) as usize;
                    indexed.invalidate_byte_range(BaseReg::SimState, offset, byte_len);
                    let Some((start, end)) = byte_range(offset, byte_len) else {
                        naive.retain(|slot, _| slot.base != BaseReg::SimState);
                        continue;
                    };
                    naive.retain(|slot, _| {
                        if slot.base != BaseReg::SimState {
                            return true;
                        }
                        let slot_start = i64::from(slot.offset);
                        let slot_end = slot_start + i64::from(slot.size.bytes());
                        slot_end <= start || end <= slot_start
                    });
                }
                _ => {
                    indexed.clear();
                    naive.clear();
                }
            }

            // After every mutation, compare full-content and coverage
            // queries across all loads.
            let contents: Vec<_> = naive.iter().collect();
            assert_eq!(
                contents.len(),
                indexed.slots.values().map(BTreeMap::len).sum::<usize>()
            );
            for (slot, &value) in &naive {
                assert_eq!(indexed.get(slot.base, slot.offset, slot.size), Some(value));
            }
            for base in bases {
                for load_offset in -12..72 {
                    for &load_size in &sizes {
                        let expected =
                            naive_find_best_covering_value(&naive, base, load_offset, load_size);
                        let found =
                            find_best_covering_value(&indexed, base, load_offset, load_size);
                        assert_eq!(
                            found, expected,
                            "coverage mismatch at base={base:?} offset={load_offset} size={load_size:?}"
                        );
                    }
                }
            }
            let _ = step;
        }
    }
}

#[test]
fn promotes_dead_partial_store_through_its_only_wide_observer() {
    let mut func = partial_store_round_trip(false, true);
    promote_partial_store_round_trips(&mut func);

    assert!(matches!(
        func.blocks[0].insts[0],
        MInst::Load {
            base: BaseReg::SimState,
            offset: 100,
            size: OpSize::S64,
            ..
        }
    ));
    assert_eq!(
        func.blocks[0]
            .insts
            .iter()
            .filter(|inst| matches!(
                inst,
                MInst::Store {
                    size: OpSize::S8,
                    ..
                }
            ))
            .count(),
        0
    );
    assert_eq!(
        func.blocks[0]
            .insts
            .iter()
            .filter(|inst| matches!(
                inst,
                MInst::Load {
                    size: OpSize::S64,
                    ..
                }
            ))
            .count(),
        1
    );
    assert!(func.blocks[0].insts.iter().any(|inst| matches!(
        inst,
        MInst::AndImm32 {
            src: VReg(2),
            imm: 7,
            ..
        }
    )));
    assert!(!func.blocks[0].insts.iter().any(|inst| matches!(
        inst,
        MInst::AndImm32 {
            src: VReg(3),
            imm: 0xff,
            ..
        }
    )));
    assert!(matches!(
        func.blocks[0].insts.last(),
        Some(MInst::Store {
            base: BaseReg::SimState,
            offset: 100,
            src: VReg(4),
            size: OpSize::S64,
        })
    ));
}

#[test]
fn recovers_partial_insert_when_store_provenance_is_stale() {
    let mut func = partial_store_round_trip(false, true);
    func.spill_descs[3].state_insert = None;
    promote_partial_store_round_trips(&mut func);

    assert!(func.blocks[0].insts.iter().any(|inst| matches!(
        inst,
        MInst::AndImm32 {
            src: VReg(2),
            imm: 7,
            ..
        }
    )));
    assert!(!func.blocks[0].insts.iter().any(|inst| matches!(
        inst,
        MInst::AndImm32 {
            src: VReg(3),
            imm: 0xff,
            ..
        }
    )));
}

#[test]
fn keeps_partial_store_observed_again_before_covering_overwrite() {
    let mut func = partial_store_round_trip(true, true);
    promote_partial_store_round_trips(&mut func);

    assert!(func.blocks[0].insts.iter().any(|inst| matches!(
        inst,
        MInst::Store {
            base: BaseReg::SimState,
            offset: 100,
            size: OpSize::S8,
            ..
        }
    )));
}

#[test]
fn keeps_partial_store_live_at_block_exit() {
    let mut func = partial_store_round_trip(false, false);
    promote_partial_store_round_trips(&mut func);

    assert!(func.blocks[0].insts.iter().any(|inst| matches!(
        inst,
        MInst::Store {
            base: BaseReg::SimState,
            offset: 100,
            size: OpSize::S8,
            ..
        }
    )));
}

#[test]
fn forwards_live_partial_stores_without_losing_later_observers() {
    use crate::native::{emit, jit_mem::JitCode, mir_legalize, regalloc};
    fn compile(mut function: MFunction) -> (JitCode, usize) {
        mir_legalize::legalize(&mut function);
        let allocation = regalloc::run_regalloc(&mut function).unwrap();
        let emitted = emit::emit(
            &function,
            &allocation.assignment,
            allocation.spill_frame_size,
        )
        .unwrap();
        (
            JitCode::new(&emitted.code).unwrap(),
            emitted.required_state_size.max(128) as usize,
        )
    }
    for provenance in [false, true] {
        for barrier in [false, true] {
            let mut original = partial_store_round_trip(true, false);
            if !provenance {
                original.spill_descs[3].state_insert = None;
            }
            if barrier {
                original.blocks[0].insts.insert(
                    5,
                    MInst::MemFill {
                        dst_offset: 101,
                        byte_len: 1,
                        value: 0xa5,
                    },
                );
            }
            original.blocks[0].push(MInst::Store {
                base: BaseReg::SimState,
                offset: 0,
                src: VReg(4),
                size: OpSize::S64,
            });
            original.blocks[0].push(MInst::Store {
                base: BaseReg::SimState,
                offset: 8,
                src: VReg(5),
                size: OpSize::S64,
            });
            original.blocks[0].push(MInst::Return);
            let mut optimized = original.clone();
            forward_live_partial_stores(&mut optimized);
            optimized.verify();
            assert_eq!(
                optimized.blocks[0]
                    .insts
                    .iter()
                    .filter(|inst| matches!(
                        inst,
                        MInst::Store {
                            offset: 100,
                            size: OpSize::S8,
                            ..
                        }
                    ))
                    .count(),
                1
            );
            assert_eq!(
                optimized.blocks[0]
                    .insts
                    .iter()
                    .any(|inst| matches!(inst, MInst::Load { dst: VReg(4), .. })),
                barrier
            );
            let (before, before_size) = compile(original);
            let (after, after_size) = compile(optimized);
            for value in [0u64, 1, 0x0123_4567_89ab_cdef, u64::MAX] {
                let mut left = vec![0u8; before_size.max(after_size)];
                left[100..108].copy_from_slice(&value.to_le_bytes());
                let mut right = left.clone();
                assert_eq!(unsafe { before.call(&mut left) }, 0);
                assert_eq!(unsafe { after.call(&mut right) }, 0);
                assert_eq!(&left[..128], &right[..128]);
            }
        }
    }
}

#[test]
fn promotes_disjoint_partial_stores_in_program_order() {
    let mut func = make_func(
        vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 0x12,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 100,
                src: VReg(0),
                size: OpSize::S8,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 0x34,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 101,
                src: VReg(1),
                size: OpSize::S8,
            },
            MInst::Load {
                dst: VReg(2),
                base: BaseReg::SimState,
                offset: 100,
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 100,
                src: VReg(2),
                size: OpSize::S64,
            },
        ],
        3,
    );
    promote_partial_store_round_trips(&mut func);

    assert_eq!(
        func.blocks[0]
            .insts
            .iter()
            .filter(|inst| matches!(
                inst,
                MInst::Store {
                    size: OpSize::S8,
                    ..
                }
            ))
            .count(),
        0
    );
    assert!(
        func.blocks[0]
            .insts
            .iter()
            .any(|inst| matches!(inst, MInst::ShlImm { imm: 8, .. }))
    );
}

#[test]
fn recovers_nonzero_bit_insert_without_assuming_padding_is_zero() {
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 100,
                size: OpSize::S8,
            },
            MInst::AndImm {
                dst: VReg(1),
                src: VReg(0),
                imm: !0x38,
            },
            MInst::LoadImm {
                dst: VReg(2),
                value: 5,
            },
            MInst::ShlImm {
                dst: VReg(3),
                src: VReg(2),
                imm: 3,
            },
            MInst::Or {
                dst: VReg(4),
                lhs: VReg(1),
                rhs: VReg(3),
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 100,
                src: VReg(4),
                size: OpSize::S8,
            },
            MInst::Load {
                dst: VReg(5),
                base: BaseReg::SimState,
                offset: 100,
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 100,
                src: VReg(5),
                size: OpSize::S64,
            },
        ],
        6,
    );
    promote_partial_store_round_trips(&mut func);

    assert!(func.blocks[0].insts.iter().any(|inst| matches!(
        inst,
        MInst::ShlImm {
            src: VReg(2),
            imm: 3,
            ..
        }
    )));
    assert!(!func.blocks[0].insts.iter().any(|inst| matches!(
        inst,
        MInst::AndImm32 {
            src: VReg(4),
            imm: 0xff,
            ..
        }
    )));
}

#[test]
fn keeps_partial_store_across_unknown_direct_alias() {
    let mut func = partial_store_round_trip(false, true);
    func.blocks[0].insts.insert(
        5,
        MInst::StoreIndexed {
            base: BaseReg::SimState,
            offset: 0,
            index: VReg(2),
            src: VReg(2),
            size: OpSize::S8,
            alias_range: None,
        },
    );
    promote_partial_store_round_trips(&mut func);

    assert!(func.blocks[0].insts.iter().any(|inst| matches!(
        inst,
        MInst::Store {
            base: BaseReg::SimState,
            offset: 100,
            size: OpSize::S8,
            ..
        }
    )));
}

#[test]
fn bounded_disjoint_indexed_read_does_not_block_dead_store_elimination() {
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
                value: 0,
            },
            MInst::LoadIndexed {
                dst: VReg(2),
                base: BaseReg::SimState,
                offset: 64,
                index: VReg(1),
                scale: 1,
                size: OpSize::S8,
                alias_range: MemoryAliasRange::new(64, 8),
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

    eliminate_redundant_local_stores(&mut func);

    assert!(!func.blocks[0].insts.iter().any(|inst| matches!(
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
fn bounded_overlapping_indexed_read_keeps_preceding_store() {
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
                value: 0,
            },
            MInst::LoadIndexed {
                dst: VReg(2),
                base: BaseReg::SimState,
                offset: 16,
                index: VReg(1),
                scale: 1,
                size: OpSize::S8,
                alias_range: MemoryAliasRange::new(16, 8),
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

    eliminate_redundant_local_stores(&mut func);

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
fn indirect_read_does_not_block_direct_state_dead_store_elimination() {
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
                value: 0,
            },
            MInst::LoadPtr {
                dst: VReg(2),
                ptr: VReg(1),
                offset: 0,
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

    eliminate_redundant_local_stores(&mut func);

    assert!(!func.blocks[0].insts.iter().any(|inst| matches!(
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
fn partial_load_uses_smallest_covering_value() {
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 6,
                size: OpSize::S16,
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
                offset: 7,
                size: OpSize::S8,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 16,
                src: VReg(2),
                size: OpSize::S8,
            },
            MInst::Return,
        ],
        3,
    );

    forward_local_store_loads(&mut func);

    let insts = &func.blocks[0].insts;
    assert!(insts.iter().any(|inst| matches!(
        inst,
        MInst::ShrImm {
            src: VReg(0),
            imm: 8,
            ..
        }
    )));
    assert!(!insts.iter().any(|inst| matches!(
        inst,
        MInst::ShrImm {
            src: VReg(1),
            imm: 56,
            ..
        }
    )));
}

#[test]
fn memcopy_destination_invalidates_local_load_forwarding() {
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

    forward_local_store_loads(&mut func);

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
fn memcopy_preserves_nonoverlapping_local_load_forwarding() {
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
            MInst::Return,
        ],
        2,
    );

    forward_local_store_loads(&mut func);

    assert!(matches!(
        func.blocks[0].insts[2],
        MInst::Mov {
            dst: VReg(1),
            src: VReg(0),
        }
    ));
}

#[test]
fn memcopy_source_read_keeps_preceding_store_live() {
    let mut func = make_func(
        vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 1,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 64,
                src: VReg(0),
                size: OpSize::S64,
            },
            MInst::MemCopy {
                src_offset: 64,
                dst_offset: 16,
                byte_len: 8,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 2,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 64,
                src: VReg(1),
                size: OpSize::S64,
            },
            MInst::Return,
        ],
        2,
    );

    eliminate_redundant_local_stores(&mut func);

    assert!(func.blocks[0].insts.iter().any(|inst| matches!(
        inst,
        MInst::Store {
            base: BaseReg::SimState,
            offset: 64,
            src: VReg(0),
            size: OpSize::S64,
        }
    )));
}

#[test]
fn memcopy_preserves_nonoverlapping_dead_store_elimination() {
    let mut func = make_func(
        vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 1,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 80,
                src: VReg(0),
                size: OpSize::S64,
            },
            MInst::MemCopy {
                src_offset: 64,
                dst_offset: 16,
                byte_len: 8,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 2,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 80,
                src: VReg(1),
                size: OpSize::S64,
            },
            MInst::Return,
        ],
        2,
    );

    eliminate_redundant_local_stores(&mut func);

    assert!(!func.blocks[0].insts.iter().any(|inst| matches!(
        inst,
        MInst::Store {
            base: BaseReg::SimState,
            offset: 80,
            src: VReg(0),
            size: OpSize::S64,
        }
    )));
}

/// A candidate window that is empty or wholly outside the i32 domain
/// must never clamp into a nonempty in-domain window: an S8 store at
/// `i32::MAX` does not cover an S64 load at the same offset even though
/// both window endpoints clamp to `i32::MAX`.
#[test]
fn find_best_covering_value_ignores_windows_empty_or_outside_i32_domain() {
    let mut available = AvailableStores::default();
    available.insert(BaseReg::SimState, i32::MAX, OpSize::S8, VReg(7));
    assert_eq!(
        find_best_covering_value(&available, BaseReg::SimState, i32::MAX, OpSize::S64),
        None,
    );
    available.clear();
    available.insert(BaseReg::SimState, i32::MIN, OpSize::S8, VReg(7));
    assert_eq!(
        find_best_covering_value(&available, BaseReg::SimState, i32::MIN, OpSize::S64),
        None,
    );
    assert_eq!(
        clamped_key_range(0, -1),
        None,
        "empty windows stay empty under clamping",
    );
}

fn partial_store_round_trip(extra_read: bool, final_store: bool) -> MFunction {
    let mut insts = vec![
        MInst::Load {
            dst: VReg(0),
            base: BaseReg::SimState,
            offset: 100,
            size: OpSize::S8,
        },
        MInst::AndImm {
            dst: VReg(1),
            src: VReg(0),
            imm: !7,
        },
        MInst::LoadImm {
            dst: VReg(2),
            value: 5,
        },
        MInst::Or {
            dst: VReg(3),
            lhs: VReg(1),
            rhs: VReg(2),
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 100,
            src: VReg(3),
            size: OpSize::S8,
        },
        MInst::Load {
            dst: VReg(4),
            base: BaseReg::SimState,
            offset: 100,
            size: OpSize::S64,
        },
    ];
    if extra_read {
        insts.push(MInst::Load {
            dst: VReg(5),
            base: BaseReg::SimState,
            offset: 100,
            size: OpSize::S8,
        });
    }
    if final_store {
        insts.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 100,
            src: VReg(4),
            size: OpSize::S64,
        });
    }
    let mut func = make_func(insts, 6);
    func.spill_descs[3] = SpillDesc::transient().with_state_insert(VReg(2), 0, 3);
    func
}

fn indexed_forward_load(dst: u32, alias_range: Option<MemoryAliasRange>) -> MInst {
    MInst::LoadIndexed {
        dst: VReg(dst),
        base: BaseReg::SimState,
        offset: 16,
        index: VReg(0),
        scale: 1,
        size: OpSize::S64,
        alias_range,
    }
}

#[test]
fn indexed_forwarding_executes_original_value_after_first_consumer() {
    let mut func = make_func(
        vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 8,
            },
            indexed_forward_load(1, MemoryAliasRange::new(16, 32)),
            MInst::AndImm {
                dst: VReg(3),
                src: VReg(1),
                imm: 0xff,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 0,
                src: VReg(3),
                size: OpSize::S64,
            },
            indexed_forward_load(2, None),
            MInst::Store {
                base: BaseReg::SimState,
                offset: 8,
                src: VReg(2),
                size: OpSize::S64,
            },
            MInst::Return,
        ],
        4,
    );
    forward_local_indexed_loads(&mut func);
    assert!(matches!(
        func.blocks[0].insts[4],
        MInst::Mov {
            dst: VReg(2),
            src: VReg(1)
        }
    ));
    let mut assignment = AssignmentMap::default();
    for (index, register) in [PhysReg::R8, PhysReg::R9, PhysReg::R10, PhysReg::R11]
        .into_iter()
        .enumerate()
    {
        assignment.set(VReg(index as u32), register);
    }
    crate::native::regalloc::verify_assignment(&func, &assignment).unwrap();
    let emitted = crate::native::emit::emit(&func, &assignment, 0).unwrap();
    let jit = crate::native::jit_mem::JitCode::new(&emitted.code).unwrap();
    let value = 0xdead_beef_cafe_babeu64;
    let mut state = [0u8; 64];
    state[24..32].copy_from_slice(&value.to_le_bytes());
    assert_eq!(unsafe { jit.call(&mut state) }, 0);
    assert_eq!(
        u64::from_le_bytes(state[..8].try_into().unwrap()),
        value & 0xff
    );
    assert_eq!(u64::from_le_bytes(state[8..16].try_into().unwrap()), value);
}

#[test]
fn indexed_forwarding_invalidates_aliasing_writes_and_virtual_definitions() {
    for (alias, barrier, expected) in [
        (
            MemoryAliasRange::new(16, 32),
            MInst::Store {
                base: BaseReg::SimState,
                offset: 24,
                src: VReg(1),
                size: OpSize::S64,
            },
            false,
        ),
        (
            MemoryAliasRange::new(16, 32),
            MInst::Store {
                base: BaseReg::SimState,
                offset: 48,
                src: VReg(1),
                size: OpSize::S64,
            },
            true,
        ),
        (
            None,
            MInst::Store {
                base: BaseReg::SimState,
                offset: 48,
                src: VReg(1),
                size: OpSize::S64,
            },
            false,
        ),
        (
            None,
            MInst::Store {
                base: BaseReg::StackFrame,
                offset: 24,
                src: VReg(1),
                size: OpSize::S64,
            },
            true,
        ),
        (
            MemoryAliasRange::new(16, 32),
            MInst::StoreIndexed {
                base: BaseReg::SimState,
                offset: 48,
                index: VReg(0),
                src: VReg(1),
                size: OpSize::S64,
                alias_range: None,
            },
            false,
        ),
        (
            None,
            MInst::LoadImm {
                dst: VReg(0),
                value: 0,
            },
            false,
        ),
        (
            None,
            MInst::LoadImm {
                dst: VReg(1),
                value: 0,
            },
            false,
        ),
    ] {
        let mut func = make_func(
            vec![
                indexed_forward_load(1, alias),
                barrier,
                indexed_forward_load(2, None),
                MInst::Return,
            ],
            3,
        );
        forward_local_indexed_loads(&mut func);
        assert_eq!(
            matches!(func.blocks[0].insts[2], MInst::Mov { .. }),
            expected
        );
    }
}

#[test]
fn indexed_forwarding_keeps_different_addresses_and_widths() {
    for change in 0..5 {
        let mut second = indexed_forward_load(2, None);
        let MInst::LoadIndexed {
            base,
            offset,
            index,
            scale,
            size,
            ..
        } = &mut second
        else {
            unreachable!()
        };
        match change {
            0 => *base = BaseReg::StackFrame,
            1 => *offset += 1,
            2 => *index = VReg(3),
            3 => *scale = 8,
            _ => *size = OpSize::S32,
        }
        let mut func = make_func(
            vec![indexed_forward_load(1, None), second, MInst::Return],
            4,
        );
        forward_local_indexed_loads(&mut func);
        assert!(matches!(func.blocks[0].insts[1], MInst::LoadIndexed { .. }));
    }
}

#[test]
fn indexed_forwarding_limits_reuse_distance() {
    for intervening in [31u32, 32] {
        let mut instructions = vec![indexed_forward_load(1, None)];
        instructions.extend((0..intervening).map(|value| MInst::LoadImm {
            dst: VReg(value + 3),
            value: 0,
        }));
        instructions.push(indexed_forward_load(2, None));
        instructions.push(MInst::Return);
        let mut func = make_func(instructions, intervening + 3);
        forward_local_indexed_loads(&mut func);
        assert_eq!(
            matches!(
                func.blocks[0].insts[intervening as usize + 1],
                MInst::Mov { .. }
            ),
            intervening == 31
        );
    }
}

#[test]
fn indexed_forwarding_does_not_carry_values_across_blocks_or_unbounded_tables() {
    let mut instructions = vec![indexed_forward_load(1, None)];
    for n in 0..16u32 {
        let mut load = indexed_forward_load(n + 3, None);
        if let MInst::LoadIndexed { offset, .. } = &mut load {
            *offset = 32 + n as i32 * 8;
        }
        instructions.push(load);
    }
    instructions.push(indexed_forward_load(2, None));
    instructions.push(MInst::Return);
    let mut func = make_func(instructions, 19);
    forward_local_indexed_loads(&mut func);
    assert!(matches!(
        func.blocks[0].insts[17],
        MInst::LoadIndexed { .. }
    ));

    let mut func = make_func(
        vec![
            indexed_forward_load(1, None),
            MInst::Jump { target: BlockId(1) },
        ],
        3,
    );
    let mut exit = MBlock::new(BlockId(1));
    exit.push(indexed_forward_load(2, None));
    exit.push(MInst::Return);
    func.push_block(exit);
    forward_local_indexed_loads(&mut func);
    assert!(matches!(func.blocks[1].insts[0], MInst::LoadIndexed { .. }));
}
