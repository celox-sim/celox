use super::*;
use crate::native::{emit, jit_mem::JitCode, mir_legalize, regalloc};

fn fixture(guarded: bool, blocker: u8) -> MFunction {
    let mut vregs = VRegAllocator::new();
    for _ in 0..80 {
        vregs.alloc();
    }
    let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 80]);
    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm {
        dst: VReg(0),
        value: 0,
    });
    entry.push(MInst::Jump { target: BlockId(1) });
    let mut header = MBlock::new(BlockId(1));
    header.phis.push(PhiNode {
        dst: VReg(2),
        sources: vec![(BlockId(0), VReg(0)), (BlockId(4), VReg(65))],
    });
    header.phis.push(PhiNode {
        dst: VReg(3),
        sources: vec![(BlockId(0), VReg(0)), (BlockId(4), VReg(66))],
    });
    header.push(MInst::Load {
        dst: VReg(4),
        base: BaseReg::SimState,
        offset: 0,
        size: OpSize::S64,
    });
    header.push(MInst::Load {
        dst: VReg(5),
        base: BaseReg::SimState,
        offset: 8,
        size: OpSize::S64,
    });
    header.push(MInst::AndImm {
        dst: VReg(6),
        src: VReg(5),
        imm: 1,
    });
    for case in 0..3 {
        let start = 10 + case * 12;
        header.push(MInst::CmpImm {
            dst: VReg(start),
            lhs: VReg(4),
            imm: if blocker == 3 && case == 1 {
                0
            } else {
                case as i32
            },
            kind: CmpKind::Eq,
        });
        header.push(MInst::Load {
            dst: VReg(start + 1),
            base: BaseReg::SimState,
            offset: 32 + case as i32 * 8,
            size: OpSize::S64,
        });
        for step in 2..=5 {
            header.push(MInst::AddImm {
                dst: VReg(start + step),
                src: VReg(start + step - 1),
                imm: 1,
            });
        }
        header.push(MInst::CmpImm {
            dst: VReg(start + 10),
            lhs: VReg(start + 5),
            imm: case as i32 + 7,
            kind: CmpKind::Eq,
        });
        header.push(MInst::And32 {
            dst: VReg(start + 11),
            lhs: VReg(start),
            rhs: VReg(start + 10),
        });
    }
    header.push(MInst::Or32 {
        dst: VReg(60),
        lhs: VReg(21),
        rhs: VReg(33),
    });
    header.push(MInst::Or {
        dst: VReg(61),
        lhs: VReg(60),
        rhs: VReg(45),
    });
    if blocker == 1 {
        header.push(MInst::MemFill {
            dst_offset: 32,
            byte_len: 8,
            value: 0xa5,
        });
    }
    let cond = if guarded {
        header.push(MInst::And32 {
            dst: VReg(62),
            lhs: VReg(6),
            rhs: VReg(61),
        });
        VReg(62)
    } else {
        VReg(61)
    };
    if blocker == 4 {
        header.push(MInst::AddImm {
            dst: VReg(68),
            src: cond,
            imm: 10,
        });
    }
    header.push(MInst::Branch {
        cond,
        true_bb: BlockId(2),
        false_bb: BlockId(3),
    });
    let mut hit = MBlock::new(BlockId(2));
    hit.push(MInst::AddImm {
        dst: VReg(64),
        src: VReg(2),
        imm: 1,
    });
    hit.push(MInst::Jump { target: BlockId(4) });
    let mut miss = MBlock::new(BlockId(3));
    miss.push(MInst::Jump { target: BlockId(4) });
    let mut latch = MBlock::new(BlockId(4));
    latch.phis.push(PhiNode {
        dst: VReg(65),
        sources: vec![(BlockId(2), VReg(64)), (BlockId(3), VReg(2))],
    });
    latch.push(MInst::AddImm {
        dst: VReg(66),
        src: VReg(3),
        imm: 1,
    });
    latch.push(MInst::CmpImm {
        dst: VReg(67),
        lhs: VReg(66),
        imm: 4,
        kind: CmpKind::LtU,
    });
    latch.push(MInst::Branch {
        cond: VReg(67),
        true_bb: BlockId(1),
        false_bb: BlockId(5),
    });
    let mut exit = MBlock::new(BlockId(5));
    exit.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 80,
        src: VReg(65),
        size: OpSize::S64,
    });
    exit.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 88,
        src: cond,
        size: OpSize::S64,
    });
    if blocker == 2 {
        exit.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 72,
            src: VReg(20),
            size: OpSize::S64,
        });
    }
    if blocker == 4 {
        exit.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 72,
            src: VReg(68),
            size: OpSize::S64,
        });
    }
    exit.push(MInst::Return);
    function.blocks = vec![entry, header, hit, miss, latch, exit];
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
        code.required_state_size.max(96) as usize,
    )
}

