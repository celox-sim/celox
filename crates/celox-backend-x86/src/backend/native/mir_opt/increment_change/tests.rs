use super::*;
use crate::native::{emit, jit_mem::JitCode, mir_legalize, regalloc};

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
fn pipeline_reuses_comparisons_exposed_by_tree_scheduling() {
    let mut vregs = VRegAllocator::new();
    for _ in 0..18 {
        vregs.alloc();
    }
    let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 18]);
    let mut block = MBlock::new(BlockId(0));
    for i in 0..3 {
        block.push(MInst::Load {
            dst: VReg(i),
            base: BaseReg::SimState,
            offset: i as i32 * 8,
            size: OpSize::S32,
        });
    }
    block.push(MInst::LoadImm {
        dst: VReg(3),
        value: 1,
    });
    block.push(MInst::Cmp {
        dst: VReg(4),
        lhs: VReg(1),
        rhs: VReg(2),
        kind: CmpKind::Ne,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 24,
        src: VReg(4),
        size: OpSize::S64,
    });
    for i in 9..13 {
        block.push(MInst::Load {
            dst: VReg(i),
            base: BaseReg::SimState,
            offset: 40 + (i - 9) as i32,
            size: OpSize::S8,
        });
    }
    block.push(MInst::Or32 {
        dst: VReg(13),
        lhs: VReg(4),
        rhs: VReg(9),
    });
    block.push(MInst::Add32 {
        dst: VReg(5),
        lhs: VReg(0),
        rhs: VReg(3),
    });
    block.push(MInst::CmpSelect {
        dst: VReg(6),
        lhs: VReg(1),
        rhs: VReg(2),
        kind: CmpKind::Ne,
        true_val: VReg(5),
        false_val: VReg(0),
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 0,
        src: VReg(6),
        size: OpSize::S32,
    });
    block.push(MInst::Xor32 {
        dst: VReg(7),
        lhs: VReg(0),
        rhs: VReg(6),
    });
    block.push(MInst::CmpImm {
        dst: VReg(8),
        lhs: VReg(7),
        imm: 0,
        kind: CmpKind::Ne,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 32,
        src: VReg(8),
        size: OpSize::S64,
    });
    for (i, rhs) in [VReg(8), VReg(10), VReg(11), VReg(12)]
        .into_iter()
        .enumerate()
    {
        block.push(MInst::Or32 {
            dst: VReg(14 + i as u32),
            lhs: VReg(13 + i as u32),
            rhs,
        });
    }
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 48,
        src: VReg(17),
        size: OpSize::S64,
    });
    block.push(MInst::Return);
    function.push_block(block);
    pipeline::optimize(&mut function);
    function.verify();
    let stored = |offset| {
        function
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .find_map(|inst| match inst {
                MInst::Store {
                    offset: at, src, ..
                } if *at == offset => Some(*src),
                _ => None,
            })
            .unwrap()
    };
    assert_eq!(stored(24), stored(32), "{:?}", function.blocks);
    let (code, size) = compile(function);
    for count in [0u32, u32::MAX] {
        for changed in [false, true] {
            let mut memory = vec![0; size];
            memory[..4].copy_from_slice(&count.to_le_bytes());
            memory[8..12].copy_from_slice(&u32::from(changed).to_le_bytes());
            assert_eq!(unsafe { code.call(&mut memory) }, 0);
            assert_eq!(
                u32::from_le_bytes(memory[..4].try_into().unwrap()),
                count.wrapping_add(u32::from(changed))
            );
            assert_eq!(
                u64::from_le_bytes(memory[32..40].try_into().unwrap()),
                u64::from(changed)
            );
        }
    }
}

