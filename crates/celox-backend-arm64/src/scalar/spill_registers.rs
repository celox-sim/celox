//! Keep frequently accessed spill slots in unused caller-saved SIMD registers.

use std::cmp::Reverse;
use std::collections::BTreeMap;

use celox_analysis::cfg::ForwardControlFlowGraph;

use super::*;

#[derive(Default)]
pub(super) struct SpillRegisters {
    slots: BTreeMap<i32, u8>,
}

impl SpillRegisters {
    pub(super) fn select(
        function: &MFunction,
        plan: &EdgeCopyPlan<BlockId>,
        frame_size: u32,
    ) -> Self {
        if frame_size == 0 {
            return Self::default();
        }
        let indices = function
            .blocks
            .iter()
            .enumerate()
            .map(|(i, block)| (block.id, i))
            .collect::<HashMap<_, _>>();
        let successors = function
            .blocks
            .iter()
            .map(|block| block.successors().iter().map(|id| indices[id]).collect())
            .collect();
        let mut weights = vec![1usize; function.blocks.len()];
        if let Ok(cfg) = ForwardControlFlowGraph::analyze_structure(successors, 0) {
            for natural_loop in cfg.loops {
                for block in natural_loop.blocks {
                    weights[block] = (weights[block] * 8).min(512);
                }
            }
        }
        let mut reads = BTreeMap::<i32, usize>::new();
        for (index, block) in function.blocks.iter().enumerate() {
            for inst in &block.insts {
                match *inst {
                    MInst::Load {
                        base: BaseReg::StackFrame,
                        offset,
                        size: OpSize::S64,
                        ..
                    } if offset >= 0 && offset % 8 == 0 => {
                        *reads.entry(offset).or_default() += weights[index];
                    }
                    MInst::Store {
                        base: BaseReg::StackFrame,
                        offset,
                        size: OpSize::S64,
                        ..
                    } if offset >= 0 && offset % 8 == 0 => {}
                    // Mixed-width or indirect frame accesses need their
                    // memory representation at every instruction boundary.
                    MInst::Load {
                        base: BaseReg::StackFrame,
                        ..
                    }
                    | MInst::Store {
                        base: BaseReg::StackFrame,
                        ..
                    }
                    | MInst::LoadIndexed {
                        base: BaseReg::StackFrame,
                        ..
                    }
                    | MInst::StoreIndexed {
                        base: BaseReg::StackFrame,
                        ..
                    }
                    | MInst::OrStoreIndexed {
                        base: BaseReg::StackFrame,
                        ..
                    }
                    | MInst::BranchPred {
                        predicate:
                            BranchPredicate::MemoryNonZero {
                                base: BaseReg::StackFrame,
                                ..
                            },
                        ..
                    } => return Self::default(),
                    _ => {}
                }
            }
            for successor in block.successors() {
                for operation in plan.edge(block.id, successor).into_iter().flatten() {
                    let offset = match *operation {
                        CopyOperation::Move {
                            source: CopySource::Stack(offset),
                            ..
                        }
                        | CopyOperation::SaveTemporary(CopyDestination::Stack(offset)) => {
                            Some(offset)
                        }
                        _ => None,
                    };
                    if let Some(offset) = offset {
                        *reads.entry(offset).or_default() += weights[index];
                    }
                }
            }
        }
        let mut candidates = reads
            .into_iter()
            .filter_map(|(offset, reads)| {
                (reads >= 8
                    && offset >= 0
                    && offset % 8 == 0
                    && i64::from(offset) + 8 <= i64::from(frame_size))
                .then_some((Reverse(reads), offset))
            })
            .collect::<Vec<_>>();
        candidates.sort_unstable();
        // Emission pseudos use v0-v7. v8-v15 are callee-saved; d29/d31
        // already retain native tick-loop state. No emitted code calls out
        // while these caller-saved slots are live.
        let slots = candidates
            .into_iter()
            .zip((16..=28).chain(std::iter::once(30)))
            .map(|((_, offset), register)| (offset, register))
            .collect();
        Self { slots }
    }

    pub(super) fn get(&self, offset: i32) -> Option<u8> {
        self.slots.get(&offset).copied()
    }

    pub(super) fn enter(&self, ops: &mut VecAssembler<Aarch64Relocation>) {
        for (&offset, &register) in &self.slots {
            emit_load_at(ops, 30, SPILL_REG, i64::from(offset), OpSize::S64);
            dynasm!(ops ; .arch aarch64 ; fmov D(register), x30);
        }
    }

    pub(super) fn leave(&self, ops: &mut VecAssembler<Aarch64Relocation>) {
        for (&offset, &register) in &self.slots {
            dynasm!(ops ; .arch aarch64 ; fmov x30, D(register));
            emit_store_at(ops, 30, SPILL_REG, i64::from(offset), OpSize::S64);
        }
    }

    pub(super) fn load(
        &self,
        ops: &mut VecAssembler<Aarch64Relocation>,
        destination: u8,
        offset: i32,
    ) {
        if let Some(register) = self.get(offset) {
            dynasm!(ops ; .arch aarch64 ; fmov X(destination), D(register));
        } else {
            emit_load_at(ops, destination, SPILL_REG, i64::from(offset), OpSize::S64);
        }
    }
}
