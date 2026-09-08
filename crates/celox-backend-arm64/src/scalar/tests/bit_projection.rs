use super::*;
use crate::mir_opt::bit_projection::run;

fn compile_raw(function: MFunction, bytes: usize) -> (JitCode, usize) {
    let allocation = crate::regalloc::allocate_with_spills(function, || false).unwrap();
    let emitted = emit_function(
        &allocation.allocated.function,
        &allocation.allocated.assignment,
        allocation.spill_frame_size,
        bytes,
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

fn inputs() -> Vec<u64> {
    let mut values = vec![0, 1, u64::MAX, 1 << 63, 0x0123_4567_89ab_cdef];
    let mut random = 0x9e37_79b9_7f4a_7c15;
    for _ in 0..50 {
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        values.push(random);
    }
    values
}

#[test]
fn composed_projections_keep_discarded_bits_zero() {
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Load {
        dst: VReg(0),
        base: BaseReg::SimState,
        offset: 0,
        size: OpSize::S64,
    });
    let mut next = 1;
    let mut cases = Vec::new();
    for lsb in [0u8, 1, 7, 31, 32, 63] {
        for width in [1u8, 5.min(64 - lsb), 64 - lsb] {
            for shift in [0u8, 1, 7, 31, 32, 63] {
                for left in [false, true] {
                    let extracted = VReg(next);
                    let shifted = VReg(next + 1);
                    let restored = VReg(next + 2);
                    let masked = VReg(next + 3);
                    next += 4;
                    block.push(MInst::BitExtract {
                        dst: extracted,
                        src: VReg(0),
                        lsb,
                        width,
                    });
                    block.push(if left {
                        MInst::ShlImm {
                            dst: shifted,
                            src: extracted,
                            imm: shift,
                        }
                    } else {
                        MInst::ShrImm {
                            dst: shifted,
                            src: extracted,
                            imm: shift,
                        }
                    });
                    block.push(if left {
                        MInst::ShrImm {
                            dst: restored,
                            src: shifted,
                            imm: shift,
                        }
                    } else {
                        MInst::ShlImm {
                            dst: restored,
                            src: shifted,
                            imm: shift,
                        }
                    });
                    block.push(MInst::AndImm32 {
                        dst: masked,
                        src: restored,
                        imm: 0xff00_ff00,
                    });
                    block.push(MInst::Store {
                        base: BaseReg::SimState,
                        offset: 16 + cases.len() as i32 * 8,
                        src: masked,
                        size: OpSize::S64,
                    });
                    cases.push((lsb, width, shift, left));
                }
            }
        }
    }
    block.push(MInst::Return);
    let bytes = 16 + cases.len() * 8;
    let original = MFunction::new(vec![block], vec![]);
    let mut optimized = original.clone();
    run(&mut optimized);
    let (before, a_size) = compile_raw(original, bytes);
    let (after, b_size) = compile_raw(optimized, bytes);
    for input in inputs() {
        let mut a = vec![0xa5; a_size.max(b_size)];
        a[..8].copy_from_slice(&input.to_le_bytes());
        let mut b = a.clone();
        assert_eq!(unsafe { (before.fn_ptr)(a.as_mut_ptr()) }, 0);
        assert_eq!(unsafe { (after.fn_ptr)(b.as_mut_ptr()) }, 0);
        assert_eq!(&a[..bytes], &b[..bytes]);
        for (index, &(lsb, width, shift, left)) in cases.iter().enumerate() {
            let field = (input >> lsb) & (u64::MAX >> (64 - width));
            let value = if left {
                field
                    .checked_shl(u32::from(shift))
                    .unwrap_or(0)
                    .checked_shr(u32::from(shift))
                    .unwrap_or(0)
            } else {
                field
                    .checked_shr(u32::from(shift))
                    .unwrap_or(0)
                    .checked_shl(u32::from(shift))
                    .unwrap_or(0)
            } & 0xff00_ff00;
            assert_eq!(&b[16 + index * 8..24 + index * 8], &value.to_le_bytes());
        }
    }
}

#[test]
fn reconstructed_partitions_preserve_truncations_and_mixed_sources() {
    for mode in 0..3 {
        for word32 in [false, true] {
            for mixed in [false, true] {
                let mut block = MBlock::new(BlockId(0));
                for value in 0..2 {
                    block.push(MInst::Load {
                        dst: VReg(value),
                        base: BaseReg::SimState,
                        offset: value as i32 * 8,
                        size: OpSize::S64,
                    });
                }
                block.push(MInst::LoadImm {
                    dst: VReg(2),
                    value: 0,
                });
                let mut accumulator = VReg(2);
                let mut next = 3;
                let fields = [(0u8, 3u8), (3, 5), (8, 12), (20, 1), (21, 17), (38, 26)];
                for (index, &(lsb, width)) in fields.iter().enumerate() {
                    let field = VReg(next);
                    let shifted = VReg(next + 1);
                    let combined = VReg(next + 2);
                    next += 3;
                    block.push(MInst::BitExtract {
                        dst: field,
                        src: VReg(u32::from(mixed && index % 2 == 1)),
                        lsb,
                        width,
                    });
                    match mode {
                        0 => {
                            block.push(MInst::ShlImm {
                                dst: shifted,
                                src: field,
                                imm: lsb,
                            });
                            block.push(MInst::Or {
                                dst: combined,
                                lhs: accumulator,
                                rhs: shifted,
                            });
                        }
                        1 => block.push(MInst::OrShifted {
                            dst: combined,
                            lhs: accumulator,
                            rhs: field,
                            shift: lsb,
                        }),
                        _ => block.push(MInst::BitInsert {
                            dst: combined,
                            base: accumulator,
                            src: field,
                            lsb,
                            width,
                        }),
                    }
                    accumulator = combined;
                }
                let result = VReg(next);
                block.push(if word32 {
                    MInst::Mov32 {
                        dst: result,
                        src: accumulator,
                    }
                } else {
                    MInst::Mov {
                        dst: result,
                        src: accumulator,
                    }
                });
                block.push(MInst::Store {
                    base: BaseReg::SimState,
                    offset: 16,
                    src: result,
                    size: OpSize::S64,
                });
                block.push(MInst::Return);
                let original = MFunction::new(vec![block], vec![]);
                let mut optimized = original.clone();
                run(&mut optimized);
                if !mixed {
                    assert!(optimized.blocks[0].insts.iter().any(|inst| matches!(inst, MInst::Mov { dst, src: VReg(0) } | MInst::Mov32 { dst, src: VReg(0) } if *dst == result)));
                }
                let (before, a_size) = compile_raw(original, 24);
                let (after, b_size) = compile_raw(optimized, 24);
                for input in inputs() {
                    let other = !input.rotate_left(17);
                    let mut a = vec![0xa5; a_size.max(b_size)];
                    a[..8].copy_from_slice(&input.to_le_bytes());
                    a[8..16].copy_from_slice(&other.to_le_bytes());
                    let mut b = a.clone();
                    assert_eq!(unsafe { (before.fn_ptr)(a.as_mut_ptr()) }, 0);
                    assert_eq!(unsafe { (after.fn_ptr)(b.as_mut_ptr()) }, 0);
                    assert_eq!(&a[..24], &b[..24]);
                    let mut expected = 0;
                    for (index, &(lsb, width)) in fields.iter().enumerate() {
                        let source = if mixed && index % 2 == 1 {
                            other
                        } else {
                            input
                        };
                        expected |= source & ((u64::MAX >> (64 - width)) << lsb);
                    }
                    if word32 {
                        expected &= u64::from(u32::MAX);
                    }
                    assert_eq!(&b[16..24], &expected.to_le_bytes());
                }
            }
        }
    }
}
