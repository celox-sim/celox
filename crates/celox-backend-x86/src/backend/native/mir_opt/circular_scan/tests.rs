use super::*;
use crate::native::{emit, jit_mem::JitCode, mir_legalize, regalloc};

pub(crate) fn fixture(width: u32, blocker: u8) -> MFunction {
    let mut vregs = VRegAllocator::new();
    for _ in 0..30 {
        vregs.alloc();
    }
    let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 30]);
    let mut entry = MBlock::new(BlockId(0));
    for (dst, value) in [(VReg(0), 0), (VReg(1), 1)] {
        entry.push(MInst::LoadImm { dst, value });
    }
    for (dst, offset) in [
        (VReg(2), 0),
        (VReg(3), 8),
        (VReg(4), 16),
        (VReg(5), 24),
        (VReg(6), 32),
    ] {
        entry.push(MInst::Load {
            dst,
            base: BaseReg::SimState,
            offset,
            size: OpSize::S64,
        });
    }
    entry.push(MInst::Jump { target: BlockId(1) });
    let mut header = MBlock::new(BlockId(1));
    for (dst, next) in [
        (VReg(7), VReg(22)),
        (VReg(8), VReg(20)),
        (VReg(9), VReg(21)),
    ] {
        header.phis.push(PhiNode {
            dst,
            sources: vec![(BlockId(0), VReg(0)), (BlockId(5), next)],
        });
    }
    for (dst, lhs) in [
        (VReg(10), VReg(2)),
        (VReg(11), VReg(3)),
        (VReg(12), VReg(4)),
    ] {
        header.push(MInst::Shr {
            dst,
            lhs,
            rhs: VReg(7),
        });
    }
    header.push(MInst::And32 {
        dst: VReg(13),
        lhs: VReg(10),
        rhs: VReg(11),
    });
    header.push(MInst::Or32 {
        dst: VReg(14),
        lhs: VReg(12),
        rhs: VReg(5),
    });
    header.push(MInst::And32 {
        dst: VReg(15),
        lhs: VReg(13),
        rhs: VReg(14),
    });
    header.push(MInst::AndImm32 {
        dst: VReg(16),
        src: VReg(15),
        imm: 1,
    });
    header.push(MInst::Branch {
        cond: VReg(16),
        true_bb: BlockId(2),
        false_bb: BlockId(4),
    });
    let mut choose = MBlock::new(BlockId(2));
    choose.push(MInst::Sub32 {
        dst: VReg(17),
        lhs: VReg(7),
        rhs: VReg(6),
    });
    choose.push(MInst::AndImm32 {
        dst: VReg(18),
        src: VReg(17),
        imm: if blocker == 2 { width - 2 } else { width - 1 },
    });
    choose.push(MInst::CmpImm {
        dst: VReg(19),
        lhs: VReg(8),
        imm: 0,
        kind: CmpKind::Eq,
    });
    choose.push(MInst::Cmp {
        dst: VReg(24),
        lhs: VReg(18),
        rhs: VReg(9),
        kind: CmpKind::LtU,
    });
    choose.push(MInst::Or32 {
        dst: VReg(25),
        lhs: VReg(19),
        rhs: VReg(24),
    });
    choose.push(MInst::Branch {
        cond: VReg(25),
        true_bb: BlockId(3),
        false_bb: BlockId(4),
    });
    let mut update = MBlock::new(BlockId(3));
    if blocker == 1 {
        update.push(MInst::MemFill {
            dst_offset: 64,
            byte_len: 1,
            value: 0x55,
        });
    }
    update.push(MInst::Jump { target: BlockId(5) });
    let mut skip = MBlock::new(BlockId(4));
    skip.push(MInst::Jump { target: BlockId(5) });
    let mut latch = MBlock::new(BlockId(5));
    latch.phis.push(PhiNode {
        dst: VReg(20),
        sources: vec![(BlockId(3), VReg(1)), (BlockId(4), VReg(8))],
    });
    latch.phis.push(PhiNode {
        dst: VReg(21),
        sources: vec![(BlockId(3), VReg(18)), (BlockId(4), VReg(9))],
    });
    latch.push(MInst::Add32 {
        dst: VReg(22),
        lhs: VReg(7),
        rhs: VReg(1),
    });
    latch.push(MInst::CmpImm {
        dst: VReg(23),
        lhs: VReg(22),
        imm: width as i32,
        kind: CmpKind::Ne,
    });
    latch.push(MInst::Branch {
        cond: VReg(23),
        true_bb: BlockId(1),
        false_bb: BlockId(6),
    });
    let mut exit = MBlock::new(BlockId(6));
    for (offset, src) in [(40, VReg(20)), (48, VReg(21))] {
        exit.push(MInst::Store {
            base: BaseReg::SimState,
            offset,
            src,
            size: OpSize::S64,
        });
    }
    if blocker == 3 {
        exit.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 56,
            src: VReg(22),
            size: OpSize::S64,
        });
    }
    exit.push(MInst::Return);
    function.blocks = vec![entry, header, choose, update, skip, latch, exit];
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
        emitted.required_state_size.max(72) as usize,
    )
}

