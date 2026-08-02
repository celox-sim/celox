//! AArch64 register-allocation boundary.
//!
//! Target MIR is projected into opcode-free facts consumed by shared analyses,
//! then colored against the AArch64 register file. The legacy bridge still
//! supplies spill placement while target-native spill insertion is developed;
//! it no longer supplies physical register colors.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;

use celox_backend_common::regalloc::{
    BlockAllocationFacts, FunctionAllocationFacts, InstructionAllocationFacts, PhiAllocationFacts,
    PhiSource, analyze_live_intervals,
};

use crate::Arm64Reg;
use crate::allocation::{Assignment, CopyDestination, CopyOperation, CopySource, EdgeCopyPlan};
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
    RegisterPressure {
        value: VReg,
    },
    ParallelCopy {
        predecessor: BlockId,
        successor: BlockId,
        message: String,
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
            Self::RegisterPressure { value } => write!(
                formatter,
                "AArch64 register pressure cannot assign {value} without spilling"
            ),
            Self::ParallelCopy {
                predecessor,
                successor,
                message,
            } => write!(
                formatter,
                "AArch64 parallel copy on {predecessor} -> {successor}: {message}"
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
                if !(1..=15).contains(&register.number()) && !(19..=28).contains(&register.number())
                {
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

const ALLOCATABLE_REGISTERS: [Arm64Reg; 25] = [
    Arm64Reg::new(1),
    Arm64Reg::new(2),
    Arm64Reg::new(3),
    Arm64Reg::new(4),
    Arm64Reg::new(5),
    Arm64Reg::new(6),
    Arm64Reg::new(7),
    Arm64Reg::new(8),
    Arm64Reg::new(9),
    Arm64Reg::new(10),
    Arm64Reg::new(11),
    Arm64Reg::new(12),
    Arm64Reg::new(13),
    Arm64Reg::new(14),
    Arm64Reg::new(15),
    Arm64Reg::new(19),
    Arm64Reg::new(20),
    Arm64Reg::new(21),
    Arm64Reg::new(22),
    Arm64Reg::new(23),
    Arm64Reg::new(24),
    Arm64Reg::new(25),
    Arm64Reg::new(26),
    Arm64Reg::new(27),
    Arm64Reg::new(28),
];

/// Allocate AArch64-owned target MIR without importing x86 register colors.
///
/// This first target-native path intentionally reports pressure instead of
/// hiding a spill policy. Spill/reload insertion is the next AArch64-owned
/// layer and can preserve this coloring and edge-copy boundary.
pub(crate) fn allocate_without_spills(
    function: MFunction,
) -> Result<AllocatedFunction, TargetRegallocError> {
    let facts = build_facts(&function)?;
    let intervals = analyze_live_intervals(&facts)
        .map_err(|error| TargetRegallocError::InvalidFacts(error.to_string()))?;
    let assignment = color_intervals(&function, &intervals)?;
    let edge_copies = build_edge_copies(&function, &assignment)?;
    let allocated = AllocatedFunction {
        function,
        assignment,
        edge_copies,
    };
    verify_allocated(&allocated)?;
    Ok(allocated)
}

fn color_intervals(
    function: &MFunction,
    intervals: &celox_backend_common::regalloc::LiveIntervals<VReg>,
) -> Result<Assignment<VReg>, TargetRegallocError> {
    let mut interference = intervals
        .iter()
        .map(|(&value, _)| (value, BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();
    for block in 0..function.blocks.len() {
        let mut segments = intervals
            .iter()
            .filter_map(|(&value, interval)| {
                interval
                    .segment_in_block(block)
                    .map(|segment| (segment.start, segment.end, value))
            })
            .collect::<Vec<_>>();
        segments.sort_unstable();
        let mut active = Vec::<(u64, VReg)>::new();
        for (start, end, value) in segments {
            active.retain(|(active_end, _)| *active_end > start);
            for &(_, other) in &active {
                interference.entry(value).or_default().insert(other);
                interference.entry(other).or_default().insert(value);
            }
            active.push((end, value));
        }
    }

    let mut affinities = BTreeMap::<VReg, BTreeSet<VReg>>::new();
    for block in &function.blocks {
        for phi in &block.phis {
            for &(_, source) in &phi.sources {
                affinities.entry(phi.dst).or_default().insert(source);
                affinities.entry(source).or_default().insert(phi.dst);
            }
        }
    }
    let mut order = interference
        .iter()
        .map(|(&value, neighbors)| (Reverse(neighbors.len()), value))
        .collect::<Vec<_>>();
    order.sort_unstable();

    let mut assignment = Assignment::default();
    for (_, value) in order {
        let used = interference[&value]
            .iter()
            .filter_map(|neighbor| assignment.get(neighbor))
            .collect::<BTreeSet<_>>();
        let preferred = affinities
            .get(&value)
            .into_iter()
            .flatten()
            .filter_map(|neighbor| assignment.get(neighbor))
            .find(|register| !used.contains(register));
        let register = preferred
            .or_else(|| {
                ALLOCATABLE_REGISTERS
                    .iter()
                    .copied()
                    .find(|register| !used.contains(register))
            })
            .ok_or(TargetRegallocError::RegisterPressure { value })?;
        assignment.set(value, register);
    }
    Ok(assignment)
}

fn build_edge_copies(
    function: &MFunction,
    assignment: &Assignment<VReg>,
) -> Result<EdgeCopyPlan<BlockId>, TargetRegallocError> {
    use celox_backend_common::regalloc as common;

    let mut plan = EdgeCopyPlan::default();
    for successor in &function.blocks {
        let mut rows_by_predecessor =
            BTreeMap::<BlockId, Vec<common::ParallelCopy<Arm64Reg>>>::new();
        for phi in &successor.phis {
            let destination =
                assignment
                    .get(&phi.dst)
                    .ok_or(TargetRegallocError::MissingAssignment {
                        block: successor.id,
                        instruction: 0,
                        value: phi.dst,
                    })?;
            for &(predecessor, source_value) in &phi.sources {
                let source = assignment.get(&source_value).ok_or(
                    TargetRegallocError::MissingAssignment {
                        block: predecessor,
                        instruction: 0,
                        value: source_value,
                    },
                )?;
                rows_by_predecessor
                    .entry(predecessor)
                    .or_default()
                    .push(common::ParallelCopy {
                        destination: common::CopyDestination::Register(destination),
                        source: common::CopySource::Register(source),
                    });
            }
        }
        for (predecessor, rows) in rows_by_predecessor {
            let resolution = common::resolve_parallel_copies(&rows).map_err(|error| {
                TargetRegallocError::ParallelCopy {
                    predecessor,
                    successor: successor.id,
                    message: error.to_string(),
                }
            })?;
            let operations = resolution
                .operations
                .into_iter()
                .map(|operation| match operation {
                    common::CopyOperation::Move {
                        destination,
                        source,
                    } => CopyOperation::Move {
                        destination: adapt_copy_destination(destination),
                        source: adapt_copy_source(source),
                    },
                    common::CopyOperation::SwapRegisters { left, right } => {
                        CopyOperation::SwapRegisters { left, right }
                    }
                    common::CopyOperation::SaveTemporary(destination) => {
                        CopyOperation::SaveTemporary(adapt_copy_destination(destination))
                    }
                    common::CopyOperation::RestoreTemporary(destination) => {
                        CopyOperation::RestoreTemporary(adapt_copy_destination(destination))
                    }
                })
                .collect();
            plan.insert(predecessor, successor.id, operations);
        }
    }
    Ok(plan)
}

fn adapt_copy_destination(
    destination: celox_backend_common::regalloc::CopyDestination<Arm64Reg>,
) -> CopyDestination {
    match destination {
        celox_backend_common::regalloc::CopyDestination::Register(register) => {
            CopyDestination::Register(register)
        }
        celox_backend_common::regalloc::CopyDestination::Stack(offset) => {
            CopyDestination::Stack(offset)
        }
    }
}

fn adapt_copy_source(source: celox_backend_common::regalloc::CopySource<Arm64Reg>) -> CopySource {
    match source {
        celox_backend_common::regalloc::CopySource::Register(register) => {
            CopySource::Register(register)
        }
        celox_backend_common::regalloc::CopySource::Stack(offset) => CopySource::Stack(offset),
        celox_backend_common::regalloc::CopySource::Immediate(value) => {
            CopySource::Immediate(value)
        }
    }
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
    use crate::mir::{BaseReg, MBlock, MInst, OpSize, PhiNode};

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

    #[test]
    fn allocates_target_owned_mir_without_legacy_colors() {
        let allocated = allocate_without_spills(diamond_function()).unwrap();

        for value in [VReg(0), VReg(1), VReg(2), VReg(3)] {
            let register = allocated.assignment.get(&value).unwrap();
            assert!(
                (1..=15).contains(&register.number()) || (19..=28).contains(&register.number())
            );
        }
        verify_allocated(&allocated).unwrap();
    }

    #[test]
    fn lowers_target_phi_rows_with_the_common_copy_resolver() {
        let predecessor = MBlock {
            id: BlockId(0),
            phis: Vec::new(),
            insts: vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: 7,
                },
                MInst::Jump { target: BlockId(1) },
            ],
        };
        let successor = MBlock {
            id: BlockId(1),
            phis: vec![PhiNode {
                dst: VReg(1),
                sources: vec![(BlockId(0), VReg(0))],
            }],
            insts: vec![MInst::Return],
        };
        let function = MFunction::new(vec![predecessor, successor], Vec::new());
        let mut assignment = Assignment::default();
        assignment.set(VReg(0), Arm64Reg::new(1));
        assignment.set(VReg(1), Arm64Reg::new(2));

        let copies = build_edge_copies(&function, &assignment).unwrap();

        assert_eq!(
            copies.edge(BlockId(0), BlockId(1)),
            Some(
                [CopyOperation::Move {
                    destination: CopyDestination::Register(Arm64Reg::new(2)),
                    source: CopySource::Register(Arm64Reg::new(1)),
                }]
                .as_slice()
            )
        );
    }

    #[test]
    fn reports_pressure_before_spill_support_is_enabled() {
        let mut instructions = (0..26)
            .map(|value| MInst::LoadImm {
                dst: VReg(value),
                value: u64::from(value),
            })
            .collect::<Vec<_>>();
        instructions.extend((0..26).map(|value| MInst::Store {
            base: BaseReg::SimState,
            offset: value * 8,
            src: VReg(value as u32),
            size: OpSize::S64,
        }));
        instructions.push(MInst::Return);
        let function = MFunction::new(
            vec![MBlock {
                id: BlockId(0),
                phis: Vec::new(),
                insts: instructions,
            }],
            Vec::new(),
        );

        assert!(matches!(
            allocate_without_spills(function),
            Err(TargetRegallocError::RegisterPressure { .. })
        ));
    }

    #[test]
    fn uses_preserved_registers_above_caller_saved_pressure() {
        let mut instructions = (0..16)
            .map(|value| MInst::LoadImm {
                dst: VReg(value),
                value: u64::from(value),
            })
            .collect::<Vec<_>>();
        instructions.extend((0..16).map(|value| MInst::Store {
            base: BaseReg::SimState,
            offset: value * 8,
            src: VReg(value as u32),
            size: OpSize::S64,
        }));
        instructions.push(MInst::Return);
        let allocated = allocate_without_spills(MFunction::new(
            vec![MBlock {
                id: BlockId(0),
                phis: Vec::new(),
                insts: instructions,
            }],
            Vec::new(),
        ))
        .unwrap();

        assert!(
            allocated
                .assignment
                .iter()
                .any(|(_, register)| (19..=28).contains(&register.number()))
        );
    }
}
