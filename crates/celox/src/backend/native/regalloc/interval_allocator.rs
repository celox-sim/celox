//! Allocation policy over sparse physical interval unions.
//!
//! This is intentionally separate from MIR reconstruction. It chooses
//! register residency or one proved home for complete live bundles, and it
//! records every eviction/recoloring decision in allocation-owned state.
//! Later slices split bundles and lower the selected transitions into SSA.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BinaryHeap};
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

fn select_home(
    graph: &HomeGraph,
    root: LiveBundleId,
    uses: &[BundleUseId],
) -> Result<HomeSelection, IntervalAllocationError> {
    let Some(candidates) = graph.candidates.get(root.0 as usize) else {
        return Err(IntervalAllocationError::new(
            "INTERVAL_ALLOC.HOME_ROOT",
            None,
            None,
            format!("root bundle {root:?} has no home-candidate row"),
        ));
    };
    let mut best = None::<HomeSelection>;
    for candidate in candidates {
        if candidate.kind == HomeKind::Register
            || !uses
                .iter()
                .all(|use_id| candidate.uses.binary_search(use_id).is_ok())
        {
            continue;
        }
        let materializations = candidate
            .materializations
            .iter()
            .copied()
            .filter(|item| uses.binary_search(&item.use_id).is_ok())
            .collect::<Vec<_>>();
        let materialization_cost = match candidate.kind {
            HomeKind::Stack => u32::try_from(uses.len()).unwrap_or(u32::MAX),
            HomeKind::Rematerialize(_) | HomeKind::State(_) => {
                if materializations.len() != uses.len() {
                    continue;
                }
                materializations
                    .iter()
                    .fold(0_u32, |cost, item| cost.saturating_add(item.cost))
            }
            HomeKind::Register => unreachable!(),
        };
        let selection = HomeSelection {
            kind: candidate.kind,
            materializations,
            creation_cost: candidate.creation_cost,
            materialization_cost,
        };
        let key = (selection.total_cost(), home_rank(selection.kind));
        if best
            .as_ref()
            .is_none_or(|current| key < (current.total_cost(), home_rank(current.kind)))
        {
            best = Some(selection);
        }
    }
    best.ok_or_else(|| {
        IntervalAllocationError::new(
            "INTERVAL_ALLOC.NO_HOME",
            None,
            None,
            format!("root bundle {root:?} has no home covering uses {uses:?}"),
        )
    })
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
                    select_home(graph, root.id, &uses)?.total_cost(),
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
        let home = select_home(self.graph, bundle.root, &bundle.uses)?;
        self.bundles[id.0 as usize].assignment = BundleAssignment::Home(home);
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
        if self.bundles.len() != graph.bundles.len() {
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
            let expected_uses = root.uses.iter().map(|use_| use_.id).collect::<Vec<_>>();
            if bundle.id != expected_id
                || bundle.root.0 as usize != index
                || bundle.parent.is_some()
                || bundle.origin != root.origin
                || bundle.definition != root.definition
                || bundle.segments != root.segments
                || bundle.uses != expected_uses
            {
                return Err(IntervalAllocationError::new(
                    "INTERVAL_ALLOC.ROOT_MATCH",
                    Some(root.definition.block()),
                    Some(bundle.id),
                    "unsplit allocation bundle differs from its HomeGraph root",
                ));
            }

            let expected_cost = if bundle.uses.is_empty() {
                0
            } else {
                select_home(graph, bundle.root, &bundle.uses)?.total_cost()
            };
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
                    if bundle.uses.is_empty()
                        || !graph.candidates[bundle.root.0 as usize]
                            .iter()
                            .any(|candidate| candidate_covers(candidate, &bundle.uses, selection))
                        || selection != &select_home(graph, bundle.root, &bundle.uses)?
                    {
                        return Err(IntervalAllocationError::new(
                            "INTERVAL_ALLOC.HOME_SELECTION",
                            Some(bundle.definition.block()),
                            Some(bundle.id),
                            "selected home is not the cheapest candidate covering every use",
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
}
