use super::*;
use crate::jit_mem::JitCode;
use crate::mir::{MBlock, MemoryAliasRange, PhiNode};

fn compile(mut function: MFunction, state_size: usize) -> (JitCode, Vec<u8>) {
    crate::mir_opt::optimize(&mut function);
    crate::mir_legalize::legalize_variable_shift_counts(&mut function);
    let allocation = crate::regalloc::allocate_with_spills(function, || false).unwrap();
    let emitted = emit_function(
        &allocation.allocated.function,
        &allocation.allocated.assignment,
        allocation.spill_frame_size,
        state_size,
        &allocation.allocated.edge_copies,
        false,
        false,
    )
    .unwrap();
    (
        JitCode::new(&emitted.code).unwrap(),
        vec![0; emitted.required_state_size as usize],
    )
}

#[test]
fn optimized_bit_addresses_preserve_wrapping_and_input_width() {
    for size in [OpSize::S32, OpSize::S64] {
        for stride in [8, 32, 288, 512, 1_u64 << 63] {
            for offset in [0, 9, 18, 163, u64::MAX] {
                let mut block = MBlock::new(BlockId(0));
                block.insts = vec![
                    MInst::Load {
                        dst: VReg(0),
                        base: BaseReg::SimState,
                        offset: 0,
                        size,
                    },
                    MInst::LoadImm {
                        dst: VReg(1),
                        value: stride,
                    },
                    MInst::Mul {
                        dst: VReg(2),
                        lhs: VReg(0),
                        rhs: VReg(1),
                    },
                    MInst::LoadImm {
                        dst: VReg(3),
                        value: offset,
                    },
                    MInst::Add {
                        dst: VReg(4),
                        lhs: VReg(2),
                        rhs: VReg(3),
                    },
                    MInst::AndImm {
                        dst: VReg(5),
                        src: VReg(4),
                        imm: 7,
                    },
                    MInst::ShrImm {
                        dst: VReg(6),
                        src: VReg(4),
                        imm: 3,
                    },
                    MInst::Store {
                        base: BaseReg::SimState,
                        offset: 8,
                        src: VReg(5),
                        size: OpSize::S64,
                    },
                    MInst::Store {
                        base: BaseReg::SimState,
                        offset: 16,
                        src: VReg(6),
                        size: OpSize::S64,
                    },
                    MInst::Return,
                ];
                let (jit, mut state) = compile(MFunction::new(vec![block], vec![]), 24);
                for input in [
                    0,
                    1,
                    31,
                    255,
                    u64::from(u32::MAX),
                    1 << 40,
                    1 << 63,
                    u64::MAX,
                ] {
                    state[..8].copy_from_slice(&input.to_le_bytes());
                    assert_eq!(unsafe { (jit.fn_ptr)(state.as_mut_ptr()) }, 0);
                    let index = if size == OpSize::S32 {
                        input & u64::from(u32::MAX)
                    } else {
                        input
                    };
                    let address = index.wrapping_mul(stride).wrapping_add(offset);
                    assert_eq!(
                        u64::from_le_bytes(state[8..16].try_into().unwrap()),
                        address & 7
                    );
                    assert_eq!(
                        u64::from_le_bytes(state[16..24].try_into().unwrap()),
                        address >> 3,
                        "size={size:?} stride={stride} offset={offset} input={input}"
                    );
                }
            }
        }
    }
}

