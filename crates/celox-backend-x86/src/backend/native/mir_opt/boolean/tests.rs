use super::*;
use crate::native::{emit, jit_mem::JitCode, mir_legalize, regalloc};

fn conjunction(shared: bool) -> MFunction {
    let mut vregs = VRegAllocator::new();
    for _ in 0..12 {
        vregs.alloc();
    }
    let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 12]);
    let mut block = MBlock::new(BlockId(0));
    for index in 0..4 {
        block.push(MInst::Load {
            dst: VReg(index),
            base: BaseReg::SimState,
            offset: index as i32 * 8,
            size: OpSize::S64,
        });
    }
    for index in 0..3 {
        block.push(MInst::CmpImm {
            dst: VReg(4 + index),
            lhs: VReg(index),
            imm: 0,
            kind: CmpKind::Eq,
        });
    }
    block.push(MInst::And {
        dst: VReg(7),
        lhs: VReg(4),
        rhs: VReg(3),
    });
    block.push(MInst::And32 {
        dst: VReg(8),
        lhs: VReg(7),
        rhs: VReg(5),
    });
    block.push(MInst::And {
        dst: VReg(9),
        lhs: VReg(8),
        rhs: VReg(6),
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 32,
        src: VReg(9),
        size: OpSize::S64,
    });
    if shared {
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 40,
            src: VReg(5),
            size: OpSize::S64,
        });
    }
    block.push(MInst::Return);
    function.push_block(block);
    function
}

fn compile(mut function: MFunction) -> (JitCode, usize) {
    mir_legalize::legalize(&mut function);
    let allocation = regalloc::run_regalloc(&mut function).unwrap();
    let code = emit::emit(
        &function,
        &allocation.assignment,
        allocation.spill_frame_size,
    )
    .unwrap();
    (
        JitCode::new(&code.code).unwrap(),
        code.required_state_size.max(48) as usize,
    )
}

