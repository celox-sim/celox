//! AArch64 register-allocation boundary.
//!
//! Target MIR is projected into opcode-free facts consumed by shared analyses.
//! The production pipeline still imports a mature allocation through the
//! legacy bridge, but that result is independently checked here against the
//! AArch64 MIR and register file before emission.

use std::collections::HashMap;
use std::fmt;

use celox_backend_common::regalloc::{
    BlockAllocationFacts, FunctionAllocationFacts, InstructionAllocationFacts, PhiAllocationFacts,
    PhiSource, analyze_live_intervals,
};

use crate::Arm64Reg;
use crate::mir::{AllocatedFunction, BlockId, MFunction, VReg};

pub(crate) type AllocationFacts = FunctionAllocationFacts<VReg, Arm64Reg>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TargetRegallocError {
    EmptyFunction,
    MissingBlock {
        block: BlockId,
        target: BlockId,
    },
    InvalidFacts(String),
    MissingAssignment {
        block: BlockId,
        instruction: usize,
        value: VReg,
    },
    ReservedAssignment {
        block: BlockId,
        instruction: usize,
        value: VReg,
        register: Arm64Reg,
    },
    RegisterConflict {
        block: BlockId,
        instruction: usize,
        left: VReg,
        right: VReg,
        register: Arm64Reg,
    },
}

impl fmt::Display for TargetRegallocError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyFunction => formatter.write_str("AArch64 allocation has no entry block"),
            Self::MissingBlock { block, target } => {
                write!(
                    formatter,
                    "AArch64 block {block} targets missing block {target}"
                )
            }
            Self::InvalidFacts(error) => {
                write!(formatter, "invalid AArch64 allocation facts: {error}")
            }
            Self::MissingAssignment {
                block,
                instruction,
                value,
            } => write!(
                formatter,
                "AArch64 {block} instruction {instruction} value {value} has no register assignment"
            ),
            Self::ReservedAssignment {
                block,
                instruction,
                value,
                register,
            } => write!(
                formatter,
                "AArch64 {block} instruction {instruction} value {value} uses reserved x{}",
                register.number()
            ),
            Self::RegisterConflict {
                block,
                instruction,
                left,
                right,
                register,
            } => write!(
                formatter,
                "AArch64 {block} point {instruction} keeps {left} and {right} live in x{}",
                register.number()
            ),
        }
    }
}

impl std::error::Error for TargetRegallocError {}

pub(crate) fn build_facts(function: &MFunction) -> Result<AllocationFacts, TargetRegallocError> {
    if function.blocks.is_empty() {
        return Err(TargetRegallocError::EmptyFunction);
    }
    let indices = function
        .blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.id, index))
        .collect::<HashMap<_, _>>();
    let blocks = function
        .blocks
        .iter()
        .map(|block| {
            let block_target = |target: BlockId| {
                indices
                    .get(&target)
                    .copied()
                    .ok_or(TargetRegallocError::MissingBlock {
                        block: block.id,
                        target,
                    })
            };
            let successors = block
                .successors()
                .into_iter()
                .map(block_target)
                .collect::<Result<_, _>>()?;
            let phis = block
                .phis
                .iter()
                .map(|phi| {
                    Ok(PhiAllocationFacts {
                        destination: phi.dst,
                        sources: phi
                            .sources
                            .iter()
                            .map(|&(predecessor, value)| {
                                Ok(PhiSource {
                                    predecessor: block_target(predecessor)?,
                                    value,
                                })
                            })
                            .collect::<Result<_, TargetRegallocError>>()?,
                    })
                })
                .collect::<Result<_, TargetRegallocError>>()?;
            let instructions = block
                .insts
                .iter()
                .map(|instruction| InstructionAllocationFacts {
                    uses: instruction.uses(),
                    defs: instruction.def().into_iter().collect(),
                    is_copy: instruction.is_copy(),
                    ..InstructionAllocationFacts::default()
                })
                .collect();
            Ok(BlockAllocationFacts {
                successors,
                phis,
                instructions,
            })
        })
        .collect::<Result<_, TargetRegallocError>>()?;
    let facts = FunctionAllocationFacts { entry: 0, blocks };
    facts
        .verify()
        .map_err(|error| TargetRegallocError::InvalidFacts(error.to_string()))?;
    Ok(facts)
}

pub(crate) fn verify_allocated(function: &AllocatedFunction) -> Result<(), TargetRegallocError> {
    let facts = build_facts(&function.function)?;
    let intervals = analyze_live_intervals(&facts)
        .map_err(|error| TargetRegallocError::InvalidFacts(error.to_string()))?;

    for block in &function.function.blocks {
        for (instruction_index, instruction) in block.insts.iter().enumerate() {
            for value in instruction.uses().into_iter().chain(instruction.def()) {
                let Some(register) = function.assignment.get(&value) else {
                    return Err(TargetRegallocError::MissingAssignment {
                        block: block.id,
                        instruction: instruction_index,
                        value,
                    });
                };
                if !(1..=15).contains(&register.number()) {
                    return Err(TargetRegallocError::ReservedAssignment {
                        block: block.id,
                        instruction: instruction_index,
                        value,
                        register,
                    });
                }
            }
        }
    }
    verify_interval_registers(function, &intervals)
}

