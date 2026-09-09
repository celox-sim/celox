use super::*;
use crate::mir_opt::byte_predicates::run;

fn fixture(width: usize, raw_use: bool) -> MFunction {
    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm {
        dst: VReg(0),
        value: 0,
    });
    entry.push(MInst::Jump { target: BlockId(1) });
    let mut body = MBlock::new(BlockId(1));
    body.phis.push(PhiNode {
        dst: VReg(2),
        sources: vec![(BlockId(0), VReg(0)), (BlockId(1), VReg(20))],
    });
    for (dst, offset) in [(3, 0), (4, 8), (5, 16)] {
        body.push(MInst::LoadIndexed {
            dst: VReg(dst),
            base: BaseReg::SimState,
            offset,
            index: VReg(2),
            scale: 1,
            size: OpSize::S8,
            alias_range: MemoryAliasRange::new(offset, width),
        });
    }
    body.push(MInst::AndImm {
        dst: VReg(6),
        src: VReg(3),
        imm: 1,
    });
    body.push(MInst::AndImm32 {
        dst: VReg(7),
        src: VReg(4),
        imm: 1,
    });
    body.push(MInst::BitExtract {
        dst: VReg(8),
        src: VReg(5),
        lsb: 0,
        width: 1,
    });
    body.push(MInst::CmpImm {
        dst: VReg(9),
        lhs: VReg(8),
        imm: 0,
        kind: CmpKind::Eq,
    });
    body.push(MInst::And {
        dst: VReg(10),
        lhs: VReg(6),
        rhs: VReg(7),
    });
    body.push(MInst::Or32 {
        dst: VReg(11),
        lhs: VReg(10),
        rhs: VReg(9),
    });
    body.push(MInst::Xor32 {
        dst: VReg(12),
        lhs: VReg(11),
        rhs: VReg(7),
    });
    for (src, offset) in [(VReg(11), 32), (VReg(12), 48)]
        .into_iter()
        .chain(raw_use.then_some((VReg(3), 64)))
    {
        body.push(MInst::StoreIndexed {
            base: BaseReg::SimState,
            offset,
            index: VReg(2),
            src,
            size: OpSize::S8,
            alias_range: MemoryAliasRange::new(offset, width),
        });
    }
    body.push(MInst::AddImm {
        dst: VReg(20),
        src: VReg(2),
        imm: 1,
    });
    body.push(MInst::CmpImm {
        dst: VReg(21),
        lhs: VReg(20),
        imm: width as i32,
        kind: CmpKind::Ne,
    });
    body.push(MInst::Branch {
        cond: VReg(21),
        true_bb: BlockId(1),
        false_bb: BlockId(2),
    });
    let mut exit = MBlock::new(BlockId(2));
    exit.push(MInst::Return);
    MFunction::new(vec![entry, body, exit], vec![])
}

fn compile_raw(mut function: MFunction) -> JitCode {
    crate::mir_legalize::legalize_variable_shift_counts(&mut function);
    let allocated = crate::regalloc::allocate_with_spills(function, || false).unwrap();
    let emitted = emit_function(
        &allocated.allocated.function,
        &allocated.allocated.assignment,
        allocated.spill_frame_size,
        128,
        &allocated.allocated.edge_copies,
        false,
        false,
    )
    .unwrap();
    assert!(emitted.required_state_size <= 4096);
    JitCode::new(&emitted.code).unwrap()
}

#[test]
fn invariant_byte_predicates_preserve_each_lane_and_shared_raw_bytes() {
    for width in [2, 4, 8] {
        for (raw_use, split) in [(false, false), (true, false), (true, true)] {
            let mut original = fixture(width, raw_use);
            if split {
                let mut tail = MBlock::new(BlockId(3));
                tail.insts = original.blocks[1].insts.split_off(3);
                original.blocks[1].push(MInst::Jump { target: tail.id });
                original.blocks[1].phis[0].sources[1].0 = tail.id;
                original.blocks.push(tail);
            }
            let mut optimized = original.clone();
            run(&mut optimized);
            assert_eq!(
                optimized
                    .blocks
                    .iter()
                    .flat_map(|block| &block.insts)
                    .filter(|inst| matches!(inst, MInst::LoadIndexed { .. }))
                    .count(),
                usize::from(raw_use)
            );
            let before = compile_raw(original);
            let after = compile_raw(optimized);
            for seed in 0..64_u8 {
                let mut left = vec![0xa5; 4096];
                for (index, byte) in left[..24].iter_mut().enumerate() {
                    *byte = seed
                        .wrapping_mul(71)
                        .wrapping_add((index as u8).wrapping_mul(37));
                }
                let mut expected = left[..128].to_vec();
                for index in 0..width {
                    let (a, b, c) = (left[index], left[8 + index], left[16 + index]);
                    let predicate = ((a & b) | !c) & 1;
                    expected[32 + index] = predicate;
                    expected[48 + index] = predicate ^ (b & 1);
                    if raw_use {
                        expected[64 + index] = a;
                    }
                }
                let mut right = left.clone();
                assert_eq!(unsafe { (before.fn_ptr)(left.as_mut_ptr()) }, 0);
                assert_eq!(unsafe { (after.fn_ptr)(right.as_mut_ptr()) }, 0);
                assert_eq!(&left[..128], expected, "raw width={width} seed={seed}");
                assert_eq!(
                    &right[..128],
                    expected,
                    "optimized width={width} raw_use={raw_use} seed={seed}"
                );
            }
        }
    }
}

#[test]
fn invariant_byte_predicates_keep_loop_writes_and_unproven_ranges() {
    for blocker in 0..4 {
        let mut function = fixture(8, false);
        match blocker {
            0 => function.blocks[1].insts.insert(
                0,
                MInst::MemFill {
                    dst_offset: 0,
                    byte_len: 24,
                    value: 0,
                },
            ),
            1 | 2 => {
                for inst in &mut function.blocks[1].insts {
                    if let MInst::LoadIndexed {
                        alias_range,
                        offset,
                        ..
                    } = inst
                    {
                        *alias_range = if blocker == 1 {
                            None
                        } else {
                            MemoryAliasRange::new(*offset, 3)
                        };
                    }
                }
            }
            3 => {
                *function.blocks[0].insts.last_mut().unwrap() = MInst::Branch {
                    cond: VReg(0),
                    true_bb: BlockId(1),
                    false_bb: BlockId(2),
                };
            }
            _ => unreachable!(),
        }
        let original = format!("{function:?}");
        run(&mut function);
        assert_eq!(format!("{function:?}"), original, "blocker={blocker}");
    }
}
