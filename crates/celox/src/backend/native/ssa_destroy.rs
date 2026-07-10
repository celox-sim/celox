//! Verified destruction of MIR SSA phi nodes into edge-local parallel copies.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use super::mir::{BlockId, MFunction, VReg};
use super::regalloc::assignment::{AssignmentMap, EdgeLocation, PhysReg};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum ParallelCopyDestination {
    Register(PhysReg),
    Stack(i32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum ParallelCopySource {
    Register(PhysReg),
    Stack(i32),
    Immediate(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ParallelCopy {
    pub phi_destination: VReg,
    pub source_value: VReg,
    pub destination: ParallelCopyDestination,
    pub source: ParallelCopySource,
}

impl ParallelCopy {
    pub(crate) fn is_identity(self) -> bool {
        matches!(
            (self.destination, self.source),
            (
                ParallelCopyDestination::Register(destination),
                ParallelCopySource::Register(source)
            ) if destination == source
        ) || matches!(
            (self.destination, self.source),
            (
                ParallelCopyDestination::Stack(destination),
                ParallelCopySource::Stack(source)
            ) if destination == source
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EdgeCopyPlan {
    pub predecessor: BlockId,
    pub successor: BlockId,
    pub rows: Vec<ParallelCopy>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct SsaDestructionPlan {
    edges: BTreeMap<(BlockId, BlockId), EdgeCopyPlan>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SsaDestructionError {
    pub rule: &'static str,
    pub predecessor: Option<BlockId>,
    pub successor: Option<BlockId>,
    pub phi_destination: Option<VReg>,
    pub source_value: Option<VReg>,
    pub message: String,
}

impl SsaDestructionError {
    fn new(rule: &'static str, message: impl Into<String>) -> Self {
        Self {
            rule,
            predecessor: None,
            successor: None,
            phi_destination: None,
            source_value: None,
            message: message.into(),
        }
    }

    fn edge(mut self, predecessor: BlockId, successor: BlockId) -> Self {
        self.predecessor = Some(predecessor);
        self.successor = Some(successor);
        self
    }

    fn at_successor(mut self, successor: BlockId) -> Self {
        self.successor = Some(successor);
        self
    }

    fn values(mut self, destination: VReg, source: Option<VReg>) -> Self {
        self.phi_destination = Some(destination);
        self.source_value = source;
        self
    }
}

impl fmt::Display for SsaDestructionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "SSA destruction [{}]", self.rule)?;
        if let (Some(predecessor), Some(successor)) = (self.predecessor, self.successor) {
            write!(formatter, " on {predecessor} -> {successor}")?;
        } else if let Some(successor) = self.successor {
            write!(formatter, " at {successor}")?;
        }
        if let Some(destination) = self.phi_destination {
            write!(formatter, " destination={destination}")?;
        }
        if let Some(source) = self.source_value {
            write!(formatter, " source={source}")?;
        }
        write!(formatter, ": {}", self.message)
    }
}

impl std::error::Error for SsaDestructionError {}

impl SsaDestructionPlan {
    /// Resolve every semantic phi row to its source and destination location.
    /// Identity rows remain in the plan so the verifier can prove completeness;
    /// lowering alone is allowed to elide them.
    pub(crate) fn build(
        func: &MFunction,
        assignment: &AssignmentMap,
    ) -> Result<Self, SsaDestructionError> {
        let predecessors = cfg_predecessors(func)?;
        let mut edges = BTreeMap::<(BlockId, BlockId), EdgeCopyPlan>::new();

        for successor in &func.blocks {
            if successor.phis.is_empty() {
                continue;
            }
            let incoming = &predecessors[&successor.id];
            if incoming.is_empty() {
                return Err(SsaDestructionError::new(
                    "SSA_DEST.PHI_WITHOUT_PREDECESSOR",
                    "block with phi nodes has no CFG predecessor",
                )
                .at_successor(successor.id));
            }
            for &predecessor in incoming {
                let mut rows = Vec::with_capacity(successor.phis.len());
                for phi in &successor.phis {
                    let source =
                        unique_phi_source(phi.dst, &phi.sources, predecessor, successor.id)?;
                    rows.push(ParallelCopy {
                        phi_destination: phi.dst,
                        source_value: source,
                        destination: destination_location(
                            assignment,
                            predecessor,
                            successor.id,
                            phi.dst,
                        )?,
                        source: source_location(
                            assignment,
                            predecessor,
                            successor.id,
                            phi.dst,
                            source,
                        )?,
                    });
                }
                edges.insert(
                    (predecessor, successor.id),
                    EdgeCopyPlan {
                        predecessor,
                        successor: successor.id,
                        rows,
                    },
                );
            }
            for phi in &successor.phis {
                for &(predecessor, source) in &phi.sources {
                    if !incoming.contains(&predecessor) {
                        return Err(SsaDestructionError::new(
                            "SSA_DEST.NON_CFG_PHI_SOURCE",
                            "phi row names a predecessor which has no CFG edge to the block",
                        )
                        .edge(predecessor, successor.id)
                        .values(phi.dst, Some(source)));
                    }
                }
            }
        }

        Ok(Self { edges })
    }

    /// Independently compare a materialized plan with MIR phi semantics and the
    /// completed assignment before any machine code is emitted.
    pub(crate) fn verify(
        &self,
        func: &MFunction,
        assignment: &AssignmentMap,
        spill_frame_size: u32,
    ) -> Result<(), SsaDestructionError> {
        let predecessors = cfg_predecessors(func)?;
        let mut expected_edges = BTreeSet::<(BlockId, BlockId)>::new();

        for successor in &func.blocks {
            if successor.phis.is_empty() {
                continue;
            }
            let incoming = &predecessors[&successor.id];
            if incoming.is_empty() {
                return Err(SsaDestructionError::new(
                    "SSA_DEST.PHI_WITHOUT_PREDECESSOR",
                    "block with phi nodes has no CFG predecessor",
                )
                .at_successor(successor.id));
            }
            for phi in &successor.phis {
                for &(predecessor, source) in &phi.sources {
                    if !incoming.contains(&predecessor) {
                        return Err(SsaDestructionError::new(
                            "SSA_DEST.NON_CFG_PHI_SOURCE",
                            "phi row names a predecessor which has no CFG edge to the block",
                        )
                        .edge(predecessor, successor.id)
                        .values(phi.dst, Some(source)));
                    }
                }
            }
            for &predecessor in incoming {
                expected_edges.insert((predecessor, successor.id));
                let Some(edge) = self.edges.get(&(predecessor, successor.id)) else {
                    return Err(SsaDestructionError::new(
                        "SSA_DEST.MISSING_EDGE_PLAN",
                        "phi-bearing CFG edge has no parallel-copy plan",
                    )
                    .edge(predecessor, successor.id));
                };
                if edge.predecessor != predecessor || edge.successor != successor.id {
                    return Err(SsaDestructionError::new(
                        "SSA_DEST.EDGE_KEY_MISMATCH",
                        "parallel-copy edge payload disagrees with its plan key",
                    )
                    .edge(predecessor, successor.id));
                }
                if edge.rows.len() != successor.phis.len() {
                    return Err(SsaDestructionError::new(
                        "SSA_DEST.INCOMPLETE_EDGE_ROWS",
                        format!(
                            "plan has {} rows but successor has {} phi nodes",
                            edge.rows.len(),
                            successor.phis.len()
                        ),
                    )
                    .edge(predecessor, successor.id));
                }

                let mut rows_by_destination = BTreeMap::<VReg, &ParallelCopy>::new();
                let mut physical_destinations = BTreeMap::<ParallelCopyDestination, VReg>::new();
                for row in &edge.rows {
                    if rows_by_destination
                        .insert(row.phi_destination, row)
                        .is_some()
                    {
                        return Err(SsaDestructionError::new(
                            "SSA_DEST.DUPLICATE_PHI_ROW",
                            "edge plan contains the same phi destination more than once",
                        )
                        .edge(predecessor, successor.id)
                        .values(row.phi_destination, Some(row.source_value)));
                    }
                    if let Some(other) =
                        physical_destinations.insert(row.destination, row.phi_destination)
                    {
                        return Err(SsaDestructionError::new(
                            "SSA_DEST.NON_UNIQUE_DESTINATION",
                            format!(
                                "phi destinations {other} and {} both write {:?}",
                                row.phi_destination, row.destination
                            ),
                        )
                        .edge(predecessor, successor.id)
                        .values(row.phi_destination, Some(row.source_value)));
                    }
                }

                for phi in &successor.phis {
                    let source =
                        unique_phi_source(phi.dst, &phi.sources, predecessor, successor.id)?;
                    let Some(row) = rows_by_destination.get(&phi.dst).copied() else {
                        return Err(SsaDestructionError::new(
                            "SSA_DEST.MISSING_PHI_ROW",
                            "edge plan omits a successor phi destination",
                        )
                        .edge(predecessor, successor.id)
                        .values(phi.dst, Some(source)));
                    };
                    let expected_destination =
                        destination_location(assignment, predecessor, successor.id, phi.dst)?;
                    let expected_source =
                        source_location(assignment, predecessor, successor.id, phi.dst, source)?;
                    if row.source_value != source
                        || row.destination != expected_destination
                        || row.source != expected_source
                    {
                        return Err(SsaDestructionError::new(
                            "SSA_DEST.LOCATION_MISMATCH",
                            format!(
                                "planned {:?} <- {:?}, expected {:?} <- {:?}",
                                row.destination, row.source, expected_destination, expected_source
                            ),
                        )
                        .edge(predecessor, successor.id)
                        .values(phi.dst, Some(source)));
                    }
                }

                verify_stack_assumptions(edge, spill_frame_size)?;
            }
        }

        if let Some(&(predecessor, successor)) = self
            .edges
            .keys()
            .find(|edge| !expected_edges.contains(edge))
        {
            return Err(SsaDestructionError::new(
                "SSA_DEST.EXTRA_EDGE_PLAN",
                "parallel-copy plan names an edge without successor phi nodes",
            )
            .edge(predecessor, successor));
        }
        Ok(())
    }

    pub(crate) fn edge(&self, predecessor: BlockId, successor: BlockId) -> Option<&EdgeCopyPlan> {
        self.edges.get(&(predecessor, successor))
    }

    pub(crate) fn edges(&self) -> impl Iterator<Item = &EdgeCopyPlan> {
        self.edges.values()
    }
}

fn cfg_predecessors(
    func: &MFunction,
) -> Result<BTreeMap<BlockId, BTreeSet<BlockId>>, SsaDestructionError> {
    let mut predecessors = BTreeMap::<BlockId, BTreeSet<BlockId>>::new();
    for block in &func.blocks {
        if predecessors.insert(block.id, BTreeSet::new()).is_some() {
            return Err(SsaDestructionError::new(
                "SSA_DEST.DUPLICATE_BLOCK",
                format!("block {} occurs more than once", block.id),
            ));
        }
    }
    for block in &func.blocks {
        for successor in block.successors() {
            let Some(incoming) = predecessors.get_mut(&successor) else {
                return Err(SsaDestructionError::new(
                    "SSA_DEST.MISSING_CFG_TARGET",
                    format!("{} targets missing block {successor}", block.id),
                )
                .edge(block.id, successor));
            };
            incoming.insert(block.id);
        }
    }
    Ok(predecessors)
}

fn unique_phi_source(
    destination: VReg,
    sources: &[(BlockId, VReg)],
    predecessor: BlockId,
    successor: BlockId,
) -> Result<VReg, SsaDestructionError> {
    let mut matching = sources
        .iter()
        .filter(|(source_predecessor, _)| *source_predecessor == predecessor)
        .map(|(_, source)| *source);
    let Some(source) = matching.next() else {
        return Err(SsaDestructionError::new(
            "SSA_DEST.MISSING_PHI_SOURCE",
            "phi has no source for this CFG predecessor",
        )
        .edge(predecessor, successor)
        .values(destination, None));
    };
    if matching.next().is_some() {
        return Err(SsaDestructionError::new(
            "SSA_DEST.DUPLICATE_PHI_SOURCE",
            "phi has more than one source for this CFG predecessor",
        )
        .edge(predecessor, successor)
        .values(destination, Some(source)));
    }
    Ok(source)
}

fn destination_location(
    assignment: &AssignmentMap,
    predecessor: BlockId,
    successor: BlockId,
    destination: VReg,
) -> Result<ParallelCopyDestination, SsaDestructionError> {
    if let Some(slot) = assignment.edge_spill_slot(destination) {
        return Ok(ParallelCopyDestination::Stack(slot));
    }
    if let Some(register) = assignment.get(destination) {
        return Ok(ParallelCopyDestination::Register(register));
    }
    Err(SsaDestructionError::new(
        "SSA_DEST.DESTINATION_LOCATION",
        "phi destination has neither a physical register nor a stack slot",
    )
    .edge(predecessor, successor)
    .values(destination, None))
}

fn source_location(
    assignment: &AssignmentMap,
    predecessor: BlockId,
    successor: BlockId,
    destination: VReg,
    source: VReg,
) -> Result<ParallelCopySource, SsaDestructionError> {
    if let Some(location) = assignment.edge_location(predecessor, source) {
        return Ok(match location {
            EdgeLocation::Register(register) => ParallelCopySource::Register(register),
            EdgeLocation::Stack(slot) => ParallelCopySource::Stack(slot),
            EdgeLocation::Immediate(value) => ParallelCopySource::Immediate(value),
        });
    }
    if let Some(slot) = assignment.edge_spill_slot(source) {
        return Ok(ParallelCopySource::Stack(slot));
    }
    if let Some(register) = assignment.get(source) {
        return Ok(ParallelCopySource::Register(register));
    }
    Err(SsaDestructionError::new(
        "SSA_DEST.SOURCE_LOCATION",
        "phi source has neither an edge location, physical register, nor stack slot",
    )
    .edge(predecessor, successor)
    .values(destination, Some(source)))
}

fn verify_stack_assumptions(
    edge: &EdgeCopyPlan,
    spill_frame_size: u32,
) -> Result<(), SsaDestructionError> {
    let effective = edge
        .rows
        .iter()
        .copied()
        .filter(|row| !row.is_identity())
        .collect::<Vec<_>>();
    for row in &effective {
        if let ParallelCopyDestination::Stack(slot) = row.destination {
            verify_stack_slot(edge, row, slot, spill_frame_size)?;
        }
        if let ParallelCopySource::Stack(slot) = row.source {
            verify_stack_slot(edge, row, slot, spill_frame_size)?;
        }
    }

    if let [row] = effective.as_slice() {
        match (row.destination, row.source) {
            (ParallelCopyDestination::Stack(destination), ParallelCopySource::Stack(source)) => {
                checked_temporary_offset(edge, row, source, 8)?;
                checked_temporary_offset(edge, row, destination, 8)?;
            }
            (ParallelCopyDestination::Stack(destination), ParallelCopySource::Immediate(_)) => {
                checked_temporary_offset(edge, row, destination, 8)?
            }
            _ => {}
        }
    } else {
        for (depth, row) in effective.iter().enumerate() {
            let adjustment = i32::try_from(depth)
                .ok()
                .and_then(|depth| depth.checked_mul(8))
                .ok_or_else(|| {
                    SsaDestructionError::new(
                        "SSA_DEST.TEMPORARY_STACK_DEPTH",
                        "parallel-copy temporary stack depth exceeds i32 addressing",
                    )
                    .edge(edge.predecessor, edge.successor)
                    .values(row.phi_destination, Some(row.source_value))
                })?;
            if let ParallelCopySource::Stack(slot) = row.source {
                checked_temporary_offset(edge, row, slot, adjustment)?;
            }
            if let ParallelCopyDestination::Stack(slot) = row.destination {
                checked_temporary_offset(edge, row, slot, adjustment)?;
            }
        }
    }
    Ok(())
}

fn verify_stack_slot(
    edge: &EdgeCopyPlan,
    row: &ParallelCopy,
    slot: i32,
    spill_frame_size: u32,
) -> Result<(), SsaDestructionError> {
    let valid = slot >= 0
        && slot % 8 == 0
        && u32::try_from(slot)
            .ok()
            .and_then(|slot| slot.checked_add(8))
            .is_some_and(|end| end <= spill_frame_size);
    if valid {
        return Ok(());
    }
    Err(SsaDestructionError::new(
        "SSA_DEST.STACK_SLOT",
        format!("stack slot {slot} is not an aligned qword inside {spill_frame_size} bytes"),
    )
    .edge(edge.predecessor, edge.successor)
    .values(row.phi_destination, Some(row.source_value)))
}

fn checked_temporary_offset(
    edge: &EdgeCopyPlan,
    row: &ParallelCopy,
    slot: i32,
    adjustment: i32,
) -> Result<(), SsaDestructionError> {
    if slot.checked_add(adjustment).is_some() {
        return Ok(());
    }
    Err(SsaDestructionError::new(
        "SSA_DEST.TEMPORARY_STACK_OFFSET",
        format!("stack slot {slot} overflows after temporary adjustment {adjustment}"),
    )
    .edge(edge.predecessor, edge.successor)
    .values(row.phi_destination, Some(row.source_value)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::native::emit::{self, EmitError};
    use crate::backend::native::mir::{
        BaseReg, MBlock, MInst, OpSize, PhiNode, SpillDesc, VRegAllocator,
    };

    fn one_edge_function(phi_sources: &[(VReg, VReg)]) -> MFunction {
        let max_vreg = phi_sources
            .iter()
            .flat_map(|(destination, source)| [destination.0, source.0])
            .max()
            .map_or(0, |maximum| maximum + 1);
        let mut vregs = VRegAllocator::new();
        let mut spill_descs = Vec::new();
        while vregs.count() < max_vreg {
            vregs.alloc();
            spill_descs.push(SpillDesc::transient());
        }
        let mut function = MFunction::new(vregs, spill_descs);
        let mut predecessor = MBlock::new(BlockId(0));
        predecessor.push(MInst::Jump { target: BlockId(1) });
        let mut successor = MBlock::new(BlockId(1));
        successor.phis = phi_sources
            .iter()
            .map(|&(destination, source)| PhiNode {
                dst: destination,
                sources: vec![(BlockId(0), source)],
            })
            .collect();
        successor.push(MInst::Return);
        function.push_block(predecessor);
        function.push_block(successor);
        function
    }

    #[test]
    fn missing_phi_source_location_is_a_structured_plan_error() {
        let function = one_edge_function(&[(VReg(1), VReg(0))]);
        let mut assignment = AssignmentMap::default();
        assignment.set(VReg(1), PhysReg::RAX);

        let error = SsaDestructionPlan::build(&function, &assignment).unwrap_err();

        assert_eq!(error.rule, "SSA_DEST.SOURCE_LOCATION");
        assert_eq!(error.predecessor, Some(BlockId(0)));
        assert_eq!(error.successor, Some(BlockId(1)));
        assert_eq!(error.phi_destination, Some(VReg(1)));
        assert_eq!(error.source_value, Some(VReg(0)));
    }

    #[test]
    fn missing_instruction_assignment_is_rejected_before_encoding() {
        let mut vregs = VRegAllocator::new();
        let value = vregs.alloc();
        let mut function = MFunction::new(vregs, vec![SpillDesc::transient()]);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: value,
            value: 1,
        });
        entry.push(MInst::Return);
        function.push_block(entry);

        let error = match emit::emit(&function, &AssignmentMap::default(), 0) {
            Ok(_) => panic!("missing assignment must be rejected before encoding"),
            Err(error) => error,
        };

        let EmitError::Input(error) = error else {
            panic!("expected emission-input error, got {error}");
        };
        assert_eq!(error.rule, "EMIT.ASSIGNMENT_COMPLETE");
        assert_eq!(error.block, Some(BlockId(0)));
        assert_eq!(error.instruction, Some(0));
        assert_eq!(error.value, Some(value));
    }

    #[test]
    fn unencodable_frame_size_is_rejected_before_encoding() {
        let mut function = MFunction::new(VRegAllocator::new(), Vec::new());
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::Return);
        function.push_block(entry);

        let error = match emit::emit(&function, &AssignmentMap::default(), i32::MAX as u32) {
            Ok(_) => panic!("unencodable frame must be rejected before encoding"),
            Err(error) => error,
        };

        let EmitError::Input(error) = error else {
            panic!("expected emission-input error, got {error}");
        };
        assert_eq!(error.rule, "EMIT.FRAME_SIZE_RANGE");
    }

    #[test]
    fn verifier_rejects_two_phi_rows_with_one_physical_destination() {
        let function = one_edge_function(&[(VReg(2), VReg(0)), (VReg(3), VReg(1))]);
        let mut assignment = AssignmentMap::default();
        assignment.set(VReg(0), PhysReg::RAX);
        assignment.set(VReg(1), PhysReg::RDX);
        assignment.set(VReg(2), PhysReg::RSI);
        assignment.set(VReg(3), PhysReg::RSI);

        let plan = SsaDestructionPlan::build(&function, &assignment).unwrap();
        let error = plan.verify(&function, &assignment, 0).unwrap_err();
        assert_eq!(error.rule, "SSA_DEST.NON_UNIQUE_DESTINATION");
        assert_eq!(error.predecessor, Some(BlockId(0)));
        assert_eq!(error.successor, Some(BlockId(1)));
    }

    #[test]
    fn verifier_rejects_an_incomplete_materialized_edge_plan() {
        let function = one_edge_function(&[(VReg(2), VReg(0)), (VReg(3), VReg(1))]);
        let mut assignment = AssignmentMap::default();
        assignment.set(VReg(0), PhysReg::RAX);
        assignment.set(VReg(1), PhysReg::RDX);
        assignment.set(VReg(2), PhysReg::RSI);
        assignment.set(VReg(3), PhysReg::RDI);

        let mut plan = SsaDestructionPlan::build(&function, &assignment).unwrap();
        plan.edges
            .get_mut(&(BlockId(0), BlockId(1)))
            .unwrap()
            .rows
            .pop();
        let error = plan.verify(&function, &assignment, 0).unwrap_err();
        assert_eq!(error.rule, "SSA_DEST.INCOMPLETE_EDGE_ROWS");
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn mixed_register_stack_immediate_cycles_preserve_parallel_semantics() {
        use crate::backend::native::jit_mem::JitCode;

        let mut vregs = VRegAllocator::new();
        let values = (0..15).map(|_| vregs.alloc()).collect::<Vec<_>>();
        let [
            a,
            b,
            stack_zero,
            stack_eight,
            immediate,
            dst_a,
            dst_b,
            dst_stack_reg,
            dst_stack_eight,
            dst_stack_zero,
            dst_immediate_reg,
            dst_immediate_stack,
            load_zero,
            load_eight,
            load_sixteen,
        ] = values.as_slice()
        else {
            unreachable!()
        };
        let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 15]);
        let mut predecessor = MBlock::new(BlockId(0));
        predecessor.push(MInst::LoadImm { dst: *a, value: 11 });
        predecessor.push(MInst::LoadImm { dst: *b, value: 22 });
        predecessor.push(MInst::LoadImm {
            dst: *stack_zero,
            value: 33,
        });
        predecessor.push(MInst::Store {
            base: BaseReg::StackFrame,
            offset: 0,
            src: *stack_zero,
            size: OpSize::S64,
        });
        predecessor.push(MInst::LoadImm {
            dst: *stack_eight,
            value: 44,
        });
        predecessor.push(MInst::Store {
            base: BaseReg::StackFrame,
            offset: 8,
            src: *stack_eight,
            size: OpSize::S64,
        });
        predecessor.push(MInst::LoadImm {
            dst: *immediate,
            value: 55,
        });
        predecessor.push(MInst::Jump { target: BlockId(1) });

        let mut successor = MBlock::new(BlockId(1));
        successor.phis = vec![
            PhiNode {
                dst: *dst_a,
                sources: vec![(BlockId(0), *a)],
            },
            PhiNode {
                dst: *dst_b,
                sources: vec![(BlockId(0), *b)],
            },
            PhiNode {
                dst: *dst_stack_reg,
                sources: vec![(BlockId(0), *stack_zero)],
            },
            PhiNode {
                dst: *dst_stack_eight,
                sources: vec![(BlockId(0), *stack_zero)],
            },
            PhiNode {
                dst: *dst_stack_zero,
                sources: vec![(BlockId(0), *stack_eight)],
            },
            PhiNode {
                dst: *dst_immediate_reg,
                sources: vec![(BlockId(0), *immediate)],
            },
            PhiNode {
                dst: *dst_immediate_stack,
                sources: vec![(BlockId(0), *immediate)],
            },
        ];
        for (offset, source) in [
            (0, *dst_a),
            (8, *dst_b),
            (16, *dst_stack_reg),
            (24, *dst_immediate_reg),
        ] {
            successor.push(MInst::Store {
                base: BaseReg::SimState,
                offset,
                src: source,
                size: OpSize::S64,
            });
        }
        for (stack_offset, state_offset, destination) in [
            (0, 32, *load_zero),
            (8, 40, *load_eight),
            (16, 48, *load_sixteen),
        ] {
            successor.push(MInst::Load {
                dst: destination,
                base: BaseReg::StackFrame,
                offset: stack_offset,
                size: OpSize::S64,
            });
            successor.push(MInst::Store {
                base: BaseReg::SimState,
                offset: state_offset,
                src: destination,
                size: OpSize::S64,
            });
        }
        successor.push(MInst::Return);
        function.push_block(predecessor);
        function.push_block(successor);

        let mut assignment = AssignmentMap::default();
        for (value, register) in [
            (*a, PhysReg::RAX),
            (*b, PhysReg::RDX),
            (*stack_zero, PhysReg::RSI),
            (*stack_eight, PhysReg::RDI),
            (*immediate, PhysReg::R8),
            (*dst_a, PhysReg::RDX),
            (*dst_b, PhysReg::RAX),
            (*dst_stack_reg, PhysReg::R9),
            (*dst_immediate_reg, PhysReg::R10),
            (*load_zero, PhysReg::R11),
            (*load_eight, PhysReg::R11),
            (*load_sixteen, PhysReg::R11),
        ] {
            assignment.set(value, register);
        }
        assignment.set_edge_location(BlockId(0), *stack_zero, EdgeLocation::Stack(0));
        assignment.set_edge_location(BlockId(0), *stack_eight, EdgeLocation::Stack(8));
        assignment.set_edge_location(BlockId(0), *immediate, EdgeLocation::Immediate(77));
        assignment.set_edge_spill_slot(*dst_stack_eight, 8);
        assignment.set_edge_spill_slot(*dst_stack_zero, 0);
        assignment.set_edge_spill_slot(*dst_immediate_stack, 16);

        let emitted = emit::emit(&function, &assignment, 24).unwrap();
        let jit = JitCode::new(&emitted.code).unwrap();
        let mut state = vec![0_u8; 56];
        assert_eq!(unsafe { jit.call(&mut state) }, 0);
        let actual = state
            .chunks_exact(8)
            .map(|bytes| u64::from_le_bytes(bytes.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(actual, [11, 22, 33, 77, 44, 33, 77]);
    }
}
