use super::*;
use crate::mir::SpillDesc;
use crate::mir_opt::counted_loop::run;

fn fixture(trips: u64, mask: u64, index_mask: u64, reversed: bool, observed: bool) -> MFunction {
    let mut entry = MBlock::new(BlockId(0));
    for (dst, value) in [(VReg(0), 0), (VReg(1), trips), (VReg(2), 1)] {
        entry.push(MInst::LoadImm { dst, value });
    }
    entry.push(MInst::Jump { target: BlockId(1) });
    let mut header = MBlock::new(BlockId(1));
    for (dst, first, next) in [
        (VReg(3), VReg(1), VReg(7)),
        (VReg(4), VReg(0), VReg(9)),
        (VReg(5), VReg(0), VReg(10)),
    ] {
        header.phis.push(PhiNode {
            dst,
            sources: vec![(BlockId(0), first), (BlockId(2), next)],
        });
    }
    header.push(MInst::Add {
        dst: VReg(10),
        lhs: VReg(5),
        rhs: VReg(4),
    });
    if observed {
        header.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 16,
            src: VReg(3),
            size: OpSize::S64,
        });
    }
    header.push(MInst::Jump { target: BlockId(2) });
    let mut latch = MBlock::new(BlockId(2));
    latch.push(MInst::Sub32 {
        dst: VReg(6),
        lhs: VReg(3),
        rhs: VReg(2),
    });
    latch.push(MInst::AndImm {
        dst: VReg(7),
        src: VReg(6),
        imm: mask,
    });
    latch.push(MInst::Add32 {
        dst: VReg(8),
        lhs: VReg(4),
        rhs: VReg(2),
    });
    latch.push(MInst::AndImm {
        dst: VReg(9),
        src: VReg(8),
        imm: index_mask,
    });
    latch.push(MInst::CmpImm {
        dst: VReg(11),
        lhs: VReg(7),
        imm: 0,
        kind: if reversed { CmpKind::Eq } else { CmpKind::Ne },
    });
    latch.push(MInst::Branch {
        cond: VReg(11),
        true_bb: BlockId(if reversed { 3 } else { 1 }),
        false_bb: BlockId(if reversed { 1 } else { 3 }),
    });
    let mut exit = MBlock::new(BlockId(3));
    for (offset, src) in [(0, VReg(10)), (8, VReg(9))] {
        exit.push(MInst::Store {
            base: BaseReg::SimState,
            offset,
            src,
            size: OpSize::S64,
        });
    }
    exit.push(MInst::Return);
    let mut function = MFunction::new(vec![entry, header, latch, exit], vec![]);
    for _ in 0..14 {
        function.vregs.alloc();
        function.spill_descs.push(SpillDesc::transient());
    }
    function
}

#[test]
fn reuses_index_without_changing_trip_counts_or_observed_counters() {
    for trips in [1u64, 2, 7, 8, 31, 32, 63] {
        for reversed in [false, true] {
            for observed in [false, true] {
                let mut function = fixture(trips, 63, u32::MAX as u64, reversed, observed);
                run(&mut function);
                celox_backend_common::regalloc::analyze_live_intervals(
                    &crate::regalloc::build_facts(&function).unwrap(),
                )
                .unwrap();
                assert_eq!(
                    function.blocks[1].phis.iter().any(|phi| phi.dst == VReg(3)),
                    observed
                );
                assert!(function.blocks[2].insts.iter().any(|inst| matches!(inst, MInst::CmpImm { lhs: VReg(9), imm, .. } if *imm == trips as i32)));
                let (jit, mut state) = compile(function, 24);
                assert_eq!(unsafe { (jit.fn_ptr)(state.as_mut_ptr()) }, 0);
                assert_eq!(&state[..8], &(trips * (trips - 1) / 2).to_le_bytes());
                assert_eq!(&state[8..16], &trips.to_le_bytes());
                assert_eq!(&state[16..24], &u64::from(observed).to_le_bytes());
            }
        }
    }
}

#[test]
fn rejects_wrapping_and_noncontiguous_counters() {
    for (trips, mask, index_mask) in [
        (0, 63, 63),
        (64, 63, 255),
        (32, 63, 31),
        (8, 61, 63),
        (8, 63, 61),
    ] {
        let mut function = fixture(trips, mask, index_mask, false, false);
        let before = format!("{function:?}");
        run(&mut function);
        celox_backend_common::regalloc::analyze_live_intervals(
            &crate::regalloc::build_facts(&function).unwrap(),
        )
        .unwrap();
        assert_eq!(format!("{function:?}"), before);
    }
}

