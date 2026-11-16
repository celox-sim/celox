use super::*;

#[test]
fn boolean_selects_preserve_nonzero_truth_and_full_width_arms() {
    use crate::native::{emit, features::X86Features, jit_mem::JitCode, mir_legalize, regalloc};
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
    for normalized in [false, true] {
        for bmi1 in [false, true] {
            if bmi1 && !std::arch::is_x86_feature_detected!("bmi1") {
                continue;
            }
            let mut vregs = VRegAllocator::new();
            for _ in 0..24 {
                vregs.alloc();
            }
            let mut original = MFunction::new(vregs, vec![SpillDesc::transient(); 24]);
            original.target_features = X86Features::for_test(false).with_bmi1(bmi1);
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
                value: 1,
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
            block.push(MInst::AndImm {
                dst: VReg(6),
                src: VReg(1),
                imm: 1,
            });
            block.push(MInst::AndImm {
                dst: VReg(7),
                src: VReg(2),
                imm: 1,
            });
            let arms = [
                (3, 4),
                (4, 3),
                (6, 3),
                (4, 7),
                (3, 7),
                (6, 7),
                (1, 3),
                (4, 2),
                (3, 2),
            ];
            for (index, (a, b)) in arms.into_iter().enumerate() {
                let dst = VReg(8 + index as u32);
                block.push(MInst::Select {
                    dst,
                    cond: VReg(5),
                    true_val: VReg(a),
                    false_val: VReg(b),
                });
                block.push(MInst::Store {
                    base: BaseReg::SimState,
                    offset: 32 + index as i32 * 8,
                    src: dst,
                    size: OpSize::S64,
                });
            }
            block.push(MInst::Shl {
                dst: VReg(20),
                lhs: VReg(3),
                rhs: VReg(0),
            });
            block.push(MInst::Shr {
                dst: VReg(21),
                lhs: VReg(3),
                rhs: VReg(0),
            });
            block.push(MInst::Store {
                base: BaseReg::SimState,
                offset: 104,
                src: VReg(20),
                size: OpSize::S64,
            });
            block.push(MInst::Store {
                base: BaseReg::SimState,
                offset: 112,
                src: VReg(21),
                size: OpSize::S64,
            });
            block.push(MInst::Return);
            original.push_block(block);
            let mut optimized = original.clone();
            fold(&mut optimized);
            copy_propagate(&mut optimized);
            dead_code_eliminate(&mut optimized);
            optimized.verify();
            let (before, before_size) = compile(original);
            let (after, after_size) = compile(optimized);
            let values = [
                0u64,
                1,
                2,
                3,
                31,
                32,
                63,
                64,
                65,
                1 << 32,
                1 << 63,
                u64::MAX,
            ];
            for condition in values {
                for a in values {
                    for b in values {
                        let mut left = vec![0u8; before_size.max(after_size)];
                        for (index, value) in [condition, a, b].into_iter().enumerate() {
                            left[index * 8..index * 8 + 8].copy_from_slice(&value.to_le_bytes());
                        }
                        let mut right = left.clone();
                        assert_eq!(unsafe { before.call(&mut left) }, 0);
                        assert_eq!(unsafe { after.call(&mut right) }, 0);
                        assert_eq!(
                            &left[32..120],
                            &right[32..120],
                            "normalized={normalized}, bmi1={bmi1}, condition={condition:#x}, a={a:#x}, b={b:#x}"
                        );
                    }
                }
            }
        }
    }
}

fn contains(facts: KnownBits, value: u64) -> bool {
    facts.zero & value == 0 && facts.one & value == facts.one
}