#[test]
fn balanced_reductions_consume_interleaved_inputs_without_retaining_all_leaves() {
    fn peak_live(block: &MBlock) -> usize {
        let mut live = HashSet::default();
        let mut peak = 0;
        for inst in block.insts.iter().rev() {
            if let Some(dst) = inst.def() {
                live.remove(&dst);
            }
            live.extend(inst.uses());
            peak = peak.max(live.len());
        }
        peak
    }
    fn depth(block: &MBlock, result: VReg) -> usize {
        let mut depths = HashMap::default();
        for inst in &block.insts {
            if let Some(dst) = inst.def() {
                let inputs = inst.uses().iter().map(|v| depths[v]).max().unwrap_or(0);
                let reduction = matches!(
                    inst,
                    MInst::And { .. } | MInst::And32 { .. } | MInst::Or { .. } | MInst::Or32 { .. }
                );
                depths.insert(dst, inputs + usize::from(reduction));
            }
        }
        depths[&result]
    }
    for count in [64usize, 257] {
        for is_and in [false, true] {
            for word32 in [false, true] {
                for reversed in [false, true] {
                    let mut vregs = VRegAllocator::new();
                    for _ in 0..count * 2 + 2 {
                        vregs.alloc();
                    }
                    let mut function =
                        MFunction::new(vregs, vec![SpillDesc::transient(); count * 2 + 2]);
                    let mut block = MBlock::new(BlockId(0));
                    block.push(MInst::LoadImm {
                        dst: VReg(0),
                        value: 0,
                    });
                    let mut result = VReg(1);
                    for index in 0..count {
                        let input = VReg(index as u32 + 1);
                        block.push(MInst::Load {
                            dst: input,
                            base: BaseReg::SimState,
                            offset: index as i32 * 8,
                            size: OpSize::S64,
                        });
                        // Leaves are captured values: moving a load past
                        // this overwrite would change the reduction.
                        block.push(MInst::Store {
                            base: BaseReg::SimState,
                            offset: index as i32 * 8,
                            src: VReg(0),
                            size: OpSize::S64,
                        });
                        if index != 0 {
                            let dst = VReg((count + index + 1) as u32);
                            let (lhs, rhs) = if reversed {
                                (input, result)
                            } else {
                                (result, input)
                            };
                            block.push(match (is_and, word32) {
                                (false, false) => MInst::Or { dst, lhs, rhs },
                                (false, true) => MInst::Or32 { dst, lhs, rhs },
                                (true, false) => MInst::And { dst, lhs, rhs },
                                (true, true) => MInst::And32 { dst, lhs, rhs },
                            });
                            result = dst;
                        }
                    }
                    block.push(MInst::Store {
                        base: BaseReg::SimState,
                        offset: count as i32 * 8,
                        src: result,
                        size: OpSize::S64,
                    });
                    block.push(MInst::Return);
                    function.push_block(block);
                    let mut optimized = function.clone();
                    balance_private_bitwise_trees(&mut optimized);
                    copy_propagate(&mut optimized);
                    dead_code_eliminate(&mut optimized);
                    optimized.verify();
                    let bound = count.next_power_of_two().ilog2() as usize;
                    assert!(peak_live(&optimized.blocks[0]) <= bound + 3);
                    let MInst::Store { src, .. } =
                        optimized.blocks[0].insts[optimized.blocks[0].insts.len() - 2]
                    else {
                        panic!("reduction output store");
                    };
                    assert_eq!(depth(&function.blocks[0], result), count - 1);
                    assert!(depth(&optimized.blocks[0], src) <= bound);
                    let (before, before_size) = compile(function);
                    let (after, after_size) = compile(optimized);
                    for bit in [0, 31, 32, 63] {
                        let mut left = vec![0xa5; before_size.max(after_size)];
                        let value = if is_and { !(1u64 << bit) } else { 1u64 << bit };
                        for index in 0..count {
                            let lane = if index == count / 2 {
                                value
                            } else if is_and {
                                u64::MAX
                            } else {
                                0
                            };
                            left[index * 8..index * 8 + 8].copy_from_slice(&lane.to_le_bytes());
                        }
                        let mut right = left.clone();
                        assert_eq!(unsafe { before.call(&mut left) }, 0);
                        assert_eq!(unsafe { after.call(&mut right) }, 0);
                        assert_eq!(&left[..count * 8 + 8], &right[..count * 8 + 8]);
                        let expected = if word32 {
                            value & u64::from(u32::MAX)
                        } else {
                            value
                        };
                        assert_eq!(&right[count * 8..count * 8 + 8], &expected.to_le_bytes());
                    }
                }
            }
        }
    }
}

