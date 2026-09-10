//! Keep the SSA values reachable from observable instructions.

use super::ValueDefinition;
use crate::native::mir::*;

pub(super) fn eliminate(func: &mut MFunction, remove_unused_phis: bool) {
    let value_count = func.vregs.count() as usize;
    let mut definitions = vec![None; value_count];
    let mut live = vec![false; value_count];
    let mut work = Vec::new();
    let mark = |value: VReg, live: &mut [bool], work: &mut Vec<VReg>| {
        if !std::mem::replace(&mut live[value.0 as usize], true) {
            work.push(value);
        }
    };

    for (block_index, block) in func.blocks.iter().enumerate() {
        for (phi_index, phi) in block.phis.iter().enumerate() {
            definitions[phi.dst.0 as usize] = Some(ValueDefinition::Phi {
                block: block_index,
                phi: phi_index,
            });
            // After allocation, every retained phi row still needs its edge
            // inputs, even when the destination has no instruction users.
            if !remove_unused_phis {
                mark(phi.dst, &mut live, &mut work);
            }
        }
        for (instruction, inst) in block.insts.iter().enumerate() {
            if let Some(dst) = inst.def() {
                definitions[dst.0 as usize] = Some(ValueDefinition::Instruction {
                    block: block_index,
                    instruction,
                });
            } else {
                // Stores, control flow, memory pseudos, and SIMD operations
                // have no scalar definition. Preserve them and their scalar
                // inputs; vector liveness belongs to the SIMD pipeline.
                for source in inst.uses() {
                    mark(source, &mut live, &mut work);
                }
            }
        }
    }

    // Visit each live definition once. Unlike repeated use-set scans, this
    // is linear in the SSA graph and also removes unobserved phi cycles.
    // The explicit worklist keeps long dependency chains off the call stack.
    while let Some(value) = work.pop() {
        match definitions[value.0 as usize] {
            Some(ValueDefinition::Phi { block, phi }) => {
                for &(_, source) in &func.blocks[block].phis[phi].sources {
                    mark(source, &mut live, &mut work);
                }
            }
            Some(ValueDefinition::Instruction { block, instruction }) => {
                for source in func.blocks[block].insts[instruction].uses() {
                    mark(source, &mut live, &mut work);
                }
            }
            None => {}
        }
    }

    for block in &mut func.blocks {
        block
            .insts
            .retain(|inst| inst.def().is_none_or(|dst| live[dst.0 as usize]));
        if remove_unused_phis {
            block.phis.retain(|phi| live[phi.dst.0 as usize]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loop_function(observe_recurrence: bool) -> MFunction {
        let mut vregs = VRegAllocator::new();
        for _ in 0..8 {
            vregs.alloc();
        }
        let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 8]);
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
        entry.push(MInst::Jump { target: BlockId(1) });
        let mut header = MBlock::new(BlockId(1));
        header.phis = vec![
            PhiNode {
                dst: VReg(2),
                sources: vec![(BlockId(0), VReg(0)), (BlockId(2), VReg(5))],
            },
            PhiNode {
                dst: VReg(3),
                sources: vec![(BlockId(0), VReg(1)), (BlockId(2), VReg(6))],
            },
        ];
        header.push(MInst::CmpImm {
            dst: VReg(4),
            lhs: VReg(2),
            imm: 8,
            kind: CmpKind::LtU,
        });
        header.push(MInst::Branch {
            cond: VReg(4),
            true_bb: BlockId(2),
            false_bb: BlockId(3),
        });
        let mut latch = MBlock::new(BlockId(2));
        latch.push(MInst::AddImm {
            dst: VReg(5),
            src: VReg(2),
            imm: 1,
        });
        latch.push(MInst::Add {
            dst: VReg(6),
            lhs: VReg(3),
            rhs: VReg(3),
        });
        latch.push(MInst::Jump { target: BlockId(1) });
        let mut exit = MBlock::new(BlockId(3));
        exit.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 16,
            src: if observe_recurrence { VReg(3) } else { VReg(2) },
            size: OpSize::S64,
        });
        exit.push(MInst::LoadImm {
            dst: VReg(7),
            value: 42,
        });
        exit.push(MInst::Return);
        function.blocks = vec![entry, header, latch, exit];
        function
    }

    #[test]
    fn removes_unobserved_phi_cycles_but_keeps_control_dependencies() {
        for observed in [false, true] {
            let mut function = loop_function(observed);
            function.verify_result().unwrap();
            eliminate(&mut function, true);
            function.verify_result().unwrap();
            assert_eq!(function.blocks[1].phis.len(), if observed { 2 } else { 1 });
            for value in [VReg(1), VReg(6)] {
                assert_eq!(
                    function
                        .blocks
                        .iter()
                        .flat_map(|block| &block.insts)
                        .any(|inst| inst.def() == Some(value)),
                    observed,
                );
            }
            assert_eq!(function.blocks[3].insts.len(), 2);
        }
    }

    #[test]
    fn preserves_allocated_phi_rows_and_their_transitive_inputs() {
        let mut function = loop_function(false);
        let phis = function.blocks[1].phis.clone();
        eliminate(&mut function, false);
        function.verify_result().unwrap();
        assert_eq!(function.blocks[1].phis.len(), phis.len());
        for (actual, expected) in function.blocks[1].phis.iter().zip(phis) {
            assert_eq!(actual.dst, expected.dst);
            assert_eq!(actual.sources, expected.sources);
        }
        assert_eq!(function.blocks[0].insts.len(), 3);
        assert_eq!(function.blocks[2].insts.len(), 3);
        assert_eq!(function.blocks[3].insts.len(), 2);
    }

    #[test]
    fn handles_long_live_and_dead_dependency_chains() {
        const LENGTH: u32 = 16_384;
        for observed in [false, true] {
            let mut vregs = VRegAllocator::new();
            let mut block = MBlock::new(BlockId(0));
            let mut source = vregs.alloc();
            block.push(MInst::LoadImm {
                dst: source,
                value: 1,
            });
            for _ in 0..LENGTH {
                let dst = vregs.alloc();
                block.push(MInst::Add {
                    dst,
                    lhs: source,
                    rhs: source,
                });
                source = dst;
            }
            if observed {
                block.push(MInst::Store {
                    base: BaseReg::SimState,
                    offset: 0,
                    src: source,
                    size: OpSize::S64,
                });
            }
            block.push(MInst::Return);
            let spill_descs = vec![SpillDesc::transient(); vregs.count() as usize];
            let mut function = MFunction::new(vregs, spill_descs);
            function.blocks.push(block);
            eliminate(&mut function, true);
            assert_eq!(
                function.blocks[0].insts.len(),
                if observed { LENGTH as usize + 3 } else { 1 }
            );
        }
    }

    #[test]
    fn preserves_scalar_dependencies_of_vector_packs() {
        let mut function = loop_function(true);
        let vector = function.alloc_x86_vec();
        let scratch = function.alloc_x86_vec();
        function.blocks[3].insts[0] = MInst::X86Simd(X86SimdInst::Pack128 {
            dst: vector,
            scratch: Some(scratch),
            low: VReg(2),
            high: VReg(3),
        });
        function.blocks[3]
            .insts
            .insert(0, MInst::X86Simd(X86SimdInst::Scratch128 { dst: scratch }));
        eliminate(&mut function, true);
        function.verify_result().unwrap();
        assert_eq!(function.blocks[1].phis.len(), 2);
        assert_eq!(function.blocks[0].insts.len(), 3);
        assert_eq!(function.blocks[2].insts.len(), 3);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn loop_cleanup_preserves_execution() {
        use crate::native::{emit, jit_mem::JitCode, regalloc};

        fn compile(mut function: MFunction) -> (JitCode, usize, usize) {
            let allocation = regalloc::run_regalloc(&mut function).unwrap();
            let emitted = emit::emit(
                &function,
                &allocation.assignment,
                allocation.spill_frame_size,
            )
            .unwrap();
            (
                JitCode::new(&emitted.code).unwrap(),
                emitted.required_state_size.max(32) as usize,
                emitted.text_size,
            )
        }

        for observed in [false, true] {
            let mut function = loop_function(observed);
            let (original, original_state, original_size) = compile(function.clone());
            eliminate(&mut function, true);
            let (optimized, optimized_state, optimized_size) = compile(function);
            if !observed {
                assert!(
                    optimized_size <= original_size,
                    "{optimized_size} > {original_size}"
                );
            }
            for start in [0u64, 1, 7, 8, 9, u64::MAX] {
                for seed in [0u64, 1, u64::MAX, 1 << 63] {
                    let mut before = vec![0u8; original_state.max(optimized_state)];
                    before[..8].copy_from_slice(&start.to_le_bytes());
                    before[8..16].copy_from_slice(&seed.to_le_bytes());
                    let mut after = before.clone();
                    assert_eq!(unsafe { original.call(&mut before) }, 0);
                    assert_eq!(unsafe { optimized.call(&mut after) }, 0);
                    assert_eq!(
                        &before[..24],
                        &after[..24],
                        "start={start}, seed={seed}, observed={observed}"
                    );
                    let expected = if observed {
                        seed.wrapping_shl(8u64.saturating_sub(start) as u32)
                    } else {
                        start.max(8)
                    };
                    assert_eq!(
                        u64::from_le_bytes(after[16..24].try_into().unwrap()),
                        expected
                    );
                }
            }
        }
    }
}