#[test]
fn demanded_bits_keep_a_controlling_operand_when_both_sides_agree() {
    use crate::native::{emit, jit_mem::JitCode, mir_legalize, regalloc};
    let masks = [0, 1, 0xff00, 0x8000_0001, 0xffff_0000_ffff_0000, u64::MAX];
    for is_or in [false, true] {
        for word32 in [false, true] {
            for left_mask in masks {
                for right_mask in masks {
                    let mut vregs = VRegAllocator::new();
                    for _ in 0..7 {
                        vregs.alloc();
                    }
                    let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 7]);
                    let mut block = MBlock::new(BlockId(0));
                    for index in 0..2 {
                        block.push(MInst::Load {
                            dst: VReg(index),
                            base: BaseReg::SimState,
                            offset: index as i32 * 8,
                            size: OpSize::S64,
                        });
                    }
                    for (index, value) in [(2, left_mask), (3, right_mask)] {
                        block.push(MInst::LoadImm {
                            dst: VReg(index),
                            value,
                        });
                    }
                    for (dst, lhs, rhs) in [
                        (VReg(4), VReg(0), VReg(2)),
                        (VReg(5), VReg(1), VReg(3)),
                        (VReg(6), VReg(4), VReg(5)),
                    ] {
                        block.push(match (is_or, word32) {
                            (false, false) => MInst::And { dst, lhs, rhs },
                            (false, true) => MInst::And32 { dst, lhs, rhs },
                            (true, false) => MInst::Or { dst, lhs, rhs },
                            (true, true) => MInst::Or32 { dst, lhs, rhs },
                        });
                    }
                    block.push(MInst::Store {
                        base: BaseReg::SimState,
                        offset: 16,
                        src: VReg(6),
                        size: OpSize::S64,
                    });
                    block.push(MInst::Return);
                    function.push_block(block);
                    for _ in 0..2 {
                        fold_demanded(&mut function);
                        copy_propagate(&mut function);
                        dead_code_eliminate(&mut function);
                    }
                    function.verify();
                    mir_legalize::legalize(&mut function);
                    let allocation = regalloc::run_regalloc(&mut function).unwrap();
                    let emitted = emit::emit(
                        &function,
                        &allocation.assignment,
                        allocation.spill_frame_size,
                    )
                    .unwrap();
                    let jit = JitCode::new(&emitted.code).unwrap();
                    for bit in 0..64 {
                        for inverted in [false, true] {
                            let left = if inverted {
                                !(1u64 << bit)
                            } else {
                                1u64 << bit
                            };
                            let right = left.rotate_left(17);
                            let mut expected = if is_or {
                                left | left_mask | right | right_mask
                            } else {
                                left & left_mask & right & right_mask
                            };
                            if word32 {
                                expected &= u64::from(u32::MAX);
                            }
                            let mut state = vec![0u8; emitted.required_state_size.max(24) as usize];
                            state[..8].copy_from_slice(&left.to_le_bytes());
                            state[8..16].copy_from_slice(&right.to_le_bytes());
                            assert_eq!(unsafe { jit.call(&mut state) }, 0);
                            assert_eq!(
                                u64::from_le_bytes(state[16..24].try_into().unwrap()),
                                expected,
                                "or={is_or} word32={word32} left_mask={left_mask:#x} right_mask={right_mask:#x} bit={bit} inverted={inverted}"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn demanded_bits_preserve_generated_word_and_field_extractions() {
    use crate::native::{emit, jit_mem::JitCode, mir_legalize, regalloc};
    fn compile(mut function: MFunction, simplify: bool) -> JitCode {
        mir_legalize::legalize(&mut function);
        if simplify {
            for _ in 0..2 {
                fold_demanded(&mut function);
                copy_propagate(&mut function);
                dead_code_eliminate(&mut function);
            }
            refresh_constant_spill_descs(&mut function);
        }
        function.verify();
        let allocation = regalloc::run_regalloc(&mut function).unwrap();
        let emitted = emit::emit(
            &function,
            &allocation.assignment,
            allocation.spill_frame_size,
        )
        .unwrap();
        JitCode::new(&emitted.code).unwrap()
    }
    fn random(seed: &mut u64) -> u64 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        *seed
    }
    for case in 1..=24 {
        let mut seed = case;
        let mut vregs = VRegAllocator::new();
        for _ in 0..40 {
            vregs.alloc();
        }
        let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 40]);
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
            value: u64::MAX,
        });
        for index in 4..32 {
            let dst = VReg(index);
            let lhs = VReg((random(&mut seed) % u64::from(index)) as u32);
            let rhs = VReg((random(&mut seed) % u64::from(index)) as u32);
            let shift = [0, 1, 7, 8, 15, 31, 32, 62, 63][(random(&mut seed) % 9) as usize];
            block.push(match index % 13 {
                0 => MInst::And { dst, lhs, rhs },
                1 => MInst::And32 { dst, lhs, rhs },
                2 => MInst::Or { dst, lhs, rhs },
                3 => MInst::Or32 { dst, lhs, rhs },
                4 => MInst::Xor { dst, lhs, rhs },
                5 => MInst::Xor32 { dst, lhs, rhs },
                6 => MInst::ShlImm {
                    dst,
                    src: lhs,
                    imm: shift,
                },
                7 => MInst::ShrImm {
                    dst,
                    src: lhs,
                    imm: shift,
                },
                8 => MInst::AndImm32 {
                    dst,
                    src: lhs,
                    imm: 0xff00_ff00,
                },
                9 => MInst::Mov32 { dst, src: lhs },
                10 => MInst::BitNot { dst, src: lhs },
                11 => MInst::Select {
                    dst,
                    cond: VReg(2),
                    true_val: lhs,
                    false_val: rhs,
                },
                _ => MInst::Add { dst, lhs, rhs },
            });
        }
        // Observe different fields of shared graphs, and retain a complete
        // 64-bit use so narrowing a second use cannot discard its upper bits.
        for index in 0..8 {
            block.push(MInst::ShrImm {
                dst: VReg(32 + index),
                src: VReg(31 - index / 2),
                imm: (index * 8) as u8,
            });
            block.push(MInst::Store {
                base: BaseReg::SimState,
                offset: 24 + index as i32 * 8,
                src: VReg(32 + index),
                size: [OpSize::S8, OpSize::S16, OpSize::S32, OpSize::S64][index as usize % 4],
            });
        }
        block.push(MInst::Return);
        function.push_block(block);
        let reference = compile(function.clone(), false);
        let optimized = compile(function, true);
        for sample in 0..128 {
            let mut expected = [0xa5u8; 88];
            for lane in 0..3 {
                let value = if sample < 64 {
                    1u64 << sample
                } else {
                    random(&mut seed)
                };
                expected[lane * 8..lane * 8 + 8].copy_from_slice(&value.to_le_bytes());
            }
            if sample % 2 == 0 {
                expected[16..24].fill(0);
            }
            let mut actual = expected;
            assert_eq!(unsafe { reference.call(&mut expected) }, 0);
            assert_eq!(unsafe { optimized.call(&mut actual) }, 0);
            assert_eq!(actual, expected, "case={case}, sample={sample}");
        }
    }
}

#[test]
fn demanded_bits_remove_unused_packed_fields() {
    let mut vregs = VRegAllocator::new();
    for _ in 0..6 {
        vregs.alloc();
    }
    let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 6]);
    let mut block = MBlock::new(BlockId(0));
    for index in 0..2 {
        block.push(MInst::Load {
            dst: VReg(index),
            base: BaseReg::SimState,
            offset: index as i32 * 8,
            size: OpSize::S8,
        });
    }
    block.push(MInst::ShlImm {
        dst: VReg(2),
        src: VReg(1),
        imm: 8,
    });
    block.push(MInst::Or {
        dst: VReg(3),
        lhs: VReg(0),
        rhs: VReg(2),
    });
    block.push(MInst::ShlImm {
        dst: VReg(4),
        src: VReg(3),
        imm: 16,
    });
    block.push(MInst::ShrImm {
        dst: VReg(5),
        src: VReg(4),
        imm: 16,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 16,
        src: VReg(5),
        size: OpSize::S8,
    });
    block.push(MInst::Return);
    function.push_block(block);
    fold_demanded(&mut function);
    copy_propagate(&mut function);
    dead_code_eliminate(&mut function);
    function.verify();
    assert!(
        !function.blocks[0]
            .insts
            .iter()
            .any(|inst| matches!(inst, MInst::Load { offset: 8, .. } | MInst::Or { .. }))
    );
}

