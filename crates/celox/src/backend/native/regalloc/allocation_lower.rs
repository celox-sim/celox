//! Atomic lowering of a completed expanded allocation to strict-SSA MIR.
//!
//! The allocator's machine-value IR is the authority for def/use identity and
//! liveness.  This phase first re-verifies the completed joint coloring, then
//! materializes every original rewrite and allocator-owned transition in a
//! private `MFunction`.  The result is returned only if canonical MIR
//! verification and an independent liveness rebuild exactly reproduce the
//! allocation problem which was colored.

use std::collections::BTreeSet;
use std::fmt;

use crate::backend::native::mir::{BlockId, MFunction, VReg};

use super::allocation_expand::{
    ExpandedAllocationProblem, ExpandedEdgeLocation, ExpandedStackDefinition, ExpandedUseSource,
};
use super::allocation_ir::AllocationIrError;
use super::allocation_reallocate::{JointAllocation, JointAllocationError, JointAllocationProblem};
use super::assignment::{AssignmentMap, EdgeLocation, PhysReg};
use super::cfg::NormalizedCfg;
use super::home_graph::{HomeGraph, HomeGraphError, RecipeNode};
use super::live_interval::{
    self, LiveIntervalError, NonRegisterPhiDefinition, NonRegisterPhiSource, UseSite,
};

#[derive(Debug)]
pub(super) struct LoweredAllocation {
    pub function: MFunction,
    pub assignment: AssignmentMap,
    pub spill_frame_size: u32,
    pub stack_offsets: Vec<i32>,
    pub ssa_destruction: crate::backend::native::ssa_destroy::SsaDestructionPlan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AllocationLowerError {
    pub rule: &'static str,
    pub block: Option<BlockId>,
    pub instruction: Option<usize>,
    pub values: Vec<VReg>,
    pub message: String,
}

impl AllocationLowerError {
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

    fn graph(error: HomeGraphError) -> Self {
        Self::new(
            error.rule,
            error.block,
            error.instruction,
            error.values,
            error.message,
        )
    }

    fn joint(error: JointAllocationError) -> Self {
        Self::new(
            error.rule,
            error.block,
            None,
            error.value.into_iter().collect(),
            error.message,
        )
    }

    fn ir(error: AllocationIrError) -> Self {
        Self::new(
            error.rule,
            error.block,
            error.instruction,
            error.values,
            error.message,
        )
    }

    fn live(error: LiveIntervalError) -> Self {
        Self::new(
            error.rule,
            error.block,
            error.instruction,
            error.values,
            error.message,
        )
    }

    fn ssa(error: crate::backend::native::ssa_destroy::SsaDestructionError) -> Self {
        let mut values = Vec::with_capacity(2);
        if let Some(destination) = error.phi_destination {
            values.push(destination);
        }
        if let Some(source) = error.source_value {
            values.push(source);
        }
        let block = error.successor.or(error.predecessor);
        Self::new(error.rule, block, None, values, error.message)
    }
}

impl fmt::Display for AllocationLowerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.rule)?;
        if let Some(block) = self.block {
            write!(formatter, " at {block}")?;
        }
        if let Some(instruction) = self.instruction {
            write!(formatter, "/i{instruction}")?;
        }
        if !self.values.is_empty() {
            write!(formatter, " values={:?}", self.values)?;
        }
        write!(formatter, ": {}", self.message)
    }
}

impl std::error::Error for AllocationLowerError {}

