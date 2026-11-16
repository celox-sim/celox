//! Verified destruction of MIR SSA phi nodes into edge-local parallel copies.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use super::mir::{BlockId, MFunction, MInst, VReg};
use super::regalloc::assignment::{AssignmentMap, EdgeLocation, PhysReg, clobbers};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ParallelCopyDestination {
    Register(PhysReg),
    Stack(i32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ParallelCopySource {
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

/// A dependency-ordered lowering step for one edge's parallel assignment.
///
/// Two-register cycles use `SwapRegisters`. Longer register cycles and cycles
/// involving a stack location use `SaveTemporary`/`RestoreTemporary`; while
/// that temporary is live, it occupies one qword below the frame. On x86 a
/// register `xchg` is multiple uops, so K-1 exchanges are not competitive
/// with K+1 ordinary moves once K is at least three.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParallelCopyOperation {
    Move {
        destination: ParallelCopyDestination,
        source: ParallelCopySource,
    },
    /// Exchange two allocated registers for a two-cycle, without borrowing a
    /// register or touching the stack.
    SwapRegisters {
        left: PhysReg,
        right: PhysReg,
    },
    SaveTemporary(ParallelCopyDestination),
    RestoreTemporary(ParallelCopyDestination),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ParallelCopyWork {
    pub effective_copies: usize,
    pub direct_moves: usize,
    pub register_swaps: usize,
    pub cycle_breaks: usize,
    pub temporary_cycle_breaks: usize,
    pub ready_queue_pops: usize,
    pub dependency_releases: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeCopyPlan {
    pub predecessor: BlockId,
    pub successor: BlockId,
    pub(crate) rows: Vec<ParallelCopy>,
    pub operations: Vec<ParallelCopyOperation>,
    pub(crate) work: ParallelCopyWork,
}

impl EdgeCopyPlan {
    pub(crate) fn has_effective_copies(&self) -> bool {
        self.work.effective_copies != 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SsaDestructionPlan {
    edges: BTreeMap<(BlockId, BlockId), EdgeCopyPlan>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct SsaDestructionStats {
    pub edges: usize,
    pub rows: usize,
    pub identity_rows: usize,
    pub effective_copies: usize,
    pub identity_only_edges: usize,
    pub direct_moves: usize,
    pub register_swaps: usize,
    pub cycle_breaks: usize,
    pub temporary_cycle_breaks: usize,
    pub ready_queue_pops: usize,
    pub dependency_releases: usize,
    pub max_effective_copies_per_edge: usize,
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
    pub fn build(
        func: &MFunction,
        assignment: &AssignmentMap,
    ) -> Result<Self, SsaDestructionError> {
        let predecessors = cfg_predecessors(func)?;
        let exit_values = exit_register_values(func, assignment);
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
                    if assignment.is_semantic_phi_definition(phi.dst) {
                        continue;
                    }
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
                            exit_values.get(&predecessor),
                            predecessor,
                            successor.id,
                            phi.dst,
                            source,
                        )?,
                    });
                }
                let (operations, work) = resolve_parallel_copies(predecessor, successor.id, &rows)?;
                edges.insert(
                    (predecessor, successor.id),
                    EdgeCopyPlan {
                        predecessor,
                        successor: successor.id,
                        rows,
                        operations,
                        work,
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
    pub fn verify(
        &self,
        func: &MFunction,
        assignment: &AssignmentMap,
        spill_frame_size: u32,
    ) -> Result<(), SsaDestructionError> {
        let predecessors = cfg_predecessors(func)?;
        let exit_values = exit_register_values(func, assignment);
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
                let expected_rows = successor
                    .phis
                    .iter()
                    .filter(|phi| !assignment.is_semantic_phi_definition(phi.dst))
                    .count();
                if edge.rows.len() != expected_rows {
                    return Err(SsaDestructionError::new(
                        "SSA_DEST.INCOMPLETE_EDGE_ROWS",
                        format!(
                            "plan has {} rows but successor has {expected_rows} located phi nodes",
                            edge.rows.len(),
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
                    if assignment.is_semantic_phi_definition(phi.dst) {
                        if rows_by_destination.contains_key(&phi.dst) {
                            return Err(SsaDestructionError::new(
                                "SSA_DEST.SEMANTIC_PHI_ROW",
                                "semantic-only phi unexpectedly owns a machine copy row",
                            )
                            .edge(predecessor, successor.id)
                            .values(phi.dst, Some(source)));
                        }
                        continue;
                    }
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
                    let expected_source = source_location(
                        assignment,
                        exit_values.get(&predecessor),
                        predecessor,
                        successor.id,
                        phi.dst,
                        source,
                    )?;
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

                let (expected_operations, expected_work) =
                    resolve_parallel_copies(predecessor, successor.id, &edge.rows)?;
                if edge.operations != expected_operations || edge.work != expected_work {
                    return Err(SsaDestructionError::new(
                        "SSA_DEST.RESOLUTION_MISMATCH",
                        "parallel-copy resolver output is stale or inconsistent with its rows",
                    )
                    .edge(predecessor, successor.id));
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

    pub fn edge(&self, predecessor: BlockId, successor: BlockId) -> Option<&EdgeCopyPlan> {
        self.edges.get(&(predecessor, successor))
    }

    pub(crate) fn edges(&self) -> impl Iterator<Item = &EdgeCopyPlan> {
        self.edges.values()
    }

    pub(crate) fn stats(&self) -> SsaDestructionStats {
        let mut stats = SsaDestructionStats {
            edges: self.edges.len(),
            ..SsaDestructionStats::default()
        };
        for edge in self.edges.values() {
            stats.rows += edge.rows.len();
            stats.effective_copies += edge.work.effective_copies;
            stats.identity_rows += edge.rows.len() - edge.work.effective_copies;
            stats.identity_only_edges += usize::from(!edge.has_effective_copies());
            stats.direct_moves += edge.work.direct_moves;
            stats.register_swaps += edge.work.register_swaps;
            stats.cycle_breaks += edge.work.cycle_breaks;
            stats.temporary_cycle_breaks += edge.work.temporary_cycle_breaks;
            stats.ready_queue_pops += edge.work.ready_queue_pops;
            stats.dependency_releases += edge.work.dependency_releases;
            stats.max_effective_copies_per_edge = stats
                .max_effective_copies_per_edge
                .max(edge.work.effective_copies);
        }
        stats
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
    exit_values: Option<&BTreeMap<PhysReg, VReg>>,
    predecessor: BlockId,
    successor: BlockId,
    destination: VReg,
    source: VReg,
) -> Result<ParallelCopySource, SsaDestructionError> {
    if let Some(location) =
        assignment.resolved_phi_source_location(predecessor, successor, destination, source)
    {
        if let EdgeLocation::Register(source_register) = location
            && let Some(destination_register) = assignment.get(destination)
            && exit_values
                .and_then(|values| values.get(&source_register))
                .is_some_and(|source_value| {
                    exit_values.and_then(|values| values.get(&destination_register))
                        == Some(source_value)
                })
        {
            return Ok(ParallelCopySource::Register(destination_register));
        }
        return Ok(match location {
            EdgeLocation::Register(register) => ParallelCopySource::Register(register),
            EdgeLocation::Stack(slot) => ParallelCopySource::Stack(slot),
            EdgeLocation::Immediate(value) => ParallelCopySource::Immediate(value),
        });
    }
    Err(SsaDestructionError::new(
        "SSA_DEST.SOURCE_LOCATION",
        "phi source has neither an edge location, physical register, nor stack slot",
    )
    .edge(predecessor, successor)
    .values(destination, Some(source)))
}

/// Track exact register-value aliases at each predecessor exit.
///
/// Register allocation gives every SSA value one canonical register, but an
/// explicit copy leaves the same value available in both its source and
/// destination registers until either register is overwritten. Phi lowering
/// may use either occurrence. Retaining this small physical state prevents a
/// parallel-copy cycle from exchanging registers which already contain equal
/// values. The scan is linear in MIR size and stores at most one VReg token per
/// physical register and block.
fn exit_register_values(
    func: &MFunction,
    assignment: &AssignmentMap,
) -> BTreeMap<BlockId, BTreeMap<PhysReg, VReg>> {
    let mut result = BTreeMap::new();
    for block in &func.blocks {
        let mut values = BTreeMap::<PhysReg, VReg>::new();
        for inst in &block.insts {
            for used in inst.uses() {
                if let Some(register) = assignment.get(used) {
                    values.entry(register).or_insert(used);
                }
            }

            let copied_value = match inst {
                MInst::Mov { src, .. } => assignment
                    .get(*src)
                    .and_then(|register| values.get(&register).copied())
                    .or(Some(*src)),
                _ => None,
            };
            for &register in clobbers(inst) {
                values.remove(&register);
            }
            if let Some(definition) = inst.def()
                && let Some(register) = assignment.get(definition)
            {
                values.insert(register, copied_value.unwrap_or(definition));
            }
        }
        result.insert(block.id, values);
    }
    result
}

/// Schedule one parallel assignment without snapshotting acyclic rows.
///
/// A row is ready once overwriting its destination cannot destroy a source of
/// another pending row.  If no row is ready, unique destinations imply that
/// the remaining dependency graph contains a cycle.  Saving one destination
/// breaks that cycle; all other rows continue to use direct moves.
fn resolve_parallel_copies(
    predecessor: BlockId,
    successor: BlockId,
    rows: &[ParallelCopy],
) -> Result<(Vec<ParallelCopyOperation>, ParallelCopyWork), SsaDestructionError> {
    let mut destination_owner = BTreeMap::<ParallelCopyDestination, &ParallelCopy>::new();
    for row in rows {
        if let Some(other) = destination_owner.insert(row.destination, row) {
            return Err(SsaDestructionError::new(
                "SSA_DEST.NON_UNIQUE_DESTINATION",
                format!(
                    "phi destinations {} and {} both write {:?}",
                    other.phi_destination, row.phi_destination, row.destination
                ),
            )
            .edge(predecessor, successor)
            .values(row.phi_destination, Some(row.source_value)));
        }
    }

    use celox_backend_common::regalloc as common;

    let common_destination = |destination| match destination {
        ParallelCopyDestination::Register(register) => common::CopyDestination::Register(register),
        ParallelCopyDestination::Stack(offset) => common::CopyDestination::Stack(offset),
    };
    let common_source = |source| match source {
        ParallelCopySource::Register(register) => common::CopySource::Register(register),
        ParallelCopySource::Stack(offset) => common::CopySource::Stack(offset),
        ParallelCopySource::Immediate(value) => common::CopySource::Immediate(value),
    };
    let common_rows = rows
        .iter()
        .map(|row| common::ParallelCopy {
            destination: common_destination(row.destination),
            source: common_source(row.source),
        })
        .collect::<Vec<_>>();
    let resolution = common::resolve_parallel_copies(&common_rows).map_err(|error| {
        SsaDestructionError::new(error.rule, error.message).edge(predecessor, successor)
    })?;
    let local_destination = |destination| match destination {
        common::CopyDestination::Register(register) => ParallelCopyDestination::Register(register),
        common::CopyDestination::Stack(offset) => ParallelCopyDestination::Stack(offset),
    };
    let local_source = |source| match source {
        common::CopySource::Register(register) => ParallelCopySource::Register(register),
        common::CopySource::Stack(offset) => ParallelCopySource::Stack(offset),
        common::CopySource::Immediate(value) => ParallelCopySource::Immediate(value),
    };
    let operations = resolution
        .operations
        .into_iter()
        .map(|operation| match operation {
            common::CopyOperation::Move {
                destination,
                source,
            } => ParallelCopyOperation::Move {
                destination: local_destination(destination),
                source: local_source(source),
            },
            common::CopyOperation::SwapRegisters { left, right } => {
                ParallelCopyOperation::SwapRegisters { left, right }
            }
            common::CopyOperation::SaveTemporary(destination) => {
                ParallelCopyOperation::SaveTemporary(local_destination(destination))
            }
            common::CopyOperation::RestoreTemporary(destination) => {
                ParallelCopyOperation::RestoreTemporary(local_destination(destination))
            }
        })
        .collect();
    let common::CopyResolutionWork {
        effective_copies,
        direct_moves,
        register_swaps,
        cycle_breaks,
        temporary_cycle_breaks,
        ready_queue_pops,
        dependency_releases,
    } = resolution.work;
    let work = ParallelCopyWork {
        effective_copies,
        direct_moves,
        register_swaps,
        cycle_breaks,
        temporary_cycle_breaks,
        ready_queue_pops,
        dependency_releases,
    };
    Ok((operations, work))
}

fn verify_stack_assumptions(
    edge: &EdgeCopyPlan,
    spill_frame_size: u32,
) -> Result<(), SsaDestructionError> {
    // Validate semantic locations even for identity rows.  Eliding a machine
    // move must not make an out-of-frame assignment appear well formed.
    for row in &edge.rows {
        if let ParallelCopyDestination::Stack(slot) = row.destination {
            verify_stack_slot(edge, row, slot, spill_frame_size)?;
        }
        if let ParallelCopySource::Stack(slot) = row.source {
            verify_stack_slot(edge, row, slot, spill_frame_size)?;
        }
    }

    let mut temporary_live = false;
    for operation in &edge.operations {
        match *operation {
            ParallelCopyOperation::SwapRegisters { .. } => {}
            ParallelCopyOperation::SaveTemporary(location) => {
                if temporary_live {
                    return Err(SsaDestructionError::new(
                        "SSA_DEST.TEMPORARY_NESTING",
                        "parallel-copy schedule nests temporary saves",
                    )
                    .edge(edge.predecessor, edge.successor));
                }
                if let ParallelCopyDestination::Stack(slot) = location {
                    checked_operation_offset(edge, slot, 0)?;
                }
                temporary_live = true;
            }
            ParallelCopyOperation::Move {
                destination,
                source,
            } => {
                let temporary_adjustment = if temporary_live { 8 } else { 0 };
                if let ParallelCopySource::Stack(slot) = source {
                    checked_operation_offset(edge, slot, temporary_adjustment)?;
                }
                if let ParallelCopyDestination::Stack(slot) = destination {
                    checked_operation_offset(edge, slot, temporary_adjustment)?;
                    if matches!(source, ParallelCopySource::Immediate(_)) {
                        checked_operation_offset(edge, slot, temporary_adjustment + 4)?;
                    }
                }
            }
            ParallelCopyOperation::RestoreTemporary(location) => {
                if !temporary_live {
                    return Err(SsaDestructionError::new(
                        "SSA_DEST.TEMPORARY_NESTING",
                        "parallel-copy schedule restores an inactive temporary",
                    )
                    .edge(edge.predecessor, edge.successor));
                }
                if let ParallelCopyDestination::Stack(slot) = location {
                    // POP computes an RSP-based memory destination after
                    // incrementing RSP, so the encoded displacement is the
                    // ordinary frame slot with no +8 adjustment.
                    checked_operation_offset(edge, slot, 0)?;
                }
                temporary_live = false;
            }
        }
    }
    if temporary_live {
        return Err(SsaDestructionError::new(
            "SSA_DEST.TEMPORARY_NESTING",
            "parallel-copy schedule leaves a temporary live",
        )
        .edge(edge.predecessor, edge.successor));
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

fn checked_operation_offset(
    edge: &EdgeCopyPlan,
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
    .edge(edge.predecessor, edge.successor))
}

#[cfg(test)]
mod tests;