#[test]
fn circular_bitmap_scans_preserve_empty_sets_wrapping_origins_and_distances() {
    for (width, variant) in [8u32, 16, 32]
        .into_iter()
        .flat_map(|width| (0..4).map(move |variant| (width, variant)))
    {
        let mut original = fixture(width, 0);
        for inst in &mut original.blocks[1].insts {
            if variant == 1 && inst.def() == Some(VReg(13)) {
                *inst = MInst::Xor32 {
                    dst: VReg(13),
                    lhs: VReg(10),
                    rhs: VReg(11),
                };
            }
            if variant == 2 && inst.def() == Some(VReg(14)) {
                *inst = MInst::BitNot {
                    dst: VReg(14),
                    src: VReg(12),
                };
            }
        }
        if variant == 3 {
            original.blocks[6].phis.push(PhiNode {
                dst: VReg(26),
                sources: vec![(BlockId(5), VReg(20))],
            });
            original.blocks[6].phis.push(PhiNode {
                dst: VReg(27),
                sources: vec![(BlockId(5), VReg(21))],
            });
            for inst in &mut original.blocks[6].insts {
                inst.rewrite_use(VReg(20), VReg(26));
                inst.rewrite_use(VReg(21), VReg(27));
            }
        }
        original.verify();
        let mut optimized = original.clone();
        run(&mut optimized);
        optimized.verify();
        assert_eq!(optimized.blocks.len(), 2);
        let (before, before_size) = compile(original);
        let (after, after_size) = compile(optimized);
        let inputs = [
            0u64,
            1,
            0xaaaa_aaaa_5555_5555,
            0x0123_4567_89ab_cdef,
            u64::MAX,
        ];
        for input in inputs.into_iter().chain((0..64).map(|bit| 1u64 << bit)) {
            for guard in [0u64, 1, 2, 3, 1 << 32, u64::MAX] {
                for origin in (0..=width as u64).chain([63, 64, 255, u64::MAX]) {
                    let a = input;
                    let b = if guard & 2 == 0 {
                        u64::MAX
                    } else {
                        input.rotate_left(1)
                    };
                    let c = input.rotate_left(3);
                    let mut expected = None;
                    for index in 0..width {
                        let core = if variant == 1 {
                            (a >> index) ^ (b >> index)
                        } else {
                            (a >> index) & (b >> index)
                        };
                        let gate = if variant == 2 {
                            !(c >> index)
                        } else {
                            (c >> index) | guard
                        };
                        if (core & gate) & 1 != 0 {
                            let distance =
                                (u64::from(index).wrapping_sub(origin)) & u64::from(width - 1);
                            expected =
                                Some(expected.map_or(distance, |old: u64| old.min(distance)));
                        }
                    }
                    let mut left = vec![0u8; before_size.max(after_size)];
                    for (offset, value) in [(0, a), (8, b), (16, c), (24, guard), (32, origin)] {
                        left[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
                    }
                    let mut right = left.clone();
                    assert_eq!(unsafe { before.call(&mut left) }, 0);
                    assert_eq!(unsafe { after.call(&mut right) }, 0);
                    assert_eq!(
                        &left[..72],
                        &right[..72],
                        "width={width} input={input:#x} guard={guard:#x} origin={origin}"
                    );
                    assert_eq!(&right[40..48], &u64::from(expected.is_some()).to_le_bytes());
                    assert_eq!(&right[48..56], &expected.unwrap_or(0).to_le_bytes());
                }
            }
        }
    }
}

#[test]
fn circular_bitmap_scans_keep_side_effects_non_circular_masks_and_other_outputs() {
    for blocker in 1..=3 {
        let mut function = fixture(32, blocker);
        let before = format!("{function:?}");
        function.verify();
        run(&mut function);
        function.verify();
        assert_eq!(format!("{function:?}"), before);
    }
}