pub(super) fn lower(
    original: &MFunction,
    cfg: &NormalizedCfg,
    graph: &HomeGraph,
    expanded: &ExpandedAllocationProblem,
    allocation: &JointAllocation,
    registers: &[PhysReg],
) -> Result<LoweredAllocation, AllocationLowerError> {
    graph
        .verify(original, cfg)
        .map_err(AllocationLowerError::graph)?;
    expanded
        .ir
        .verify_stack_homes(cfg)
        .map_err(AllocationLowerError::ir)?;
    let problem =
        JointAllocationProblem::build(expanded, cfg).map_err(AllocationLowerError::joint)?;
    problem
        .verify(cfg, registers, allocation)
        .map_err(AllocationLowerError::joint)?;

    let (stack_offsets, spill_frame_size) = stack_layout(expanded)?;
    let function = expanded
        .ir
        .materialize(original, graph, &stack_offsets)
        .map_err(AllocationLowerError::ir)?;
    let edge_locations = edge_locations(expanded, graph, &stack_offsets)?;
    let phi_destinations = phi_destinations(expanded, &stack_offsets)?;
    let nonregister = edge_locations
        .iter()
        .map(|(source, _)| *source)
        .collect::<BTreeSet<_>>();
    if nonregister.len() != edge_locations.len() {
        return Err(AllocationLowerError::new(
            "ALLOCATION_LOWER.EDGE_LOCATION_IDENTITY",
            None,
            None,
            Vec::new(),
            "two expanded uses claim the same semantic phi-edge location",
        ));
    }
    let nonregister_definitions = phi_destinations
        .iter()
        .map(|(definition, _)| *definition)
        .collect::<BTreeSet<_>>();
    if nonregister_definitions.len() != phi_destinations.len() {
        return Err(AllocationLowerError::new(
            "ALLOCATION_LOWER.PHI_HOME_IDENTITY",
            None,
            None,
            Vec::new(),
            "two stack homes claim the same semantic phi destination",
        ));
    }
    let rebuilt = live_interval::analyze_with_nonregister_phi_sources(
        &function,
        cfg,
        &nonregister,
        &nonregister_definitions,
    )
    .map_err(AllocationLowerError::live)?;
    if rebuilt != expanded.intervals {
        return Err(AllocationLowerError::new(
            "ALLOCATION_LOWER.LIVENESS_IDENTITY",
            None,
            None,
            Vec::new(),
            "lowered strict-SSA MIR does not reproduce the exact allocation-IR live intervals",
        ));
    }

    let mut assignment = AssignmentMap::default();
    for (value, register) in allocation.assignments.iter().copied().enumerate() {
        if let Some(register) = register {
            let value = u32::try_from(value).map_err(|_| {
                AllocationLowerError::new(
                    "ALLOCATION_LOWER.VALUE_ID_RANGE",
                    None,
                    None,
                    Vec::new(),
                    "joint assignment value identity exceeds u32",
                )
            })?;
            assignment.set(VReg(value), register);
        }
    }
    for (source, location) in edge_locations {
        let successor = cfg.block_index[&source.successor];
        let destination = function.blocks[successor].phis[source.phi].dst;
        assignment.set_phi_edge_location(
            source.predecessor,
            source.successor,
            destination,
            source.value,
            location,
        );
    }
    for (definition, offset) in phi_destinations {
        if assignment.get(definition.value).is_some() {
            return Err(AllocationLowerError::new(
                "ALLOCATION_LOWER.PHI_HOME_ASSIGNMENT",
                Some(definition.block),
                None,
                vec![definition.value],
                "stack-resident phi destination also has a physical-register assignment",
            ));
        }
        assignment.set_edge_spill_slot(definition.value, offset);
    }
    let ssa_destruction =
        crate::backend::native::ssa_destroy::SsaDestructionPlan::build(&function, &assignment)
            .map_err(AllocationLowerError::ssa)?;
    ssa_destruction
        .verify(&function, &assignment, spill_frame_size)
        .map_err(AllocationLowerError::ssa)?;

    Ok(LoweredAllocation {
        function,
        assignment,
        spill_frame_size,
        stack_offsets,
        ssa_destruction,
    })
}

fn phi_destinations(
    expanded: &ExpandedAllocationProblem,
    stack_offsets: &[i32],
) -> Result<Vec<(NonRegisterPhiDefinition, i32)>, AllocationLowerError> {
    let mut output = Vec::new();
    for home in &expanded.stack_homes {
        let ExpandedStackDefinition::Phi {
            block,
            phi,
            destination,
        } = home.definition
        else {
            continue;
        };
        let offset = stack_offsets
            .get(home.id.0 as usize)
            .copied()
            .ok_or_else(|| {
                AllocationLowerError::new(
                    "ALLOCATION_LOWER.PHI_STACK_HOME",
                    Some(block),
                    None,
                    vec![destination],
                    "phi stack destination has no concrete frame slot",
                )
            })?;
        output.push((
            NonRegisterPhiDefinition {
                block,
                phi,
                value: destination,
            },
            offset,
        ));
    }
    Ok(output)
}

