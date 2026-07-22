//! Shared, read-only SIR queries used by CFG-producing coalescing passes.

use crate::HashMap;
use crate::ir::cfg::SirCfg;
use crate::ir::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum UseSite {
    Instruction { block: BlockId, index: usize },
    BranchCondition { block: BlockId },
    TrueEdgeArgument { block: BlockId },
    FalseEdgeArgument { block: BlockId },
    JumpArgument { block: BlockId },
}

impl UseSite {
    pub(super) fn block(self) -> BlockId {
        match self {
            Self::Instruction { block, .. }
            | Self::BranchCondition { block }
            | Self::TrueEdgeArgument { block }
            | Self::FalseEdgeArgument { block }
            | Self::JumpArgument { block } => block,
        }
    }
}

pub(super) fn predicate_facts(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirCfg,
) -> Vec<Vec<(RegisterId, bool)>> {
    let mut facts = vec![Vec::new(); cfg.block_ids.len()];
    for block in 1..cfg.block_ids.len() {
        let Some(parent) = cfg.dominators.idom[block] else {
            continue;
        };
        facts[block] = facts[parent].clone();
        let SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } = &eu.blocks[&cfg.block_ids[parent]].terminator
        else {
            continue;
        };
        let true_index = cfg.block_index(true_block.0).unwrap();
        let false_index = cfg.block_index(false_block.0).unwrap();
        let fact = if cfg.dominators.dominates(true_index, block) {
            Some((*cond, true))
        } else if cfg.dominators.dominates(false_index, block) {
            Some((*cond, false))
        } else {
            None
        };
        if let Some(fact) = fact
            && !facts[block].contains(&fact)
        {
            facts[block].push(fact);
        }
    }
    facts
}

pub(super) fn instruction_uses(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> Vec<RegisterId> {
    match inst {
        SIRInstruction::Imm(..) => Vec::new(),
        SIRInstruction::Binary(_, lhs, _, rhs) => vec![*lhs, *rhs],
        SIRInstruction::Unary(_, _, source) | SIRInstruction::Slice(_, source, _, _) => {
            vec![*source]
        }
        SIRInstruction::Load(_, _, offset, _) => {
            offset.dynamic_registers().into_iter().flatten().collect()
        }
        SIRInstruction::Store(_, offset, _, source, _, _) => offset
            .dynamic_registers()
            .into_iter()
            .flatten()
            .chain(std::iter::once(*source))
            .collect(),
        SIRInstruction::Commit(_, _, offset, _, _) => {
            offset.dynamic_registers().into_iter().flatten().collect()
        }
        SIRInstruction::Concat(_, args)
        | SIRInstruction::RuntimeEvent { args, .. }
        | SIRInstruction::CombCaptureEvent { args, .. } => args.clone(),
        SIRInstruction::Mux(_, cond, true_value, false_value) => {
            vec![*cond, *true_value, *false_value]
        }
        SIRInstruction::CombCaptureEnableIfChanged { old, new, .. } => vec![*old, *new],
    }
}

pub(super) fn collect_uses(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> HashMap<RegisterId, Vec<UseSite>> {
    let mut result = HashMap::<RegisterId, Vec<UseSite>>::default();
    for block in eu.blocks.values() {
        for (index, inst) in block.instructions.iter().enumerate() {
            for reg in instruction_uses(inst) {
                result.entry(reg).or_default().push(UseSite::Instruction {
                    block: block.id,
                    index,
                });
            }
        }
        match &block.terminator {
            SIRTerminator::Jump(_, args) => {
                for &reg in args {
                    result
                        .entry(reg)
                        .or_default()
                        .push(UseSite::JumpArgument { block: block.id });
                }
            }
            SIRTerminator::Branch {
                cond,
                true_block,
                false_block,
            } => {
                result
                    .entry(*cond)
                    .or_default()
                    .push(UseSite::BranchCondition { block: block.id });
                for &reg in &true_block.1 {
                    result
                        .entry(reg)
                        .or_default()
                        .push(UseSite::TrueEdgeArgument { block: block.id });
                }
                for &reg in &false_block.1 {
                    result
                        .entry(reg)
                        .or_default()
                        .push(UseSite::FalseEdgeArgument { block: block.id });
                }
            }
            SIRTerminator::Return | SIRTerminator::Error(_) => {}
        }
    }
    result
}
