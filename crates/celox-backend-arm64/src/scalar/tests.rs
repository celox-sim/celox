use super::*;
use crate::jit_mem::JitCode;
use crate::mir::{MBlock, MemoryAliasRange, PhiNode};

mod circular_scan;
mod counted_loop;
mod memory;

fn branch_range_function(predicate: bool, stores: usize) -> (MFunction, Assignment<VReg>) {
    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::Load {
        dst: VReg(0),
        base: BaseReg::SimState,
        offset: 0,
        size: OpSize::S64,
    });
    entry.push(if predicate {
        MInst::BranchPred {
            predicate: BranchPredicate::CompareImm {
                lhs: VReg(0),
                imm: 0,
                kind: CmpKind::Ne,
            },
            true_bb: BlockId(2),
            false_bb: BlockId(1),
        }
    } else {
        MInst::Branch {
            cond: VReg(0),
            true_bb: BlockId(2),
            false_bb: BlockId(1),
        }
    });
    let mut blocks = vec![entry];
    for (id, value, count) in [(1, 11, stores), (2, 29, 1)] {
        let mut block = MBlock::new(BlockId(id));
        block.push(MInst::LoadImm {
            dst: VReg(id),
            value,
        });
        block.insts.extend(std::iter::repeat_n(
            MInst::Store {
                base: BaseReg::SimState,
                offset: 8,
                src: VReg(id),
                size: OpSize::S64,
            },
            count,
        ));
        block.push(MInst::Jump { target: BlockId(3) });
        blocks.push(block);
    }
    let mut exit = MBlock::new(BlockId(3));
    exit.push(MInst::Return);
    blocks.push(exit);
    let mut assignment = Assignment::default();
    assignment.set(VReg(0), Arm64Reg::new(1));
    assignment.set(VReg(1), Arm64Reg::new(2));
    assignment.set(VReg(2), Arm64Reg::new(2));
    (MFunction::new(blocks, vec![]), assignment)
}

fn check_branch_outcomes(emitted: &EmitResult) {
    let jit = JitCode::new(&emitted.code).unwrap();
    let mut state = vec![0; emitted.required_state_size as usize];
    for condition in [0, 1, u64::MAX] {
        state[..8].copy_from_slice(&condition.to_le_bytes());
        state[8..16].fill(0);
        assert_eq!(unsafe { (jit.fn_ptr)(state.as_mut_ptr()) }, 0);
        assert_eq!(
            u64::from_le_bytes(state[8..16].try_into().unwrap()),
            if condition == 0 { 11 } else { 29 },
        );
    }
}

#[test]
fn direct_conditional_branches_preserve_both_outcomes() {
    for predicate in [false, true] {
        let (function, assignment) = branch_range_function(predicate, 1);
        let plan = EdgeCopyPlan::default();
        let compact = emit_function(&function, &assignment, 0, 16, &plan, false, false).unwrap();
        let stubs =
            emit_function_with_branches(&function, &assignment, 0, 16, &plan, false, false, false)
                .unwrap();
        assert!(compact.text_size < stubs.text_size);
        check_branch_outcomes(&compact);
    }
}

#[test]
fn far_conditional_branches_fall_back_to_copy_stubs() {
    for predicate in [false, true] {
        let (function, assignment) = branch_range_function(predicate, 262_144);
        let plan = EdgeCopyPlan::default();
        assert!(matches!(
            emit_function_with_branches(&function, &assignment, 0, 16, &plan, false, false, true,),
            Err(EmitError::Assembly(DynasmError::ImpossibleRelocation(_))),
        ));
        let emitted = emit_function(&function, &assignment, 0, 16, &plan, false, false).unwrap();
        check_branch_outcomes(&emitted);
    }
}

