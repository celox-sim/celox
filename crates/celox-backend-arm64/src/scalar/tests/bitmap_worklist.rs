use super::*;
use crate::mir::SpillDesc;
use crate::mir_opt::bitmap_worklist::{merge_header_tails, run};

pub(crate) fn shared_condition_fixture(width: u32) -> MFunction {
    let mut function = super::circular_scan::fixture(width, 0);
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
    crate::mir_legalize::legalize_variable_shift_counts(&mut function);
    let allocation = crate::regalloc::allocate_with_spills(function, || false).unwrap();
    let emitted = emit_function(
        &allocation.allocated.function,
        &allocation.allocated.assignment,
        allocation.spill_frame_size,
        768,
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
fn bitmap_worklists_preserve_empty_full_and_sparse_candidates_and_payloads() {
    for width in [8u32, 9, 16, 32, 48, 64] {
        for shape in 0..3 {
            let shared = shape != 0;
            let mut original = if shared {
                shared_condition_fixture(width)
            } else {
                super::circular_scan::fixture(width, 0)
            };
            if shape == 2 {
                make_predicated_header(&mut original);
            }
            crate::regalloc::build_facts(&original).unwrap();
            let mut optimized = original.clone();
            merge_header_tails(&mut optimized);
            let merged_blocks = optimized.blocks.len();
            run(&mut optimized);
            crate::regalloc::build_facts(&optimized).unwrap();
            assert_eq!(
                optimized.blocks.len(),
                merged_blocks + 1,
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
                        assert_eq!(unsafe { (before.fn_ptr)(left.as_mut_ptr()) }, 0);
                        assert_eq!(unsafe { (after.fn_ptr)(right.as_mut_ptr()) }, 0);
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
        let mut function = super::circular_scan::fixture(32, blocker);
        let original = format!("{function:?}");
        run(&mut function);
        crate::regalloc::build_facts(&function).unwrap();
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
    crate::regalloc::build_facts(&function).unwrap();
    assert_eq!(format!("{function:?}"), original);
}

// Match SIR's layout: selections execute in the header, then a private block
// branches to either the payload load or a skip path.
fn make_predicated_header(function: &mut MFunction) {
    let header = &mut function.blocks[1];
    header.insts.pop();
    header.push(MInst::Select {
        dst: VReg(20),
        cond: VReg(28),
        true_val: VReg(1),
        false_val: VReg(8),
    });
    header.push(MInst::Select {
        dst: VReg(21),
        cond: VReg(28),
        true_val: VReg(18),
        false_val: VReg(9),
    });
    header.push(MInst::Jump { target: BlockId(7) });
    function
        .blocks
        .iter_mut()
        .find(|block| block.id == BlockId(7))
        .unwrap()
        .phis
        .clear();
    function
        .blocks
        .retain(|block| ![BlockId(3), BlockId(4)].contains(&block.id));
}

#[test]
fn bitmap_worklists_keep_updates_on_the_skipped_path() {
    let mut function = shared_condition_fixture(32);
    make_predicated_header(&mut function);
    let header = function
        .blocks
        .iter_mut()
        .find(|block| block.id == BlockId(1))
        .unwrap();
    for inst in &mut header.insts {
        if let MInst::Select {
            dst: VReg(20),
            false_val,
            ..
        } = inst
        {
            *false_val = VReg(1);
        }
    }
    merge_header_tails(&mut function);
    let original = format!("{function:?}");
    run(&mut function);
    assert_eq!(format!("{function:?}"), original);
}

#[test]
fn loop_header_tails_preserve_phi_boundaries() {
    let mut function = shared_condition_fixture(32);
    make_predicated_header(&mut function);
    function
        .blocks
        .iter_mut()
        .find(|block| block.id == BlockId(7))
        .unwrap()
        .phis
        .push(PhiNode {
            dst: VReg(33),
            sources: vec![(BlockId(1), VReg(0))],
        });
    crate::regalloc::build_facts(&function).unwrap();
    let original = format!("{function:?}");
    merge_header_tails(&mut function);
    assert_eq!(format!("{function:?}"), original);
}
