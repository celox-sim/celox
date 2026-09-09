use super::*;

fn raw(mut function: MFunction) -> (JitCode, usize) {
    crate::mir_legalize::legalize_variable_shift_counts(&mut function);
    let allocation = crate::regalloc::allocate_with_spills(function, || false).unwrap();
    let emitted = emit_function(
        &allocation.allocated.function,
        &allocation.allocated.assignment,
        allocation.spill_frame_size,
        40,
        &allocation.allocated.edge_copies,
        false,
        false,
    )
    .unwrap();
    (
        JitCode::new(&emitted.code).unwrap(),
        emitted.required_state_size as usize,
    )
}

#[test]
fn setting_masked_bits_preserves_widths_distinct_masks_and_shared_values() {
    for narrow in [false, true] {
        for narrow_clear in [false, true] {
            for distinct in [false, true] {
                for swap in [false, true] {
                    let mut block = MBlock::new(BlockId(0));
                    for (dst, offset) in [(VReg(0), 0), (VReg(1), 8)] {
                        block.push(MInst::Load {
                            dst,
                            base: BaseReg::SimState,
                            offset,
                            size: OpSize::S64,
                        });
                    }
                    block.push(MInst::LoadImm {
                        dst: VReg(2),
                        value: 1,
                    });
                    block.push(MInst::LoadImm {
                        dst: VReg(3),
                        value: if distinct { 3 } else { 1 },
                    });
                    for (dst, lhs) in [(VReg(4), VReg(2)), (VReg(5), VReg(3))] {
                        block.push(MInst::Shl {
                            dst,
                            lhs,
                            rhs: VReg(1),
                        });
                    }
                    block.push(MInst::BitNot {
                        dst: VReg(6),
                        src: VReg(5),
                    });
                    let (lhs, rhs) = if swap {
                        (VReg(6), VReg(0))
                    } else {
                        (VReg(0), VReg(6))
                    };
                    block.push(if narrow_clear {
                        MInst::And32 {
                            dst: VReg(7),
                            lhs,
                            rhs,
                        }
                    } else {
                        MInst::And {
                            dst: VReg(7),
                            lhs,
                            rhs,
                        }
                    });
                    let (lhs, rhs) = if swap {
                        (VReg(4), VReg(7))
                    } else {
                        (VReg(7), VReg(4))
                    };
                    block.push(if narrow {
                        MInst::Or32 {
                            dst: VReg(8),
                            lhs,
                            rhs,
                        }
                    } else {
                        MInst::Or {
                            dst: VReg(8),
                            lhs,
                            rhs,
                        }
                    });
                    for (offset, src) in [(16, VReg(8)), (24, VReg(7)), (32, VReg(5))] {
                        block.push(MInst::Store {
                            base: BaseReg::SimState,
                            offset,
                            src,
                            size: OpSize::S64,
                        });
                    }
                    block.push(MInst::Return);
                    let mut vregs = crate::mir::VRegAllocator::new();
                    for _ in 0..9 {
                        vregs.alloc();
                    }
                    let mut original =
                        MFunction::for_isel(vregs, vec![crate::mir::SpillDesc::transient(); 9]);
                    original.blocks.push(block);
                    let mut optimized = original.clone();
                    crate::mir_opt::bit_updates::run(&mut optimized);
                    let changed = format!("{original:?}") != format!("{optimized:?}");
                    assert_eq!(changed, !distinct && (narrow || !narrow_clear));
                    let (before, before_size) = raw(original);
                    let (after, after_size) = raw(optimized);
                    for value in [0u64, 1, u64::MAX, 1 << 63, 0x0123_4567_89ab_cdef] {
                        for shift in [0u64, 1, 7, 31, 32, 63, 64, 65, u64::MAX] {
                            let mut left = vec![0u8; before_size.max(after_size)];
                            left[..8].copy_from_slice(&value.to_le_bytes());
                            left[8..16].copy_from_slice(&shift.to_le_bytes());
                            let mut right = left.clone();
                            assert_eq!(unsafe { (before.fn_ptr)(left.as_mut_ptr()) }, 0);
                            assert_eq!(unsafe { (after.fn_ptr)(right.as_mut_ptr()) }, 0);
                            assert_eq!(&left[..40], &right[..40]);
                            let shifted = |value: u64| {
                                if shift < 64 { value << shift } else { 0 }
                            };
                            let mut cleared = value & !shifted(if distinct { 3 } else { 1 });
                            if narrow_clear {
                                cleared &= u64::from(u32::MAX);
                            }
                            let mut expected = cleared | shifted(1);
                            if narrow {
                                expected &= u64::from(u32::MAX);
                            }
                            assert_eq!(&right[16..24], &expected.to_le_bytes());
                            assert_eq!(&right[24..32], &cleared.to_le_bytes());
                        }
                    }
                }
            }
        }
    }
}
