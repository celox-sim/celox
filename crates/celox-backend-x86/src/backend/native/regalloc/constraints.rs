//! Stable machine-constraint facts after legalization.

use crate::backend::native::mir::{BlockId, MFunction, VReg};

use super::assignment::{PhysReg, RegConstraint, clobbers, use_constraints};
use super::cfg::NormalizedCfg;

#[derive(Debug, Clone, Default)]
pub(super) struct InstructionConstraints {
    pub fixed_uses: Vec<(VReg, PhysReg)>,
    pub clobbers: Vec<PhysReg>,
}

#[derive(Debug)]
pub(super) struct ConstraintModel {
    pub instructions: Vec<Vec<InstructionConstraints>>,
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
        let instructions: Vec<Vec<InstructionConstraints>> = func
            .blocks
            .iter()
            .map(|block| {
                block
                    .insts
                    .iter()
                    .map(|inst| InstructionConstraints {
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
                        clobbers: clobbers(inst).to_vec(),
                    })
                    .collect()
            })
            .collect();
        Ok(Self { instructions })
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
    use crate::backend::native::mir::{MBlock, MInst, SpillDesc, VRegAllocator};

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
}
