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
    BundleUseId, HomeGraph, HomeKind, LiveBundleId, STACK_HOME_CREATION_COST,
    STACK_HOME_MATERIALIZATION_COST, UseMaterialization,
};
use super::interval_union::{
    AllocationBundleId, IntervalUnionError, LiveIntervalMatrix, live_length,
};
use super::live_interval::{DefinitionSite, LiveSegment, UseSite};

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
pub(super) struct BundleTransition {
    /// Existing use point at which this exact recipe is proved.
    pub at: UseSite,
    pub home: HomeSelection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum BundleAssignment {
    Unassigned,
    Register(PhysReg),
    Home(HomeSelection),
    Split {
        children: Vec<AllocationBundleId>,
        /// Stack-backed children and transitions share one logical home.
        stack_home_created: bool,
    },
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
    pub transitions: Vec<BundleTransition>,
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
    let Some(homes) = graph.homes.get(root.0 as usize) else {
        return Err(IntervalAllocationError::new(
            "INTERVAL_ALLOC.HOME_ROOT",
            None,
            None,
            format!("root bundle {root:?} has no use-home row"),
        ));
    };
    let build = |stack_available: bool| -> Option<HomePartition> {
        let mut grouped = BTreeMap::<HomeKind, (Vec<BundleUseId>, Vec<UseMaterialization>)>::new();
        for &use_id in uses {
            let mut best = None::<(u32, u8, HomeKind, Option<UseMaterialization>)>;
            if stack_available {
                best = Some((
                    STACK_HOME_MATERIALIZATION_COST,
                    home_rank(HomeKind::Stack),
                    HomeKind::Stack,
                    None,
                ));
            }
            let options = homes.uses.get(use_id.0 as usize)?;
            for option in options {
                let materialization = UseMaterialization {
                    use_id,
                    recipe: option.recipe,
                    cost: option.cost,
                };
                let choice = (
                    materialization.cost,
                    home_rank(option.kind),
                    option.kind,
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
            Some(STACK_HOME_CREATION_COST)
        } else {
            None
        };
        let mut total_cost = u64::from(stack_creation.unwrap_or(0));
        let mut pieces = Vec::with_capacity(grouped.len());
        for (kind, (piece_uses, materializations)) in grouped {
            let materialization_cost = match kind {
                HomeKind::Stack => STACK_HOME_MATERIALIZATION_COST
                    .saturating_mul(u32::try_from(piece_uses.len()).unwrap_or(u32::MAX)),
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

fn selection_covers(
    graph: &HomeGraph,
    root: LiveBundleId,
    uses: &[BundleUseId],
    selection: &HomeSelection,
) -> bool {
    let Some(homes) = graph.homes.get(root.0 as usize) else {
        return false;
    };
    match selection.kind {
        HomeKind::Register => false,
        HomeKind::Stack => {
            selection.creation_cost == STACK_HOME_CREATION_COST
                && selection.materialization_cost
                    == STACK_HOME_MATERIALIZATION_COST
                        .saturating_mul(u32::try_from(uses.len()).unwrap_or(u32::MAX))
                && selection.materializations.is_empty()
        }
        HomeKind::Rematerialize(_) | HomeKind::State(_) => {
            selection.creation_cost == 0
                && selection.materializations.len() == uses.len()
                && uses
                    .iter()
                    .zip(&selection.materializations)
                    .all(|(&use_id, materialization)| {
                        materialization.use_id == use_id
                            && homes.uses.get(use_id.0 as usize).is_some_and(|options| {
                                options.iter().any(|option| {
                                    option.kind == selection.kind
                                        && option.recipe == materialization.recipe
                                        && option.cost == materialization.cost
                                })
                            })
                    })
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

#[derive(Debug)]
struct Dominance {
    enter: Vec<usize>,
    exit: Vec<usize>,
}

impl Dominance {
    fn new(cfg: &NormalizedCfg) -> Result<Self, IntervalAllocationError> {
        let blocks = cfg.idom.len();
        if blocks == 0 {
            return Err(IntervalAllocationError::new(
                "INTERVAL_ALLOC.DOMINATOR_TREE",
                None,
                None,
                "allocator requires a non-empty dominator tree",
            ));
        }
        let mut children = vec![Vec::new(); blocks];
        for block in 1..blocks {
            let Some(parent) = cfg.idom[block] else {
                return Err(IntervalAllocationError::new(
                    "INTERVAL_ALLOC.DOMINATOR_TREE",
                    None,
                    None,
                    format!("reachable block {block} lacks an immediate dominator"),
                ));
            };
            if parent >= blocks {
                return Err(IntervalAllocationError::new(
                    "INTERVAL_ALLOC.DOMINATOR_TREE",
                    None,
                    None,
                    format!("block {block} has out-of-range dominator {parent}"),
                ));
            }
            children[parent].push(block);
        }
        let mut enter = vec![0; blocks];
        let mut exit = vec![0; blocks];
        let mut clock = 0usize;
        let mut work = vec![(0usize, false)];
        while let Some((block, leaving)) = work.pop() {
            if leaving {
                exit[block] = clock;
                clock = clock.checked_add(1).ok_or_else(|| {
                    IntervalAllocationError::new(
                        "INTERVAL_ALLOC.DOMINATOR_TREE",
                        None,
                        None,
                        "dominator traversal index overflows usize",
                    )
                })?;
                continue;
            }
            enter[block] = clock;
            clock = clock.checked_add(1).ok_or_else(|| {
                IntervalAllocationError::new(
                    "INTERVAL_ALLOC.DOMINATOR_TREE",
                    None,
                    None,
                    "dominator traversal index overflows usize",
                )
            })?;
            work.push((block, true));
            work.extend(children[block].iter().rev().map(|child| (*child, false)));
        }
        Ok(Self { enter, exit })
    }

    fn block_dominates(&self, dominator: usize, block: usize) -> bool {
        self.enter[dominator] <= self.enter[block] && self.exit[block] <= self.exit[dominator]
    }

    fn use_dominates(&self, cfg: &NormalizedCfg, dominator: UseSite, use_: UseSite) -> bool {
        let UseSite::Instruction {
            block: dominator_block,
            slot: dominator_slot,
            ..
        } = dominator
        else {
            return false;
        };
        let Some(&dominator_index) = cfg.block_index.get(&dominator_block) else {
            return false;
        };
        let Some(&use_index) = cfg.block_index.get(&use_.block()) else {
            return false;
        };
        if dominator_index == use_index {
            dominator_slot <= use_.slot()
        } else {
            self.block_dominates(dominator_index, use_index)
        }
    }
}

#[derive(Debug)]
struct RegionCandidate {
    register: PhysReg,
    register_order: usize,
    segments: Vec<LiveSegment>,
    uses: Vec<BundleUseId>,
    transition: BundleTransition,
    remaining: HomePartition,
    total_cost: u64,
    stack_home_created: bool,
}

fn segment_node_at(
    nodes: &[usize],
    segments: &[LiveSegment],
    slot: super::live_interval::SlotIndex,
) -> Option<usize> {
    let position = nodes.partition_point(|&node| segments[node].start <= slot);
    (position != 0)
        .then(|| nodes[position - 1])
        .filter(|&node| segments[node].contains(slot))
}

struct Allocator<'a> {
    graph: &'a HomeGraph,
    cfg: &'a NormalizedCfg,
    dominance: Dominance,
    registers: Vec<PhysReg>,
    matrix: LiveIntervalMatrix,
    bundles: Vec<AllocatedBundle>,
    queue: BinaryHeap<QueueItem>,
}

impl<'a> Allocator<'a> {
    fn new(
        graph: &'a HomeGraph,
        cfg: &'a NormalizedCfg,
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
                transitions: Vec::new(),
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
            cfg,
            dominance: Dominance::new(cfg)?,
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

    fn region_candidates(
        &self,
        id: AllocationBundleId,
        register: PhysReg,
        register_order: usize,
    ) -> Result<Vec<RegionCandidate>, IntervalAllocationError> {
        let bundle = self.bundle(id)?;
        let root = &self.graph.bundles[bundle.root.0 as usize];
        let free = self
            .matrix
            .free_segments(register, &bundle.segments)
            .map_err(IntervalAllocationError::union)?;
        if free.is_empty() {
            return Ok(Vec::new());
        }

        let mut block_nodes = (0..self.cfg.successors.len())
            .map(|_| Vec::new())
            .collect::<Vec<Vec<usize>>>();
        for (node, segment) in free.iter().enumerate() {
            let Some(&block) = self.cfg.block_index.get(&segment.block) else {
                return Err(IntervalAllocationError::new(
                    "INTERVAL_ALLOC.SPLIT_BLOCK",
                    Some(segment.block),
                    Some(id),
                    "free segment references a block outside the CFG",
                ));
            };
            block_nodes[block].push(node);
        }
        let mut forward = vec![Vec::new(); free.len()];
        let mut reverse = vec![Vec::new(); free.len()];
        for (block, successors) in self.cfg.successors.iter().enumerate() {
            let source_slot = self.graph.intervals.block_slots[block].exit;
            let source = segment_node_at(&block_nodes[block], &free, source_slot);
            let Some(source) = source else {
                continue;
            };
            for &successor in successors {
                let target_slot = self.graph.intervals.block_slots[successor].entry;
                if let Some(target) = segment_node_at(&block_nodes[successor], &free, target_slot) {
                    forward[source].push(target);
                    reverse[target].push(source);
                }
            }
        }

        let mut use_nodes = BTreeMap::new();
        for &use_id in &bundle.uses {
            let Some(use_) = root.uses.get(use_id.0 as usize) else {
                return Err(IntervalAllocationError::new(
                    "INTERVAL_ALLOC.USE_RANGE",
                    Some(bundle.definition.block()),
                    Some(id),
                    format!("bundle use {use_id:?} is outside its HomeGraph root"),
                ));
            };
            let Some(&block) = self.cfg.block_index.get(&use_.site.block()) else {
                continue;
            };
            if let Some(node) = segment_node_at(&block_nodes[block], &free, use_.site.slot()) {
                use_nodes.insert(use_id, node);
            }
        }

        let mut candidates = Vec::new();
        for &entry_use in &bundle.uses {
            let Some(&entry_node) = use_nodes.get(&entry_use) else {
                continue;
            };
            let entry_site = root.uses[entry_use.0 as usize].site;
            if !matches!(entry_site, UseSite::Instruction { .. }) {
                continue;
            }
            let mut reachable = vec![false; free.len()];
            let mut work = vec![entry_node];
            reachable[entry_node] = true;
            while let Some(node) = work.pop() {
                for &successor in &forward[node] {
                    if !reachable[successor] {
                        reachable[successor] = true;
                        work.push(successor);
                    }
                }
            }
            let covered = bundle
                .uses
                .iter()
                .copied()
                .filter(|use_id| {
                    use_nodes.get(use_id).is_some_and(|&node| reachable[node])
                        && self.dominance.use_dominates(
                            self.cfg,
                            entry_site,
                            root.uses[use_id.0 as usize].site,
                        )
                })
                .collect::<Vec<_>>();
            if covered.len() < 2 || covered.binary_search(&entry_use).is_err() {
                continue;
            }

            let mut needed = vec![false; free.len()];
            let mut work = covered
                .iter()
                .filter_map(|use_id| use_nodes.get(use_id).copied())
                .collect::<Vec<_>>();
            for &node in &work {
                needed[node] = true;
            }
            while let Some(node) = work.pop() {
                for &predecessor in &reverse[node] {
                    if reachable[predecessor] && !needed[predecessor] {
                        needed[predecessor] = true;
                        work.push(predecessor);
                    }
                }
            }
            let mut segments = free
                .iter()
                .copied()
                .enumerate()
                .filter_map(|(node, mut segment)| {
                    if !needed[node] {
                        return None;
                    }
                    if node == entry_node {
                        segment.start = segment.start.max(entry_site.slot());
                    }
                    (segment.start < segment.end).then_some(segment)
                })
                .collect::<Vec<_>>();
            segments.sort_unstable_by_key(|segment| (segment.block, segment.start));
            if !covered.iter().all(|use_id| {
                let site = root.uses[use_id.0 as usize].site;
                segments
                    .iter()
                    .any(|segment| segment.block == site.block() && segment.contains(site.slot()))
            }) {
                continue;
            }

            let entry_partition = partition_homes(self.graph, bundle.root, &[entry_use])?;
            let [entry_piece] = entry_partition.pieces.as_slice() else {
                return Err(IntervalAllocationError::new(
                    "INTERVAL_ALLOC.TRANSITION_HOME",
                    Some(entry_site.block()),
                    Some(id),
                    "one transition use produced more than one home piece",
                ));
            };
            let remaining_uses = bundle
                .uses
                .iter()
                .copied()
                .filter(|use_id| covered.binary_search(use_id).is_err())
                .collect::<Vec<_>>();
            let remaining = partition_homes(self.graph, bundle.root, &remaining_uses)?;
            let entry_uses_stack = entry_piece.selection.kind == HomeKind::Stack;
            let remaining_uses_stack = remaining
                .pieces
                .iter()
                .any(|piece| piece.selection.kind == HomeKind::Stack);
            let mut total_cost = entry_partition
                .total_cost
                .saturating_add(remaining.total_cost);
            if entry_uses_stack && remaining_uses_stack {
                total_cost = total_cost.saturating_sub(u64::from(STACK_HOME_CREATION_COST));
            }
            candidates.push(RegionCandidate {
                register,
                register_order,
                segments,
                uses: covered,
                transition: BundleTransition {
                    at: entry_site,
                    home: entry_piece.selection.clone(),
                },
                remaining,
                total_cost,
                stack_home_created: entry_uses_stack || remaining_uses_stack,
            });
        }
        Ok(candidates)
    }

    fn try_split(&mut self, id: AllocationBundleId) -> Result<bool, IntervalAllocationError> {
        let baseline = self.bundle(id)?.spill_cost;
        let mut best = None::<RegionCandidate>;
        for (order, register) in self.registers.iter().copied().enumerate() {
            for candidate in self.region_candidates(id, register, order)? {
                if candidate.total_cost >= baseline {
                    continue;
                }
                let candidate_key = (
                    candidate.total_cost,
                    std::cmp::Reverse(candidate.uses.len()),
                    candidate.register_order,
                    candidate.transition.at,
                );
                if best.as_ref().is_none_or(|current| {
                    candidate_key
                        < (
                            current.total_cost,
                            std::cmp::Reverse(current.uses.len()),
                            current.register_order,
                            current.transition.at,
                        )
                }) {
                    best = Some(candidate);
                }
            }
        }
        let Some(candidate) = best else {
            return Ok(false);
        };

        let parent = self.bundle(id)?.clone();
        let register_child =
            AllocationBundleId(u32::try_from(self.bundles.len()).map_err(|_| {
                IntervalAllocationError::new(
                    "INTERVAL_ALLOC.BUNDLE_ID_RANGE",
                    Some(parent.definition.block()),
                    Some(id),
                    "split bundle count exceeds u32",
                )
            })?);
        let register_spill_cost =
            partition_homes(self.graph, parent.root, &candidate.uses)?.total_cost;
        self.bundles.push(AllocatedBundle {
            id: register_child,
            root: parent.root,
            parent: Some(id),
            origin: parent.origin,
            definition: parent.definition,
            segments: candidate.segments.clone(),
            uses: candidate.uses,
            stage: parent.stage,
            spill_cost: register_spill_cost,
            transitions: vec![candidate.transition],
            assignment: BundleAssignment::Register(candidate.register),
        });
        self.matrix
            .assign(register_child, candidate.register, &candidate.segments)
            .map_err(IntervalAllocationError::union)?;

        let mut children = vec![register_child];
        for piece in candidate.remaining.pieces {
            let child = AllocationBundleId(u32::try_from(self.bundles.len()).map_err(|_| {
                IntervalAllocationError::new(
                    "INTERVAL_ALLOC.BUNDLE_ID_RANGE",
                    Some(parent.definition.block()),
                    Some(id),
                    "split bundle count exceeds u32",
                )
            })?);
            self.bundles.push(AllocatedBundle {
                id: child,
                root: parent.root,
                parent: Some(id),
                origin: parent.origin,
                definition: parent.definition,
                segments: Vec::new(),
                uses: piece.uses,
                stage: parent.stage,
                spill_cost: piece.selection.total_cost(),
                transitions: Vec::new(),
                assignment: BundleAssignment::Home(piece.selection),
            });
            children.push(child);
        }
        self.bundles[id.0 as usize].assignment = BundleAssignment::Split {
            children,
            stack_home_created: candidate.stack_home_created,
        };
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

        let stack_home_created = partition
            .pieces
            .iter()
            .any(|piece| piece.selection.kind == HomeKind::Stack);
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
                transitions: Vec::new(),
                assignment: BundleAssignment::Home(piece.selection),
            });
            children.push(child);
        }
        self.bundles[id.0 as usize].assignment = BundleAssignment::Split {
            children,
            stack_home_created,
        };
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
                || self.try_split(item.id)?
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
        let dominance = Dominance::new(cfg)?;
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
                    || self.bundles[parent.0 as usize].stage != bundle.stage
                    || bundle.uses.is_empty()
                    || bundle.uses.windows(2).any(|pair| pair[0] >= pair[1])
                {
                    return Err(IntervalAllocationError::new(
                        "INTERVAL_ALLOC.CHILD_SHAPE",
                        Some(root.definition.block()),
                        Some(bundle.id),
                        "split child has an invalid parent, root, stage, or use set",
                    ));
                }
                match &bundle.assignment {
                    BundleAssignment::Home(selection) => {
                        if !bundle.segments.is_empty()
                            || !bundle.transitions.is_empty()
                            || bundle.spill_cost != selection.total_cost()
                            || !selection_covers(graph, bundle.root, &bundle.uses, selection)
                        {
                            return Err(IntervalAllocationError::new(
                                "INTERVAL_ALLOC.HOME_SELECTION",
                                Some(root.definition.block()),
                                Some(bundle.id),
                                "split child home does not exactly cover its use subset",
                            ));
                        }
                    }
                    BundleAssignment::Register(register) => {
                        let [transition] = bundle.transitions.as_slice() else {
                            return Err(IntervalAllocationError::new(
                                "INTERVAL_ALLOC.REGION_TRANSITION",
                                Some(root.definition.block()),
                                Some(bundle.id),
                                "register split child requires exactly one proved transition",
                            ));
                        };
                        let entry_use = root
                            .uses
                            .iter()
                            .find(|use_| use_.site == transition.at)
                            .map(|use_| use_.id);
                        let Some(entry_use) = entry_use else {
                            return Err(IntervalAllocationError::new(
                                "INTERVAL_ALLOC.REGION_TRANSITION",
                                Some(transition.at.block()),
                                Some(bundle.id),
                                "transition point is not a use of the HomeGraph root",
                            ));
                        };
                        let expected_entry = partition_homes(graph, bundle.root, &[entry_use])?;
                        if !bundle.uses.contains(&entry_use)
                            || expected_entry.pieces.len() != 1
                            || expected_entry.pieces[0].selection != transition.home
                            || bundle.segments.is_empty()
                            || bundle.spill_cost
                                != partition_homes(graph, bundle.root, &bundle.uses)?.total_cost
                            || !bundle.segments.iter().all(|segment| {
                                root.segments.iter().any(|root_segment| {
                                    root_segment.block == segment.block
                                        && root_segment.start <= segment.start
                                        && segment.end <= root_segment.end
                                })
                            })
                            || !bundle.uses.iter().all(|use_id| {
                                let site = root.uses[use_id.0 as usize].site;
                                dominance.use_dominates(cfg, transition.at, site)
                                    && bundle.segments.iter().any(|segment| {
                                        segment.block == site.block()
                                            && segment.contains(site.slot())
                                    })
                            })
                        {
                            return Err(IntervalAllocationError::new(
                                "INTERVAL_ALLOC.REGION_SHAPE",
                                Some(transition.at.block()),
                                Some(bundle.id),
                                "register child is not a dominated subrange of its root",
                            ));
                        }
                        rebuilt
                            .assign(bundle.id, *register, &bundle.segments)
                            .map_err(IntervalAllocationError::union)?;
                    }
                    BundleAssignment::Unassigned
                    | BundleAssignment::Split { .. }
                    | BundleAssignment::Dead => {
                        return Err(IntervalAllocationError::new(
                            "INTERVAL_ALLOC.CHILD_ASSIGNMENT",
                            Some(root.definition.block()),
                            Some(bundle.id),
                            "split child has no final register or home assignment",
                        ));
                    }
                }
                continue;
            }

            let expected_uses = root.uses.iter().map(|use_| use_.id).collect::<Vec<_>>();
            if index >= graph.bundles.len()
                || bundle.root.0 as usize != index
                || bundle.segments != root.segments
                || bundle.uses != expected_uses
                || !bundle.transitions.is_empty()
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
                BundleAssignment::Split {
                    children,
                    stack_home_created,
                } => {
                    let Some(partition) = &partition else {
                        return Err(IntervalAllocationError::new(
                            "INTERVAL_ALLOC.SPLIT_COVERAGE",
                            Some(bundle.definition.block()),
                            Some(bundle.id),
                            "dead root unexpectedly has split children",
                        ));
                    };
                    if children.is_empty() {
                        return Err(IntervalAllocationError::new(
                            "INTERVAL_ALLOC.SPLIT_COVERAGE",
                            Some(bundle.definition.block()),
                            Some(bundle.id),
                            "split root has no children",
                        ));
                    }
                    let mut seen_uses = BTreeSet::new();
                    let mut seen_children = BTreeSet::new();
                    let mut register_children = Vec::new();
                    let mut home_children = Vec::new();
                    let mut expected_stack_home = false;
                    for &child_id in children {
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
                            || child.stage != bundle.stage
                            || !seen_children.insert(child_id)
                            || child.uses.iter().any(|use_id| !seen_uses.insert(*use_id))
                        {
                            return Err(IntervalAllocationError::new(
                                "INTERVAL_ALLOC.SPLIT_COVERAGE",
                                Some(bundle.definition.block()),
                                Some(child_id),
                                "split child identity or use ownership is inconsistent",
                            ));
                        }
                        match &child.assignment {
                            BundleAssignment::Register(_) => {
                                expected_stack_home |= child
                                    .transitions
                                    .iter()
                                    .any(|transition| transition.home.kind == HomeKind::Stack);
                                register_children.push(child);
                            }
                            BundleAssignment::Home(selection) => {
                                expected_stack_home |= selection.kind == HomeKind::Stack;
                                home_children.push(child);
                            }
                            _ => {
                                return Err(IntervalAllocationError::new(
                                    "INTERVAL_ALLOC.CHILD_ASSIGNMENT",
                                    Some(bundle.definition.block()),
                                    Some(child_id),
                                    "split root references a non-final child",
                                ));
                            }
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
                    if *stack_home_created != expected_stack_home {
                        return Err(IntervalAllocationError::new(
                            "INTERVAL_ALLOC.STACK_HOME_SHARING",
                            Some(bundle.definition.block()),
                            Some(bundle.id),
                            "split stack-home identity differs from its children and transitions",
                        ));
                    }

                    if register_children.is_empty() {
                        if partition.pieces.len() <= 1
                            || home_children.len() != partition.pieces.len()
                            || home_children
                                .iter()
                                .zip(&partition.pieces)
                                .any(|(child, piece)| {
                                    child.uses != piece.uses
                                        || child.assignment
                                            != BundleAssignment::Home(piece.selection.clone())
                                })
                        {
                            return Err(IntervalAllocationError::new(
                                "INTERVAL_ALLOC.HOME_PARTITION",
                                Some(bundle.definition.block()),
                                Some(bundle.id),
                                "home-only children differ from the exact minimum partition",
                            ));
                        }
                    } else {
                        let [register_child] = register_children.as_slice() else {
                            return Err(IntervalAllocationError::new(
                                "INTERVAL_ALLOC.REGION_COUNT",
                                Some(bundle.definition.block()),
                                Some(bundle.id),
                                "current region split must contain exactly one register child",
                            ));
                        };
                        let remaining_uses = bundle
                            .uses
                            .iter()
                            .copied()
                            .filter(|use_id| register_child.uses.binary_search(use_id).is_err())
                            .collect::<Vec<_>>();
                        let remaining_partition =
                            partition_homes(graph, bundle.root, &remaining_uses)?;
                        if home_children.len() != remaining_partition.pieces.len()
                            || home_children.iter().zip(&remaining_partition.pieces).any(
                                |(child, piece)| {
                                    child.uses != piece.uses
                                        || child.assignment
                                            != BundleAssignment::Home(piece.selection.clone())
                                },
                            )
                        {
                            return Err(IntervalAllocationError::new(
                                "INTERVAL_ALLOC.REGION_REMAINDER",
                                Some(bundle.definition.block()),
                                Some(bundle.id),
                                "region split remainder differs from its exact home partition",
                            ));
                        }
                        let [transition] = register_child.transitions.as_slice() else {
                            unreachable!("register child was structurally checked above");
                        };
                        let entry_use = root
                            .uses
                            .iter()
                            .find(|use_| use_.site == transition.at)
                            .map(|use_| use_.id)
                            .expect("register-child transition was structurally checked above");
                        let entry_partition = partition_homes(graph, bundle.root, &[entry_use])?;
                        let transition_uses_stack = transition.home.kind == HomeKind::Stack;
                        let remainder_uses_stack = remaining_partition
                            .pieces
                            .iter()
                            .any(|piece| piece.selection.kind == HomeKind::Stack);
                        let mut split_cost = entry_partition
                            .total_cost
                            .saturating_add(remaining_partition.total_cost);
                        if transition_uses_stack && remainder_uses_stack {
                            split_cost =
                                split_cost.saturating_sub(u64::from(STACK_HOME_CREATION_COST));
                        }
                        if split_cost >= bundle.spill_cost {
                            return Err(IntervalAllocationError::new(
                                "INTERVAL_ALLOC.REGION_COST",
                                Some(transition.at.block()),
                                Some(bundle.id),
                                "region split does not reduce exact materialization cost",
                            ));
                        }
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

    #[test]
    fn free_suffix_region_keeps_dominated_use_cluster_in_one_register() {
        let insts = vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 1,
            },
            MInst::LoadImm {
                dst: VReg(2),
                value: 2,
            },
            MInst::Add {
                dst: VReg(1),
                lhs: VReg(0),
                rhs: VReg(2),
            },
            MInst::Mov {
                dst: VReg(3),
                src: VReg(2),
            },
            MInst::Mov {
                dst: VReg(4),
                src: VReg(2),
            },
            MInst::Mov {
                dst: VReg(5),
                src: VReg(0),
            },
            MInst::Mov {
                dst: VReg(6),
                src: VReg(0),
            },
            MInst::Return,
        ];
        let mut function = function(7, insts);
        let (cfg, graph) = model(&mut function);
        let plan = allocate_roots(&graph, &cfg, &[PhysReg::RAX]).unwrap();
        let candidate_id = bundle_id(&graph, VReg(0));
        let BundleAssignment::Split { children, .. } =
            &plan.bundles[candidate_id.0 as usize].assignment
        else {
            panic!("long bundle should split around the more valuable prefix resident");
        };
        let register_child = children
            .iter()
            .map(|child| &plan.bundles[child.0 as usize])
            .find(|child| matches!(child.assignment, BundleAssignment::Register(_)))
            .unwrap();
        assert_eq!(register_child.uses, vec![BundleUseId(1), BundleUseId(2)]);
        assert_eq!(register_child.transitions.len(), 1);
        assert_eq!(
            register_child.transitions[0].at,
            graph.bundles[candidate_id.0 as usize].uses[1].site
        );
        assert_eq!(
            register_child.segments[0].start,
            register_child.transitions[0].at.slot()
        );
        let home_child = children
            .iter()
            .map(|child| &plan.bundles[child.0 as usize])
            .find(|child| matches!(child.assignment, BundleAssignment::Home(_)))
            .unwrap();
        assert_eq!(home_child.uses, vec![BundleUseId(0)]);
    }

    #[test]
    fn cfg_connected_free_region_carries_one_transition_across_blocks() {
        let mut values = VRegAllocator::new();
        for _ in 0..7 {
            values.alloc();
        }
        let mut function = MFunction::new(values, vec![SpillDesc::transient(); 7]);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: VReg(0),
            value: 1,
        });
        entry.push(MInst::LoadImm {
            dst: VReg(1),
            value: 2,
        });
        entry.push(MInst::Add {
            dst: VReg(2),
            lhs: VReg(0),
            rhs: VReg(1),
        });
        entry.push(MInst::Mov {
            dst: VReg(3),
            src: VReg(1),
        });
        entry.push(MInst::Mov {
            dst: VReg(4),
            src: VReg(1),
        });
        entry.push(MInst::Jump { target: BlockId(1) });
        let mut middle = MBlock::new(BlockId(1));
        middle.push(MInst::Mov {
            dst: VReg(5),
            src: VReg(0),
        });
        middle.push(MInst::Jump { target: BlockId(2) });
        let mut exit = MBlock::new(BlockId(2));
        exit.push(MInst::Mov {
            dst: VReg(6),
            src: VReg(0),
        });
        exit.push(MInst::Return);
        function.blocks = vec![entry, middle, exit];

        let (cfg, graph) = model(&mut function);
        let plan = allocate_roots(&graph, &cfg, &[PhysReg::RAX]).unwrap();
        let candidate = bundle_id(&graph, VReg(0));
        let BundleAssignment::Split { children, .. } =
            &plan.bundles[candidate.0 as usize].assignment
        else {
            panic!("cross-block suffix should be a register region");
        };
        let register_child = children
            .iter()
            .map(|child| &plan.bundles[child.0 as usize])
            .find(|child| matches!(child.assignment, BundleAssignment::Register(_)))
            .unwrap();
        assert_eq!(register_child.uses, vec![BundleUseId(1), BundleUseId(2)]);
        assert_eq!(
            register_child
                .segments
                .iter()
                .map(|segment| segment.block)
                .collect::<Vec<_>>(),
            vec![BlockId(1), BlockId(2)]
        );
        assert_eq!(
            register_child.transitions[0].at,
            graph.bundles[candidate.0 as usize].uses[1].site
        );
    }

    #[test]
    fn one_arm_transition_never_claims_uses_from_its_sibling() {
        let mut values = VRegAllocator::new();
        for _ in 0..12 {
            values.alloc();
        }
        let mut function = MFunction::new(values, vec![SpillDesc::transient(); 12]);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: VReg(0),
            value: 1,
        });
        entry.push(MInst::LoadImm {
            dst: VReg(1),
            value: 2,
        });
        entry.push(MInst::Add {
            dst: VReg(2),
            lhs: VReg(0),
            rhs: VReg(1),
        });
        for destination in 3..7 {
            entry.push(MInst::Mov {
                dst: VReg(destination),
                src: VReg(1),
            });
        }
        entry.push(MInst::LoadImm {
            dst: VReg(7),
            value: 1,
        });
        entry.push(MInst::Branch {
            cond: VReg(7),
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });
        let mut left = MBlock::new(BlockId(1));
        left.push(MInst::Mov {
            dst: VReg(8),
            src: VReg(0),
        });
        left.push(MInst::Mov {
            dst: VReg(9),
            src: VReg(0),
        });
        left.push(MInst::Jump { target: BlockId(3) });
        let mut right = MBlock::new(BlockId(2));
        right.push(MInst::Mov {
            dst: VReg(10),
            src: VReg(0),
        });
        right.push(MInst::Mov {
            dst: VReg(11),
            src: VReg(0),
        });
        right.push(MInst::Jump { target: BlockId(3) });
        let mut merge = MBlock::new(BlockId(3));
        merge.push(MInst::Return);
        function.blocks = vec![entry, left, right, merge];

        let (cfg, graph) = model(&mut function);
        let mut allocator = Allocator::new(&graph, &cfg, &[PhysReg::RAX]).unwrap();
        let candidate = bundle_id(&graph, VReg(0));
        let resident = bundle_id(&graph, VReg(1));
        allocator.assign_register(resident, PhysReg::RAX).unwrap();
        assert!(allocator.try_split(candidate).unwrap());
        let BundleAssignment::Split { children, .. } =
            &allocator.bundles[candidate.0 as usize].assignment
        else {
            unreachable!();
        };
        let register_child = children
            .iter()
            .map(|child| &allocator.bundles[child.0 as usize])
            .find(|child| matches!(child.assignment, BundleAssignment::Register(_)))
            .unwrap();
        assert_eq!(register_child.uses, vec![BundleUseId(1), BundleUseId(2)]);
        assert!(
            register_child
                .segments
                .iter()
                .all(|segment| segment.block != BlockId(2))
        );
    }
}