#[test]
fn conditional_fallthrough_preserves_both_phi_edges() {
    let kinds = [
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
    ];
    for kind in kinds {
        for true_is_next in [true, false] {
            for materialized in [true, false] {
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
                    size: OpSize::S64,
                });
                if materialized {
                    entry.push(MInst::Cmp {
                        dst: VReg(5),
                        lhs: VReg(0),
                        rhs: VReg(1),
                        kind,
                    });
                    // A second consumer keeps this a register branch.
                    entry.push(MInst::Store {
                        base: BaseReg::SimState,
                        offset: 24,
                        src: VReg(5),
                        size: OpSize::S64,
                    });
                    entry.push(MInst::Branch {
                        cond: VReg(5),
                        true_bb: BlockId(1),
                        false_bb: BlockId(2),
                    });
                } else {
                    entry.push(MInst::BranchPred {
                        predicate: BranchPredicate::Compare {
                            lhs: VReg(0),
                            rhs: VReg(1),
                            kind,
                        },
                        true_bb: BlockId(1),
                        false_bb: BlockId(2),
                    });
                }
                let mut hit = MBlock::new(BlockId(1));
                hit.push(MInst::LoadImm {
                    dst: VReg(2),
                    value: 0xa1,
                });
                hit.push(MInst::Jump { target: BlockId(3) });
                let mut miss = MBlock::new(BlockId(2));
                miss.push(MInst::LoadImm {
                    dst: VReg(3),
                    value: 0xb2,
                });
                miss.push(MInst::Jump { target: BlockId(3) });
                let mut join = MBlock::new(BlockId(3));
                join.phis.push(PhiNode {
                    dst: VReg(4),
                    sources: vec![(BlockId(1), VReg(2)), (BlockId(2), VReg(3))],
                });
                join.push(MInst::Store {
                    base: BaseReg::SimState,
                    offset: 16,
                    src: VReg(4),
                    size: OpSize::S64,
                });
                join.push(MInst::Return);
                let blocks = if true_is_next {
                    vec![entry, hit, miss, join]
                } else {
                    vec![entry, miss, hit, join]
                };
                let (jit, mut state) = compile(MFunction::new(blocks, vec![]), 32);
                for lhs in [0_u64, 1, 1 << 63, u64::MAX] {
                    for rhs in [0_u64, 1, 1 << 63, u64::MAX] {
                        state[..8].copy_from_slice(&lhs.to_le_bytes());
                        state[8..16].copy_from_slice(&rhs.to_le_bytes());
                        assert_eq!(unsafe { (jit.fn_ptr)(state.as_mut_ptr()) }, 0);
                        let hit = match kind {
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
                        assert_eq!(
                            u64::from_le_bytes(state[16..24].try_into().unwrap()),
                            if hit { 0xa1 } else { 0xb2 },
                            "{kind:?} lhs={lhs} rhs={rhs} true_is_next={true_is_next} materialized={materialized}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn packed_bitmap_loads_preserve_one_bit_and_wider_consumers() {
    for bit_mask in [15, 31, 63] {
        for result_mask in [1, 3, 255] {
            let mut block = MBlock::new(BlockId(0));
            block.insts = vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 0,
                    size: OpSize::S64,
                },
                MInst::AndImm {
                    dst: VReg(1),
                    src: VReg(0),
                    imm: bit_mask,
                },
                MInst::ShrImm {
                    dst: VReg(2),
                    src: VReg(1),
                    imm: 3,
                },
                MInst::AndImm {
                    dst: VReg(3),
                    src: VReg(1),
                    imm: 7,
                },
                MInst::LoadIndexed {
                    dst: VReg(4),
                    base: BaseReg::SimState,
                    offset: 8,
                    index: VReg(2),
                    scale: 1,
                    size: OpSize::S8,
                    alias_range: MemoryAliasRange::new(8, (bit_mask as usize + 1) / 8),
                },
                MInst::Shr {
                    dst: VReg(5),
                    lhs: VReg(4),
                    rhs: VReg(3),
                },
                MInst::AndImm {
                    dst: VReg(6),
                    src: VReg(5),
                    imm: result_mask,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 16,
                    src: VReg(6),
                    size: OpSize::S64,
                },
                MInst::Return,
            ];
            let (jit, mut state) = compile(MFunction::new(vec![block], vec![]), 24);
            for bitmap in [0, 1, 1_u64 << 63, 0xa579_3c01_e887_5fa3, u64::MAX] {
                state[8..16].copy_from_slice(&bitmap.to_le_bytes());
                for index in 0..128_u64 {
                    state[..8].copy_from_slice(&index.to_le_bytes());
                    assert_eq!(unsafe { (jit.fn_ptr)(state.as_mut_ptr()) }, 0);
                    let bit = index & bit_mask;
                    let byte = (bitmap >> ((bit / 8) * 8)) & 255;
                    assert_eq!(
                        u64::from_le_bytes(state[16..24].try_into().unwrap()),
                        (byte >> (bit % 8)) & result_mask
                    );
                }
            }
        }
    }
}

#[test]
fn split_memory_offsets_preserve_unaligned_and_indexed_accesses() {
    for size in [OpSize::S8, OpSize::S16, OpSize::S32, OpSize::S64] {
        for offset in [
            4096 + 31,
            8192 + 32,
            -4096 - 31,
            -8192 - 32,
            65536 + 120,
            -65536 + 120,
        ] {
            for scale in [0, 1, 2, 4, 8] {
                let mut ops = VecAssembler::<Aarch64Relocation>::new(0);
                dynasm!(ops ; .arch aarch64 ; mov x1, #3);
                if scale == 0 {
                    emit_load_at(&mut ops, 2, 0, offset, size);
                    emit_store_at(&mut ops, 2, 0, offset + 16, size);
                } else {
                    emit_load_indexed_at(&mut ops, 2, 0, 1, offset, scale, size);
                    emit_store_indexed_at(&mut ops, 2, 0, 1, offset + 16, scale, size);
                }
                dynasm!(ops ; .arch aarch64 ; mov x0, xzr ; ret);
                let jit = JitCode::new(&ops.finalize().unwrap()).unwrap();
                let mut state = vec![0xa5; 140_000];
                let source = (70_000 + offset + i64::from(scale) * 3) as usize;
                let width = usize::from(size.bytes());
                let pattern = 0x1234_5678_9abc_def0_u64.to_le_bytes();
                state[source..source + width].copy_from_slice(&pattern[..width]);
                let mut expected = state.clone();
                expected[source + 16..source + 16 + width].copy_from_slice(&pattern[..width]);
                assert_eq!(unsafe { (jit.fn_ptr)(state.as_mut_ptr().add(70_000)) }, 0);
                assert_eq!(
                    state, expected,
                    "size={size:?} offset={offset} scale={scale}"
                );
            }
        }
    }
}