#[test]
fn demanded_bits_reach_a_fixed_point_through_loop_phis() {
    use crate::native::{emit, jit_mem::JitCode, regalloc};
    let mut vregs = VRegAllocator::new();
    for _ in 0..9 {
        vregs.alloc();
    }
    let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 9]);
    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::Load {
        dst: VReg(0),
        base: BaseReg::SimState,
        offset: 0,
        size: OpSize::S64,
    });
    entry.push(MInst::Load {
        dst: VReg(1),
        base: BaseReg::SimState,
        offset: 8,
        size: OpSize::S8,
    });
    entry.push(MInst::Jump { target: BlockId(1) });
    let mut header = MBlock::new(BlockId(1));
    header.phis.push(PhiNode {
        dst: VReg(2),
        sources: vec![(BlockId(0), VReg(0)), (BlockId(2), VReg(7))],
    });
    header.phis.push(PhiNode {
        dst: VReg(3),
        sources: vec![(BlockId(0), VReg(1)), (BlockId(2), VReg(8))],
    });
    header.push(MInst::Branch {
        cond: VReg(3),
        true_bb: BlockId(2),
        false_bb: BlockId(3),
    });
    let mut body = MBlock::new(BlockId(2));
    body.push(MInst::ShlImm {
        dst: VReg(5),
        src: VReg(2),
        imm: 8,
    });
    body.push(MInst::ShrImm {
        dst: VReg(6),
        src: VReg(2),
        imm: 56,
    });
    body.push(MInst::Or {
        dst: VReg(7),
        lhs: VReg(5),
        rhs: VReg(6),
    });
    body.push(MInst::SubImm {
        dst: VReg(8),
        src: VReg(3),
        imm: 1,
    });
    body.push(MInst::Jump { target: BlockId(1) });
    let mut exit = MBlock::new(BlockId(3));
    exit.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 9,
        src: VReg(2),
        size: OpSize::S8,
    });
    exit.push(MInst::Return);
    for block in [entry, header, exit, body] {
        function.push_block(block);
    }
    fold_demanded(&mut function);
    copy_propagate(&mut function);
    dead_code_eliminate(&mut function);
    function.verify();
    let allocation = regalloc::run_regalloc(&mut function).unwrap();
    let emitted = emit::emit(
        &function,
        &allocation.assignment,
        allocation.spill_frame_size,
    )
    .unwrap();
    let jit = JitCode::new(&emitted.code).unwrap();
    let input = 0x0123_4567_89ab_cdefu64;
    for rotations in 0..=16 {
        let mut state = [0u8; 16];
        state[..8].copy_from_slice(&input.to_le_bytes());
        state[8] = rotations;
        assert_eq!(unsafe { jit.call(&mut state) }, 0);
        assert_eq!(state[9], input.rotate_left(u32::from(rotations) * 8) as u8);
    }
}

