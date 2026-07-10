//! Stable machine-constraint facts after legalization.

use crate::backend::native::mir::{MFunction, VReg};

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

impl ConstraintModel {
    pub(super) fn build(func: &MFunction, cfg: &NormalizedCfg) -> Self {
        assert_eq!(func.blocks.len(), cfg.predecessors.len());
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
                            .zip(use_constraints(inst))
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
        Self { instructions }
    }

    pub(super) fn verify(&self, func: &MFunction) {
        assert_eq!(self.instructions.len(), func.blocks.len());
        for (block_index, block) in func.blocks.iter().enumerate() {
            assert_eq!(self.instructions[block_index].len(), block.insts.len());
            for (instruction_index, constraints) in
                self.instructions[block_index].iter().enumerate()
            {
                let mut required = std::collections::HashMap::new();
                for &(value, register) in &constraints.fixed_uses {
                    if let Some(previous) = required.insert(value, register) {
                        assert_eq!(
                            previous, register,
                            "fixed operand {value} at {}/i{} requires incompatible registers",
                            block.id, instruction_index
                        );
                    }
                }
                assert_eq!(
                    constraints.clobbers.as_slice(),
                    clobbers(&block.insts[instruction_index])
                );
            }
        }
    }
}