#[test]
fn forwarded_jump_chains_preserve_incoming_and_outgoing_edge_copies() {
    for predicate in [false, true] {
        for outgoing_copies in [false, true] {
            let (source, _) = branch_range_function(predicate, 1);
            let mut entry = source.blocks[0].clone();
            match entry.insts.last_mut().unwrap() {
                MInst::Branch {
                    true_bb, false_bb, ..
                }
                | MInst::BranchPred {
                    true_bb, false_bb, ..
                } => {
                    *true_bb = BlockId(1);
                    *false_bb = BlockId(2);
                }
                _ => unreachable!(),
            }
            let mut blocks = vec![entry];
            for (id, target) in [(1, 3), (2, 4), (3, 5), (4, 5)] {
                let mut block = MBlock::new(BlockId(id));
                block.push(MInst::KeepAlive { src: VReg(0) });
                block.push(MInst::Jump {
                    target: BlockId(target),
                });
                blocks.push(block);
            }
            let mut exit = MBlock::new(BlockId(5));
            exit.push(MInst::Store {
                base: BaseReg::SimState,
                offset: 8,
                src: VReg(1),
                size: OpSize::S64,
            });
            exit.push(MInst::Return);
            blocks.push(exit);
            let function = MFunction::new(blocks, vec![]);
            let mut assignment = Assignment::default();
            assignment.set(VReg(0), Arm64Reg::new(1));
            assignment.set(VReg(1), Arm64Reg::new(2));
            let mut plan = EdgeCopyPlan::default();
            let edges = if outgoing_copies {
                [(3, 5, 29), (4, 5, 11)]
            } else {
                [(0, 1, 29), (0, 2, 11)]
            };
            for (predecessor, successor, value) in edges {
                plan.insert(
                    BlockId(predecessor),
                    BlockId(successor),
                    vec![CopyOperation::Move {
                        destination: CopyDestination::Register(Arm64Reg::new(2)),
                        source: CopySource::Immediate(value),
                    }],
                );
            }
            for direct in [false, true] {
                let emitted = emit_function_with_branches(
                    &function,
                    &assignment,
                    0,
                    16,
                    &plan,
                    false,
                    false,
                    direct,
                )
                .unwrap();
                assert_eq!(
                    emitted.block_offsets.len(),
                    if outgoing_copies { 4 } else { 2 }
                );
                check_branch_outcomes(&emitted);
            }
        }
    }
}

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
fn nearby_spill_uses_preserve_snapshots_under_register_pressure() {
    let mut block = MBlock::new(BlockId(0));
    for index in 0..32 {
        block.push(MInst::Load {
            dst: VReg(index),
            base: BaseReg::SimState,
            offset: (index * 8) as i32,
            size: OpSize::S64,
        });
    }
    for index in 0..32 {
        for step in 0..3 {
            let dst = VReg(32 + index * 3 + step);
            block.push(MInst::AddImm {
                dst,
                src: VReg(index),
                imm: (step + 1) as i32,
            });
            // Overwrite the original state while its snapshot is still live.
            block.push(MInst::Store {
                base: BaseReg::SimState,
                offset: (index * 8) as i32,
                src: dst,
                size: OpSize::S64,
            });
            block.push(MInst::Store {
                base: BaseReg::SimState,
                offset: ((32 + index * 3 + step) * 8) as i32,
                src: dst,
                size: OpSize::S64,
            });
        }
    }
    block.push(MInst::Return);
    let (jit, mut state) = compile(MFunction::new(vec![block], vec![]), 128 * 8);
    for seed in [0, 0x9e37_79b9_7f4a_7c15u64, u64::MAX - 31] {
        let values = (0..32)
            .map(|index| seed.wrapping_add(index))
            .collect::<Vec<_>>();
        state[..128 * 8].fill(0xa5);
        for (index, value) in values.iter().enumerate() {
            state[index * 8..index * 8 + 8].copy_from_slice(&value.to_le_bytes());
        }
        assert_eq!(unsafe { (jit.fn_ptr)(state.as_mut_ptr()) }, 0);
        for (index, value) in values.iter().enumerate() {
            for step in 0..3 {
                let offset = (32 + index * 3 + step) * 8;
                assert_eq!(
                    u64::from_le_bytes(state[offset..offset + 8].try_into().unwrap()),
                    value.wrapping_add(step as u64 + 1),
                );
            }
        }
    }
}

