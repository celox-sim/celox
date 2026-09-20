use super::*;
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
        emitted.required_state_size.max(768) as usize,
    )
}

#[test]
fn repeated_diamonds_preserve_loop_phis_arm_effects_and_edge_values() {
    for variant in 0..5 {
        let mut original = bitmap_worklist::tests::shared_condition_fixture(32);
        if variant == 1 {
            bitmap_worklist::run(&mut original);
        }
        if variant == 2 {
            for (body, offset) in [(BlockId(8), 64), (BlockId(9), 72)] {
                let block = original
                    .blocks
                    .iter_mut()
                    .find(|block| block.id == body)
                    .unwrap();
                block.insts.insert(
                    0,
                    MInst::Store {
                        base: BaseReg::SimState,
                        offset,
                        src: VReg(21),
                        size: OpSize::S64,
                    },
                );
            }
        }
        if variant == 3 {
            let value = original.vregs.alloc();
            original.spill_descs.push(SpillDesc::transient());
            let block = original
                .blocks
                .iter_mut()
                .find(|block| block.id == BlockId(8))
                .unwrap();
            block.phis.push(PhiNode {
                dst: value,
                sources: vec![(BlockId(7), VReg(21))],
            });
            block.insts.insert(
                0,
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 64,
                    src: value,
                    size: OpSize::S64,
                },
            );
            let block = original
                .blocks
                .iter_mut()
                .find(|block| block.id == BlockId(5))
                .unwrap();
            block.phis[0].sources[1].1 = VReg(21);
        }
        if variant == 4 {
            let block = original
                .blocks
                .iter_mut()
                .find(|block| block.id == BlockId(7))
                .unwrap();
            if let MInst::Branch {
                true_bb, false_bb, ..
            } = &mut block.insts[0]
            {
                std::mem::swap(true_bb, false_bb);
            }
        }
        original.verify();
        let mut optimized = original.clone();
        run(&mut optimized);
        optimized.verify();
        assert_eq!(
            optimized.blocks.len() + 3,
            original.blocks.len(),
            "variant={variant}"
        );
        let (before, before_size) = compile(original);
        let (after, after_size) = compile(optimized);
        for input in [
            0u64,
            1,
            0x5555_5555_aaaa_aaaa,
            0x0123_4567_89ab_cdef,
            u64::MAX,
        ]
        .into_iter()
        .chain((0..64).map(|bit| 1u64 << bit))
        {
            for guard in [0u64, 1, 2, 3, 1 << 32, u64::MAX] {
                for origin in [0u64, 1, 15, 31, 32, 63, u64::MAX] {
                    let mut left = vec![0u8; before_size.max(after_size)];
                    for (offset, value) in [
                        (0, input),
                        (8, input.rotate_right(1) | 1),
                        (16, input.rotate_left(3)),
                        (24, guard),
                        (32, origin),
                    ] {
                        left[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
                    }
                    for index in 0..32usize {
                        left[128 + index * 8..136 + index * 8]
                            .copy_from_slice(&(index as u64 * 7 + 5).to_le_bytes());
                    }
                    let mut right = left.clone();
                    assert_eq!(unsafe { before.call(&mut left) }, 0);
                    assert_eq!(unsafe { after.call(&mut right) }, 0);
                    assert_eq!(
                        &left[..768],
                        &right[..768],
                        "variant={variant} input={input:#x} guard={guard:#x} origin={origin}"
                    );
                }
            }
        }
    }
}

#[test]
fn repeated_diamonds_keep_distinct_conditions_and_observable_first_arms() {
    for blocker in 0..4 {
        let mut function = bitmap_worklist::tests::shared_condition_fixture(32);
        let id = if blocker == 0 || blocker == 2 {
            BlockId(7)
        } else {
            BlockId(3)
        };
        let block = function
            .blocks
            .iter_mut()
            .find(|block| block.id == id)
            .unwrap();
        match blocker {
            0 => {
                if let MInst::Branch { cond, .. } = &mut block.insts[0] {
                    *cond = VReg(5);
                }
            }
            1 => block.insts.insert(
                0,
                MInst::MemFill {
                    dst_offset: 64,
                    byte_len: 1,
                    value: 5,
                },
            ),
            2 => block.insts.insert(
                0,
                MInst::Load {
                    dst: VReg(26),
                    base: BaseReg::SimState,
                    offset: 64,
                    size: OpSize::S64,
                },
            ),
            3 => block.phis.push(PhiNode {
                dst: VReg(26),
                sources: vec![(BlockId(1), VReg(5))],
            }),
            _ => unreachable!(),
        }
        function.verify();
        let before = format!("{function:?}");
        run(&mut function);
        function.verify();
        assert_eq!(format!("{function:?}"), before, "blocker={blocker}");
    }
}
