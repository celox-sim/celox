use super::*;
use crate::native::{emit, jit_mem::JitCode, mir_legalize, regalloc};

pub(crate) fn shared_condition_fixture(width: u32) -> MFunction {
    let mut function = circular_scan::tests::fixture(width, 0);
    for _ in 0..2 {
        function.vregs.alloc();
        function.spill_descs.push(SpillDesc::transient());
    }
    let mut comparisons = std::mem::take(&mut function.blocks[2].insts);
    comparisons.pop();
    let header = &mut function.blocks[1];
    header.insts.pop();
    header.insts.extend(comparisons);
    header.push(MInst::And32 {
        dst: VReg(28),
        lhs: VReg(16),
        rhs: VReg(25),
    });
    header.push(MInst::Branch {
        cond: VReg(28),
        true_bb: BlockId(3),
        false_bb: BlockId(4),
    });
    header.phis.push(PhiNode {
        dst: VReg(31),
        sources: vec![(BlockId(0), VReg(0)), (BlockId(5), VReg(30))],
    });
    for position in [3, 4] {
        function.blocks[position].insts = vec![MInst::Jump { target: BlockId(7) }];
    }
    let mut join = MBlock::new(BlockId(7));
    join.phis = std::mem::take(&mut function.blocks[5].phis);
    join.push(MInst::Branch {
        cond: VReg(28),
        true_bb: BlockId(8),
        false_bb: BlockId(9),
    });
    let mut update = MBlock::new(BlockId(8));
    update.push(MInst::LoadIndexed {
        dst: VReg(29),
        base: BaseReg::SimState,
        offset: 128,
        index: VReg(7),
        scale: 8,
        size: OpSize::S64,
        alias_range: MemoryAliasRange::new(128, width as usize * 8),
    });
    update.push(MInst::Jump { target: BlockId(5) });
    let mut skip = MBlock::new(BlockId(9));
    skip.push(MInst::Jump { target: BlockId(5) });
    function.blocks[5].phis.push(PhiNode {
        dst: VReg(30),
        sources: vec![(BlockId(8), VReg(29)), (BlockId(9), VReg(31))],
    });
    function.blocks[6].insts.insert(
        0,
        MInst::Store {
            base: BaseReg::SimState,
            offset: 56,
            src: VReg(30),
            size: OpSize::S64,
        },
    );
    function.blocks.retain(|block| block.id != BlockId(2));
    function.blocks.extend([join, update, skip]);
    function
}

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
fn bitmap_worklists_preserve_empty_full_and_sparse_candidates_and_payloads() {
    for width in [8u32, 9, 16, 32, 48, 64] {
        for shared in [false, true] {
            let original = if shared {
                shared_condition_fixture(width)
            } else {
                circular_scan::tests::fixture(width, 0)
            };
            original.verify();
            let mut optimized = original.clone();
            run(&mut optimized);
            optimized.verify();
            assert_eq!(
                optimized.blocks.len(),
                original.blocks.len() + 1,
                "width={width} shared={shared}"
            );
            let (before, before_size) = compile(original);
            let (after, after_size) = compile(optimized);
            for input in [
                0u64,
                1,
                0xaaaa_aaaa_5555_5555,
                0x0123_4567_89ab_cdef,
                u64::MAX,
            ]
            .into_iter()
            .chain((0..64).map(|bit| 1u64 << bit))
            {
                for guard in [0u64, 1, 2, 3, 1 << 32, u64::MAX] {
                    for origin in [
                        0u64,
                        1,
                        u64::from(width - 1),
                        u64::from(width),
                        63,
                        64,
                        u64::MAX,
                    ] {
                        let a = input;
                        let b = if guard & 2 == 0 {
                            u64::MAX
                        } else {
                            input.rotate_left(1)
                        };
                        let c = input.rotate_left(3);
                        let mut expected = None;
                        for index in 0..width {
                            if (((a >> index) & (b >> index)) & ((c >> index) | guard)) & 1 != 0 {
                                let distance =
                                    u64::from(index).wrapping_sub(origin) & u64::from(width - 1);
                                if expected.is_none_or(|(old, _)| distance < old) {
                                    expected = Some((distance, index));
                                }
                            }
                        }
                        let mut left = vec![0u8; before_size.max(after_size)];
                        for (offset, value) in [(0, a), (8, b), (16, c), (24, guard), (32, origin)]
                        {
                            left[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
                        }
                        for index in 0..width as usize {
                            left[128 + index * 8..136 + index * 8]
                                .copy_from_slice(&(index as u64 * 7 + 5).to_le_bytes());
                        }
                        let mut right = left.clone();
                        assert_eq!(unsafe { before.call(&mut left) }, 0);
                        assert_eq!(unsafe { after.call(&mut right) }, 0);
                        assert_eq!(
                            &left[..768],
                            &right[..768],
                            "width={width} shared={shared} input={input:#x} guard={guard:#x} origin={origin}"
                        );
                        assert_eq!(&right[40..48], &u64::from(expected.is_some()).to_le_bytes());
                        assert_eq!(
                            &right[48..56],
                            &expected.map_or(0, |(distance, _)| distance).to_le_bytes()
                        );
                        if shared {
                            assert_eq!(
                                &right[56..64],
                                &expected
                                    .map_or(0, |(_, index)| u64::from(index) * 7 + 5)
                                    .to_le_bytes()
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn bitmap_worklists_keep_observable_skips_and_exported_iteration_indices() {
    for blocker in [1, 3] {
        let mut function = circular_scan::tests::fixture(32, blocker);
        let original = format!("{function:?}");
        run(&mut function);
        function.verify();
        assert_eq!(format!("{function:?}"), original);
    }
    let mut function = shared_condition_fixture(32);
    function
        .blocks
        .iter_mut()
        .find(|block| block.id == BlockId(7))
        .unwrap()
        .insts[0] = MInst::Branch {
        cond: VReg(5),
        true_bb: BlockId(8),
        false_bb: BlockId(9),
    };
    let original = format!("{function:?}");
    run(&mut function);
    function.verify();
    assert_eq!(format!("{function:?}"), original);
}