#[test]
fn increment_changes_preserve_wraparound_widths_and_shared_xor_values() {
    for bounded in [false, true] {
        for arithmetic in 0..6 {
            for observation in 0..3 {
                for selector in 0..3 {
                    for reversed in [false, true] {
                        for equal in [false, true] {
                            for amount in [0, 1, u32::MAX as u64, 1 << 32, u64::MAX] {
                                if arithmetic >= 4 && amount as i64 != amount as i32 as i64 {
                                    continue;
                                }
                                let mut vregs = VRegAllocator::new();
                                for _ in 0..8 {
                                    vregs.alloc();
                                }
                                let mut original =
                                    MFunction::new(vregs, vec![SpillDesc::transient(); 8]);
                                let mut block = MBlock::new(BlockId(0));
                                for i in 0..2 {
                                    block.push(MInst::Load {
                                        dst: VReg(i),
                                        base: BaseReg::SimState,
                                        offset: i as i32 * 8,
                                        size: if bounded && i == 0 {
                                            OpSize::S32
                                        } else {
                                            OpSize::S64
                                        },
                                    });
                                }
                                block.push(MInst::LoadImm {
                                    dst: VReg(2),
                                    value: amount,
                                });
                                let (dst, lhs, rhs) = (VReg(3), VReg(0), VReg(2));
                                block.push(match arithmetic {
                                    0 => MInst::Add {
                                        dst,
                                        lhs: rhs,
                                        rhs: lhs,
                                    },
                                    1 => MInst::Add32 { dst, lhs, rhs },
                                    2 => MInst::Sub { dst, lhs, rhs },
                                    3 => MInst::Sub32 { dst, lhs, rhs },
                                    4 => MInst::AddImm {
                                        dst,
                                        src: lhs,
                                        imm: amount as i32,
                                    },
                                    _ => MInst::SubImm {
                                        dst,
                                        src: lhs,
                                        imm: amount as i32,
                                    },
                                });
                                let (true_val, false_val) = if reversed {
                                    (VReg(0), VReg(3))
                                } else {
                                    (VReg(3), VReg(0))
                                };
                                block.push(match selector {
                                    0 => MInst::Select {
                                        dst: VReg(4),
                                        cond: VReg(1),
                                        true_val,
                                        false_val,
                                    },
                                    1 => MInst::CmpSelect {
                                        dst: VReg(4),
                                        lhs: VReg(1),
                                        rhs: VReg(2),
                                        kind: CmpKind::LtU,
                                        true_val,
                                        false_val,
                                    },
                                    _ => MInst::CmpImmSelect {
                                        dst: VReg(4),
                                        lhs: VReg(1),
                                        imm: -1,
                                        kind: CmpKind::GtS,
                                        true_val,
                                        false_val,
                                    },
                                });
                                let (dst, lhs, rhs) = (VReg(5), VReg(4), VReg(0));
                                block.push(if observation == 2 {
                                    MInst::Xor32 { dst, lhs, rhs }
                                } else {
                                    MInst::Xor { dst, lhs, rhs }
                                });
                                let kind = if equal { CmpKind::Eq } else { CmpKind::Ne };
                                block.push(if observation == 0 {
                                    MInst::Cmp {
                                        dst: VReg(6),
                                        lhs: VReg(4),
                                        rhs: VReg(0),
                                        kind,
                                    }
                                } else {
                                    MInst::CmpImm {
                                        dst: VReg(6),
                                        lhs: VReg(5),
                                        imm: 0,
                                        kind,
                                    }
                                });
                                for (offset, src) in [(16, VReg(4)), (24, VReg(5)), (32, VReg(6))] {
                                    block.push(MInst::Store {
                                        base: BaseReg::SimState,
                                        offset,
                                        src,
                                        size: OpSize::S64,
                                    });
                                }
                                block.push(MInst::Return);
                                original.push_block(block);
                                let mut optimized = original.clone();
                                fold(&mut optimized);
                                lower_unit_steps(&mut optimized);
                                let narrow = matches!(arithmetic, 1 | 3);
                                let mask = if narrow {
                                    u64::from(u32::MAX)
                                } else {
                                    u64::MAX
                                };
                                let unit = amount & mask;
                                let should_lower =
                                    !equal && (!narrow || bounded) && (unit == 1 || unit == mask);
                                let selected = optimized.blocks[0]
                                    .insts
                                    .iter()
                                    .find(|inst| inst.def() == Some(VReg(4)))
                                    .unwrap();
                                assert_eq!(
                                    matches!(
                                        selected,
                                        MInst::Add { .. }
                                            | MInst::Add32 { .. }
                                            | MInst::Sub { .. }
                                            | MInst::Sub32 { .. }
                                    ),
                                    should_lower
                                );
                                optimized.verify();
                                let should_fold = if observation == 2 || matches!(arithmetic, 1 | 3)
                                {
                                    amount as u32 != 0
                                } else {
                                    amount != 0
                                };
                                let result = optimized.blocks[0]
                                    .insts
                                    .iter()
                                    .find(|inst| inst.def() == Some(VReg(6)))
                                    .unwrap();
                                assert_eq!(result.uses().contains(&VReg(1)), should_fold);
                                let (before, before_size) = compile(original);
                                let (after, after_size) = compile(optimized);
                                for base in [0u64, 1, u32::MAX as u64, 1 << 32, 1 << 63, u64::MAX] {
                                    for condition in [0u64, 1, 2, 1 << 32, 1 << 63, u64::MAX] {
                                        let mut left = vec![0xa5; before_size.max(after_size)];
                                        left[..8].copy_from_slice(&base.to_le_bytes());
                                        left[8..16].copy_from_slice(&condition.to_le_bytes());
                                        let mut right = left.clone();
                                        assert_eq!(unsafe { before.call(&mut left) }, 0);
                                        assert_eq!(unsafe { after.call(&mut right) }, 0);
                                        assert_eq!(
                                            &left[..40],
                                            &right[..40],
                                            "bounded={bounded} arithmetic={arithmetic} observation={observation} selector={selector} reversed={reversed} equal={equal} amount={amount:#x} base={base:#x} condition={condition:#x}"
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