#[test]
fn balanced_bitwise_trees_preserve_width_complements_and_shared_values() {
    for count in [5usize, 6, 7, 12, 33] {
        for is_and in [false, true] {
            for word32 in [false, true] {
                for complement in 0..3 {
                    for shared in [false, true] {
                        let mut vregs = VRegAllocator::new();
                        for _ in 0..count * 3 {
                            vregs.alloc();
                        }
                        let mut function =
                            MFunction::new(vregs, vec![SpillDesc::transient(); count * 3]);
                        let mut block = MBlock::new(BlockId(0));
                        let negated =
                            |i: usize| complement == 2 || (complement == 1 && !i.is_multiple_of(3));
                        let mut leaves = Vec::new();
                        for index in 0..count {
                            let input = VReg(index as u32);
                            block.push(MInst::Load {
                                dst: input,
                                base: BaseReg::SimState,
                                offset: index as i32 * 8,
                                size: OpSize::S64,
                            });
                            leaves.push(if negated(index) {
                                let dst = VReg((count + index) as u32);
                                block.push(MInst::BitNot { dst, src: input });
                                dst
                            } else {
                                input
                            });
                        }
                        let mut result = leaves[0];
                        for (index, &rhs) in leaves.iter().enumerate().skip(1) {
                            let dst = VReg((count * 2 + index) as u32);
                            let lhs = result;
                            block.push(match (is_and, word32) {
                                (false, false) => MInst::Or { dst, lhs, rhs },
                                (false, true) => MInst::Or32 { dst, lhs, rhs },
                                (true, false) => MInst::And { dst, lhs, rhs },
                                (true, true) => MInst::And32 { dst, lhs, rhs },
                            });
                            if shared && index == 2 {
                                block.push(MInst::Store {
                                    base: BaseReg::SimState,
                                    offset: (count * 8 + 8) as i32,
                                    src: dst,
                                    size: OpSize::S64,
                                });
                            }
                            result = dst;
                        }
                        block.push(MInst::Store {
                            base: BaseReg::SimState,
                            offset: count as i32 * 8,
                            src: result,
                            size: OpSize::S64,
                        });
                        block.push(MInst::Return);
                        function.push_block(block);
                        let mut optimized = function.clone();
                        balance_private_bitwise_trees(&mut optimized);
                        copy_propagate(&mut optimized);
                        dead_code_eliminate(&mut optimized);
                        optimized.verify();
                        let (before, before_size) = compile(function);
                        let (after, after_size) = compile(optimized);
                        for sample in 0..128 {
                            let mut left = vec![0xa5u8; before_size.max(after_size)];
                            let mut expected = if is_and { u64::MAX } else { 0 };
                            for lane in 0..count {
                                let value = if lane == sample % count {
                                    if sample < 64 {
                                        1u64 << sample
                                    } else {
                                        !(1u64 << (sample - 64))
                                    }
                                } else if is_and {
                                    u64::MAX
                                } else {
                                    0
                                };
                                expected = if is_and {
                                    expected & value
                                } else {
                                    expected | value
                                };
                                let input = if negated(lane) { !value } else { value };
                                left[lane * 8..lane * 8 + 8].copy_from_slice(&input.to_le_bytes());
                            }
                            if word32 {
                                expected &= u64::from(u32::MAX);
                            }
                            let mut right = left.clone();
                            assert_eq!(unsafe { before.call(&mut left) }, 0);
                            assert_eq!(unsafe { after.call(&mut right) }, 0);
                            assert_eq!(
                                &left[..count * 8 + 16],
                                &right[..count * 8 + 16],
                                "count={count} and={is_and} word32={word32} complement={complement} shared={shared} sample={sample}"
                            );
                            assert_eq!(
                                u64::from_le_bytes(
                                    right[count * 8..count * 8 + 8].try_into().unwrap()
                                ),
                                expected
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn zero_conjunctions_preserve_arbitrary_bits_and_shared_comparisons() {
    for shared in [false, true] {
        let original = conjunction(shared);
        let mut optimized = original.clone();
        fold_zero_conjunctions(&mut optimized);
        dead_code_eliminate(&mut optimized);
        optimized.verify();
        let compares = optimized
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .filter(|inst| matches!(inst, MInst::CmpImm { .. }))
            .count();
        assert_eq!(compares, if shared { 2 } else { 1 });
        let (before, before_size) = compile(original);
        let (after, after_size) = compile(optimized);
        let values = [0u64, 1, 2, 3, 1 << 32, 1 << 63, u64::MAX];
        for a in values {
            for b in values {
                for c in values {
                    for guard in values {
                        let mut left = vec![0u8; before_size.max(after_size)];
                        for (index, value) in [a, b, c, guard].into_iter().enumerate() {
                            left[index * 8..index * 8 + 8].copy_from_slice(&value.to_le_bytes());
                        }
                        let mut right = left.clone();
                        assert_eq!(unsafe { before.call(&mut left) }, 0);
                        assert_eq!(unsafe { after.call(&mut right) }, 0);
                        assert_eq!(
                            &left[32..48],
                            &right[32..48],
                            "{a:#x}, {b:#x}, {c:#x}, {guard:#x}"
                        );
                        assert_eq!(
                            u64::from_le_bytes(right[32..40].try_into().unwrap()),
                            u64::from(a == 0 && b == 0 && c == 0) & guard
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn masked_selects_preserve_condition_truth_and_truncated_masks() {
    for word32 in [false, true] {
        for normalized in [false, true] {
            for mask in [0x3ff, 0x3fff_ffff_ffff_ffff, u64::MAX] {
                for bounded in [false, true] {
                    let mut vregs = VRegAllocator::new();
                    for _ in 0..14 {
                        vregs.alloc();
                    }
                    let mut original = MFunction::new(vregs, vec![SpillDesc::transient(); 14]);
                    let mut block = MBlock::new(BlockId(0));
                    for index in 0..3 {
                        block.push(MInst::Load {
                            dst: VReg(index),
                            base: BaseReg::SimState,
                            offset: index as i32 * 8,
                            size: OpSize::S64,
                        });
                    }
                    block.push(MInst::LoadImm {
                        dst: VReg(3),
                        value: 0,
                    });
                    block.push(MInst::LoadImm {
                        dst: VReg(4),
                        value: mask,
                    });
                    block.push(if normalized {
                        MInst::AndImm {
                            dst: VReg(5),
                            src: VReg(0),
                            imm: 1,
                        }
                    } else {
                        MInst::Mov {
                            dst: VReg(5),
                            src: VReg(0),
                        }
                    });
                    for (dst, src) in [(VReg(6), VReg(1)), (VReg(7), VReg(2))] {
                        block.push(if bounded {
                            MInst::And {
                                dst,
                                lhs: src,
                                rhs: VReg(4),
                            }
                        } else {
                            MInst::Mov { dst, src }
                        });
                    }
                    block.push(MInst::Sub {
                        dst: VReg(8),
                        lhs: VReg(3),
                        rhs: VReg(5),
                    });
                    block.push(MInst::And {
                        dst: VReg(9),
                        lhs: VReg(8),
                        rhs: VReg(4),
                    });
                    block.push(MInst::Xor {
                        dst: VReg(10),
                        lhs: VReg(6),
                        rhs: VReg(7),
                    });
                    block.push(MInst::And {
                        dst: VReg(11),
                        lhs: VReg(10),
                        rhs: VReg(9),
                    });
                    block.push(if word32 {
                        MInst::Xor32 {
                            dst: VReg(12),
                            lhs: VReg(7),
                            rhs: VReg(11),
                        }
                    } else {
                        MInst::Xor {
                            dst: VReg(12),
                            lhs: VReg(7),
                            rhs: VReg(11),
                        }
                    });
                    block.push(MInst::Store {
                        base: BaseReg::SimState,
                        offset: 32,
                        src: VReg(12),
                        size: OpSize::S64,
                    });
                    // The mask is also observed: folding must preserve shared producers.
                    block.push(MInst::Store {
                        base: BaseReg::SimState,
                        offset: 40,
                        src: VReg(9),
                        size: OpSize::S64,
                    });
                    block.push(MInst::Return);
                    original.push_block(block);
                    let mut optimized = original.clone();
                    fold_masked_selects(&mut optimized);
                    dead_code_eliminate(&mut optimized);
                    optimized.verify();
                    let expected_fold = normalized
                        && (bounded
                            || mask == u64::MAX
                            || (word32 && mask & u64::from(u32::MAX) == u64::from(u32::MAX)));
                    assert_eq!(
                        optimized.blocks[0]
                            .insts
                            .iter()
                            .any(|inst| matches!(inst, MInst::Select { .. })),
                        expected_fold
                    );
                    let (before, before_size) = compile(original);
                    let (after, after_size) = compile(optimized);
                    let values = [0u64, 1, 2, 3, 1 << 31, 1 << 32, 1 << 63, u64::MAX];
                    for a in values {
                        for b in values {
                            for condition in values {
                                let mut left = vec![0u8; before_size.max(after_size)];
                                for (index, value) in [condition, a, b].into_iter().enumerate() {
                                    left[index * 8..index * 8 + 8]
                                        .copy_from_slice(&value.to_le_bytes());
                                }
                                let mut right = left.clone();
                                assert_eq!(unsafe { before.call(&mut left) }, 0);
                                assert_eq!(unsafe { after.call(&mut right) }, 0);
                                assert_eq!(
                                    &left[32..48],
                                    &right[32..48],
                                    "word32={word32}, normalized={normalized}, mask={mask:#x}, bounded={bounded}, condition={condition:#x}, a={a:#x}, b={b:#x}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn zero_conjunctions_preserve_shifted_and_truncated_fields() {
    for (left_shift, right_shift, mask) in [
        (0, 0, u64::MAX),
        (3, 9, 0x1fff_ffff),
        (31, 32, 0x8000_0000_ffff_0000),
        (32, 1, 0xffff_ffff),
        (63, 63, u64::MAX),
    ] {
        let mut vregs = VRegAllocator::new();
        for _ in 0..15 {
            vregs.alloc();
        }
        let mut original = MFunction::new(vregs, vec![SpillDesc::transient(); 15]);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: VReg(0),
            base: BaseReg::SimState,
            offset: 0,
            size: OpSize::S64,
        });
        block.push(MInst::Load {
            dst: VReg(1),
            base: BaseReg::SimState,
            offset: 8,
            size: OpSize::S64,
        });
        block.push(MInst::LoadImm {
            dst: VReg(2),
            value: mask,
        });
        block.push(MInst::ShlImm {
            dst: VReg(3),
            src: VReg(0),
            imm: left_shift,
        });
        block.push(MInst::And32 {
            dst: VReg(4),
            lhs: VReg(3),
            rhs: VReg(2),
        });
        block.push(MInst::ShrImm {
            dst: VReg(5),
            src: VReg(0),
            imm: right_shift,
        });
        block.push(MInst::And {
            dst: VReg(6),
            lhs: VReg(5),
            rhs: VReg(2),
        });
        block.push(MInst::Mov32 {
            dst: VReg(7),
            src: VReg(0),
        });
        block.push(MInst::AndImm32 {
            dst: VReg(8),
            src: VReg(7),
            imm: 0x8000_0000,
        });
        for (dst, lhs) in [(9, 4), (10, 6), (11, 8)] {
            block.push(MInst::CmpImm {
                dst: VReg(dst),
                lhs: VReg(lhs),
                imm: 0,
                kind: CmpKind::Eq,
            });
        }
        block.push(MInst::And {
            dst: VReg(12),
            lhs: VReg(9),
            rhs: VReg(10),
        });
        block.push(MInst::And32 {
            dst: VReg(13),
            lhs: VReg(12),
            rhs: VReg(11),
        });
        block.push(MInst::And {
            dst: VReg(14),
            lhs: VReg(13),
            rhs: VReg(1),
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 32,
            src: VReg(14),
            size: OpSize::S64,
        });
        block.push(MInst::Return);
        original.push_block(block);
        let mut optimized = original.clone();
        fold_zero_conjunctions(&mut optimized);
        dead_code_eliminate(&mut optimized);
        optimized.verify();
        let (before, before_size) = compile(original);
        let (after, after_size) = compile(optimized);
        let inputs = [0u64, u64::MAX]
            .into_iter()
            .chain((0..64).flat_map(|bit| [1 << bit, !(1 << bit)]));
        for value in inputs {
            for guard in [0u64, 1, 2, u64::MAX] {
                let mut state = vec![0u8; before_size.max(after_size)];
                state[..8].copy_from_slice(&value.to_le_bytes());
                state[8..16].copy_from_slice(&guard.to_le_bytes());
                let mut actual = state.clone();
                assert_eq!(unsafe { before.call(&mut state) }, 0);
                assert_eq!(unsafe { after.call(&mut actual) }, 0);
                assert_eq!(
                    &state[32..40],
                    &actual[32..40],
                    "left={left_shift}, right={right_shift}, mask={mask:#x}, value={value:#x}, guard={guard:#x}"
                );
                let expected = u64::from(
                    ((value << left_shift) as u32 & mask as u32) == 0
                        && ((value >> right_shift) & mask) == 0
                        && value & 0x8000_0000 == 0,
                ) & guard;
                assert_eq!(
                    u64::from_le_bytes(actual[32..40].try_into().unwrap()),
                    expected
                );
            }
        }
    }
}

#[test]
fn inverted_conjunctions_preserve_boolean_and_wide_truth_values() {
    use crate::native::features::X86Features;
    if !std::arch::is_x86_feature_detected!("bmi1") {
        return;
    }
    for bounded in [false, true] {
        for shared in [false, true] {
            for bmi1 in [false, true] {
                let mut vregs = VRegAllocator::new();
                for _ in 0..7 {
                    vregs.alloc();
                }
                let mut original = MFunction::new(vregs, vec![SpillDesc::transient(); 7]);
                original.target_features = X86Features::for_test(false).with_bmi1(bmi1);
                let mut block = MBlock::new(BlockId(0));
                block.push(MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 0,
                    size: OpSize::S64,
                });
                block.push(MInst::Load {
                    dst: VReg(1),
                    base: BaseReg::SimState,
                    offset: 8,
                    size: OpSize::S64,
                });
                block.push(if bounded {
                    MInst::AndImm {
                        dst: VReg(2),
                        src: VReg(0),
                        imm: 1,
                    }
                } else {
                    MInst::Mov {
                        dst: VReg(2),
                        src: VReg(0),
                    }
                });
                block.push(MInst::AndImm {
                    dst: VReg(3),
                    src: VReg(1),
                    imm: 1,
                });
                block.push(MInst::CmpImm {
                    dst: VReg(4),
                    lhs: VReg(2),
                    imm: 0,
                    kind: CmpKind::Eq,
                });
                block.push(MInst::And {
                    dst: VReg(5),
                    lhs: VReg(3),
                    rhs: VReg(4),
                });
                block.push(MInst::Store {
                    base: BaseReg::SimState,
                    offset: 32,
                    src: VReg(5),
                    size: OpSize::S64,
                });
                if shared {
                    block.push(MInst::Store {
                        base: BaseReg::SimState,
                        offset: 40,
                        src: VReg(4),
                        size: OpSize::S64,
                    });
                }
                block.push(MInst::Return);
                original.push_block(block);
                let mut optimized = original.clone();
                fold_inverted_conjunctions(&mut optimized);
                dead_code_eliminate(&mut optimized);
                optimized.verify();
                assert_eq!(
                    optimized.blocks[0]
                        .insts
                        .iter()
                        .any(|inst| matches!(inst, MInst::BitNot { .. })),
                    bounded && !shared && bmi1
                );
                let (before, before_size) = compile(original);
                let (after, after_size) = compile(optimized);
                for a in [0u64, 1, 2, 3, 1 << 32, 1 << 63, u64::MAX] {
                    for b in [0u64, 1, 2, 3, 1 << 63, u64::MAX] {
                        let mut left = vec![0u8; before_size.max(after_size)];
                        left[..8].copy_from_slice(&a.to_le_bytes());
                        left[8..16].copy_from_slice(&b.to_le_bytes());
                        let mut right = left.clone();
                        assert_eq!(unsafe { before.call(&mut left) }, 0);
                        assert_eq!(unsafe { after.call(&mut right) }, 0);
                        assert_eq!(
                            &left[32..48],
                            &right[32..48],
                            "bounded={bounded}, shared={shared}, bmi1={bmi1}, a={a:#x}, b={b:#x}"
                        );
                    }
                }
            }
        }
    }
}