#[test]
fn places_the_exit_test_after_the_index_update_and_keeps_shared_tests() {
    let mut function = fixture(32, 63, 255, false, false);
    let condition = function.blocks[2].insts.remove(4);
    function.blocks[2].insts.insert(2, condition);
    celox_backend_common::regalloc::analyze_live_intervals(
        &crate::regalloc::build_facts(&function).unwrap(),
    )
    .unwrap();
    let mut shared = function.clone();
    shared.blocks[3].insts.insert(
        0,
        MInst::Store {
            base: BaseReg::SimState,
            offset: 24,
            src: VReg(11),
            size: OpSize::S64,
        },
    );
    let unchanged = format!("{shared:?}");
    run(&mut shared);
    celox_backend_common::regalloc::analyze_live_intervals(
        &crate::regalloc::build_facts(&shared).unwrap(),
    )
    .unwrap();
    assert_eq!(format!("{shared:?}"), unchanged);
    run(&mut function);
    celox_backend_common::regalloc::analyze_live_intervals(
        &crate::regalloc::build_facts(&function).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        function.blocks[2].insts[function.blocks[2].insts.len() - 2],
        MInst::CmpImm {
            lhs: VReg(9),
            imm: 32,
            ..
        }
    ));
}

#[test]
fn packed_loop_loads_respect_bounds_consumers_and_loop_writes() {
    for trips in [9u64, 16, 17, 32, 33, 64] {
        for effect in 0..8 {
            let mut function = fixture(trips, 127, 255, false, false);
            let mut allocate = || {
                let value = function.vregs.alloc();
                function.spill_descs.push(SpillDesc::transient());
                value
            };
            let byte = allocate();
            let bit = allocate();
            let raw = allocate();
            let shifted = allocate();
            let selected = allocate();
            let size = if trips <= 16 {
                2
            } else if trips <= 32 {
                4
            } else {
                8
            };
            let envelope_size = if effect == 7 {
                trips.div_ceil(8) as usize
            } else {
                size
            };
            let header = &mut function.blocks[1];
            header.insts[0] = MInst::Add {
                dst: VReg(10),
                lhs: VReg(5),
                rhs: selected,
            };
            header.insts.splice(
                0..0,
                [
                    MInst::ShrImm {
                        dst: byte,
                        src: VReg(4),
                        imm: 3,
                    },
                    MInst::AndImm32 {
                        dst: bit,
                        src: VReg(4),
                        imm: 7,
                    },
                    MInst::LoadIndexed {
                        dst: raw,
                        base: BaseReg::SimState,
                        offset: 64,
                        index: byte,
                        scale: 1,
                        size: OpSize::S8,
                        alias_range: MemoryAliasRange::new(64, envelope_size),
                    },
                    MInst::Shr {
                        dst: shifted,
                        lhs: raw,
                        rhs: bit,
                    },
                    MInst::AndImm32 {
                        dst: selected,
                        src: shifted,
                        imm: if effect == 6 { 255 } else { 1 },
                    },
                ],
            );
            if effect == 5 {
                let at = header.insts.len() - 1;
                header.insts.insert(
                    at,
                    MInst::Store {
                        base: BaseReg::SimState,
                        offset: 24,
                        src: raw,
                        size: OpSize::S64,
                    },
                );
            }
            let write = match effect {
                1 | 2 => Some(MInst::MemFill {
                    dst_offset: if effect == 1 { 64 } else { 96 },
                    byte_len: 1,
                    value: 0,
                }),
                3 | 4 => Some(MInst::StoreIndexed {
                    base: BaseReg::SimState,
                    offset: if effect == 3 { 64 } else { 96 },
                    index: VReg(0),
                    src: VReg(0),
                    size: OpSize::S8,
                    alias_range: if effect == 3 {
                        None
                    } else {
                        MemoryAliasRange::new(96, 1)
                    },
                }),
                _ => None,
            };
            if let Some(write) = write {
                function.blocks[2].insts.insert(0, write);
            }
            crate::mir_opt::optimize(&mut function);
            let widened = !matches!(effect, 5 | 6) && envelope_size == size;
            let hoisted = widened && !matches!(effect, 1 | 3);
            assert_eq!(
                function.blocks[0]
                    .insts
                    .iter()
                    .any(|inst| matches!(inst, MInst::Load { offset: 64, .. })),
                hoisted,
                "trips={trips} effect={effect}"
            );
            let (jit, mut state) = compile(function, 104);
            for input in [0u64, 1, 0x0123_4567_89ab_cdef, 1 << 63, u64::MAX] {
                state.fill(0);
                state[64..72].copy_from_slice(&input.to_le_bytes());
                let mut bytes = input.to_le_bytes();
                let mut sum = 0u64;
                for index in 0..trips as usize {
                    sum += u64::from(bytes[index / 8] >> (index % 8))
                        & if effect == 6 { 255 } else { 1 };
                    if matches!(effect, 1 | 3) {
                        bytes[0] = 0;
                    }
                }
                assert_eq!(unsafe { (jit.fn_ptr)(state.as_mut_ptr()) }, 0);
                assert_eq!(
                    &state[..8],
                    &sum.to_le_bytes(),
                    "trips={trips} effect={effect} input={input:#x}"
                );
                assert_eq!(&state[64..72], &bytes);
            }
        }
    }
}