fn edge_locations(
    expanded: &ExpandedAllocationProblem,
    graph: &HomeGraph,
    stack_offsets: &[i32],
) -> Result<Vec<(NonRegisterPhiSource, EdgeLocation)>, AllocationLowerError> {
    let mut output = Vec::new();
    for root in &expanded.roots {
        for use_ in &root.uses {
            let ExpandedUseSource::Edge(location) = &use_.source else {
                continue;
            };
            let UseSite::PhiEdge {
                predecessor,
                successor,
                phi,
                ..
            } = use_.site
            else {
                return Err(AllocationLowerError::new(
                    "ALLOCATION_LOWER.EDGE_LOCATION_SITE",
                    Some(use_.site.block()),
                    None,
                    vec![use_.value],
                    "expanded edge location is not attached to a phi-edge use",
                ));
            };
            if use_.value != root.origin {
                return Err(AllocationLowerError::new(
                    "ALLOCATION_LOWER.EDGE_LOCATION_VALUE",
                    Some(predecessor),
                    None,
                    vec![use_.value, root.origin],
                    "expanded edge location does not retain its semantic root value",
                ));
            }
            let location = match *location {
                ExpandedEdgeLocation::Stack { home } => {
                    let offset = stack_offsets.get(home.0 as usize).copied().ok_or_else(|| {
                        AllocationLowerError::new(
                            "ALLOCATION_LOWER.EDGE_STACK_HOME",
                            Some(predecessor),
                            None,
                            vec![root.origin],
                            "phi-edge stack location has no concrete frame slot",
                        )
                    })?;
                    EdgeLocation::Stack(offset)
                }
                ExpandedEdgeLocation::Immediate { value, recipe } => {
                    if graph.recipe_nodes.get(recipe.0 as usize)
                        != Some(&RecipeNode::Constant(value))
                    {
                        return Err(AllocationLowerError::new(
                            "ALLOCATION_LOWER.EDGE_IMMEDIATE_RECIPE",
                            Some(predecessor),
                            None,
                            vec![root.origin],
                            "phi-edge immediate differs from its exact HomeGraph recipe",
                        ));
                    }
                    EdgeLocation::Immediate(value)
                }
            };
            output.push((
                NonRegisterPhiSource {
                    predecessor,
                    successor,
                    phi,
                    value: root.origin,
                },
                location,
            ));
        }
    }
    Ok(output)
}

