use super::*;
use crate::mir_opt::if_select::run;

fn fixture(shape: u8, predicate: u8, blocker: u8) -> MFunction {
    let mut entry = MBlock::new(BlockId(0));
    for value in 0..4 {
        entry.push(MInst::Load {
            dst: VReg(value),
            base: BaseReg::SimState,
            offset: value as i32 * 8,
            size: OpSize::S64,
        });
    }
    entry.push(MInst::Jump { target: BlockId(1) });
    let true_bb = BlockId(if shape == 2 { 4 } else { 2 });
    let false_bb = BlockId(if shape == 1 { 4 } else { 3 });
    let mut head = MBlock::new(BlockId(1));
    head.push(match predicate {
        0 => MInst::Branch {
            cond: VReg(0),
            true_bb,
            false_bb,
        },
        1 => MInst::BranchPred {
            predicate: BranchPredicate::Compare {
                lhs: VReg(0),
                rhs: VReg(3),
                kind: CmpKind::LtU,
            },
            true_bb,
            false_bb,
        },
        2 => MInst::BranchPred {
            predicate: BranchPredicate::CompareImm {
                lhs: VReg(0),
                imm: 7,
                kind: CmpKind::LtU,
            },
            true_bb,
            false_bb,
        },
        _ => MInst::BranchPred {
            predicate: BranchPredicate::MemoryNonZero {
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
            },
            true_bb,
            false_bb,
        },
    });
    let mut blocks = vec![entry, head];
    let arm_id = if shape == 2 { false_bb } else { true_bb };
    for id in [true_bb, false_bb] {
        if id == BlockId(4) {
            continue;
        }
        let mut arm = MBlock::new(id);
        if id == arm_id && [1, 2].contains(&blocker) {
            if blocker == 2 {
                arm.phis.push(PhiNode {
                    dst: VReg(7),
                    sources: vec![(BlockId(1), VReg(1))],
                });
            }
            arm.push(MInst::Store {
                base: BaseReg::SimState,
                offset: 56,
                src: VReg(if blocker == 2 { 7 } else { 1 }),
                size: OpSize::S64,
            });
        }
        arm.push(MInst::Jump { target: BlockId(4) });
        blocks.push(arm);
    }
    let mut join = MBlock::new(BlockId(4));
    for (dst, true_val, false_val) in [
        (VReg(4), VReg(1), VReg(2)),
        (VReg(5), VReg(2), VReg(1)),
        (VReg(6), VReg(1), VReg(1)),
    ] {
        join.phis.push(PhiNode {
            dst,
            sources: vec![
                (if shape == 2 { BlockId(1) } else { true_bb }, true_val),
                (if shape == 1 { BlockId(1) } else { false_bb }, false_val),
            ],
        });
        join.push(MInst::Store {
            base: BaseReg::SimState,
            offset: dst.0 as i32 * 8,
            src: dst,
            size: OpSize::S64,
        });
    }
    if [3, 4].contains(&blocker) {
        let target = if blocker == 3 { arm_id } else { join.id };
        *blocks[0].insts.last_mut().unwrap() = MInst::Branch {
            cond: VReg(3),
            true_bb: target,
            false_bb: BlockId(1),
        };
        if blocker == 4 {
            for phi in &mut join.phis {
                phi.sources.push((BlockId(0), VReg(1)));
            }
        }
    }
    join.push(MInst::Return);
    blocks.push(join);
    MFunction::new(blocks, vec![])
}

fn compile_raw(function: MFunction) -> (JitCode, usize) {
    let allocation = crate::regalloc::allocate_with_spills(function, || false).unwrap();
    let emitted = emit_function(
        &allocation.allocated.function,
        &allocation.allocated.assignment,
        allocation.spill_frame_size,
        64,
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
fn empty_selection_arms_preserve_each_value_and_predicate() {
    for shape in 0..3 {
        for predicate in 0..3 {
            let original = fixture(shape, predicate, 0);
            let mut optimized = original.clone();
            run(&mut optimized);
            assert_eq!(optimized.blocks.len(), 3);
            assert!(optimized.blocks.iter().all(|block| block.phis.is_empty()));
            let (before, before_size) = compile_raw(original);
            let (after, after_size) = compile_raw(optimized);
            for guard in [0u64, 1, 2, 6, 7, 8, 1 << 32, 1 << 63, u64::MAX] {
                for rhs in [0u64, 1, 7, u64::MAX] {
                    for (left, right) in [(0u64, 0u64), (1, u64::MAX), (1 << 63, 5), (u64::MAX, 0)]
                    {
                        let mut a = vec![0xa5; before_size.max(after_size)];
                        for (index, value) in [guard, left, right, rhs].into_iter().enumerate() {
                            a[index * 8..index * 8 + 8].copy_from_slice(&value.to_le_bytes());
                        }
                        let mut b = a.clone();
                        assert_eq!(unsafe { (before.fn_ptr)(a.as_mut_ptr()) }, 0);
                        assert_eq!(unsafe { (after.fn_ptr)(b.as_mut_ptr()) }, 0);
                        assert_eq!(&a[..64], &b[..64]);
                        let chosen = match predicate {
                            0 => guard != 0,
                            1 => guard < rhs,
                            _ => guard < 7,
                        };
                        assert_eq!(&b[32..40], &if chosen { left } else { right }.to_le_bytes());
                        assert_eq!(&b[40..48], &if chosen { right } else { left }.to_le_bytes());
                        assert_eq!(&b[48..56], &left.to_le_bytes());
                    }
                }
            }
        }
    }
}

#[test]
fn empty_selection_arms_keep_effects_external_entries_and_memory_predicates() {
    for shape in 0..3 {
        for blocker in 1..5 {
            let mut function = fixture(shape, 0, blocker);
            let before = format!("{function:?}");
            run(&mut function);
            assert_eq!(
                format!("{function:?}"),
                before,
                "shape={shape} blocker={blocker}"
            );
        }
        let mut function = fixture(shape, 3, 0);
        let before = format!("{function:?}");
        run(&mut function);
        assert_eq!(format!("{function:?}"), before);
    }
}