fn verify_interval_registers(
    function: &AllocatedFunction,
    intervals: &celox_backend_common::regalloc::LiveIntervals<VReg>,
) -> Result<(), TargetRegallocError> {
    let mut segments = Vec::new();
    for (&value, interval) in intervals.iter() {
        let Some(register) = function.assignment.get(&value) else {
            // Legacy edge values may reside in a stack home or immediate.
            continue;
        };
        segments.extend(
            interval
                .segments
                .iter()
                .map(|segment| (segment.block, register, segment.start, segment.end, value)),
        );
    }
    segments.sort_unstable_by_key(|&(block, register, start, end, value)| {
        (block, register, start, end, value)
    });
    let mut previous = None;
    for (block, register, start, end, value) in segments {
        if let Some((left_block, left_register, _, left_end, left)) = previous
            && left_block == block
            && left_register == register
            && start < left_end
            && left != value
        {
            return Err(TargetRegallocError::RegisterConflict {
                block: function.function.blocks[block].id,
                instruction: usize::try_from(start / 3).unwrap_or(usize::MAX),
                left,
                right: value,
                register,
            });
        }
        previous = Some((block, register, start, end, value));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::allocation::{Assignment, EdgeCopyPlan};
    use crate::mir::{MBlock, MInst, PhiNode};

    fn diamond_function() -> MFunction {
        let entry = MBlock {
            id: BlockId(10),
            phis: Vec::new(),
            insts: vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: 1,
                },
                MInst::Branch {
                    cond: VReg(0),
                    true_bb: BlockId(20),
                    false_bb: BlockId(30),
                },
            ],
        };
        let left = MBlock {
            id: BlockId(20),
            phis: Vec::new(),
            insts: vec![
                MInst::LoadImm {
                    dst: VReg(1),
                    value: 2,
                },
                MInst::Jump {
                    target: BlockId(40),
                },
            ],
        };
        let right = MBlock {
            id: BlockId(30),
            phis: Vec::new(),
            insts: vec![
                MInst::LoadImm {
                    dst: VReg(2),
                    value: 3,
                },
                MInst::Jump {
                    target: BlockId(40),
                },
            ],
        };
        let join = MBlock {
            id: BlockId(40),
            phis: vec![PhiNode {
                dst: VReg(3),
                sources: vec![(BlockId(20), VReg(1)), (BlockId(30), VReg(2))],
            }],
            insts: vec![MInst::Return],
        };
        MFunction::new(vec![entry, left, right, join], Vec::new())
    }

    #[test]
    fn exports_normalized_cfg_phi_and_instruction_facts() {
        let facts = build_facts(&diamond_function()).unwrap();

        assert_eq!(facts.blocks[0].successors, vec![1, 2]);
        assert_eq!(facts.blocks[0].instructions[0].defs, vec![VReg(0)]);
        assert_eq!(facts.blocks[0].instructions[1].uses, vec![VReg(0)]);
        assert_eq!(facts.blocks[3].phis[0].destination, VReg(3));
        assert_eq!(facts.blocks[3].phis[0].sources[0].predecessor, 1);
        assert_eq!(facts.blocks[3].phis[0].sources[1].predecessor, 2);
        analyze_live_intervals(&facts).unwrap();
    }

    #[test]
    fn rejects_missing_and_reserved_instruction_assignments() {
        let function = diamond_function();
        let mut allocated = AllocatedFunction {
            function,
            assignment: Assignment::default(),
            edge_copies: EdgeCopyPlan::default(),
        };
        assert!(matches!(
            verify_allocated(&allocated),
            Err(TargetRegallocError::MissingAssignment { value: VReg(0), .. })
        ));

        for (value, register) in [
            (VReg(0), Arm64Reg::new(1)),
            (VReg(1), Arm64Reg::new(2)),
            (VReg(2), Arm64Reg::new(3)),
        ] {
            allocated.assignment.set(value, register);
        }
        allocated.assignment.set(VReg(0), Arm64Reg::new(16));
        assert!(matches!(
            verify_allocated(&allocated),
            Err(TargetRegallocError::ReservedAssignment { value: VReg(0), .. })
        ));
    }

    #[test]
    fn rejects_interfering_values_assigned_to_one_register() {
        let function = MFunction::new(
            vec![MBlock {
                id: BlockId(0),
                phis: Vec::new(),
                insts: vec![
                    MInst::LoadImm {
                        dst: VReg(0),
                        value: 1,
                    },
                    MInst::LoadImm {
                        dst: VReg(1),
                        value: 2,
                    },
                    MInst::Add {
                        dst: VReg(2),
                        lhs: VReg(0),
                        rhs: VReg(1),
                    },
                    MInst::Return,
                ],
            }],
            Vec::new(),
        );
        let mut assignment = Assignment::default();
        assignment.set(VReg(0), Arm64Reg::new(1));
        assignment.set(VReg(1), Arm64Reg::new(1));
        assignment.set(VReg(2), Arm64Reg::new(2));
        let allocated = AllocatedFunction {
            function,
            assignment,
            edge_copies: EdgeCopyPlan::default(),
        };

        assert!(matches!(
            verify_allocated(&allocated),
            Err(TargetRegallocError::RegisterConflict {
                register,
                ..
            }) if register == Arm64Reg::new(1)
        ));
    }
}