/// Correctness-first stack layout. Every logical stack home receives one
/// stable 64-bit slot; interference-based slot coloring is a later allocation
/// stage and must not be hidden inside lowering.
fn stack_layout(
    expanded: &ExpandedAllocationProblem,
) -> Result<(Vec<i32>, u32), AllocationLowerError> {
    let mut offsets = Vec::with_capacity(expanded.stack_homes.len());
    for (index, home) in expanded.stack_homes.iter().enumerate() {
        if home.id.0 as usize != index {
            return Err(AllocationLowerError::new(
                "ALLOCATION_LOWER.STACK_HOME_IDENTITY",
                None,
                None,
                Vec::new(),
                "expanded stack homes are not a dense identity-ordered domain",
            ));
        }
        let byte_offset = index.checked_mul(8).ok_or_else(|| {
            AllocationLowerError::new(
                "ALLOCATION_LOWER.STACK_FRAME_RANGE",
                None,
                None,
                Vec::new(),
                "stack-home byte offset exceeds usize",
            )
        })?;
        offsets.push(i32::try_from(byte_offset).map_err(|_| {
            AllocationLowerError::new(
                "ALLOCATION_LOWER.STACK_FRAME_RANGE",
                None,
                None,
                Vec::new(),
                "stack-home byte offset exceeds MIR's signed frame-offset domain",
            )
        })?);
    }
    let frame_size = expanded
        .stack_homes
        .len()
        .checked_mul(8)
        .and_then(|size| u32::try_from(size).ok())
        .ok_or_else(|| {
            AllocationLowerError::new(
                "ALLOCATION_LOWER.STACK_FRAME_RANGE",
                None,
                None,
                Vec::new(),
                "stack frame size exceeds u32",
            )
        })?;
    Ok((offsets, frame_size))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::native::mir::{
        BaseReg, MBlock, MInst, OpSize, PhiNode, SpillDesc, VRegAllocator,
    };

    use super::super::allocation_expand::{ExpandedMaterialization, ExpandedUseSource, expand};
    use super::super::allocation_split::allocate_with_splitting;
    use super::super::assignment::ALLOCATABLE_REGS;
    use super::super::cfg;
    use super::super::home_graph::{self, HomeKind};
    use super::super::interval_allocator::allocate_roots;

    fn function(value_count: u32, instructions: Vec<MInst>) -> MFunction {
        let mut values = VRegAllocator::new();
        for _ in 0..value_count {
            values.alloc();
        }
        let mut function =
            MFunction::new(values, vec![SpillDesc::transient(); value_count as usize]);
        let mut block = MBlock::new(BlockId(0));
        block.insts = instructions;
        function.blocks.push(block);
        function
    }

    fn state_and_stack_function() -> MFunction {
        function(
            10,
            vec![
                MInst::Load {
                    dst: VReg(0),
                    base: BaseReg::SimState,
                    offset: 0,
                    size: OpSize::S64,
                },
                MInst::LoadImm {
                    dst: VReg(1),
                    value: 11,
                },
                MInst::LoadImm {
                    dst: VReg(2),
                    value: 13,
                },
                MInst::Add {
                    dst: VReg(3),
                    lhs: VReg(1),
                    rhs: VReg(2),
                },
                MInst::Mov {
                    dst: VReg(4),
                    src: VReg(0),
                },
                MInst::Mov {
                    dst: VReg(5),
                    src: VReg(3),
                },
                MInst::LoadImm {
                    dst: VReg(6),
                    value: 0,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 0,
                    src: VReg(6),
                    size: OpSize::S64,
                },
                MInst::Mov {
                    dst: VReg(7),
                    src: VReg(3),
                },
                MInst::Mov {
                    dst: VReg(8),
                    src: VReg(3),
                },
                MInst::Mov {
                    dst: VReg(9),
                    src: VReg(0),
                },
                MInst::Return,
            ],
        )
    }

    #[test]
    fn complete_allocation_lowers_once_with_exact_liveness_and_stack_layout() {
        let mut source = state_and_stack_function();
        let cfg = cfg::normalize(&mut source).unwrap();
        let graph = home_graph::build(&source, &cfg).unwrap();
        let planning_registers = [PhysReg::RAX];
        let plan = allocate_roots(&graph, &cfg, &planning_registers).unwrap();
        let mut expanded = expand(&source, &cfg, &graph, &plan, &planning_registers).unwrap();
        assert!(!expanded.stack_homes.is_empty());
        assert!(
            expanded
                .roots
                .iter()
                .flat_map(|root| &root.uses)
                .any(|use_| {
                    matches!(
                        use_.source,
                        ExpandedUseSource::Materialized(ExpandedMaterialization::Recipe {
                            kind: HomeKind::State(_),
                            ..
                        })
                    )
                })
        );
        let allocation =
            allocate_with_splitting(&mut expanded, &graph, &cfg, ALLOCATABLE_REGS).unwrap();
        let source_before = format!("{source:?}");

        let lowered = lower(
            &source,
            &cfg,
            &graph,
            &expanded,
            &allocation,
            ALLOCATABLE_REGS,
        )
        .unwrap();

        assert_eq!(format!("{source:?}"), source_before);
        assert_eq!(lowered.function.vregs.count(), expanded.ir.value_count());
        assert_eq!(
            lowered.spill_frame_size,
            u32::try_from(expanded.stack_homes.len() * 8).unwrap()
        );
        assert_eq!(
            lowered.stack_offsets,
            (0..expanded.stack_homes.len())
                .map(|home| i32::try_from(home * 8).unwrap())
                .collect::<Vec<_>>()
        );
        let rebuilt = live_interval::analyze(&lowered.function, &cfg).unwrap();
        assert_eq!(rebuilt, expanded.intervals);
        assert_eq!(
            lowered.assignment.sorted_entries().len(),
            expanded
                .intervals
                .intervals
                .iter()
                .filter(|interval| interval.is_some())
                .count()
        );
        for &offset in &lowered.stack_offsets {
            assert!(lowered.function.blocks.iter().any(|block| {
                block.insts.iter().any(|instruction| {
                    matches!(
                        instruction,
                        MInst::Store {
                            base: BaseReg::StackFrame,
                            offset: candidate,
                            size: OpSize::S64,
                            ..
                        } if *candidate == offset
                    )
                })
            }));
            assert!(lowered.function.blocks.iter().any(|block| {
                block.insts.iter().any(|instruction| {
                    matches!(
                        instruction,
                        MInst::Load {
                            base: BaseReg::StackFrame,
                            offset: candidate,
                            size: OpSize::S64,
                            ..
                        } if *candidate == offset
                    )
                })
            }));
        }
    }

    #[test]
    fn stale_source_and_joint_assignment_are_rejected_before_publication() {
        let mut source = function(
            2,
            vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: 7,
                },
                MInst::Mov {
                    dst: VReg(1),
                    src: VReg(0),
                },
                MInst::Return,
            ],
        );
        let cfg = cfg::normalize(&mut source).unwrap();
        let graph = home_graph::build(&source, &cfg).unwrap();
        let plan = allocate_roots(&graph, &cfg, ALLOCATABLE_REGS).unwrap();
        let mut expanded = expand(&source, &cfg, &graph, &plan, ALLOCATABLE_REGS).unwrap();
        let allocation =
            allocate_with_splitting(&mut expanded, &graph, &cfg, ALLOCATABLE_REGS).unwrap();
        let source_before = format!("{source:?}");

        let mut stale_source = source.clone();
        let MInst::LoadImm { value, .. } = &mut stale_source.blocks[0].insts[0] else {
            unreachable!()
        };
        *value = 8;
        let error = lower(
            &stale_source,
            &cfg,
            &graph,
            &expanded,
            &allocation,
            ALLOCATABLE_REGS,
        )
        .unwrap_err();
        assert_eq!(error.rule, "ALLOCATION_IR.SOURCE_INSTRUCTION");

        let mut stale_allocation = allocation.clone();
        stale_allocation.assignments.pop();
        let error = lower(
            &source,
            &cfg,
            &graph,
            &expanded,
            &stale_allocation,
            ALLOCATABLE_REGS,
        )
        .unwrap_err();
        assert_eq!(error.rule, "JOINT_ALLOC.ASSIGNMENT_SHAPE");
        assert_eq!(format!("{source:?}"), source_before);
    }

    #[test]
    fn phi_sources_and_destinations_use_edge_locations_instead_of_fake_pressure() {
        const PHIS: u32 = 20;
        let mut values = VRegAllocator::new();
        let mut descriptors = Vec::new();
        let mut left_values = Vec::new();
        let mut right_values = Vec::new();
        let mut merged_values = Vec::new();
        for value in 0..PHIS {
            left_values.push(values.alloc());
            descriptors.push(SpillDesc::remat(u64::from(value) + 1));
        }
        for value in 0..PHIS {
            right_values.push(values.alloc());
            descriptors.push(SpillDesc::remat(u64::from(value) + 101));
        }
        for _ in 0..PHIS {
            merged_values.push(values.alloc());
            descriptors.push(SpillDesc::transient());
        }
        let condition = values.alloc();
        descriptors.push(SpillDesc::transient());
        let mut source = MFunction::new(values, descriptors);

        let mut entry = MBlock::new(BlockId(0));
        for (value, &destination) in left_values.iter().enumerate() {
            entry.push(MInst::LoadImm {
                dst: destination,
                value: value as u64 + 1,
            });
        }
        for (value, &destination) in right_values.iter().enumerate() {
            entry.push(MInst::LoadImm {
                dst: destination,
                value: value as u64 + 101,
            });
        }
        entry.push(MInst::Load {
            dst: condition,
            base: BaseReg::SimState,
            offset: 0,
            size: OpSize::S64,
        });
        entry.push(MInst::Branch {
            cond: condition,
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });
        let mut left = MBlock::new(BlockId(1));
        left.push(MInst::Jump { target: BlockId(3) });
        let mut right = MBlock::new(BlockId(2));
        right.push(MInst::Jump { target: BlockId(3) });
        let mut merge = MBlock::new(BlockId(3));
        for index in 0..PHIS as usize {
            merge.phis.push(PhiNode {
                dst: merged_values[index],
                sources: vec![
                    (BlockId(1), left_values[index]),
                    (BlockId(2), right_values[index]),
                ],
            });
        }
        let mut sum = merged_values[0];
        for &value in &merged_values[1..] {
            let destination = source.vregs.alloc();
            source.spill_descs.push(SpillDesc::transient());
            merge.push(MInst::Add {
                dst: destination,
                lhs: sum,
                rhs: value,
            });
            sum = destination;
        }
        merge.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 8,
            src: sum,
            size: OpSize::S64,
        });
        merge.push(MInst::Return);
        source.blocks = vec![entry, left, right, merge];

        let cfg = cfg::normalize(&mut source).unwrap();
        let graph = home_graph::build(&source, &cfg).unwrap();
        let plan = allocate_roots(&graph, &cfg, ALLOCATABLE_REGS).unwrap();
        let mut expanded = expand(&source, &cfg, &graph, &plan, ALLOCATABLE_REGS).unwrap();
        let allocation =
            allocate_with_splitting(&mut expanded, &graph, &cfg, ALLOCATABLE_REGS).unwrap();
        let lowered = lower(
            &source,
            &cfg,
            &graph,
            &expanded,
            &allocation,
            ALLOCATABLE_REGS,
        )
        .unwrap();

        assert!(
            lowered
                .assignment
                .phi_edge_locations
                .values()
                .any(|location| matches!(location, EdgeLocation::Immediate(_)))
        );
        assert!(!lowered.assignment.edge_spill_slots.is_empty());
        let stats = lowered.ssa_destruction.stats();
        assert_eq!(stats.edges, 2);
        assert_eq!(stats.rows, PHIS as usize * 2);
    }
}