#[test]
fn spilled_constants_survive_both_phi_edges_and_repeated_calls() {
    let values = (0..32)
        .map(|index| match index % 4 {
            0 => index as u64,
            1 => 1_u64 << index,
            2 => 0x00ff_00ff_00ff_00ff,
            _ => 0x0123_4567_89ab_cdef_u64.wrapping_add(index as u64),
        })
        .collect::<Vec<_>>();
    let mut entry = MBlock::new(BlockId(0));
    for (index, &value) in values.iter().enumerate() {
        entry.push(MInst::LoadImm {
            dst: VReg(index as u32),
            value,
        });
    }
    entry.push(MInst::Load {
        dst: VReg(32),
        base: BaseReg::SimState,
        offset: 0,
        size: OpSize::S64,
    });
    entry.push(MInst::Branch {
        cond: VReg(32),
        true_bb: BlockId(1),
        false_bb: BlockId(2),
    });
    let mut blocks = vec![entry];
    for id in 1..=2 {
        let mut block = MBlock::new(BlockId(id));
        for index in 0..32 {
            block.push(MInst::Store {
                base: BaseReg::SimState,
                offset: (index + 1) * 8,
                src: VReg(index as u32),
                size: OpSize::S64,
            });
        }
        block.push(MInst::Jump { target: BlockId(3) });
        blocks.push(block);
    }
    let mut join = MBlock::new(BlockId(3));
    join.phis.push(PhiNode {
        dst: VReg(33),
        sources: vec![(BlockId(1), VReg(1)), (BlockId(2), VReg(3))],
    });
    join.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 264,
        src: VReg(33),
        size: OpSize::S64,
    });
    join.push(MInst::Return);
    blocks.push(join);
    let (jit, mut state) = compile(MFunction::new(blocks, vec![]), 272);
    for taken in [true, false, true, false] {
        state[..272].fill(0xa5);
        state[..8].copy_from_slice(&u64::from(taken).to_le_bytes());
        assert_eq!(unsafe { (jit.fn_ptr)(state.as_mut_ptr()) }, 0);
        for (index, &value) in values.iter().enumerate() {
            let offset = (index + 1) * 8;
            assert_eq!(
                u64::from_le_bytes(state[offset..offset + 8].try_into().unwrap()),
                value
            );
        }
        assert_eq!(
            u64::from_le_bytes(state[264..272].try_into().unwrap()),
            values[if taken { 1 } else { 3 }]
        );
    }
}

#[test]
fn spilled_shared_values_do_not_spill_the_loop_induction() {
    for constant in [true, false] {
        let mut entry = MBlock::new(BlockId(0));
        entry.push(if constant {
            MInst::LoadImm {
                dst: VReg(0),
                value: 0,
            }
        } else {
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 280,
                size: OpSize::S64,
            }
        });
        for index in 1..=32 {
            entry.push(MInst::Load {
                dst: VReg(index),
                base: BaseReg::SimState,
                offset: (index * 8) as i32,
                size: OpSize::S64,
            });
        }
        entry.push(MInst::KeepAlive { src: VReg(0) });
        for index in 1..=32 {
            entry.push(MInst::Store {
                base: BaseReg::SimState,
                offset: (index * 8) as i32,
                src: VReg(index),
                size: OpSize::S64,
            });
        }
        entry.push(MInst::Jump { target: BlockId(1) });
        let mut preheader = MBlock::new(BlockId(1));
        preheader.push(MInst::Load {
            dst: VReg(36),
            base: BaseReg::SimState,
            offset: 0,
            size: OpSize::S64,
        });
        preheader.push(MInst::Jump { target: BlockId(2) });
        let mut body = MBlock::new(BlockId(2));
        body.phis.push(PhiNode {
            dst: VReg(33),
            sources: vec![(BlockId(1), VReg(0)), (BlockId(2), VReg(34))],
        });
        body.push(MInst::AddImm {
            dst: VReg(34),
            src: VReg(33),
            imm: 1,
        });
        body.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 264,
            src: VReg(34),
            size: OpSize::S64,
        });
        body.push(MInst::Cmp {
            dst: VReg(35),
            lhs: VReg(34),
            rhs: VReg(36),
            kind: CmpKind::LtU,
        });
        body.push(MInst::Branch {
            cond: VReg(35),
            true_bb: BlockId(2),
            false_bb: BlockId(3),
        });
        let mut exit = MBlock::new(BlockId(3));
        exit.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 272,
            src: VReg(0),
            size: OpSize::S64,
        });
        exit.push(MInst::Return);
        let allocation = crate::regalloc::allocate_with_spills(
            MFunction::new(vec![entry, preheader, body, exit], vec![]),
            || false,
        )
        .unwrap();
        let allocated = allocation.allocated;
        assert!(allocated.function.spill_homes.contains_key(&VReg(0)));
        assert!(!allocated.function.spill_homes.contains_key(&VReg(33)));
        let emitted = emit_function(
            &allocated.function,
            &allocated.assignment,
            allocation.spill_frame_size,
            288,
            &allocated.edge_copies,
            false,
            false,
        )
        .unwrap();
        let jit = JitCode::new(&emitted.code).unwrap();
        let mut state = vec![0; emitted.required_state_size as usize];
        for (start, count) in [(0u64, 1u64), (5, 7), (31, 64), (0, 1)] {
            let start = if constant { 0 } else { start };
            state.fill(0xa5);
            state[..8].copy_from_slice(&count.to_le_bytes());
            state[280..288].copy_from_slice(&start.to_le_bytes());
            let mut expected = state[..288].to_vec();
            expected[264..272].copy_from_slice(&count.to_le_bytes());
            expected[272..280].copy_from_slice(&start.to_le_bytes());
            assert_eq!(unsafe { (jit.fn_ptr)(state.as_mut_ptr()) }, 0);
            assert_eq!(&state[..288], expected);
        }
    }
}

