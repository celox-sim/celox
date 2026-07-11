//! Verified destruction of MIR SSA phi nodes into edge-local parallel copies.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
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

/// A dependency-ordered lowering step for one edge's parallel assignment.
///
/// Register-only cycles use `SwapRegisters`.  For cycles involving a stack
/// location, `SaveTemporary`/`RestoreTemporary` delimit one cycle at a time;
/// while that temporary is live, it occupies one qword below the frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParallelCopyOperation {
    Move {
        destination: ParallelCopyDestination,
        source: ParallelCopySource,
    },
    /// Exchange two allocated registers.  A register-only cycle of length K
    /// is lowered to K-1 exchanges, without borrowing a register or touching
    /// the stack.
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
pub(crate) struct EdgeCopyPlan {
    pub predecessor: BlockId,
    pub successor: BlockId,
    pub rows: Vec<ParallelCopy>,
    pub operations: Vec<ParallelCopyOperation>,
    pub work: ParallelCopyWork,
}

impl EdgeCopyPlan {
    pub(crate) fn has_effective_copies(&self) -> bool {
        self.work.effective_copies != 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct SsaDestructionPlan {
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

    pub(crate) fn edge(&self, predecessor: BlockId, successor: BlockId) -> Option<&EdgeCopyPlan> {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingSource {
    Value(ParallelCopySource),
    Temporary,
}

#[derive(Debug, Clone, Copy)]
struct PendingCopy {
    phi_destination: VReg,
    source_value: VReg,
    destination: ParallelCopyDestination,
    source: PendingSource,
    pending: bool,
}

fn source_as_destination(source: ParallelCopySource) -> Option<ParallelCopyDestination> {
    match source {
        ParallelCopySource::Register(register) => Some(ParallelCopyDestination::Register(register)),
        ParallelCopySource::Stack(slot) => Some(ParallelCopyDestination::Stack(slot)),
        ParallelCopySource::Immediate(_) => None,
    }
}

/// Return a register-only cycle starting at `start`, together with the
/// exchanges which realize its parallel assignment.  For
/// `A <- B, B <- C, C <- A`, the result is `xchg A, B; xchg B, C`.
fn register_cycle(
    copies: &[PendingCopy],
    destination_index: &BTreeMap<ParallelCopyDestination, usize>,
    start: usize,
) -> Option<(Vec<usize>, Vec<(PhysReg, PhysReg)>)> {
    let ParallelCopyDestination::Register(start_register) = copies.get(start)?.destination else {
        return None;
    };
    let mut current = start_register;
    let mut members = Vec::new();
    let mut swaps = Vec::new();
    let mut visited = BTreeSet::new();
    loop {
        if !visited.insert(current) {
            return None;
        }
        let destination = ParallelCopyDestination::Register(current);
        let &index = destination_index.get(&destination)?;
        let copy = copies.get(index)?;
        if !copy.pending {
            return None;
        }
        let PendingSource::Value(ParallelCopySource::Register(source)) = copy.source else {
            return None;
        };
        members.push(index);
        if source == start_register {
            return (members.len() >= 2).then_some((members, swaps));
        }
        swaps.push((current, source));
        current = source;
    }
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

    let mut copies = rows
        .iter()
        .copied()
        .filter(|row| !row.is_identity())
        .map(|row| PendingCopy {
            phi_destination: row.phi_destination,
            source_value: row.source_value,
            destination: row.destination,
            source: PendingSource::Value(row.source),
            pending: true,
        })
        .collect::<Vec<_>>();
    let mut work = ParallelCopyWork {
        effective_copies: copies.len(),
        ..ParallelCopyWork::default()
    };
    if copies.is_empty() {
        return Ok((Vec::new(), work));
    }

    let destination_index = copies
        .iter()
        .enumerate()
        .map(|(index, copy)| (copy.destination, index))
        .collect::<BTreeMap<_, _>>();
    let mut readers = BTreeMap::<ParallelCopyDestination, BTreeSet<usize>>::new();
    for (index, copy) in copies.iter().enumerate() {
        let PendingSource::Value(source) = copy.source else {
            continue;
        };
        if let Some(location) = source_as_destination(source) {
            readers.entry(location).or_default().insert(index);
        }
    }

    let mut ready = VecDeque::new();
    let mut queued = vec![false; copies.len()];
    for (index, copy) in copies.iter().enumerate() {
        if !readers.contains_key(&copy.destination) {
            ready.push_back(index);
            queued[index] = true;
        }
    }

    let mut operations = Vec::with_capacity(copies.len());
    let mut remaining = copies.len();
    let mut temporary_live = false;
    // Each completed cycle contains the lowest still-pending row selected
    // below. Resume after it so disjoint cycles do not repeatedly rescan an
    // already completed prefix.
    let mut cycle_search_start = 0usize;
    while remaining != 0 {
        while let Some(index) = ready.pop_front() {
            queued[index] = false;
            if !copies[index].pending {
                continue;
            }
            work.ready_queue_pops += 1;
            match copies[index].source {
                PendingSource::Value(source) => {
                    operations.push(ParallelCopyOperation::Move {
                        destination: copies[index].destination,
                        source,
                    });
                    work.direct_moves += 1;
                    if let Some(location) = source_as_destination(source) {
                        let released = if let Some(location_readers) = readers.get_mut(&location) {
                            location_readers.remove(&index);
                            location_readers.is_empty()
                        } else {
                            false
                        };
                        if released {
                            readers.remove(&location);
                            work.dependency_releases += 1;
                            if let Some(&writer) = destination_index.get(&location)
                                && copies[writer].pending
                                && !queued[writer]
                            {
                                ready.push_back(writer);
                                queued[writer] = true;
                            }
                        }
                    }
                }
                PendingSource::Temporary => {
                    if !temporary_live {
                        return Err(SsaDestructionError::new(
                            "SSA_DEST.RESOLVER_TEMPORARY_STATE",
                            "parallel-copy resolver attempted to restore an inactive temporary",
                        )
                        .edge(predecessor, successor)
                        .values(
                            copies[index].phi_destination,
                            Some(copies[index].source_value),
                        ));
                    }
                    operations.push(ParallelCopyOperation::RestoreTemporary(
                        copies[index].destination,
                    ));
                    temporary_live = false;
                }
            }
            copies[index].pending = false;
            remaining -= 1;
        }

        if remaining == 0 {
            break;
        }
        if temporary_live {
            let Some(copy) = copies.iter().find(|copy| copy.pending).copied() else {
                return Err(SsaDestructionError::new(
                    "SSA_DEST.RESOLVER_TEMPORARY_STATE",
                    "parallel-copy resolver lost its pending temporary consumer",
                )
                .edge(predecessor, successor));
            };
            return Err(SsaDestructionError::new(
                "SSA_DEST.RESOLVER_TEMPORARY_STATE",
                "parallel-copy resolver stalled while a temporary was live",
            )
            .edge(predecessor, successor)
            .values(copy.phi_destination, Some(copy.source_value)));
        }

        let Some(cycle) = copies
            .iter()
            .enumerate()
            .skip(cycle_search_start)
            .find_map(|(index, copy)| copy.pending.then_some(index))
        else {
            return Err(SsaDestructionError::new(
                "SSA_DEST.RESOLVER_STATE",
                "parallel-copy resolver has a nonzero pending count without a pending row",
            )
            .edge(predecessor, successor));
        };
        cycle_search_start = cycle + 1;

        // A pure register permutation needs no temporary.  Decomposing each
        // cycle into transpositions is both the standard Perm lowering and a
        // strict improvement over the old push/move/pop sequence.
        if let Some((members, swaps)) = register_cycle(&copies, &destination_index, cycle) {
            for (left, right) in swaps.iter().copied() {
                operations.push(ParallelCopyOperation::SwapRegisters { left, right });
            }
            for &member in &members {
                readers.remove(&copies[member].destination);
                copies[member].pending = false;
                queued[member] = false;
            }
            remaining -= members.len();
            work.register_swaps += swaps.len();
            work.cycle_breaks += 1;
            work.dependency_releases += members.len();
            continue;
        }

        let saved = copies[cycle].destination;
        let saved_readers = readers.remove(&saved).unwrap_or_default();
        if saved_readers.len() != 1 {
            return Err(SsaDestructionError::new(
                "SSA_DEST.RESOLVER_CYCLE_SHAPE",
                format!(
                    "stalled parallel-copy graph has {} readers of cycle location {:?}",
                    saved_readers.len(),
                    saved
                ),
            )
            .edge(predecessor, successor)
            .values(
                copies[cycle].phi_destination,
                Some(copies[cycle].source_value),
            ));
        }
        let Some(&reader) = saved_readers.iter().next() else {
            return Err(SsaDestructionError::new(
                "SSA_DEST.RESOLVER_CYCLE_SHAPE",
                "stalled parallel-copy cycle has no reader",
            )
            .edge(predecessor, successor)
            .values(
                copies[cycle].phi_destination,
                Some(copies[cycle].source_value),
            ));
        };
        copies[reader].source = PendingSource::Temporary;
        operations.push(ParallelCopyOperation::SaveTemporary(saved));
        work.cycle_breaks += 1;
        work.temporary_cycle_breaks += 1;
        work.dependency_releases += 1;
        temporary_live = true;
        if !queued[cycle] {
            ready.push_back(cycle);
            queued[cycle] = true;
        }
    }

    if temporary_live {
        return Err(SsaDestructionError::new(
            "SSA_DEST.RESOLVER_TEMPORARY_STATE",
            "parallel-copy resolver left its temporary live",
        )
        .edge(predecessor, successor));
    }
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
mod tests {
    use super::*;
    use crate::backend::native::emit::{self, EmitError};
    use crate::backend::native::mir::{
        BaseReg, MBlock, MInst, OpSize, PhiNode, SpillDesc, VRegAllocator,
    };

    fn register_copy(
        destination_value: u32,
        source_value: u32,
        destination: PhysReg,
        source: PhysReg,
    ) -> ParallelCopy {
        ParallelCopy {
            phi_destination: VReg(destination_value),
            source_value: VReg(source_value),
            destination: ParallelCopyDestination::Register(destination),
            source: ParallelCopySource::Register(source),
        }
    }

    #[test]
    fn resolver_emits_acyclic_chain_in_safe_reverse_order_without_temporary() {
        let rows = vec![
            register_copy(2, 0, PhysReg::RAX, PhysReg::RDX),
            register_copy(3, 1, PhysReg::RSI, PhysReg::RAX),
        ];

        let (operations, work) = resolve_parallel_copies(BlockId(0), BlockId(1), &rows).unwrap();

        assert_eq!(
            operations,
            vec![
                ParallelCopyOperation::Move {
                    destination: ParallelCopyDestination::Register(PhysReg::RSI),
                    source: ParallelCopySource::Register(PhysReg::RAX),
                },
                ParallelCopyOperation::Move {
                    destination: ParallelCopyDestination::Register(PhysReg::RAX),
                    source: ParallelCopySource::Register(PhysReg::RDX),
                },
            ]
        );
        assert_eq!(work.effective_copies, 2);
        assert_eq!(work.direct_moves, 2);
        assert_eq!(work.cycle_breaks, 0);
    }

    #[test]
    fn resolver_lowers_register_cycles_to_k_minus_one_swaps() {
        for (rows, expected_swaps) in [
            (
                vec![
                    register_copy(2, 0, PhysReg::RAX, PhysReg::RDX),
                    register_copy(3, 1, PhysReg::RDX, PhysReg::RAX),
                ],
                1,
            ),
            (
                vec![
                    register_copy(3, 0, PhysReg::RAX, PhysReg::RDX),
                    register_copy(4, 1, PhysReg::RDX, PhysReg::RSI),
                    register_copy(5, 2, PhysReg::RSI, PhysReg::RAX),
                ],
                2,
            ),
        ] {
            let (operations, work) =
                resolve_parallel_copies(BlockId(0), BlockId(1), &rows).unwrap();
            assert_eq!(work.cycle_breaks, 1, "{operations:?}");
            assert_eq!(work.temporary_cycle_breaks, 0, "{operations:?}");
            assert_eq!(work.register_swaps, expected_swaps, "{operations:?}");
            assert_eq!(
                operations
                    .iter()
                    .filter(|operation| matches!(
                        operation,
                        ParallelCopyOperation::SwapRegisters { .. }
                    ))
                    .count(),
                expected_swaps
            );
            assert_eq!(operations.len(), rows.len() - 1);
        }
    }

    #[test]
    fn resolver_drains_cycle_fanout_before_register_swap() {
        let rows = vec![
            register_copy(3, 0, PhysReg::RAX, PhysReg::RDX),
            register_copy(4, 1, PhysReg::RDX, PhysReg::RAX),
            register_copy(5, 2, PhysReg::RSI, PhysReg::RAX),
        ];

        let (operations, work) = resolve_parallel_copies(BlockId(0), BlockId(1), &rows).unwrap();

        assert_eq!(
            operations.first(),
            Some(&ParallelCopyOperation::Move {
                destination: ParallelCopyDestination::Register(PhysReg::RSI),
                source: ParallelCopySource::Register(PhysReg::RAX),
            })
        );
        assert_eq!(work.cycle_breaks, 1);
        assert_eq!(work.register_swaps, 1);
        assert_eq!(work.temporary_cycle_breaks, 0);
        assert_eq!(work.effective_copies, 3);
    }

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
    fn identity_rows_are_counted_but_emit_no_effective_copy() {
        let function = one_edge_function(&[(VReg(1), VReg(0))]);
        let mut assignment = AssignmentMap::default();
        assignment.set(VReg(0), PhysReg::RAX);
        assignment.set(VReg(1), PhysReg::RAX);

        let plan = SsaDestructionPlan::build(&function, &assignment).unwrap();
        plan.verify(&function, &assignment, 0).unwrap();
        let stats = plan.stats();

        assert_eq!(stats.edges, 1);
        assert_eq!(stats.rows, 1);
        assert_eq!(stats.identity_rows, 1);
        assert_eq!(stats.effective_copies, 0);
        assert_eq!(stats.identity_only_edges, 1);
        assert!(
            !plan
                .edge(BlockId(0), BlockId(1))
                .unwrap()
                .has_effective_copies()
        );
    }

    #[test]
    fn identity_stack_row_still_requires_a_valid_frame_slot() {
        let function = one_edge_function(&[(VReg(1), VReg(0))]);
        let mut assignment = AssignmentMap::default();
        assignment.set_edge_location(BlockId(0), VReg(0), EdgeLocation::Stack(1));
        assignment.set_edge_spill_slot(VReg(1), 1);

        let plan = SsaDestructionPlan::build(&function, &assignment).unwrap();
        let error = plan.verify(&function, &assignment, 16).unwrap_err();

        assert_eq!(error.rule, "SSA_DEST.STACK_SLOT");
        assert_eq!(error.predecessor, Some(BlockId(0)));
        assert_eq!(error.successor, Some(BlockId(1)));
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
    fn ordinary_stack_access_must_be_aligned_and_inside_the_spill_frame() {
        for offset in [-8, 4, 16] {
            let mut vregs = VRegAllocator::new();
            let value = vregs.alloc();
            let mut function = MFunction::new(vregs, vec![SpillDesc::transient()]);
            let mut entry = MBlock::new(BlockId(0));
            entry.push(MInst::Load {
                dst: value,
                base: BaseReg::StackFrame,
                offset,
                size: OpSize::S64,
            });
            entry.push(MInst::Return);
            function.push_block(entry);
            let mut assignment = AssignmentMap::default();
            assignment.set(value, PhysReg::RAX);

            let error = match emit::emit(&function, &assignment, 16) {
                Ok(_) => panic!("invalid stack-frame access must be rejected"),
                Err(error) => error,
            };
            let EmitError::Input(error) = error else {
                panic!("expected stack-frame input error, got {error}");
            };
            assert_eq!(error.rule, "EMIT.STACK_FRAME_ACCESS");
            assert_eq!(error.block, Some(BlockId(0)));
            assert_eq!(error.instruction, Some(0));
        }
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

    #[cfg(target_arch = "x86_64")]
    fn execute_single_stack_destination(source: EdgeLocation, expected: u64) -> u64 {
        use crate::backend::native::jit_mem::JitCode;

        let mut vregs = VRegAllocator::new();
        let source_value = vregs.alloc();
        let destination = vregs.alloc();
        let observed = vregs.alloc();
        let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);
        let mut predecessor = MBlock::new(BlockId(0));
        predecessor.push(MInst::LoadImm {
            dst: source_value,
            value: expected,
        });
        if matches!(source, EdgeLocation::Stack(_)) {
            predecessor.push(MInst::Store {
                base: BaseReg::StackFrame,
                offset: 0,
                src: source_value,
                size: OpSize::S64,
            });
        }
        predecessor.push(MInst::Jump { target: BlockId(1) });
        let mut successor = MBlock::new(BlockId(1));
        successor.phis.push(PhiNode {
            dst: destination,
            sources: vec![(BlockId(0), source_value)],
        });
        successor.push(MInst::Load {
            dst: observed,
            base: BaseReg::StackFrame,
            offset: 8,
            size: OpSize::S64,
        });
        successor.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 0,
            src: observed,
            size: OpSize::S64,
        });
        successor.push(MInst::Return);
        function.blocks = vec![predecessor, successor];

        let mut assignment = AssignmentMap::default();
        assignment.set(source_value, PhysReg::RAX);
        assignment.set(observed, PhysReg::RDX);
        assignment.set_edge_location(BlockId(0), source_value, source);
        assignment.set_edge_spill_slot(destination, 8);
        let emitted = emit::emit(&function, &assignment, 16).unwrap();
        let jit = JitCode::new(&emitted.code).unwrap();
        let mut state = [0_u8; 8];
        assert_eq!(unsafe { jit.call(&mut state) }, 0);
        u64::from_le_bytes(state)
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn single_stack_to_stack_copy_uses_the_original_frame_offsets() {
        assert_eq!(
            execute_single_stack_destination(EdgeLocation::Stack(0), 0x1122_3344_5566_7788),
            0x1122_3344_5566_7788
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn single_immediate_to_stack_copy_preserves_all_64_bits() {
        assert_eq!(
            execute_single_stack_destination(
                EdgeLocation::Immediate(0x8877_6655_4433_2211),
                0x8877_6655_4433_2211,
            ),
            0x8877_6655_4433_2211
        );
    }

    #[cfg(target_arch = "x86_64")]
    fn execute_register_parallel_copy(
        first_destination: PhysReg,
        second_destination: PhysReg,
    ) -> (Vec<iced_x86::Mnemonic>, ParallelCopyWork, [u64; 2]) {
        use crate::backend::native::jit_mem::JitCode;
        use iced_x86::{Decoder, DecoderOptions};

        let mut vregs = VRegAllocator::new();
        let first_source = vregs.alloc();
        let second_source = vregs.alloc();
        let first_result = vregs.alloc();
        let second_result = vregs.alloc();
        let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 4]);
        let mut predecessor = MBlock::new(BlockId(0));
        predecessor.push(MInst::LoadImm {
            dst: first_source,
            value: 11,
        });
        predecessor.push(MInst::LoadImm {
            dst: second_source,
            value: 22,
        });
        predecessor.push(MInst::Jump { target: BlockId(1) });
        let mut successor = MBlock::new(BlockId(1));
        successor.phis = vec![
            PhiNode {
                dst: first_result,
                sources: vec![(BlockId(0), first_source)],
            },
            PhiNode {
                dst: second_result,
                sources: vec![(BlockId(0), second_source)],
            },
        ];
        successor.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 0,
            src: first_result,
            size: OpSize::S64,
        });
        successor.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 8,
            src: second_result,
            size: OpSize::S64,
        });
        successor.push(MInst::Return);
        function.blocks = vec![predecessor, successor];

        let mut assignment = AssignmentMap::default();
        assignment.set(first_source, PhysReg::RDX);
        assignment.set(second_source, PhysReg::RAX);
        assignment.set(first_result, first_destination);
        assignment.set(second_result, second_destination);
        assignment.set_edge_location(
            BlockId(0),
            first_source,
            EdgeLocation::Register(PhysReg::RDX),
        );
        assignment.set_edge_location(
            BlockId(0),
            second_source,
            EdgeLocation::Register(PhysReg::RAX),
        );

        let plan = SsaDestructionPlan::build(&function, &assignment).unwrap();
        plan.verify(&function, &assignment, 0).unwrap();
        let work = plan.edge(BlockId(0), BlockId(1)).unwrap().work;
        let emitted = emit::emit(&function, &assignment, 0).unwrap();
        let start = emitted
            .block_offsets
            .iter()
            .find(|(block, _)| *block == BlockId(0))
            .unwrap()
            .1 as usize;
        let end = emitted
            .block_offsets
            .iter()
            .find(|(block, _)| *block == BlockId(1))
            .unwrap()
            .1 as usize;
        let mut decoder = Decoder::with_ip(
            64,
            &emitted.code[start..end],
            start as u64,
            DecoderOptions::NONE,
        );
        let mut mnemonics = Vec::new();
        while decoder.can_decode() {
            mnemonics.push(decoder.decode().mnemonic());
        }

        let jit = JitCode::new(&emitted.code).unwrap();
        let mut state = [0_u8; 16];
        assert_eq!(unsafe { jit.call(&mut state) }, 0);
        let values = [
            u64::from_le_bytes(state[..8].try_into().unwrap()),
            u64::from_le_bytes(state[8..].try_into().unwrap()),
        ];
        (mnemonics, work, values)
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn acyclic_register_copies_execute_without_stack_temporary() {
        use iced_x86::Mnemonic;

        let (mnemonics, work, values) = execute_register_parallel_copy(PhysReg::RAX, PhysReg::RSI);

        assert_eq!(values, [11, 22]);
        assert_eq!(work.cycle_breaks, 0);
        assert_eq!(work.direct_moves, 2);
        assert!(!mnemonics.contains(&Mnemonic::Push), "{mnemonics:?}");
        assert!(!mnemonics.contains(&Mnemonic::Pop), "{mnemonics:?}");
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn register_cycle_executes_with_one_exchange_and_no_stack_temporary() {
        use iced_x86::Mnemonic;

        let (mnemonics, work, values) = execute_register_parallel_copy(PhysReg::RAX, PhysReg::RDX);

        assert_eq!(values, [11, 22]);
        assert_eq!(work.cycle_breaks, 1);
        assert_eq!(work.register_swaps, 1);
        assert_eq!(work.temporary_cycle_breaks, 0);
        assert_eq!(
            mnemonics
                .iter()
                .filter(|mnemonic| **mnemonic == Mnemonic::Xchg)
                .count(),
            1,
            "{mnemonics:?}"
        );
        assert!(!mnemonics.contains(&Mnemonic::Push), "{mnemonics:?}");
        assert!(!mnemonics.contains(&Mnemonic::Pop), "{mnemonics:?}");
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn identity_only_branch_edges_keep_cmp_fusion_and_false_fallthrough() {
        use iced_x86::{Decoder, DecoderOptions, Mnemonic};

        let mut vregs = VRegAllocator::new();
        let lhs = vregs.alloc();
        let rhs = vregs.alloc();
        let value = vregs.alloc();
        let condition = vregs.alloc();
        let false_value = vregs.alloc();
        let true_value = vregs.alloc();
        let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 6]);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm { dst: lhs, value: 1 });
        entry.push(MInst::LoadImm { dst: rhs, value: 1 });
        entry.push(MInst::LoadImm {
            dst: value,
            value: 99,
        });
        entry.push(MInst::Cmp {
            dst: condition,
            lhs,
            rhs,
            kind: crate::backend::native::mir::CmpKind::Eq,
        });
        entry.push(MInst::Branch {
            cond: condition,
            true_bb: BlockId(2),
            false_bb: BlockId(1),
        });
        let mut false_block = MBlock::new(BlockId(1));
        false_block.phis.push(PhiNode {
            dst: false_value,
            sources: vec![(BlockId(0), value)],
        });
        false_block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 0,
            src: false_value,
            size: OpSize::S64,
        });
        false_block.push(MInst::Return);
        let mut true_block = MBlock::new(BlockId(2));
        true_block.phis.push(PhiNode {
            dst: true_value,
            sources: vec![(BlockId(0), value)],
        });
        true_block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 0,
            src: true_value,
            size: OpSize::S64,
        });
        true_block.push(MInst::Return);
        function.blocks = vec![entry, false_block, true_block];

        let mut assignment = AssignmentMap::default();
        for (vreg, register) in [
            (lhs, PhysReg::RAX),
            (rhs, PhysReg::RDX),
            (value, PhysReg::RSI),
            (condition, PhysReg::RDI),
            (false_value, PhysReg::RSI),
            (true_value, PhysReg::RSI),
        ] {
            assignment.set(vreg, register);
        }
        assignment.set_edge_location(BlockId(0), value, EdgeLocation::Register(PhysReg::RSI));

        let plan = SsaDestructionPlan::build(&function, &assignment).unwrap();
        assert_eq!(plan.stats().effective_copies, 0);
        let emitted = emit::emit(&function, &assignment, 0).unwrap();
        let start = emitted.block_offsets[0].1 as usize;
        let end = emitted.block_offsets[1].1 as usize;
        let mut decoder = Decoder::with_ip(
            64,
            &emitted.code[start..end],
            start as u64,
            DecoderOptions::NONE,
        );
        let mut mnemonics = Vec::new();
        while decoder.can_decode() {
            mnemonics.push(decoder.decode().mnemonic());
        }
        assert!(mnemonics.contains(&Mnemonic::Cmp), "{mnemonics:?}");
        assert_eq!(
            mnemonics
                .iter()
                .filter(|mnemonic| matches!(mnemonic, Mnemonic::Je | Mnemonic::Jne))
                .count(),
            1,
            "{mnemonics:?}"
        );
        assert!(!mnemonics.contains(&Mnemonic::Jmp), "{mnemonics:?}");
        assert!(
            !mnemonics
                .iter()
                .any(|mnemonic| matches!(mnemonic, Mnemonic::Sete | Mnemonic::Setne))
        );
    }

    #[test]
    fn consecutive_empty_fallthrough_blocks_alias_the_next_instruction() {
        use crate::backend::native::jit_mem::JitCode;
        use iced_x86::{Decoder, DecoderOptions, Mnemonic};
        use std::collections::HashMap;

        let mut vregs = VRegAllocator::new();
        let condition = vregs.alloc();
        let true_value = vregs.alloc();
        let false_value = vregs.alloc();
        let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);

        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::Load {
            dst: condition,
            base: BaseReg::SimState,
            offset: 0,
            size: OpSize::S64,
        });
        entry.push(MInst::Branch {
            cond: condition,
            true_bb: BlockId(1),
            false_bb: BlockId(4),
        });

        let mut first_empty = MBlock::new(BlockId(1));
        first_empty.push(MInst::Jump { target: BlockId(2) });
        let mut second_empty = MBlock::new(BlockId(2));
        second_empty.push(MInst::Jump { target: BlockId(3) });

        let mut true_block = MBlock::new(BlockId(3));
        true_block.push(MInst::LoadImm {
            dst: true_value,
            value: 11,
        });
        true_block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 8,
            src: true_value,
            size: OpSize::S64,
        });
        true_block.push(MInst::Return);

        let mut false_block = MBlock::new(BlockId(4));
        false_block.push(MInst::LoadImm {
            dst: false_value,
            value: 22,
        });
        false_block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 8,
            src: false_value,
            size: OpSize::S64,
        });
        false_block.push(MInst::Return);

        function.blocks = vec![entry, first_empty, second_empty, true_block, false_block];
        let mut assignment = AssignmentMap::default();
        assignment.set(condition, PhysReg::RAX);
        assignment.set(true_value, PhysReg::RDX);
        assignment.set(false_value, PhysReg::RDX);

        let emitted = emit::emit(&function, &assignment, 0).unwrap();
        let offsets = emitted
            .block_offsets
            .iter()
            .copied()
            .collect::<HashMap<_, _>>();
        assert_eq!(offsets[&BlockId(1)], offsets[&BlockId(2)]);
        assert_eq!(offsets[&BlockId(2)], offsets[&BlockId(3)]);

        let mut decoder = Decoder::with_ip(64, &emitted.code, 0, DecoderOptions::NONE);
        while decoder.can_decode() {
            assert_ne!(decoder.decode().mnemonic(), Mnemonic::Nop);
        }

        let jit = JitCode::new(&emitted.code).unwrap();
        for (condition, expected) in [(0_u64, 22_u64), (1, 11)] {
            let mut state = [0_u8; 16];
            state[..8].copy_from_slice(&condition.to_le_bytes());
            assert_eq!(unsafe { jit.call(&mut state) }, 0);
            assert_eq!(u64::from_le_bytes(state[8..].try_into().unwrap()), expected);
        }
    }

    #[test]
    fn trailing_continuation_label_aliases_an_empty_fallthrough_chain() {
        use crate::backend::native::jit_mem::JitCode;
        use iced_x86::{Decoder, DecoderOptions, Mnemonic};
        use std::collections::HashMap;

        let mut vregs = VRegAllocator::new();
        let source = vregs.alloc();
        let result = vregs.alloc();
        let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 2]);

        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::Load {
            dst: source,
            base: BaseReg::SimState,
            offset: 0,
            size: OpSize::S64,
        });
        entry.push(MInst::BsrOr {
            dst: result,
            src: source,
            zero_value: 63,
        });
        entry.push(MInst::Jump { target: BlockId(1) });

        let mut first_empty = MBlock::new(BlockId(1));
        first_empty.push(MInst::Jump { target: BlockId(2) });
        let mut second_empty = MBlock::new(BlockId(2));
        second_empty.push(MInst::Jump { target: BlockId(3) });
        let mut exit = MBlock::new(BlockId(3));
        exit.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 8,
            src: result,
            size: OpSize::S64,
        });
        exit.push(MInst::Return);
        function.blocks = vec![entry, first_empty, second_empty, exit];

        let mut assignment = AssignmentMap::default();
        assignment.set(source, PhysReg::RAX);
        assignment.set(result, PhysReg::RDX);

        let emitted = emit::emit(&function, &assignment, 0).unwrap();
        let offsets = emitted
            .block_offsets
            .iter()
            .copied()
            .collect::<HashMap<_, _>>();
        assert_eq!(offsets[&BlockId(1)], offsets[&BlockId(2)]);
        assert_eq!(offsets[&BlockId(2)], offsets[&BlockId(3)]);

        let mut decoder = Decoder::with_ip(64, &emitted.code, 0, DecoderOptions::NONE);
        while decoder.can_decode() {
            assert_ne!(decoder.decode().mnemonic(), Mnemonic::Nop);
        }

        let jit = JitCode::new(&emitted.code).unwrap();
        for (source, expected) in [(0_u64, 63_u64), (8, 3)] {
            let mut state = [0_u8; 16];
            state[..8].copy_from_slice(&source.to_le_bytes());
            assert_eq!(unsafe { jit.call(&mut state) }, 0);
            assert_eq!(u64::from_le_bytes(state[8..].try_into().unwrap()), expected);
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn branch_executes_only_the_selected_edge_copy_plan() {
        use crate::backend::native::jit_mem::JitCode;

        let mut vregs = VRegAllocator::new();
        let true_source = vregs.alloc();
        let false_source = vregs.alloc();
        let condition = vregs.alloc();
        let false_value = vregs.alloc();
        let true_value = vregs.alloc();
        let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 5]);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: true_source,
            value: 11,
        });
        entry.push(MInst::LoadImm {
            dst: false_source,
            value: 22,
        });
        entry.push(MInst::Load {
            dst: condition,
            base: BaseReg::SimState,
            offset: 0,
            size: OpSize::S64,
        });
        entry.push(MInst::Branch {
            cond: condition,
            true_bb: BlockId(2),
            false_bb: BlockId(1),
        });
        let mut false_block = MBlock::new(BlockId(1));
        false_block.phis.push(PhiNode {
            dst: false_value,
            sources: vec![(BlockId(0), false_source)],
        });
        false_block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 8,
            src: false_value,
            size: OpSize::S64,
        });
        false_block.push(MInst::Return);
        let mut true_block = MBlock::new(BlockId(2));
        true_block.phis.push(PhiNode {
            dst: true_value,
            sources: vec![(BlockId(0), true_source)],
        });
        true_block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 8,
            src: true_value,
            size: OpSize::S64,
        });
        true_block.push(MInst::Return);
        function.blocks = vec![entry, false_block, true_block];

        let mut assignment = AssignmentMap::default();
        for (vreg, register) in [
            (true_source, PhysReg::RAX),
            (false_source, PhysReg::RDX),
            (condition, PhysReg::R8),
            (false_value, PhysReg::RDI),
            (true_value, PhysReg::RSI),
        ] {
            assignment.set(vreg, register);
        }
        assignment.set_edge_location(
            BlockId(0),
            true_source,
            EdgeLocation::Register(PhysReg::RAX),
        );
        assignment.set_edge_location(
            BlockId(0),
            false_source,
            EdgeLocation::Register(PhysReg::RDX),
        );
        let plan = SsaDestructionPlan::build(&function, &assignment).unwrap();
        assert_eq!(plan.stats().effective_copies, 2);
        let emitted = emit::emit(&function, &assignment, 0).unwrap();
        let jit = JitCode::new(&emitted.code).unwrap();

        for (condition, expected) in [(0_u64, 22_u64), (1, 11)] {
            let mut state = [0_u8; 16];
            state[..8].copy_from_slice(&condition.to_le_bytes());
            assert_eq!(unsafe { jit.call(&mut state) }, 0);
            assert_eq!(u64::from_le_bytes(state[8..].try_into().unwrap()), expected);
        }
    }

    #[test]
    fn verifier_rejects_two_phi_rows_with_one_physical_destination() {
        let function = one_edge_function(&[(VReg(2), VReg(0)), (VReg(3), VReg(1))]);
        let mut assignment = AssignmentMap::default();
        assignment.set(VReg(0), PhysReg::RAX);
        assignment.set(VReg(1), PhysReg::RDX);
        assignment.set(VReg(2), PhysReg::RSI);
        assignment.set(VReg(3), PhysReg::RDI);

        let mut plan = SsaDestructionPlan::build(&function, &assignment).unwrap();
        let edge = plan.edges.get_mut(&(BlockId(0), BlockId(1))).unwrap();
        edge.rows[1].destination = edge.rows[0].destination;
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
