//! Allocation policy over sparse physical interval unions.
//!
//! This is intentionally separate from MIR reconstruction. It chooses
//! register residency or one proved home for complete live bundles, and it
//! records every eviction/recoloring decision in allocation-owned state.
//! Later slices split bundles and lower the selected transitions into SSA.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};
use std::fmt;

use crate::backend::native::mir::{BlockId, VReg};

use super::assignment::PhysReg;
use super::cfg::NormalizedCfg;
use super::home_graph::{
    BundleUseId, HomeCandidate, HomeGraph, HomeKind, LiveBundleId, UseMaterialization,
};
use super::interval_union::{
    AllocationBundleId, IntervalUnionError, LiveIntervalMatrix, live_length,
};
use super::live_interval::{DefinitionSite, LiveSegment};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AllocationStage {
    Original,
    /// This bundle has already lost a register to a more valuable bundle.
    /// It may be recolored or sent to a home, but cannot start another
    /// eviction chain.
    Evicted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HomeSelection {
    pub kind: HomeKind,
    pub materializations: Vec<UseMaterialization>,
    pub creation_cost: u32,
    pub materialization_cost: u32,
}

impl HomeSelection {
    fn total_cost(&self) -> u64 {
        u64::from(self.creation_cost) + u64::from(self.materialization_cost)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum BundleAssignment {
    Unassigned,
    Register(PhysReg),
    Home(HomeSelection),
    Split { children: Vec<AllocationBundleId> },
    Dead,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AllocatedBundle {
    pub id: AllocationBundleId,
    pub root: LiveBundleId,
    pub parent: Option<AllocationBundleId>,
    pub origin: VReg,
    pub definition: DefinitionSite,
    pub segments: Vec<LiveSegment>,
    pub uses: Vec<BundleUseId>,
    pub stage: AllocationStage,
    pub spill_cost: u64,
    pub assignment: BundleAssignment,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AllocationPlan {
    pub bundles: Vec<AllocatedBundle>,
    matrix: LiveIntervalMatrix,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HomePiece {
    uses: Vec<BundleUseId>,
    selection: HomeSelection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HomePartition {
    pieces: Vec<HomePiece>,
    total_cost: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct IntervalAllocationError {
    pub rule: &'static str,
    pub block: Option<BlockId>,
    pub bundle: Option<AllocationBundleId>,
    pub message: String,
}

impl IntervalAllocationError {
    fn new(
        rule: &'static str,
        block: Option<BlockId>,
        bundle: Option<AllocationBundleId>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            rule,
            block,
            bundle,
            message: message.into(),
        }
    }

    fn union(error: IntervalUnionError) -> Self {
        Self::new(
            error.rule,
            error.block,
            error.bundles.first().copied(),
            error.message,
        )
    }
}

impl fmt::Display for IntervalAllocationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.rule)?;
        if let Some(block) = self.block {
            write!(formatter, " at {block}")?;
        }
        if let Some(bundle) = self.bundle {
            write!(formatter, " bundle={bundle:?}")?;
        }
        write!(formatter, ": {}", self.message)
    }
}

impl std::error::Error for IntervalAllocationError {}

fn home_rank(kind: HomeKind) -> u8 {
    match kind {
        HomeKind::Rematerialize(_) => 0,
        HomeKind::State(_) => 1,
        HomeKind::Stack => 2,
        HomeKind::Register => 3,
    }
}

fn partition_homes(
    graph: &HomeGraph,
    root: LiveBundleId,
    uses: &[BundleUseId],
) -> Result<HomePartition, IntervalAllocationError> {
    let Some(candidates) = graph.candidates.get(root.0 as usize) else {
        return Err(IntervalAllocationError::new(
            "INTERVAL_ALLOC.HOME_ROOT",
            None,
            None,
            format!("root bundle {root:?} has no home-candidate row"),
        ));
    };
    let build = |stack_available: bool| -> Option<HomePartition> {
        let mut grouped = BTreeMap::<HomeKind, (Vec<BundleUseId>, Vec<UseMaterialization>)>::new();
        for &use_id in uses {
            let mut best = None::<(u32, u8, HomeKind, Option<UseMaterialization>)>;
            if stack_available {
                best = Some((1, home_rank(HomeKind::Stack), HomeKind::Stack, None));
            }
            for candidate in candidates {
                if !matches!(
                    candidate.kind,
                    HomeKind::Rematerialize(_) | HomeKind::State(_)
                ) || candidate.creation_cost != 0
                {
                    continue;
                }
                let materialization = candidate
                    .materializations
                    .iter()
                    .find(|item| item.use_id == use_id)
                    .copied();
                let Some(materialization) = materialization else {
                    continue;
                };
                let choice = (
                    materialization.cost,
                    home_rank(candidate.kind),
                    candidate.kind,
                    Some(materialization),
                );
                if best.as_ref().is_none_or(|current| choice < *current) {
                    best = Some(choice);
                }
            }
            let (_, _, kind, materialization) = best?;
            let group = grouped.entry(kind).or_default();
            group.0.push(use_id);
            if let Some(materialization) = materialization {
                group.1.push(materialization);
            }
        }

        let stack_creation = if grouped.contains_key(&HomeKind::Stack) {
            Some(
                candidates
                    .iter()
                    .find(|candidate| candidate.kind == HomeKind::Stack)
                    .map(|candidate| candidate.creation_cost)?,
            )
        } else {
            None
        };
        let mut total_cost = u64::from(stack_creation.unwrap_or(0));
        let mut pieces = Vec::with_capacity(grouped.len());
        for (kind, (piece_uses, materializations)) in grouped {
            let materialization_cost = match kind {
                HomeKind::Stack => u32::try_from(piece_uses.len()).unwrap_or(u32::MAX),
                HomeKind::Rematerialize(_) | HomeKind::State(_) => materializations
                    .iter()
                    .fold(0_u32, |cost, item| cost.saturating_add(item.cost)),
                HomeKind::Register => return None,
            };
            let creation_cost = if kind == HomeKind::Stack {
                stack_creation.unwrap_or(0)
            } else {
                0
            };
            total_cost = total_cost.saturating_add(u64::from(materialization_cost));
            pieces.push(HomePiece {
                uses: piece_uses,
                selection: HomeSelection {
                    kind,
                    materializations,
                    creation_cost,
                    materialization_cost,
                },
            });
        }
        Some(HomePartition { pieces, total_cost })
    };

    let without_stack = build(false);
    let with_stack = build(true).ok_or_else(|| {
        IntervalAllocationError::new(
            "INTERVAL_ALLOC.NO_STACK_HOME",
            None,
            None,
            format!("root bundle {root:?} lacks its mandatory stack candidate"),
        )
    })?;
    Ok(without_stack
        .filter(|partition| partition.total_cost <= with_stack.total_cost)
        .unwrap_or(with_stack))
}

fn candidate_covers(
    candidate: &HomeCandidate,
    uses: &[BundleUseId],
    selection: &HomeSelection,
) -> bool {
    if candidate.kind != selection.kind
        || candidate.creation_cost != selection.creation_cost
        || !uses
            .iter()
            .all(|use_id| candidate.uses.binary_search(use_id).is_ok())
    {
        return false;
    }
    match candidate.kind {
        HomeKind::Register => false,
        HomeKind::Stack => {
            selection.materializations.is_empty()
                && selection.materialization_cost == u32::try_from(uses.len()).unwrap_or(u32::MAX)
        }
        HomeKind::Rematerialize(_) | HomeKind::State(_) => {
            selection.materializations
                == candidate
                    .materializations
                    .iter()
                    .copied()
                    .filter(|item| uses.binary_search(&item.use_id).is_ok())
                    .collect::<Vec<_>>()
                && selection.materialization_cost
                    == selection
                        .materializations
                        .iter()
                        .fold(0_u32, |cost, item| cost.saturating_add(item.cost))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct QueueItem {
    id: AllocationBundleId,
    spill_cost: u64,
    live_length: u64,
    use_count: usize,
}

impl Ord for QueueItem {
    fn cmp(&self, other: &Self) -> Ordering {
        let left = u128::from(self.spill_cost) * u128::from(other.live_length);
        let right = u128::from(other.spill_cost) * u128::from(self.live_length);
        left.cmp(&right)
            .then_with(|| self.spill_cost.cmp(&other.spill_cost))
            .then_with(|| self.use_count.cmp(&other.use_count))
            // Stable lower bundle IDs win an otherwise exact tie.
            .then_with(|| other.id.cmp(&self.id))
    }
}

impl PartialOrd for QueueItem {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

struct Allocator<'a> {
    graph: &'a HomeGraph,
    registers: Vec<PhysReg>,
    matrix: LiveIntervalMatrix,
    bundles: Vec<AllocatedBundle>,
    queue: BinaryHeap<QueueItem>,
}

impl<'a> Allocator<'a> {
    fn new(
        graph: &'a HomeGraph,
        cfg: &NormalizedCfg,
        registers: &[PhysReg],
    ) -> Result<Self, IntervalAllocationError> {
        let matrix =
            LiveIntervalMatrix::new(cfg, registers).map_err(IntervalAllocationError::union)?;
        let mut bundles = Vec::with_capacity(graph.bundles.len());
        let mut queue = BinaryHeap::new();
        for (index, root) in graph.bundles.iter().enumerate() {
            if root.id.0 as usize != index {
                return Err(IntervalAllocationError::new(
                    "INTERVAL_ALLOC.ROOT_ID",
                    Some(root.definition.block()),
                    None,
                    "HomeGraph root identity differs from its stable table index",
                ));
            }
            let id = AllocationBundleId(u32::try_from(index).map_err(|_| {
                IntervalAllocationError::new(
                    "INTERVAL_ALLOC.BUNDLE_ID_RANGE",
                    Some(root.definition.block()),
                    None,
                    "root bundle count exceeds u32",
                )
            })?);
            let uses = root.uses.iter().map(|use_| use_.id).collect::<Vec<_>>();
            let (spill_cost, assignment) = if uses.is_empty() {
                (0, BundleAssignment::Dead)
            } else {
                (
                    partition_homes(graph, root.id, &uses)?.total_cost,
                    BundleAssignment::Unassigned,
                )
            };
            let length = live_length(&root.segments).ok_or_else(|| {
                IntervalAllocationError::new(
                    "INTERVAL_ALLOC.LIVE_LENGTH",
                    Some(root.definition.block()),
                    Some(id),
                    "sparse live-segment length overflows u64",
                )
            })?;
            if !uses.is_empty() && length == 0 {
                return Err(IntervalAllocationError::new(
                    "INTERVAL_ALLOC.LIVE_LENGTH",
                    Some(root.definition.block()),
                    Some(id),
                    "used bundle has an empty sparse live range",
                ));
            }
            bundles.push(AllocatedBundle {
                id,
                root: root.id,
                parent: None,
                origin: root.origin,
                definition: root.definition,
                segments: root.segments.clone(),
                uses,
                stage: AllocationStage::Original,
                spill_cost,
                assignment,
            });
            if !root.uses.is_empty() {
                queue.push(QueueItem {
                    id,
                    spill_cost,
                    live_length: length,
                    use_count: root.uses.len(),
                });
            }
        }
        Ok(Self {
            graph,
            registers: registers.to_vec(),
            matrix,
            bundles,
            queue,
        })
    }

    fn queue_item(&self, id: AllocationBundleId) -> Result<QueueItem, IntervalAllocationError> {
        let bundle = self.bundle(id)?;
        let length = live_length(&bundle.segments).ok_or_else(|| {
            IntervalAllocationError::new(
                "INTERVAL_ALLOC.LIVE_LENGTH",
                Some(bundle.definition.block()),
                Some(id),
                "sparse live-segment length overflows u64",
            )
        })?;
        Ok(QueueItem {
            id,
            spill_cost: bundle.spill_cost,
            live_length: length.max(1),
            use_count: bundle.uses.len(),
        })
    }

    fn bundle(&self, id: AllocationBundleId) -> Result<&AllocatedBundle, IntervalAllocationError> {
        self.bundles.get(id.0 as usize).ok_or_else(|| {
            IntervalAllocationError::new(
                "INTERVAL_ALLOC.BUNDLE_RANGE",
                None,
                Some(id),
                "allocation bundle is outside the stable bundle table",
            )
        })
    }

    fn assign_register(
        &mut self,
        id: AllocationBundleId,
        register: PhysReg,
    ) -> Result<(), IntervalAllocationError> {
        let segments = self.bundle(id)?.segments.clone();
        self.matrix
            .assign(id, register, &segments)
            .map_err(IntervalAllocationError::union)?;
        self.bundles[id.0 as usize].assignment = BundleAssignment::Register(register);
        Ok(())
    }

    fn try_free_register(
        &mut self,
        id: AllocationBundleId,
    ) -> Result<bool, IntervalAllocationError> {
        let segments = self.bundle(id)?.segments.clone();
        for register in self.registers.clone() {
            if self
                .matrix
                .conflicts(register, &segments)
                .map_err(IntervalAllocationError::union)?
                .is_empty()
            {
                self.assign_register(id, register)?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn try_recolor(&mut self, id: AllocationBundleId) -> Result<bool, IntervalAllocationError> {
        let segments = self.bundle(id)?.segments.clone();
        for target in self.registers.clone() {
            let conflicts = self
                .matrix
                .conflicts(target, &segments)
                .map_err(IntervalAllocationError::union)?;
            if conflicts.is_empty() || conflicts.len() > self.registers.len() {
                continue;
            }
            let Some((matrix, moves)) = recolor_residents(
                &self.matrix,
                &self.bundles,
                &self.registers,
                id,
                target,
                &conflicts,
            )?
            else {
                continue;
            };
            self.matrix = matrix;
            for (bundle, register) in moves {
                self.bundles[bundle.0 as usize].assignment = BundleAssignment::Register(register);
            }
            self.bundles[id.0 as usize].assignment = BundleAssignment::Register(target);
            return Ok(true);
        }
        Ok(false)
    }

    fn try_evict(&mut self, id: AllocationBundleId) -> Result<bool, IntervalAllocationError> {
        let candidate = self.bundle(id)?;
        if candidate.stage != AllocationStage::Original {
            return Ok(false);
        }
        let candidate_cost = candidate.spill_cost;
        let segments = candidate.segments.clone();
        let mut best = None::<(u64, usize, PhysReg, Vec<AllocationBundleId>)>;
        for (order, register) in self.registers.iter().copied().enumerate() {
            let conflicts = self
                .matrix
                .conflicts(register, &segments)
                .map_err(IntervalAllocationError::union)?;
            if conflicts.is_empty()
                || conflicts.iter().any(|conflict| {
                    self.bundles[conflict.0 as usize].stage == AllocationStage::Evicted
                })
            {
                continue;
            }
            let cost = conflicts.iter().fold(0_u64, |cost, conflict| {
                cost.saturating_add(self.bundles[conflict.0 as usize].spill_cost)
            });
            if candidate_cost <= cost {
                continue;
            }
            let key = (cost, order);
            if best
                .as_ref()
                .is_none_or(|(best_cost, best_order, _, _)| key < (*best_cost, *best_order))
            {
                best = Some((cost, order, register, conflicts));
            }
        }
        let Some((_, _, register, conflicts)) = best else {
            return Ok(false);
        };
        for conflict in conflicts {
            let _ = self
                .matrix
                .unassign(conflict)
                .map_err(IntervalAllocationError::union)?;
            let displaced = &mut self.bundles[conflict.0 as usize];
            displaced.assignment = BundleAssignment::Unassigned;
            displaced.stage = AllocationStage::Evicted;
            self.queue.push(self.queue_item(conflict)?);
        }
        self.assign_register(id, register)?;
        Ok(true)
    }

    fn send_home(&mut self, id: AllocationBundleId) -> Result<(), IntervalAllocationError> {
        let bundle = self.bundle(id)?;
        let root = bundle.root;
        let origin = bundle.origin;
        let definition = bundle.definition;
        let stage = bundle.stage;
        let uses = bundle.uses.clone();
        let partition = partition_homes(self.graph, root, &uses)?;
        if let [piece] = partition.pieces.as_slice() {
            self.bundles[id.0 as usize].assignment =
                BundleAssignment::Home(piece.selection.clone());
            return Ok(());
        }

        let mut children = Vec::with_capacity(partition.pieces.len());
        for piece in partition.pieces {
            let child = AllocationBundleId(u32::try_from(self.bundles.len()).map_err(|_| {
                IntervalAllocationError::new(
                    "INTERVAL_ALLOC.BUNDLE_ID_RANGE",
                    Some(definition.block()),
                    Some(id),
                    "split bundle count exceeds u32",
                )
            })?);
            let spill_cost = piece.selection.total_cost();
            self.bundles.push(AllocatedBundle {
                id: child,
                root,
                parent: Some(id),
                origin,
                definition,
                segments: Vec::new(),
                uses: piece.uses,
                stage,
                spill_cost,
                assignment: BundleAssignment::Home(piece.selection),
            });
            children.push(child);
        }
        self.bundles[id.0 as usize].assignment = BundleAssignment::Split { children };
        Ok(())
    }

    fn run(mut self) -> Result<AllocationPlan, IntervalAllocationError> {
        while let Some(item) = self.queue.pop() {
            if !matches!(
                self.bundle(item.id)?.assignment,
                BundleAssignment::Unassigned
            ) {
                continue;
            }
            if self.try_free_register(item.id)?
                || self.try_recolor(item.id)?
                || self.try_evict(item.id)?
            {
                continue;
            }
            self.send_home(item.id)?;
        }
        let plan = AllocationPlan {
            bundles: self.bundles,
            matrix: self.matrix,
        };
        Ok(plan)
    }
}

pub(super) fn allocate_roots(
    graph: &HomeGraph,
    cfg: &NormalizedCfg,
    registers: &[PhysReg],
) -> Result<AllocationPlan, IntervalAllocationError> {
    let plan = Allocator::new(graph, cfg, registers)?.run()?;
    plan.verify(graph, cfg, registers)?;
    Ok(plan)
}

fn recolor_residents(
    matrix: &LiveIntervalMatrix,
    bundles: &[AllocatedBundle],
    registers: &[PhysReg],
    candidate: AllocationBundleId,
    target: PhysReg,
    conflicts: &[AllocationBundleId],
) -> Result<
    Option<(LiveIntervalMatrix, BTreeMap<AllocationBundleId, PhysReg>)>,
    IntervalAllocationError,
> {
    let mut trial = matrix.clone();
    for &conflict in conflicts {
        if trial.register(conflict) != Some(target) {
            return Err(IntervalAllocationError::new(
                "INTERVAL_ALLOC.RECOLOR_CONFLICT",
                None,
                Some(conflict),
                "recolor set is not resident in the target register",
            ));
        }
        let _ = trial
            .unassign(conflict)
            .map_err(IntervalAllocationError::union)?;
    }

    // The search neighborhood is bounded by the physical register file, not
    // by a target-specific tuning constant. Work is additionally capped at
    // one register probe per neighborhood/register pair.
    if conflicts.len() > registers.len() {
        return Ok(None);
    }
    let mut budget = conflicts
        .len()
        .checked_add(1)
        .and_then(|count| count.checked_mul(registers.len().saturating_add(1)))
        .ok_or_else(|| {
            IntervalAllocationError::new(
                "INTERVAL_ALLOC.RECOLOR_BUDGET",
                None,
                Some(candidate),
                "recolor work bound overflows usize",
            )
        })?;
    let mut moves = BTreeMap::new();
    if !recolor_search(
        &mut trial,
        bundles,
        registers,
        target,
        conflicts.to_vec(),
        &mut budget,
        &mut moves,
    )? {
        return Ok(None);
    }
    let candidate_segments = bundles
        .get(candidate.0 as usize)
        .ok_or_else(|| {
            IntervalAllocationError::new(
                "INTERVAL_ALLOC.BUNDLE_RANGE",
                None,
                Some(candidate),
                "recolor candidate is outside the bundle table",
            )
        })?
        .segments
        .clone();
    trial
        .assign(candidate, target, &candidate_segments)
        .map_err(IntervalAllocationError::union)?;
    trial.verify().map_err(IntervalAllocationError::union)?;
    Ok(Some((trial, moves)))
}

fn recolor_search(
    matrix: &mut LiveIntervalMatrix,
    bundles: &[AllocatedBundle],
    registers: &[PhysReg],
    target: PhysReg,
    pending: Vec<AllocationBundleId>,
    budget: &mut usize,
    moves: &mut BTreeMap<AllocationBundleId, PhysReg>,
) -> Result<bool, IntervalAllocationError> {
    if pending.is_empty() {
        return Ok(true);
    }

    let mut selected = None::<(usize, Vec<PhysReg>)>;
    for (index, &bundle) in pending.iter().enumerate() {
        let segments = bundles
            .get(bundle.0 as usize)
            .ok_or_else(|| {
                IntervalAllocationError::new(
                    "INTERVAL_ALLOC.BUNDLE_RANGE",
                    None,
                    Some(bundle),
                    "recolor resident is outside the bundle table",
                )
            })?
            .segments
            .clone();
        let mut alternatives = Vec::new();
        for &register in registers {
            if register != target
                && matrix
                    .conflicts(register, &segments)
                    .map_err(IntervalAllocationError::union)?
                    .is_empty()
            {
                alternatives.push(register);
            }
        }
        if alternatives.is_empty() {
            return Ok(false);
        }
        if selected.as_ref().is_none_or(|(best_index, best)| {
            (alternatives.len(), bundle) < (best.len(), pending[*best_index])
        }) {
            selected = Some((index, alternatives));
        }
    }
    let (selected_index, alternatives) =
        selected.expect("a non-empty recolor set selects one resident");
    let selected_bundle = pending[selected_index];
    let selected_segments = bundles[selected_bundle.0 as usize].segments.clone();
    let mut remaining = pending;
    remaining.remove(selected_index);
    for register in alternatives {
        if *budget == 0 {
            return Ok(false);
        }
        *budget -= 1;
        matrix
            .assign(selected_bundle, register, &selected_segments)
            .map_err(IntervalAllocationError::union)?;
        moves.insert(selected_bundle, register);
        if recolor_search(
            matrix,
            bundles,
            registers,
            target,
            remaining.clone(),
            budget,
            moves,
        )? {
            return Ok(true);
        }
        let _ = matrix
            .unassign(selected_bundle)
            .map_err(IntervalAllocationError::union)?;
        moves.remove(&selected_bundle);
    }
    Ok(false)
}

impl AllocationPlan {
    pub(super) fn verify(
        &self,
        graph: &HomeGraph,
        cfg: &NormalizedCfg,
        registers: &[PhysReg],
    ) -> Result<(), IntervalAllocationError> {
        if self.bundles.len() < graph.bundles.len() {
            return Err(IntervalAllocationError::new(
                "INTERVAL_ALLOC.ROOT_COVERAGE",
                None,
                None,
                "allocation plan does not cover every HomeGraph root",
            ));
        }
        self.matrix
            .verify()
            .map_err(IntervalAllocationError::union)?;
        let mut rebuilt =
            LiveIntervalMatrix::new(cfg, registers).map_err(IntervalAllocationError::union)?;
        for (index, bundle) in self.bundles.iter().enumerate() {
            let expected_id = AllocationBundleId(u32::try_from(index).map_err(|_| {
                IntervalAllocationError::new(
                    "INTERVAL_ALLOC.BUNDLE_ID_RANGE",
                    None,
                    None,
                    "allocation bundle count exceeds u32",
                )
            })?);
            let Some(root) = graph.bundles.get(bundle.root.0 as usize) else {
                return Err(IntervalAllocationError::new(
                    "INTERVAL_ALLOC.ROOT_RANGE",
                    None,
                    Some(bundle.id),
                    "allocation bundle references a missing HomeGraph root",
                ));
            };
            if bundle.id != expected_id
                || bundle.origin != root.origin
                || bundle.definition != root.definition
            {
                return Err(IntervalAllocationError::new(
                    "INTERVAL_ALLOC.BUNDLE_IDENTITY",
                    Some(root.definition.block()),
                    Some(bundle.id),
                    "allocation bundle identity or machine-value metadata is inconsistent",
                ));
            }

            if let Some(parent) = bundle.parent {
                if index < graph.bundles.len()
                    || parent.0 as usize >= index
                    || self.bundles[parent.0 as usize].root != bundle.root
                    || !bundle.segments.is_empty()
                {
                    return Err(IntervalAllocationError::new(
                        "INTERVAL_ALLOC.CHILD_SHAPE",
                        Some(root.definition.block()),
                        Some(bundle.id),
                        "home child has an invalid parent, root, or live segment",
                    ));
                }
                let BundleAssignment::Home(selection) = &bundle.assignment else {
                    return Err(IntervalAllocationError::new(
                        "INTERVAL_ALLOC.CHILD_ASSIGNMENT",
                        Some(root.definition.block()),
                        Some(bundle.id),
                        "current split child must carry one materialization home",
                    ));
                };
                if bundle.uses.is_empty()
                    || bundle.spill_cost != selection.total_cost()
                    || !graph.candidates[bundle.root.0 as usize]
                        .iter()
                        .any(|candidate| candidate_covers(candidate, &bundle.uses, selection))
                {
                    return Err(IntervalAllocationError::new(
                        "INTERVAL_ALLOC.HOME_SELECTION",
                        Some(root.definition.block()),
                        Some(bundle.id),
                        "split child home does not exactly cover its use subset",
                    ));
                }
                continue;
            }

            let expected_uses = root.uses.iter().map(|use_| use_.id).collect::<Vec<_>>();
            if index >= graph.bundles.len()
                || bundle.root.0 as usize != index
                || bundle.segments != root.segments
                || bundle.uses != expected_uses
            {
                return Err(IntervalAllocationError::new(
                    "INTERVAL_ALLOC.ROOT_MATCH",
                    Some(root.definition.block()),
                    Some(bundle.id),
                    "root allocation bundle differs from its HomeGraph root",
                ));
            }
            let partition = if bundle.uses.is_empty() {
                None
            } else {
                Some(partition_homes(graph, bundle.root, &bundle.uses)?)
            };
            let expected_cost = partition
                .as_ref()
                .map_or(0, |partition| partition.total_cost);
            if bundle.spill_cost != expected_cost {
                return Err(IntervalAllocationError::new(
                    "INTERVAL_ALLOC.SPILL_COST",
                    Some(bundle.definition.block()),
                    Some(bundle.id),
                    "bundle spill cost differs from its cheapest complete home",
                ));
            }
            match &bundle.assignment {
                BundleAssignment::Register(register) => {
                    rebuilt
                        .assign(bundle.id, *register, &bundle.segments)
                        .map_err(IntervalAllocationError::union)?;
                }
                BundleAssignment::Home(selection) => {
                    let Some(partition) = &partition else {
                        return Err(IntervalAllocationError::new(
                            "INTERVAL_ALLOC.HOME_SELECTION",
                            Some(bundle.definition.block()),
                            Some(bundle.id),
                            "dead root unexpectedly has a materialization home",
                        ));
                    };
                    if partition.pieces.as_slice()
                        != [HomePiece {
                            uses: bundle.uses.clone(),
                            selection: selection.clone(),
                        }]
                    {
                        return Err(IntervalAllocationError::new(
                            "INTERVAL_ALLOC.HOME_SELECTION",
                            Some(bundle.definition.block()),
                            Some(bundle.id),
                            "unsplit home is not the exact minimum-cost home partition",
                        ));
                    }
                }
                BundleAssignment::Split { children } => {
                    let Some(partition) = &partition else {
                        return Err(IntervalAllocationError::new(
                            "INTERVAL_ALLOC.SPLIT_COVERAGE",
                            Some(bundle.definition.block()),
                            Some(bundle.id),
                            "dead root unexpectedly has split children",
                        ));
                    };
                    if partition.pieces.len() <= 1 || children.len() != partition.pieces.len() {
                        return Err(IntervalAllocationError::new(
                            "INTERVAL_ALLOC.SPLIT_COVERAGE",
                            Some(bundle.definition.block()),
                            Some(bundle.id),
                            "home split does not have one child per partition piece",
                        ));
                    }
                    let mut seen_uses = BTreeSet::new();
                    for (&child_id, piece) in children.iter().zip(&partition.pieces) {
                        let Some(child) = self.bundles.get(child_id.0 as usize) else {
                            return Err(IntervalAllocationError::new(
                                "INTERVAL_ALLOC.CHILD_RANGE",
                                Some(bundle.definition.block()),
                                Some(child_id),
                                "split references a missing child bundle",
                            ));
                        };
                        if child.parent != Some(bundle.id)
                            || child.root != bundle.root
                            || child.uses != piece.uses
                            || child.assignment != BundleAssignment::Home(piece.selection.clone())
                            || child.stage != bundle.stage
                            || child.uses.iter().any(|use_id| !seen_uses.insert(*use_id))
                        {
                            return Err(IntervalAllocationError::new(
                                "INTERVAL_ALLOC.SPLIT_COVERAGE",
                                Some(bundle.definition.block()),
                                Some(child_id),
                                "split child differs from its exact home partition",
                            ));
                        }
                    }
                    if seen_uses != bundle.uses.iter().copied().collect() {
                        return Err(IntervalAllocationError::new(
                            "INTERVAL_ALLOC.SPLIT_COVERAGE",
                            Some(bundle.definition.block()),
                            Some(bundle.id),
                            "split children do not partition every root use exactly once",
                        ));
                    }
                }
                BundleAssignment::Dead if bundle.uses.is_empty() => {}
                BundleAssignment::Dead => {
                    return Err(IntervalAllocationError::new(
                        "INTERVAL_ALLOC.LIVE_BUNDLE_DROPPED",
                        Some(bundle.definition.block()),
                        Some(bundle.id),
                        "used bundle was marked dead",
                    ));
                }
                BundleAssignment::Unassigned => {
                    return Err(IntervalAllocationError::new(
                        "INTERVAL_ALLOC.UNASSIGNED",
                        Some(bundle.definition.block()),
                        Some(bundle.id),
                        "allocation plan contains an unassigned bundle",
                    ));
                }
            }
        }
        rebuilt.verify().map_err(IntervalAllocationError::union)?;
        if self.matrix != rebuilt {
            return Err(IntervalAllocationError::new(
                "INTERVAL_ALLOC.MATRIX_MATCH",
                None,
                None,
                "cached interval unions differ from independently rebuilt assignments",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::native::mir::{
        BaseReg, MBlock, MFunction, MInst, OpSize, SpillDesc, VRegAllocator,
    };

    use super::super::home_graph;

    fn function(value_count: u32, insts: Vec<MInst>) -> MFunction {
        let mut values = VRegAllocator::new();
        for _ in 0..value_count {
            values.alloc();
        }
        let mut function =
            MFunction::new(values, vec![SpillDesc::transient(); value_count as usize]);
        let mut block = MBlock::new(BlockId(0));
        block.insts = insts;
        function.blocks.push(block);
        function
    }

    fn model(function: &mut MFunction) -> (NormalizedCfg, HomeGraph) {
        let cfg = super::super::cfg::normalize(function).unwrap();
        let graph = home_graph::build(function, &cfg).unwrap();
        (cfg, graph)
    }

    fn bundle_id(graph: &HomeGraph, value: VReg) -> AllocationBundleId {
        AllocationBundleId(
            graph
                .bundles
                .iter()
                .position(|bundle| bundle.origin == value)
                .unwrap() as u32,
        )
    }

    #[test]
    fn nonoverlapping_ssa_bundles_reuse_one_physical_register() {
        let insts = vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 7,
            },
            MInst::Mov {
                dst: VReg(1),
                src: VReg(0),
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 0,
                src: VReg(1),
                size: OpSize::S64,
            },
            MInst::Return,
        ];
        let mut function = function(2, insts);
        let (cfg, graph) = model(&mut function);
        let plan = allocate_roots(&graph, &cfg, &[PhysReg::RAX]).unwrap();
        for value in [VReg(0), VReg(1)] {
            assert_eq!(
                plan.bundles[bundle_id(&graph, value).0 as usize].assignment,
                BundleAssignment::Register(PhysReg::RAX)
            );
        }
    }

    #[test]
    fn one_register_sends_one_of_two_interfering_values_to_a_real_home() {
        let insts = vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 7,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 11,
            },
            MInst::Add {
                dst: VReg(2),
                lhs: VReg(0),
                rhs: VReg(1),
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 0,
                src: VReg(2),
                size: OpSize::S64,
            },
            MInst::Return,
        ];
        let mut function = function(3, insts);
        let (cfg, graph) = model(&mut function);
        let plan = allocate_roots(&graph, &cfg, &[PhysReg::RAX]).unwrap();
        let assignments = [VReg(0), VReg(1)]
            .map(|value| &plan.bundles[bundle_id(&graph, value).0 as usize].assignment);
        assert_eq!(
            assignments
                .iter()
                .filter(|assignment| matches!(assignment, BundleAssignment::Register(_)))
                .count(),
            1
        );
        assert_eq!(
            assignments
                .iter()
                .filter(|assignment| matches!(assignment, BundleAssignment::Home(_)))
                .count(),
            1
        );
    }

    #[test]
    fn lower_density_bundle_evicts_a_cheaper_short_resident_then_terminates() {
        let insts = vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 7,
            },
            MInst::Mov {
                dst: VReg(1),
                src: VReg(0),
            },
            MInst::LoadImm {
                dst: VReg(2),
                value: 11,
            },
            MInst::Mov {
                dst: VReg(3),
                src: VReg(2),
            },
            MInst::Mov {
                dst: VReg(4),
                src: VReg(0),
            },
            MInst::Mov {
                dst: VReg(5),
                src: VReg(0),
            },
            MInst::Return,
        ];
        let mut function = function(6, insts);
        let (cfg, graph) = model(&mut function);
        let plan = allocate_roots(&graph, &cfg, &[PhysReg::RAX]).unwrap();
        let long = &plan.bundles[bundle_id(&graph, VReg(0)).0 as usize];
        let short = &plan.bundles[bundle_id(&graph, VReg(2)).0 as usize];
        assert_eq!(long.assignment, BundleAssignment::Register(PhysReg::RAX));
        assert!(matches!(short.assignment, BundleAssignment::Home(_)));
        assert_eq!(short.stage, AllocationStage::Evicted);
        assert!(long.spill_cost > short.spill_cost);
    }

    #[test]
    fn transactional_recolor_moves_a_disjoint_resident_before_spilling() {
        let insts = vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 1,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 2,
            },
            MInst::Mov {
                dst: VReg(2),
                src: VReg(1),
            },
            MInst::LoadImm {
                dst: VReg(3),
                value: 3,
            },
            MInst::Mov {
                dst: VReg(4),
                src: VReg(3),
            },
            MInst::Mov {
                dst: VReg(5),
                src: VReg(0),
            },
            MInst::Return,
        ];
        let mut function = function(6, insts);
        let (cfg, graph) = model(&mut function);
        let mut allocator = Allocator::new(&graph, &cfg, &[PhysReg::RAX, PhysReg::RDX]).unwrap();
        let candidate = bundle_id(&graph, VReg(0));
        let early = bundle_id(&graph, VReg(1));
        let late = bundle_id(&graph, VReg(3));
        allocator.assign_register(early, PhysReg::RAX).unwrap();
        allocator.assign_register(late, PhysReg::RDX).unwrap();

        assert!(allocator.try_recolor(candidate).unwrap());
        assert_eq!(allocator.matrix.register(candidate), Some(PhysReg::RAX));
        assert_eq!(allocator.matrix.register(early), Some(PhysReg::RDX));
        assert_eq!(allocator.matrix.register(late), Some(PhysReg::RDX));
        allocator.matrix.verify().unwrap();
    }

    #[test]
    fn path_specific_state_recipe_and_stack_fallback_form_separate_home_children() {
        let insts = vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
            },
            MInst::Mov {
                dst: VReg(1),
                src: VReg(0),
            },
            MInst::LoadImm {
                dst: VReg(2),
                value: 0,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 0,
                src: VReg(2),
                size: OpSize::S64,
            },
            MInst::Mov {
                dst: VReg(3),
                src: VReg(0),
            },
            MInst::Return,
        ];
        let mut function = function(4, insts);
        let (_, graph) = model(&mut function);
        let root = graph
            .bundles
            .iter()
            .find(|bundle| bundle.origin == VReg(0))
            .unwrap();
        let uses = root.uses.iter().map(|use_| use_.id).collect::<Vec<_>>();
        let partition = partition_homes(&graph, root.id, &uses).unwrap();
        assert_eq!(partition.pieces.len(), 2);
        let stack = partition
            .pieces
            .iter()
            .find(|piece| piece.selection.kind == HomeKind::Stack)
            .unwrap();
        let state = partition
            .pieces
            .iter()
            .find(|piece| matches!(piece.selection.kind, HomeKind::State(_)))
            .unwrap();
        assert_eq!(state.uses, vec![BundleUseId(0)]);
        assert_eq!(stack.uses, vec![BundleUseId(1)]);
        assert_eq!(partition.total_cost, 3);
    }
}