#[test]
fn bitfield_insert_preserves_inputs_when_registers_overlap() {
    for (lsb, width) in [(0, 64), (0, 1), (3, 5), (7, 32), (31, 33), (63, 1)] {
        for (dst, src) in [(1, 2), (2, 2), (3, 2), (1, 1), (3, 1)] {
            let mut ops = VecAssembler::<Aarch64Relocation>::new(0);
            dynasm!(ops ; .arch aarch64 ; ldr x1, [x0] ; ldr x2, [x0, #8]);
            emit_bit_insert(&mut ops, dst, 1, src, lsb, width);
            dynasm!(ops ; .arch aarch64 ; str X(dst), [x0, #16] ; mov x0, #0 ; ret);
            let jit = JitCode::new(&ops.finalize().unwrap()).unwrap();
            let mut state = [0_u64; 3];
            for base in [0, u64::MAX, 0x0123_4567_89ab_cdef] {
                for source in [0, u64::MAX, 0xfedc_ba98_7654_3210] {
                    state[0] = base;
                    state[1] = source;
                    assert_eq!(unsafe { (jit.fn_ptr)(state.as_mut_ptr().cast()) }, 0);
                    let source = if src == 1 { base } else { source };
                    let field = (u64::MAX >> (64 - width)) << lsb;
                    assert_eq!(
                        state[2],
                        (base & !field) | ((source << lsb) & field),
                        "lsb={lsb} width={width} dst=x{dst} src=x{src}"
                    );
                }
            }
        }
    }
}

#[test]
fn packed_bitfields_preserve_surrounding_bits_and_full_width_sources() {
    for lsb in [0, 1, 3, 7, 8, 15, 31, 32, 48, 63] {
        for width in [1, 2, 5, 8, 16, 31, 32, 64 - lsb] {
            if width + lsb > 64 {
                continue;
            }
            let low_mask = u64::MAX >> (64 - width);
            let field_mask = low_mask << lsb;
            // Unmasked input can set bits outside the insertion field. It
            // must keep OR semantics even when the surrounding bits match
            // the usual bitfield pattern.
            for truncate_source in [false, true] {
                let mut block = MBlock::new(BlockId(0));
                block.insts = vec![
                    MInst::Load {
                        dst: VReg(0),
                        base: BaseReg::SimState,
                        offset: 0,
                        size: OpSize::S64,
                    },
                    MInst::Load {
                        dst: VReg(1),
                        base: BaseReg::SimState,
                        offset: 8,
                        size: OpSize::S64,
                    },
                    MInst::AndImm {
                        dst: VReg(2),
                        src: VReg(0),
                        imm: !field_mask,
                    },
                    MInst::AndImm {
                        dst: VReg(3),
                        src: VReg(1),
                        imm: if truncate_source { low_mask } else { u64::MAX },
                    },
                    MInst::ShlImm {
                        dst: VReg(4),
                        src: VReg(3),
                        imm: lsb,
                    },
                    MInst::Or {
                        dst: VReg(5),
                        lhs: VReg(2),
                        rhs: VReg(4),
                    },
                    MInst::Store {
                        base: BaseReg::SimState,
                        offset: 16,
                        src: VReg(5),
                        size: OpSize::S64,
                    },
                    MInst::ShrImm {
                        dst: VReg(6),
                        src: VReg(1),
                        imm: lsb,
                    },
                    MInst::AndImm {
                        dst: VReg(7),
                        src: VReg(6),
                        imm: low_mask,
                    },
                    MInst::Store {
                        base: BaseReg::SimState,
                        offset: 24,
                        src: VReg(7),
                        size: OpSize::S64,
                    },
                    MInst::Return,
                ];
                let (jit, mut state) = compile(MFunction::new(vec![block], vec![]), 32);
                let mut random = 0x9e37_79b9_7f4a_7c15_u64;
                for input in [0, 1, u64::MAX, 0xaaaa_aaaa_aaaa_aaaa, 1 << 63] {
                    for _ in 0..8 {
                        random ^= random << 13;
                        random ^= random >> 7;
                        random ^= random << 17;
                        state[..8].copy_from_slice(&random.to_le_bytes());
                        state[8..16].copy_from_slice(&input.to_le_bytes());
                        assert_eq!(unsafe { (jit.fn_ptr)(state.as_mut_ptr()) }, 0);
                        let inserted = if truncate_source {
                            input & low_mask
                        } else {
                            input
                        };
                        assert_eq!(
                            u64::from_le_bytes(state[16..24].try_into().unwrap()),
                            (random & !field_mask) | (inserted << lsb),
                            "insert lsb={lsb} width={width} truncate={truncate_source}"
                        );
                        assert_eq!(
                            u64::from_le_bytes(state[24..32].try_into().unwrap()),
                            (input >> lsb) & low_mask,
                            "extract lsb={lsb} width={width}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn single_chunk_sparse_commits_clear_bitmaps_and_preserve_other_bytes() {
    for byte_size in 1..=8 {
        for four_state in [false, true] {
            for (worklist, summary_words) in [(false, 1), (true, 1), (true, 2)] {
                let mut block = MBlock::new(BlockId(0));
                block.push(if worklist {
                    MInst::SparseCommitWorklist {
                        descriptor_table: crate::mir::ConstantTableId(0),
                        active_bits_offset: 112,
                        active_capacity: 1,
                    }
                } else {
                    MInst::SparseCommit {
                        src_offset: 3,
                        dst_offset: 43,
                        byte_size,
                        dirty_words_offset: 80,
                        dirty_word_count: 1,
                        summary_words_offset: 96,
                        summary_word_count: summary_words,
                        four_state,
                    }
                });
                block.push(MInst::Return);
                let table = vec![
                    3,
                    43,
                    byte_size as u64,
                    80,
                    1,
                    96,
                    summary_words as u64,
                    u64::from(four_state),
                ];
                let (jit, mut state) = compile(MFunction::new(vec![block], vec![table]), 128);
                for active in [0_u64, 1, 2, u64::MAX] {
                    for summary in [0_u64, 1, 2, u64::MAX] {
                        for dirty in [0_u64, 1, 2, u64::MAX] {
                            for (index, byte) in state.iter_mut().enumerate() {
                                *byte = (index as u8).wrapping_mul(73).wrapping_add(17);
                            }
                            state[80..88].copy_from_slice(&dirty.to_le_bytes());
                            state[96..104].copy_from_slice(&summary.to_le_bytes());
                            state[104..112].fill(0);
                            state[112..120].copy_from_slice(&active.to_le_bytes());
                            let mut expected = state[..128].to_vec();
                            if worklist {
                                expected[112..120].fill(0);
                            }
                            if !worklist || active & 1 != 0 {
                                expected[96..104].fill(0);
                                if summary & 1 != 0 {
                                    expected[80..88].fill(0);
                                    if dirty & 1 != 0 {
                                        let bytes = byte_size * if four_state { 2 } else { 1 };
                                        expected.copy_within(3..3 + bytes, 43);
                                    }
                                }
                            }
                            assert_eq!(unsafe { (jit.fn_ptr)(state.as_mut_ptr()) }, 0);
                            assert_eq!(
                                &state[..128],
                                expected,
                                "bytes={byte_size} four_state={four_state} worklist={worklist} active={active} summary={summary} dirty={dirty}"
                            );
                        }
                    }
                }
            }
        }
    }
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