#[test]
fn arithmetic_facts_cover_all_concrete_inputs_including_wrapping() {
    // Exhaust every four-bit abstract input (zero, one, or unknown per
    // bit). Move it across the 32/64-bit boundaries and exercise carries
    // into and out of the fixed surrounding bits as well.
    for shift in [0, 30, 60] {
        for surrounding in [0, 0xaaaa_aaaa_aaaa_aaaa, u64::MAX] {
            let inputs = (0..81)
                .map(|mut pattern| {
                    let mut facts = KnownBits::constant(surrounding & !(15u64 << shift));
                    for bit in shift..shift + 4 {
                        match pattern % 3 {
                            0 => facts.zero &= !(1 << bit),
                            1 => {}
                            2 => {
                                facts.zero &= !(1 << bit);
                                facts.one |= 1 << bit;
                            }
                            _ => unreachable!(),
                        }
                        pattern /= 3;
                    }
                    let values = (0..16)
                        .map(|bits| (surrounding & !(15u64 << shift)) | (bits << shift))
                        .filter(|&value| contains(facts, value))
                        .collect::<Vec<_>>();
                    (facts, values)
                })
                .collect::<Vec<_>>();
            for (left, left_values) in &inputs {
                for (right, right_values) in &inputs {
                    let sum = left.add(*right);
                    let difference = left.add(right.not()).add(KnownBits::constant(1));
                    let product = left.mul(*right);
                    for &a in left_values {
                        for &b in right_values {
                            for (facts, value) in [
                                (sum, a.wrapping_add(b)),
                                (difference, a.wrapping_sub(b)),
                                (product, a.wrapping_mul(b)),
                                (left.and(*right), a & b),
                                (left.or(*right), a | b),
                                (left.xor(*right), a ^ b),
                            ] {
                                assert_eq!(facts.zero & facts.one, 0);
                                assert!(
                                    contains(facts, value),
                                    "{left:?} {right:?} => {facts:?}, actual {value:#x}"
                                );
                                assert!(contains(
                                    facts.truncate(u64::from(u32::MAX)),
                                    value & u64::from(u32::MAX)
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn unknown_high_bits_do_not_hide_fixed_array_offset_bits() {
    let index = KnownBits::default();
    for stride in [8, 32, 288, 512, 1u64 << 63] {
        for offset in [0, 9, 18, 163, u64::MAX] {
            let address = index
                .mul(KnownBits::constant(stride))
                .add(KnownBits::constant(offset));
            assert_eq!(
                address.and(KnownBits::constant(7)).value(),
                Some(offset & 7)
            );
            for index in [0, 1, 15, u64::MAX, u64::MAX / stride] {
                assert!(contains(
                    address,
                    index.wrapping_mul(stride).wrapping_add(offset)
                ));
            }
        }
    }
    // An odd stride can change every bit of the address.
    assert_eq!(
        index
            .mul(KnownBits::constant(7))
            .add(KnownBits::constant(163)),
        KnownBits::default()
    );
}

#[test]
fn immediate_shifts_preserve_fixed_bits_at_machine_boundaries() {
    for value in [0, 1, 0x8000_0000_ffff_ffff, u64::MAX] {
        for shift in [0, 1, 31, 32, 63, 64, 255] {
            let facts = KnownBits::constant(value);
            assert_eq!(
                facts.shl(shift).value(),
                Some(value.checked_shl(u32::from(shift)).unwrap_or(0))
            );
            assert_eq!(
                facts.shr(shift).value(),
                Some(value.checked_shr(u32::from(shift)).unwrap_or(0))
            );
        }
    }
}

#[test]
fn late_address_bits_fold_comparisons_and_selects_without_losing_wrapping() {
    use crate::native::{emit, jit_mem::JitCode, mir_legalize, regalloc};

    let mut vregs = VRegAllocator::new();
    for _ in 0..10 {
        vregs.alloc();
    }
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 10]);
    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::Load {
        dst: VReg(0),
        base: BaseReg::SimState,
        offset: 0,
        size: OpSize::S64,
    });
    entry.push(MInst::LoadImm {
        dst: VReg(1),
        value: 288,
    });
    entry.push(MInst::Mul {
        dst: VReg(2),
        lhs: VReg(0),
        rhs: VReg(1),
    });
    entry.push(MInst::AddImm {
        dst: VReg(3),
        src: VReg(2),
        imm: 163,
    });
    entry.push(MInst::AndImm {
        dst: VReg(4),
        src: VReg(3),
        imm: 7,
    });
    entry.push(MInst::Jump { target: BlockId(1) });
    let mut body = MBlock::new(BlockId(1));
    body.push(MInst::CmpImm {
        dst: VReg(5),
        lhs: VReg(4),
        imm: 3,
        kind: CmpKind::Eq,
    });
    body.push(MInst::Load {
        dst: VReg(6),
        base: BaseReg::SimState,
        offset: 8,
        size: OpSize::S64,
    });
    body.push(MInst::LoadImm {
        dst: VReg(7),
        value: 0xdead_beef,
    });
    body.push(MInst::Select {
        dst: VReg(8),
        cond: VReg(5),
        true_val: VReg(6),
        false_val: VReg(7),
    });
    body.push(MInst::Shl {
        dst: VReg(9),
        lhs: VReg(6),
        rhs: VReg(4),
    });
    body.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 16,
        src: VReg(8),
        size: OpSize::S64,
    });
    body.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 24,
        src: VReg(9),
        size: OpSize::S64,
    });
    body.push(MInst::Return);
    func.blocks = vec![entry, body];
    fold(&mut func);
    assert!(
        !func
            .blocks
            .iter()
            .flat_map(|b| &b.insts)
            .any(|inst| matches!(inst, MInst::CmpImm { .. } | MInst::Select { .. }))
    );
    mir_legalize::legalize(&mut func);
    optimize(&mut func);
    let ra = regalloc::run_regalloc(&mut func).unwrap();
    post_regalloc_peephole(&mut func, &ra.assignment);
    let emitted = emit::emit(&func, &ra.assignment, ra.spill_frame_size).unwrap();
    let jit = JitCode::new(&emitted.code).unwrap();
    for input in [0u64, 1, u64::MAX / 288, 1 << 63, u64::MAX] {
        for value in [0u64, 7, 0xdead_beef, u64::MAX] {
            let mut state = vec![0u8; emitted.required_state_size.max(32) as usize];
            state[..8].copy_from_slice(&input.to_le_bytes());
            state[8..16].copy_from_slice(&value.to_le_bytes());
            assert_eq!(unsafe { jit.call(&mut state) }, 0);
            assert_eq!(u64::from_le_bytes(state[16..24].try_into().unwrap()), value);
            assert_eq!(
                u64::from_le_bytes(state[24..32].try_into().unwrap()),
                value << 3
            );
        }
    }
}

#[test]
fn comparison_bounds_preserve_unknown_signs_and_overlapping_ranges() {
    let mut cases = Vec::new();
    for free in [1u64, 7, 0xff, 0xf000_0000, 1 << 63, (1 << 63) | 7] {
        for fixed in [0u64, u64::MAX, 1 << 31, 1 << 63] {
            let fixed = fixed & !free;
            let facts = KnownBits {
                zero: !(fixed | free),
                one: fixed,
            };
            let mut values = vec![fixed, fixed | free];
            for bit in 0..64 {
                if free & (1 << bit) != 0 {
                    values.push(fixed | (1 << bit));
                    values.push(fixed | (free & !(1 << bit)));
                }
            }
            cases.push((facts, values));
        }
    }
    for (left, lhs_values) in &cases {
        for (right, rhs_values) in &cases {
            for kind in [
                CmpKind::Eq,
                CmpKind::Ne,
                CmpKind::LtU,
                CmpKind::LeU,
                CmpKind::GtU,
                CmpKind::GeU,
                CmpKind::LtS,
                CmpKind::LeS,
                CmpKind::GtS,
                CmpKind::GeS,
            ] {
                let result = left.compare(*right, kind);
                for &lhs in lhs_values {
                    for &rhs in rhs_values {
                        let value = match kind {
                            CmpKind::Eq => lhs == rhs,
                            CmpKind::Ne => lhs != rhs,
                            CmpKind::LtU => lhs < rhs,
                            CmpKind::LeU => lhs <= rhs,
                            CmpKind::GtU => lhs > rhs,
                            CmpKind::GeU => lhs >= rhs,
                            CmpKind::LtS => (lhs as i64) < (rhs as i64),
                            CmpKind::LeS => (lhs as i64) <= (rhs as i64),
                            CmpKind::GtS => (lhs as i64) > (rhs as i64),
                            CmpKind::GeS => (lhs as i64) >= (rhs as i64),
                        };
                        assert!(
                            contains(result, u64::from(value)),
                            "kind={kind:?} lhs={lhs:#x} rhs={rhs:#x}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn comparison_facts_preserve_signedness_and_unknown_bits() {
    for lhs in [0u64, 1, 1 << 31, 1 << 63, u64::MAX] {
        for rhs in [0u64, 1, 1 << 31, 1 << 63, u64::MAX] {
            for (kind, expected) in [
                (CmpKind::Eq, lhs == rhs),
                (CmpKind::Ne, lhs != rhs),
                (CmpKind::LtU, lhs < rhs),
                (CmpKind::LeU, lhs <= rhs),
                (CmpKind::GtU, lhs > rhs),
                (CmpKind::GeU, lhs >= rhs),
                (CmpKind::LtS, (lhs as i64) < (rhs as i64)),
                (CmpKind::LeS, (lhs as i64) <= (rhs as i64)),
                (CmpKind::GtS, (lhs as i64) > (rhs as i64)),
                (CmpKind::GeS, (lhs as i64) >= (rhs as i64)),
            ] {
                assert_eq!(
                    KnownBits::constant(lhs)
                        .compare(KnownBits::constant(rhs), kind)
                        .value(),
                    Some(u64::from(expected))
                );
            }
        }
    }
    let low_clear = KnownBits { zero: 1, one: 0 };
    let low_set = KnownBits { zero: 0, one: 1 };
    assert_eq!(low_clear.compare(low_set, CmpKind::Eq).value(), Some(0));
    assert_eq!(low_clear.compare(low_set, CmpKind::Ne).value(), Some(1));
    assert_eq!(low_clear.compare(low_set, CmpKind::LtU).value(), None);
    assert_eq!(low_clear.truth(), None);
    assert_eq!(low_set.truth(), Some(true));
}

#[test]
fn optimized_array_address_arithmetic_executes_with_wrapping_and_narrow_inputs() {
    use crate::native::{emit, jit_mem::JitCode, mir_legalize, regalloc};

    for size in [OpSize::S32, OpSize::S64] {
        for factor in [0u64, 7, 8, 32, 288, 1 << 63] {
            for addend in [0i32, 9, 163, -9] {
                let mut vregs = VRegAllocator::new();
                for _ in 0..8 {
                    vregs.alloc();
                }
                let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 8]);
                let mut block = MBlock::new(BlockId(0));
                block.push(MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 0,
                    size,
                });
                block.push(MInst::LoadImm {
                    dst: VReg(1),
                    value: factor,
                });
                block.push(MInst::Mul {
                    dst: VReg(2),
                    lhs: VReg(0),
                    rhs: VReg(1),
                });
                block.push(MInst::AddImm {
                    dst: VReg(3),
                    src: VReg(2),
                    imm: addend,
                });
                block.push(MInst::ShrImm {
                    dst: VReg(4),
                    src: VReg(3),
                    imm: 3,
                });
                block.push(MInst::Store {
                    base: BaseReg::SimState,
                    offset: 8,
                    src: VReg(4),
                    size: OpSize::S64,
                });
                block.push(MInst::AndImm {
                    dst: VReg(5),
                    src: VReg(3),
                    imm: 7,
                });
                block.push(MInst::Store {
                    base: BaseReg::SimState,
                    offset: 16,
                    src: VReg(5),
                    size: OpSize::S64,
                });
                block.push(MInst::LoadImm {
                    dst: VReg(6),
                    value: 0xfedc_ba98_7654_3210,
                });
                block.push(MInst::Shr {
                    dst: VReg(7),
                    lhs: VReg(6),
                    rhs: VReg(5),
                });
                block.push(MInst::Store {
                    base: BaseReg::SimState,
                    offset: 24,
                    src: VReg(7),
                    size: OpSize::S64,
                });
                block.push(MInst::Return);
                func.push_block(block);
                mir_legalize::legalize(&mut func);
                optimize(&mut func);
                let allocation = regalloc::run_regalloc(&mut func).unwrap();
                let emitted =
                    emit::emit(&func, &allocation.assignment, allocation.spill_frame_size).unwrap();
                let jit = JitCode::new(&emitted.code).unwrap();
                for input in [0u64, 1, 15, u32::MAX as u64, u64::MAX / 32, u64::MAX] {
                    let mut state = [0u8; 32];
                    state[..8].copy_from_slice(&input.to_le_bytes());
                    let source = input & machine_width_mask(size);
                    let address = source.wrapping_mul(factor).wrapping_add(addend as u64);
                    assert_eq!(unsafe { jit.call(&mut state) }, 0);
                    for (offset, expected) in [
                        (8, address >> 3),
                        (16, address & 7),
                        (24, 0xfedc_ba98_7654_3210 >> (address & 7)),
                    ] {
                        assert_eq!(
                            u64::from_le_bytes(state[offset..offset + 8].try_into().unwrap()),
                            expected,
                            "{size:?}: ({input:#x} * {factor:#x} + {addend}), output {offset}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn folds_fixed_offset_bits_across_blocks_but_keeps_distinct_phi_inputs() {
    let mut vregs = VRegAllocator::new();
    for _ in 0..8 {
        vregs.alloc();
    }
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 8]);
    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::Load {
        dst: VReg(0),
        base: BaseReg::SimState,
        offset: 0,
        size: OpSize::S64,
    });
    entry.push(MInst::LoadImm {
        dst: VReg(1),
        value: 288,
    });
    entry.push(MInst::Mul {
        dst: VReg(2),
        lhs: VReg(0),
        rhs: VReg(1),
    });
    entry.push(MInst::Branch {
        cond: VReg(0),
        true_bb: BlockId(1),
        false_bb: BlockId(2),
    });
    let mut left = MBlock::new(BlockId(1));
    left.push(MInst::AddImm {
        dst: VReg(3),
        src: VReg(2),
        imm: 163,
    });
    left.push(MInst::AndImm {
        dst: VReg(4),
        src: VReg(3),
        imm: 7,
    });
    left.push(MInst::Jump { target: BlockId(3) });
    let mut right = MBlock::new(BlockId(2));
    right.push(MInst::LoadImm {
        dst: VReg(5),
        value: 5,
    });
    right.push(MInst::Jump { target: BlockId(3) });
    let mut exit = MBlock::new(BlockId(3));
    exit.phis.push(PhiNode {
        dst: VReg(6),
        sources: vec![(BlockId(1), VReg(4)), (BlockId(2), VReg(5))],
    });
    exit.push(MInst::AndImm {
        dst: VReg(7),
        src: VReg(6),
        imm: 1,
    });
    exit.push(MInst::Return);
    // Deliberately visit the consumer before the defining predecessor.
    for block in [entry, exit, left, right] {
        func.push_block(block);
    }
    let facts = analyze(&func);
    assert_eq!(facts[4].value(), Some(3));
    assert_eq!(facts[6].value(), None);
    assert_eq!(facts[7].value(), Some(1));
    fold(&mut func);
    assert!(matches!(
        func.blocks[2].insts[1],
        MInst::LoadImm {
            dst: VReg(4),
            value: 3
        }
    ));
}