#[test]
fn exclusive_dispatch_preserves_low_bit_of_wide_predicates_and_guards() {
    for guarded in [false, true] {
        let mut original = fixture(guarded, 0);
        for inst in &mut original.blocks[1].insts {
            match *inst {
                MInst::AndImm {
                    dst: VReg(6), src, ..
                } => {
                    *inst = MInst::Mov { dst: VReg(6), src };
                }
                MInst::CmpImm { dst, lhs, .. } if [20, 32, 44].contains(&dst.0) => {
                    *inst = MInst::Mov { dst, src: lhs };
                }
                _ => {}
            }
        }
        original.verify();
        let mut optimized = original.clone();
        run(&mut optimized);
        optimized.verify();
        assert_eq!(optimized.blocks.len(), 13);
        let (before, before_size) = compile(original);
        let (after, after_size) = compile(optimized);
        for key in [0u64, 1, 2, 3, u64::MAX] {
            for guard in [0u64, 1, 2, 3, 1 << 32, 1 << 63, u64::MAX] {
                for input in [0u64, 1, 2, 3, 1 << 32, 1 << 63, u64::MAX] {
                    let mut left = vec![0u8; before_size.max(after_size)];
                    left[..8].copy_from_slice(&key.to_le_bytes());
                    left[8..16].copy_from_slice(&guard.to_le_bytes());
                    for lane in 0..3usize {
                        left[32 + lane * 8..40 + lane * 8]
                            .copy_from_slice(&input.wrapping_add(lane as u64).to_le_bytes());
                    }
                    let mut right = left.clone();
                    assert_eq!(unsafe { before.call(&mut left) }, 0);
                    assert_eq!(unsafe { after.call(&mut right) }, 0);
                    assert_eq!(
                        &left[..96],
                        &right[..96],
                        "guarded={guarded} key={key} guard={guard} input={input}"
                    );
                    let expected = u64::from(
                        key < 3
                            && input.wrapping_add(key).wrapping_add(4) & 1 != 0
                            && (!guarded || guard & 1 != 0),
                    );
                    assert_eq!(
                        u64::from_le_bytes(right[80..88].try_into().unwrap()),
                        expected * 4
                    );
                    assert_eq!(
                        u64::from_le_bytes(right[88..96].try_into().unwrap()),
                        expected
                    );
                }
            }
        }
    }
}

#[test]
fn exclusive_dispatch_preserves_shared_results_and_rejects_unsafe_trees() {
    for guarded in [false, true] {
        for blocker in 0..5 {
            let original = fixture(guarded, blocker);
            original.verify();
            let mut optimized = original.clone();
            run(&mut optimized);
            optimized.verify();
            assert_eq!(optimized.blocks.len(), if blocker == 0 { 13 } else { 6 });
            let (before, before_size) = compile(original);
            let (after, after_size) = compile(optimized);
            for key in [0u64, 1, 2, 3, u64::MAX] {
                for guard in [0u64, 1, 2, 3] {
                    for matches in 0..8 {
                        let mut left = vec![0u8; before_size.max(after_size)];
                        left[..8].copy_from_slice(&key.to_le_bytes());
                        left[8..16].copy_from_slice(&guard.to_le_bytes());
                        for lane in 0..3usize {
                            let value =
                                lane as u64 + if matches & (1 << lane) != 0 { 3 } else { 4 };
                            left[32 + lane * 8..40 + lane * 8]
                                .copy_from_slice(&value.to_le_bytes());
                        }
                        let mut right = left.clone();
                        assert_eq!(unsafe { before.call(&mut left) }, 0);
                        assert_eq!(unsafe { after.call(&mut right) }, 0);
                        assert_eq!(
                            &left[..96],
                            &right[..96],
                            "guarded={guarded}, blocker={blocker}, key={key}, guard={guard}, matches={matches}"
                        );
                    }
                }
            }
        }
    }
}
