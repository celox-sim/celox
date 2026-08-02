//! Stable machine-constraint facts after legalization.

use crate::native::mir::{BlockId, MFunction, VReg};
use celox_backend_common::regalloc::InstructionConstraints as CommonInstructionConstraints;

use super::assignment::{PhysReg, RegConstraint, clobbers, use_constraints};
use super::cfg::NormalizedCfg;

pub(super) type InstructionConstraints = CommonInstructionConstraints<VReg, PhysReg>;

#[derive(Debug)]
pub(super) struct ConstraintModel {
    pub instructions: Vec<Vec<InstructionConstraints>>,
    /// Opcode-free scalar facts exported by the x86-owned MIR. The mature
    /// allocator still consumes some MIR details directly while it is split
    /// into target driver and common algorithm, but new common analyses use
    /// this boundary instead of matching `MInst`.
    pub facts: super::facts::ScalarAllocationFacts,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ConstraintError {
    pub rule: &'static str,
    pub block: Option<BlockId>,
    pub instruction: Option<usize>,
    pub values: Vec<VReg>,
    pub message: String,
}

impl ConstraintError {
    fn new(
        rule: &'static str,
        block: Option<BlockId>,
        instruction: Option<usize>,
        values: Vec<VReg>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            rule,
            block,
            instruction,
            values,
            message: message.into(),
        }
    }
}

impl ConstraintModel {
    pub(super) fn build(func: &MFunction, cfg: &NormalizedCfg) -> Result<Self, ConstraintError> {
        if func.blocks.len() != cfg.predecessors.len() {
            return Err(ConstraintError::new(
                "CONSTRAINT.CFG_SHAPE",
                None,
                None,
                Vec::new(),
                format!(
                    "function has {} blocks but normalized CFG has {}",
                    func.blocks.len(),
                    cfg.predecessors.len()
                ),
            ));
        }
        let mut facts = super::facts::build(func, |inst| InstructionConstraints {
            fixed_uses: inst
                .uses()
                .into_iter()
                .zip(use_constraints(
                    inst,
                    func.target_features.variable_shift_encoding(),
                ))
                .filter_map(|(value, constraint)| match constraint {
                    RegConstraint::Any => None,
                    RegConstraint::Fixed(register) => Some((value, register)),
                })
                .collect(),
            fixed_defs: Vec::new(),
            clobbers: clobbers(inst).to_vec(),
        })
        .map_err(|error| {
            ConstraintError::new(
                "CONSTRAINT.ALLOCATION_FACTS",
                error.block,
                None,
                error.value.into_iter().collect(),
                error.message,
            )
        })?;
        // Jump tables may name the same destination more than once. MIR
        // liveness preserves those semantic edges, while the mature allocator
        // operates on a CFG which has already coalesced duplicate successors.
        // Constraint facts belong to that normalized allocation view.
        for (facts_block, successors) in facts.blocks.iter_mut().zip(&cfg.successors) {
            facts_block.successors.clone_from(successors);
        }
        facts.verify().map_err(|error| {
            ConstraintError::new(
                "CONSTRAINT.ALLOCATION_FACTS",
                None,
                None,
                Vec::new(),
                error.to_string(),
            )
        })?;
        celox_backend_common::regalloc::analyze_live_intervals(&facts).map_err(|error| {
            ConstraintError::new(
                "CONSTRAINT.LIVE_INTERVALS",
                error.block.map(|block| func.blocks[block].id),
                error.instruction,
                error.values,
                error.message,
            )
        })?;
        let instructions = facts
            .blocks
            .iter()
            .map(|block| {
                block
                    .instructions
                    .iter()
                    .map(|instruction| instruction.constraints.clone())
                    .collect()
            })
            .collect();
        Ok(Self {
            instructions,
            facts,
        })
    }

    pub(super) fn verify(&self, func: &MFunction) -> Result<(), ConstraintError> {
        if self.instructions.len() != func.blocks.len() {
            return Err(ConstraintError::new(
                "CONSTRAINT.BLOCK_COVERAGE",
                None,
                None,
                Vec::new(),
                format!(
                    "model covers {} blocks but function has {}",
                    self.instructions.len(),
                    func.blocks.len()
                ),
            ));
        }
        self.facts.verify().map_err(|error| {
            ConstraintError::new(
                "CONSTRAINT.ALLOCATION_FACTS",
                None,
                None,
                Vec::new(),
                error.to_string(),
            )
        })?;
        for (block_index, block) in func.blocks.iter().enumerate() {
            if self.instructions[block_index].len() != block.insts.len() {
                return Err(ConstraintError::new(
                    "CONSTRAINT.INSTRUCTION_COVERAGE",
                    Some(block.id),
                    None,
                    Vec::new(),
                    format!(
                        "model covers {} instructions but block has {}",
                        self.instructions[block_index].len(),
                        block.insts.len()
                    ),
                ));
            }
            for (instruction_index, constraints) in
                self.instructions[block_index].iter().enumerate()
            {
                let mut required = std::collections::HashMap::new();
                for &(value, register) in &constraints.fixed_uses {
                    if let Some(previous) = required.insert(value, register) {
                        if previous != register {
                            return Err(ConstraintError::new(
                                "CONSTRAINT.FIXED_USE_CONSISTENT",
                                Some(block.id),
                                Some(instruction_index),
                                vec![value],
                                format!(
                                    "fixed operand requires incompatible registers {previous:?} and {register:?}"
                                ),
                            ));
                        }
                    }
                }
                let expected = clobbers(&block.insts[instruction_index]);
                if constraints.clobbers.as_slice() != expected {
                    return Err(ConstraintError::new(
                        "CONSTRAINT.CLOBBERS_MATCH_OPCODE",
                        Some(block.id),
                        Some(instruction_index),
                        Vec::new(),
                        format!(
                            "recorded clobbers {:?} differ from opcode clobbers {expected:?}",
                            constraints.clobbers
                        ),
                    ));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::mir::{MBlock, MInst, SpillDesc, VRegAllocator};

    #[test]
    fn stale_instruction_model_is_a_structured_error() {
        let mut vregs = VRegAllocator::new();
        let value = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient()]);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm {
            dst: value,
            value: 1,
        });
        block.push(MInst::Return);
        func.push_block(block);
        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let mut model = ConstraintModel::build(&func, &cfg).unwrap();
        model.instructions[0].pop();

        let error = model.verify(&func).unwrap_err();

        assert_eq!(error.rule, "CONSTRAINT.INSTRUCTION_COVERAGE");
        assert_eq!(error.block, Some(BlockId(0)));
    }

    #[test]
    fn allocation_facts_use_normalized_jump_table_successors() {
        let mut vregs = VRegAllocator::new();
        let index = vregs.alloc();
        let table_base = vregs.alloc();
        let target = vregs.alloc();
        let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: index,
            value: 0,
        });
        entry.push(MInst::LoadImm {
            dst: table_base,
            value: 0,
        });
        entry.push(MInst::Scratch { dst: target });
        entry.push(MInst::JumpTable {
            index,
            table_base,
            target,
            targets: vec![BlockId(1), BlockId(1)].into_boxed_slice(),
        });
        let mut exit = MBlock::new(BlockId(1));
        exit.push(MInst::Return);
        function.blocks = vec![entry, exit];

        let cfg = super::super::cfg::normalize(&mut function).unwrap();
        let model = ConstraintModel::build(&function, &cfg).unwrap();

        assert_eq!(model.facts.blocks[0].successors, vec![1]);
    }
}
