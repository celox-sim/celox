//! Exact pressure-driven splitting for the expanded allocation problem.
//!
//! A coloring failure is resolved at an owner-qualified occupancy cut returned
//! by the physical interval union. Only root uses reachable from that exact
//! point through the candidate's sparse live-range graph are moved.
//! The moved uses are partitioned into dominance-connected regions, each
//! entered by one proved home materialization; isolated and loop-carried entry
//! uses are materialized independently.  The resulting machine values are fed
//! back into joint allocation instead of being assigned scratch registers.

use std::cell::OnceCell;
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::ops::Deref;

use crate::backend::native::mir::{BlockId, VReg};

use super::allocation_expand::{
    self, ExpandedAllocationProblem, ExpandedRegisterEntry, ExpandedRegisterRegion,
    ExpandedUseSource, RegisterRegionId,
};
use super::allocation_reallocate::{
    AllocationPressurePoint, AllocationValue, AllocationValueClass, GreedyAllocator,
    JointAllocation, JointAllocationError, JointAllocationOutcome, JointAllocationProblem,
    RegionSpillRequest, RegionSplitCandidate, RegionSplitRequest, RegisterPressureFrontier,
};
use super::assignment::PhysReg;
use super::cfg::NormalizedCfg;
use super::home_graph::{BundleUseId, HomeGraph, LiveBundle, LiveBundleId};
use super::live_interval::{IncrementalLivenessUpdate, LiveSegment, SlotIndex, UseSite};
use super::live_range_edit::{LiveRangeCut, LiveRangeEditError, edit_live_range};
use super::spiller::{SpillEdit, SpillEntry, SpillEntryKind, SpillPlan, Spiller, SpillerError};

type SplitEntry = SpillEntry;
type SplitEntryKind = SpillEntryKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RegionSplitPlan {
    pub blocked_value: VReg,
    pub register: PhysReg,
    pub cuts: Vec<AllocationPressurePoint>,
    pub value: VReg,
    pub root: LiveBundleId,
    pub source_region: Option<RegisterRegionId>,
    pub preferred_register: Option<PhysReg>,
    pub retained: Vec<BundleUseId>,
    pub moved: Vec<BundleUseId>,
    pub entries: Vec<SplitEntry>,
}

impl RegionSplitPlan {
    fn primary_cut(&self) -> AllocationPressurePoint {
        self.cuts[0]
    }
}

/// SplitAnalysis output and the independent concrete spill decision for its
/// complement.  Keeping these as sibling values prevents SplitEditor from
/// silently choosing stack/rematerialization policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PlannedRegionSplit {
    pub split: RegionSplitPlan,
    pub spill: SpillPlan,
}

impl Deref for PlannedRegionSplit {
    type Target = RegionSplitPlan;

    fn deref(&self) -> &Self::Target {
        &self.split
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AllocationSplitError {
    pub rule: &'static str,
    pub block: Option<BlockId>,
    pub value: Option<VReg>,
    pub root: Option<LiveBundleId>,
    pub message: String,
}

impl AllocationSplitError {
    fn new(
        rule: &'static str,
        block: Option<BlockId>,
        value: Option<VReg>,
        root: Option<LiveBundleId>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            rule,
            block,
            value,
            root,
            message: message.into(),
        }
    }

    fn joint(error: JointAllocationError) -> Self {
        Self::new(error.rule, error.block, error.value, None, error.message)
    }

    fn expand(error: super::allocation_expand::AllocationExpandError) -> Self {
        Self::new(error.rule, error.block, None, error.root, error.message)
    }

    fn spill(error: SpillerError) -> Self {
        Self::new(
            error.rule,
            error.block,
            error.value,
            error.root,
            error.message,
        )
    }

    fn edit(error: LiveRangeEditError, root: LiveBundleId) -> Self {
        Self::new(
            error.rule,
            error.block,
            error.value,
            Some(root),
            error.message,
        )
    }

    fn ir(error: super::allocation_ir::AllocationIrError, root: LiveBundleId) -> Self {
        Self::new(
            error.rule,
            error.block,
            error.values.first().copied(),
            Some(root),
            error.message,
        )
    }
}

impl fmt::Display for AllocationSplitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.rule)?;
        if let Some(block) = self.block {
            write!(formatter, " at {block}")?;
        }
        if let Some(value) = self.value {
            write!(formatter, " value={value}")?;
        }
        if let Some(root) = self.root {
            write!(formatter, " root={root:?}")?;
        }
        write!(formatter, ": {}", self.message)
    }
}

impl std::error::Error for AllocationSplitError {}

#[derive(Debug)]
struct Dominance {
    enter: Vec<usize>,
    exit: Vec<usize>,
}

impl Dominance {
    fn build(cfg: &NormalizedCfg) -> Result<Self, AllocationSplitError> {
        let block_count = cfg.idom.len();
        if block_count == 0 || cfg.block_index.len() != block_count {
            return Err(AllocationSplitError::new(
                "ALLOCATION_SPLIT.DOMINATOR_TREE",
                None,
                None,
                None,
                "split planning requires a complete non-empty dominator tree",
            ));
        }
        let mut children = vec![Vec::new(); block_count];
        for block in 1..block_count {
            let parent = cfg.idom[block].ok_or_else(|| {
                AllocationSplitError::new(
                    "ALLOCATION_SPLIT.DOMINATOR_TREE",
                    None,
                    None,
                    None,
                    format!("reachable block {block} has no immediate dominator"),
                )
            })?;
            if parent >= block_count {
                return Err(AllocationSplitError::new(
                    "ALLOCATION_SPLIT.DOMINATOR_TREE",
                    None,
                    None,
                    None,
                    format!("block {block} has out-of-range dominator {parent}"),
                ));
            }
            children[parent].push(block);
        }

        let mut enter = vec![usize::MAX; block_count];
        let mut exit = vec![usize::MAX; block_count];
        let mut clock = 0usize;
        let mut work = vec![(0usize, false)];
        while let Some((block, leaving)) = work.pop() {
            if leaving {
                exit[block] = clock;
                clock = clock.checked_add(1).ok_or_else(|| {
                    AllocationSplitError::new(
                        "ALLOCATION_SPLIT.DOMINATOR_TREE",
                        None,
                        None,
                        None,
                        "dominator traversal index exceeds usize",
                    )
                })?;
                continue;
            }
            if enter[block] != usize::MAX {
                return Err(AllocationSplitError::new(
                    "ALLOCATION_SPLIT.DOMINATOR_TREE",
                    None,
                    None,
                    None,
                    "dominator tree contains a cycle or duplicate child",
                ));
            }
            enter[block] = clock;
            clock = clock.checked_add(1).ok_or_else(|| {
                AllocationSplitError::new(
                    "ALLOCATION_SPLIT.DOMINATOR_TREE",
                    None,
                    None,
                    None,
                    "dominator traversal index exceeds usize",
                )
            })?;
            work.push((block, true));
            work.extend(children[block].iter().rev().map(|child| (*child, false)));
        }
        if enter.contains(&usize::MAX) || exit.contains(&usize::MAX) {
            return Err(AllocationSplitError::new(
                "ALLOCATION_SPLIT.DOMINATOR_TREE",
                None,
                None,
                None,
                "dominator tree does not reach every CFG block",
            ));
        }
        Ok(Self { enter, exit })
    }

    fn block_dominates(&self, dominator: usize, block: usize) -> bool {
        self.enter[dominator] <= self.enter[block] && self.exit[block] <= self.exit[dominator]
    }

    fn use_dominates_use(&self, cfg: &NormalizedCfg, dominator: UseSite, use_: UseSite) -> bool {
        let UseSite::Instruction {
            block: dominator_block,
            slot: dominator_slot,
            ..
        } = dominator
        else {
            return false;
        };
        let Some(&dominator_block) = cfg.block_index.get(&dominator_block) else {
            return false;
        };
        let Some(&use_block) = cfg.block_index.get(&use_.block()) else {
            return false;
        };
        if dominator_block == use_block {
            dominator_slot <= use_.slot()
        } else {
            self.block_dominates(dominator_block, use_block)
        }
    }

    fn use_dominates_point(
        &self,
        cfg: &NormalizedCfg,
        use_: UseSite,
        point: AllocationPressurePoint,
    ) -> bool {
        let UseSite::Instruction {
            block: use_block,
            slot: use_slot,
            ..
        } = use_
        else {
            return false;
        };
        let Some(&use_block) = cfg.block_index.get(&use_block) else {
            return false;
        };
        let Some(&point_block) = cfg.block_index.get(&point.block) else {
            return false;
        };
        if use_block == point_block {
            use_slot <= point.slot
        } else {
            self.block_dominates(use_block, point_block)
        }
    }
}

/// Immutable function-lifetime analyses used by every pressure transaction.
/// A split changes allocation-IR operands and live fragments, but it does not
/// change the normalized CFG or the HomeGraph's exact per-root home choices.
/// Rebuilding either table for every candidate cut turns a finite split
/// sequence into `splits * (CFG + root uses)` work on large RTL functions.
#[derive(Debug)]
struct SplitPlanningContext {
    dominance: Dominance,
    spiller: Spiller,
    use_topologies: Vec<OnceCell<RootUseTopology>>,
}

/// Immutable dominance order for one semantic root's exact use identities.
///
/// Expanded allocation rewrites values and inserts instructions, but it never
/// reorders the original root uses or changes the CFG.  Computing this order
/// once turns the greedy dominance partition from repeated sorting plus a
/// quadratic remaining-set scan into one ordered subset walk per split.
#[derive(Debug)]
struct RootUseTopology {
    root: LiveBundleId,
    dominance_order: Vec<BundleUseId>,
    rank_by_use: Vec<usize>,
}

impl RootUseTopology {
    fn build(
        root: &LiveBundle,
        cfg: &NormalizedCfg,
        dominance: &Dominance,
    ) -> Result<Self, AllocationSplitError> {
        let mut dominance_order = Vec::with_capacity(root.uses.len());
        for (row, use_) in root.uses.iter().enumerate() {
            if use_.id.0 as usize != row {
                return Err(AllocationSplitError::new(
                    "ALLOCATION_SPLIT.USE_ORDER",
                    Some(use_.site.block()),
                    Some(root.origin),
                    Some(root.id),
                    "root use differs from its dense function-lifetime row",
                ));
            }
            if !cfg.block_index.contains_key(&use_.site.block()) {
                return Err(AllocationSplitError::new(
                    "ALLOCATION_SPLIT.USE_BLOCK",
                    Some(use_.site.block()),
                    Some(root.origin),
                    Some(root.id),
                    "root use is outside the normalized CFG",
                ));
            }
            dominance_order.push(use_.id);
        }
        dominance_order.sort_unstable_by_key(|use_id| {
            let site = root.uses[use_id.0 as usize].site;
            let block = cfg.block_index[&site.block()];
            (dominance.enter[block], site.slot(), *use_id)
        });
        let mut rank_by_use = vec![usize::MAX; root.uses.len()];
        for (rank, use_id) in dominance_order.iter().copied().enumerate() {
            rank_by_use[use_id.0 as usize] = rank;
        }
        Ok(Self {
            root: root.id,
            dominance_order,
            rank_by_use,
        })
    }

    fn ordered_subset(
        &self,
        moved: &[BundleUseId],
    ) -> Result<Vec<BundleUseId>, AllocationSplitError> {
        if moved.is_empty()
            || moved.windows(2).any(|pair| pair[0] >= pair[1])
            || moved
                .last()
                .is_some_and(|use_id| use_id.0 as usize >= self.rank_by_use.len())
        {
            return Err(AllocationSplitError::new(
                "ALLOCATION_SPLIT.MOVED_ORDER",
                None,
                None,
                Some(self.root),
                "moved uses are empty, duplicated, unordered, or outside their root",
            ));
        }

        // A large frontier is cheaper to project by one dense root scan.
        // Small later fragments retain their
        // asymptotic locality by sorting only their own precomputed ranks.
        if moved.len() > self.dominance_order.len() / 8 {
            let mut member = vec![false; self.rank_by_use.len()];
            for &use_id in moved {
                member[use_id.0 as usize] = true;
            }
            Ok(self
                .dominance_order
                .iter()
                .copied()
                .filter(|use_id| member[use_id.0 as usize])
                .collect())
        } else {
            let mut ordered = moved.to_vec();
            ordered.sort_unstable_by_key(|use_id| self.rank_by_use[use_id.0 as usize]);
            Ok(ordered)
        }
    }
}

impl SplitPlanningContext {
    fn build(graph: &HomeGraph, cfg: &NormalizedCfg) -> Result<Self, AllocationSplitError> {
        let dominance = Dominance::build(cfg)?;
        let spiller = Spiller::build(graph).map_err(AllocationSplitError::spill)?;
        let use_topologies = std::iter::repeat_with(OnceCell::new)
            .take(graph.bundles.len())
            .collect();
        Ok(Self {
            dominance,
            spiller,
            use_topologies,
        })
    }

    fn use_topology(
        &self,
        graph: &HomeGraph,
        cfg: &NormalizedCfg,
        root: LiveBundleId,
    ) -> Result<&RootUseTopology, AllocationSplitError> {
        let cell = self.use_topologies.get(root.0 as usize).ok_or_else(|| {
            AllocationSplitError::new(
                "ALLOCATION_SPLIT.HOME_ROOT",
                None,
                None,
                Some(root),
                "split root has no function-lifetime use topology",
            )
        })?;
        let graph_root = graph.bundles.get(root.0 as usize).ok_or_else(|| {
            AllocationSplitError::new(
                "ALLOCATION_SPLIT.HOME_ROOT",
                None,
                None,
                Some(root),
                "split root is outside the immutable HomeGraph",
            )
        })?;
        if cell.get().is_none() {
            let topology = RootUseTopology::build(graph_root, cfg, &self.dominance)?;
            cell.set(topology).map_err(|_| {
                AllocationSplitError::new(
                    "ALLOCATION_SPLIT.USE_TOPOLOGY_IDENTITY",
                    Some(graph_root.definition.block()),
                    Some(graph_root.origin),
                    Some(root),
                    "root use topology was initialized twice in one planning session",
                )
            })?;
        }
        Ok(cell.get().expect("root topology initialized above"))
    }
}

#[derive(Debug, Clone, Copy)]
struct RegionSource {
    preferred_register: Option<PhysReg>,
    region: Option<RegisterRegionId>,
    entry_use: Option<BundleUseId>,
}

#[derive(Debug, Clone)]
struct EntryCluster {
    entry: BundleUseId,
    uses: Vec<BundleUseId>,
    kind: SplitEntryKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct SplitProgress {
    paired_uses: u128,
    original_uses: u128,
    register_uses: u128,
}

#[derive(Debug)]
struct AppliedSplit {
    root: LiveBundleId,
    constraint_blocks: BTreeSet<BlockId>,
    changed_values: Vec<VReg>,
    range_changed_values: Vec<VReg>,
    live_lengths: Vec<(VReg, Option<u64>)>,
}

/// Physical and semantic fact owners touched by one private split transaction.
///
/// A register region may enter in one block and rewrite uses in many later
/// blocks.  Recording only the entry block leaves liveness stale while region
/// ownership already names the rewritten values.  Every operand rewrite is
/// therefore journaled at the mutation boundary, and both incremental
/// liveness and target constraints consume this same journal.
#[derive(Debug, Default)]
struct SplitMutationJournal {
    liveness_blocks: BTreeSet<BlockId>,
    constraint_blocks: BTreeSet<BlockId>,
}

impl SplitMutationJournal {
    fn record_block(&mut self, block: BlockId) {
        self.liveness_blocks.insert(block);
        self.constraint_blocks.insert(block);
    }

    fn record_use(&mut self, use_: UseSite) {
        self.record_block(use_.block());
        if let UseSite::PhiEdge { successor, .. } = use_ {
            self.constraint_blocks.insert(successor);
        }
    }
}

/// Select one exact resident region and one owner-qualified occupancy cut with
/// minimum proved transition cost. The returned topology is colored only
/// after the source is removed from the current matrix; exact materialized
/// ranges are revalidated when the allocation-IR transaction is published.
pub(super) fn plan_split(
    expanded: &ExpandedAllocationProblem,
    graph: &HomeGraph,
    joint: &JointAllocationProblem,
    request: &RegionSplitRequest,
    cfg: &NormalizedCfg,
) -> Result<PlannedRegionSplit, AllocationSplitError> {
    let context = SplitPlanningContext::build(graph, cfg)?;
    plan_split_with_context(expanded, graph, joint, request, cfg, &context)
}

fn plan_split_with_context(
    expanded: &ExpandedAllocationProblem,
    graph: &HomeGraph,
    joint: &JointAllocationProblem,
    request: &RegionSplitRequest,
    cfg: &NormalizedCfg,
    context: &SplitPlanningContext,
) -> Result<PlannedRegionSplit, AllocationSplitError> {
    let blocked = joint.value(request.blocked_value).ok_or_else(|| {
        AllocationSplitError::new(
            "ALLOCATION_SPLIT.BLOCKED_VALUE",
            Some(request.definition.block()),
            Some(request.blocked_value),
            None,
            "split request references a value outside joint allocation",
        )
    })?;
    if blocked.interval.definition != request.definition || request.candidates.is_empty() {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.REQUEST_IDENTITY",
            Some(request.definition.block()),
            Some(request.blocked_value),
            None,
            "split request has a stale definition or no splittable resident region",
        ));
    }
    for candidate in &request.candidates {
        if candidate.frontiers.is_empty()
            || candidate
                .frontiers
                .windows(2)
                .any(|pair| pair[0].register >= pair[1].register)
            || candidate.frontiers.iter().any(|frontier| {
                frontier.points.is_empty()
                    || frontier.points.windows(2).any(|pair| pair[0] >= pair[1])
                    || frontier
                        .points
                        .iter()
                        .any(|point| point.register() != frontier.register)
            })
        {
            return Err(AllocationSplitError::new(
                "ALLOCATION_SPLIT.PRESSURE_POINTS",
                Some(request.definition.block()),
                Some(candidate.value),
                Some(candidate.root),
                "candidate register frontiers are empty, mixed, duplicated, or unordered",
            ));
        }
    }

    let mut best = None::<(i128, PlannedRegionSplit)>;
    for candidate in &request.candidates {
        let use_topology = context.use_topology(graph, cfg, candidate.root)?;
        for plan in plan_candidate_frontiers(
            expanded,
            joint,
            request.blocked_value,
            candidate,
            cfg,
            &context.dominance,
            &context.spiller,
            use_topology,
        )? {
            let home_delta = i128::from(plan.spill.total_cost);
            let key = (
                home_delta,
                plan.split.moved.len(),
                Reverse(plan.split.retained.len()),
                plan.split.value,
                plan.split.root,
                plan.split.register,
                &plan.split.cuts,
            );
            let replace = if let Some((current_home_delta, current)) = &best {
                key < (
                    *current_home_delta,
                    current.split.moved.len(),
                    Reverse(current.split.retained.len()),
                    current.split.value,
                    current.split.root,
                    current.split.register,
                    &current.split.cuts,
                )
            } else {
                true
            };
            if replace {
                best = Some((home_delta, plan));
            }
        }
    }
    best.map(|(_, plan)| plan).ok_or_else(|| {
        AllocationSplitError::new(
            "ALLOCATION_SPLIT.NO_PROGRESS",
            Some(request.definition.block()),
            Some(request.blocked_value),
            None,
            "no requested root region can be shortened at the pressure point",
        )
    })
}

/// Select one conventional strict-SSA live-range edit from exact physical
/// free-prefix frontiers. This stage deliberately knows nothing about stack,
/// State-MemorySSA, rematerialization, or HomeGraph transition costs; those
/// decisions belong exclusively to the later `Spill` stage.
fn select_live_range_frontier(
    expanded: &ExpandedAllocationProblem,
    joint: &JointAllocationProblem,
    request: &RegionSplitRequest,
) -> Result<(RegionSplitCandidate, RegisterPressureFrontier), AllocationSplitError> {
    let blocked = joint.value(request.blocked_value).ok_or_else(|| {
        AllocationSplitError::new(
            "ALLOCATION_SPLIT.BLOCKED_VALUE",
            Some(request.definition.block()),
            Some(request.blocked_value),
            None,
            "split request references a value outside joint allocation",
        )
    })?;
    if blocked.interval.definition != request.definition || request.candidates.is_empty() {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.REQUEST_IDENTITY",
            Some(request.definition.block()),
            Some(request.blocked_value),
            None,
            "split request has a stale definition or no splittable live interval",
        ));
    }

    let mut best = None::<(
        usize,
        u64,
        u64,
        VReg,
        PhysReg,
        RegionSplitCandidate,
        RegisterPressureFrontier,
    )>;
    for candidate in &request.candidates {
        if candidate.frontiers.is_empty()
            || candidate
                .frontiers
                .windows(2)
                .any(|pair| pair[0].register >= pair[1].register)
        {
            return Err(AllocationSplitError::new(
                "ALLOCATION_SPLIT.PRESSURE_POINTS",
                Some(request.definition.block()),
                Some(candidate.value),
                Some(candidate.root),
                "candidate register frontiers are empty, duplicated, or unordered",
            ));
        }
        let allocation = joint.value(candidate.value).ok_or_else(|| {
            AllocationSplitError::new(
                "ALLOCATION_SPLIT.CANDIDATE_RANGE",
                Some(request.definition.block()),
                Some(candidate.value),
                Some(candidate.root),
                "split candidate is outside joint allocation",
            )
        })?;
        let spill_cost = allocation.spill_cost.ok_or_else(|| {
            AllocationSplitError::new(
                "ALLOCATION_SPLIT.CANDIDATE_CLASS",
                Some(allocation.interval.definition.block()),
                Some(candidate.value),
                Some(candidate.root),
                "fixed transition was selected as a split candidate",
            )
        })?;
        for frontier in &candidate.frontiers {
            let Some(&first_cut) = frontier.points.first() else {
                return Err(AllocationSplitError::new(
                    "ALLOCATION_SPLIT.PRESSURE_POINTS",
                    Some(request.definition.block()),
                    Some(candidate.value),
                    Some(candidate.root),
                    "candidate has an empty physical-register frontier",
                ));
            };
            let value = verify_candidate(joint, candidate, first_cut)?;
            if frontier.register != first_cut.register()
                || frontier.points.windows(2).any(|pair| pair[0] >= pair[1])
            {
                return Err(AllocationSplitError::new(
                    "ALLOCATION_SPLIT.PRESSURE_POINTS",
                    Some(first_cut.block()),
                    Some(candidate.value),
                    Some(candidate.root),
                    "frontier points are mixed, duplicated, or unordered",
                ));
            }

            let mut placements = BTreeSet::new();
            let mut legal = true;
            for &cut in &frontier.points {
                verify_pressure_point(value, candidate, cut)?;
                let placement = match expanded.ir.plan_split_copy_before(
                    &value.interval,
                    cut.block(),
                    cut.slot(),
                ) {
                    Ok(placement) => placement,
                    Err(error) if error.rule == "ALLOCATION_IR.NO_LEGAL_SPLIT_POINT" => {
                        legal = false;
                        break;
                    }
                    Err(error) => {
                        return Err(AllocationSplitError::ir(error, candidate.root));
                    }
                };
                // Inserting another copy in the same anchor zone merely
                // lengthens a copy chain while leaving the pressure frontier
                // unchanged. A later original boundary may still be split.
                if placement.block == value.interval.definition.block()
                    && placement.definition_slot.stable_zone()
                        == value.interval.definition.slot().stable_zone()
                {
                    legal = false;
                    break;
                }
                placements.insert((
                    placement.block,
                    placement.use_slot,
                    placement.definition_slot,
                ));
            }
            if !legal || placements.is_empty() {
                continue;
            }

            // Fewer copies are preferred. Spill density is used only as a
            // conventional allocation weight; no home is selected here.
            let key = (
                placements.len(),
                spill_cost,
                value.live_length,
                candidate.value,
                frontier.register,
            );
            let replace = best.as_ref().is_none_or(
                |(best_copies, best_cost, best_length, best_value, best_register, _, _)| {
                    let candidate_density = u128::from(spill_cost) * u128::from(*best_length);
                    let best_density = u128::from(*best_cost) * u128::from(value.live_length);
                    placements.len() < *best_copies
                        || (placements.len() == *best_copies
                            && (candidate_density < best_density
                                || (candidate_density == best_density
                                    && (candidate.value, frontier.register)
                                        < (*best_value, *best_register))))
                },
            );
            if replace {
                best = Some((
                    key.0,
                    key.1,
                    key.2,
                    key.3,
                    key.4,
                    candidate.clone(),
                    frontier.clone(),
                ));
            }
        }
    }
    best.map(|(_, _, _, _, _, candidate, frontier)| (candidate, frontier))
        .ok_or_else(|| {
            AllocationSplitError::new(
                "ALLOCATION_SPLIT.NO_PROGRESS",
                Some(request.definition.block()),
                Some(request.blocked_value),
                None,
                "no requested live interval has a new legal machine boundary before its pressure frontier",
            )
        })
}

#[allow(clippy::too_many_arguments)]
fn plan_candidate_frontiers(
    expanded: &ExpandedAllocationProblem,
    joint: &JointAllocationProblem,
    blocked_value: VReg,
    candidate: &RegionSplitCandidate,
    cfg: &NormalizedCfg,
    dominance: &Dominance,
    spiller: &Spiller,
    use_topology: &RootUseTopology,
) -> Result<Vec<PlannedRegionSplit>, AllocationSplitError> {
    let first_frontier = candidate.frontiers.first().ok_or_else(|| {
        AllocationSplitError::new(
            "ALLOCATION_SPLIT.PRESSURE_POINTS",
            None,
            Some(candidate.value),
            Some(candidate.root),
            "candidate has no pressure frontier",
        )
    })?;
    let first_cut = first_frontier.points[0];
    let value = verify_candidate(joint, candidate, first_cut)?;
    for frontier in &candidate.frontiers {
        for &cut in &frontier.points {
            verify_pressure_point(value, candidate, cut)?;
        }
    }
    let root = expanded_root(expanded, candidate.root)?;
    let source = region_source(expanded, root, candidate, value)?;
    let mut plans = Vec::with_capacity(candidate.frontiers.len());
    for frontier in &candidate.frontiers {
        let moved =
            reachable_uses_at_frontier(expanded, root, candidate, value, &frontier.points, cfg)?;
        let Some(moved) = moved else {
            continue;
        };
        plans.push(plan_candidate_from_moved(
            expanded,
            joint,
            blocked_value,
            candidate,
            frontier,
            cfg,
            dominance,
            spiller,
            use_topology,
            root,
            source,
            moved,
        )?);
    }
    Ok(plans)
}

#[allow(clippy::too_many_arguments)]
fn plan_candidate_from_moved(
    expanded: &ExpandedAllocationProblem,
    joint: &JointAllocationProblem,
    blocked_value: VReg,
    candidate: &RegionSplitCandidate,
    frontier: &RegisterPressureFrontier,
    cfg: &NormalizedCfg,
    dominance: &Dominance,
    spiller: &Spiller,
    use_topology: &RootUseTopology,
    root: &super::allocation_expand::ExpandedRoot,
    source: RegionSource,
    moved: Vec<BundleUseId>,
) -> Result<PlannedRegionSplit, AllocationSplitError> {
    let clusters = partition_moved_uses(
        root,
        candidate,
        &moved,
        source.entry_use,
        &frontier.points,
        cfg,
        dominance,
        use_topology,
    )?;
    let entries = clusters
        .into_iter()
        .map(|cluster| {
            Ok(SplitEntry {
                entry: cluster.entry,
                uses: cluster.uses,
                kind: cluster.kind,
                preferred_register: (cluster.kind == SplitEntryKind::RegisterRegion)
                    .then_some(frontier.register),
            })
        })
        .collect::<Result<Vec<_>, AllocationSplitError>>()?;
    let retained = sorted_difference(&candidate.uses, &moved);
    let split = RegionSplitPlan {
        blocked_value,
        register: frontier.register,
        cuts: frontier.points.clone(),
        value: candidate.value,
        root: candidate.root,
        source_region: source.region,
        preferred_register: source.preferred_register,
        retained,
        moved,
        entries,
    };
    let spill = spiller
        .plan(expanded, split.root, split.value, &split.entries)
        .map_err(AllocationSplitError::spill)?;
    let plan = PlannedRegionSplit { split, spill };
    if super::exhaustive_verification_enabled() {
        verify_plan(expanded, joint, candidate, &plan, cfg, dominance, spiller)?;
    }
    Ok(plan)
}

/// Apply a verified split inside the private allocation session. Every
/// fallible planning and dominance check runs before mutation; an invariant
/// failure after mutation aborts and discards the session without publishing
/// MIR. Independent whole-program proofs run once at atomic lowering.
fn apply_split(
    expanded: &mut ExpandedAllocationProblem,
    graph: &HomeGraph,
    joint: &JointAllocationProblem,
    plan: &PlannedRegionSplit,
    cfg: &NormalizedCfg,
) -> Result<AppliedSplit, AllocationSplitError> {
    let context = SplitPlanningContext::build(graph, cfg)?;
    apply_split_with_context(expanded, graph, joint, plan, cfg, &context)
}

fn apply_split_with_context(
    expanded: &mut ExpandedAllocationProblem,
    graph: &HomeGraph,
    joint: &JointAllocationProblem,
    plan: &PlannedRegionSplit,
    cfg: &NormalizedCfg,
    context: &SplitPlanningContext,
) -> Result<AppliedSplit, AllocationSplitError> {
    let candidate = candidate_from_plan(joint, &plan.split)?;
    verify_plan(
        expanded,
        joint,
        &candidate,
        plan,
        cfg,
        &context.dominance,
        &context.spiller,
    )?;
    expanded
        .ir
        .begin_instruction_transaction()
        .map_err(|error| {
            AllocationSplitError::new(
                error.rule,
                error.block,
                error.values.first().copied(),
                Some(plan.root),
                error.message,
            )
        })?;
    let mut journal = SplitMutationJournal::default();
    mutate_verified_split(expanded, graph, plan, &mut journal, &context.spiller)?;
    let replacement = (plan.value, plan.root);
    let liveness = finish_split_mutations(
        expanded,
        cfg,
        &mut journal,
        std::slice::from_ref(&replacement),
    )?;
    Ok(AppliedSplit {
        root: plan.root,
        constraint_blocks: journal.constraint_blocks,
        changed_values: liveness.changed_values,
        range_changed_values: liveness.range_changed_values,
        live_lengths: liveness.live_lengths,
    })
}

/// Apply one conventional SplitEditor transaction without selecting a home.
/// Every resulting strict-SSA representative remains an ordinary allocatable
/// interval. HomeGraph use IDs are retained only as annotations on the exact
/// machine representatives that now consume those semantic uses.
fn apply_live_range_edit(
    expanded: &mut ExpandedAllocationProblem,
    joint: &JointAllocationProblem,
    candidate: &RegionSplitCandidate,
    frontier: &RegisterPressureFrontier,
    cfg: &NormalizedCfg,
) -> Result<AppliedSplit, AllocationSplitError> {
    let first_cut = frontier.points.first().copied().ok_or_else(|| {
        AllocationSplitError::new(
            "ALLOCATION_SPLIT.EMPTY_FRONTIER",
            None,
            Some(candidate.value),
            Some(candidate.root),
            "SplitEditor requires a non-empty physical-register frontier",
        )
    })?;
    let value = verify_candidate(joint, candidate, first_cut)?;
    for &cut in &frontier.points[1..] {
        verify_pressure_point(value, candidate, cut)?;
    }
    if frontier.register != first_cut.register() {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.PRESSURE_REGISTER",
            Some(first_cut.block()),
            Some(candidate.value),
            Some(candidate.root),
            "frontier register differs from its first exact pressure point",
        ));
    }
    // The source is exactly the free prefix proved for this register. New
    // suffix representatives return to the ordinary queue without inheriting
    // a color that is known to be blocked beyond the frontier.
    let source_preference = Some(frontier.register);
    let child_preference = None;
    let cuts = frontier
        .points
        .iter()
        .map(|cut| LiveRangeCut {
            block: cut.block(),
            slot: cut.slot(),
        })
        .collect::<Vec<_>>();
    let edit = edit_live_range(
        &mut expanded.ir,
        cfg,
        &expanded.intervals,
        candidate.value,
        &cuts,
    )
    .map_err(|error| AllocationSplitError::edit(error, candidate.root))?;

    let root_origin = expanded_root(expanded, candidate.root)?.origin;
    let mut regions = BTreeMap::<VReg, RegisterRegionId>::new();
    let mut region_preferences = BTreeMap::<VReg, Option<PhysReg>>::new();
    if let Some(metadata) = expanded
        .register_regions
        .iter_mut()
        .find(|metadata| metadata.root == candidate.root && metadata.value == candidate.value)
    {
        metadata.preferred_register = source_preference;
        regions.insert(candidate.value, metadata.id);
        region_preferences.insert(candidate.value, source_preference);
    } else if candidate.value == root_origin {
        let region = insert_register_representative(
            expanded,
            candidate.root,
            candidate.value,
            source_preference,
            ExpandedRegisterEntry::Original,
        )?;
        regions.insert(candidate.value, region);
        region_preferences.insert(candidate.value, source_preference);
    } else {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.REGION_METADATA",
            Some(value.interval.definition.block()),
            Some(candidate.value),
            Some(candidate.root),
            "non-origin SplitEditor source has no representative metadata",
        ));
    }
    for copy in &edit.copies {
        let region = insert_register_representative(
            expanded,
            candidate.root,
            copy.definition,
            child_preference,
            ExpandedRegisterEntry::SplitCopy {
                instruction: copy.instruction,
                source: candidate.value,
            },
        )?;
        regions.insert(copy.definition, region);
        region_preferences.insert(copy.definition, child_preference);
    }
    for phi in &edit.phis {
        let region = insert_register_representative(
            expanded,
            candidate.root,
            phi.definition,
            child_preference,
            ExpandedRegisterEntry::SplitPhi {
                block: phi.block,
                phi: phi.phi,
            },
        )?;
        regions.insert(phi.definition, region);
        region_preferences.insert(phi.definition, child_preference);
    }

    let rewritten = edit
        .rewritten_uses
        .iter()
        .map(|use_| (use_.site, use_.value))
        .collect::<BTreeMap<_, _>>();
    let root_index = candidate.root.0 as usize;
    for &use_id in &candidate.uses {
        let use_ = expanded
            .roots
            .get_mut(root_index)
            .and_then(|root| root.uses.get_mut(use_id.0 as usize))
            .ok_or_else(|| {
                AllocationSplitError::new(
                    "ALLOCATION_SPLIT.USE_RANGE",
                    None,
                    Some(candidate.value),
                    Some(candidate.root),
                    "SplitEditor semantic use is outside its expanded root",
                )
            })?;
        if use_.value != candidate.value {
            return Err(AllocationSplitError::new(
                "ALLOCATION_SPLIT.USE_OWNERSHIP",
                Some(use_.site.block()),
                Some(candidate.value),
                Some(candidate.root),
                "SplitEditor source no longer owns one requested semantic use",
            ));
        }
        let replacement = rewritten.get(&use_.site).copied().ok_or_else(|| {
            AllocationSplitError::new(
                "ALLOCATION_SPLIT.EDIT_USE_COVERAGE",
                Some(use_.site.block()),
                Some(candidate.value),
                Some(candidate.root),
                "strict-SSA rename omitted one exact semantic root use",
            )
        })?;
        use_.value = replacement;
        if replacement == root_origin {
            use_.source = ExpandedUseSource::OriginalRegister {
                preferred_register: source_preference,
            };
        } else {
            let region = regions.get(&replacement).copied().ok_or_else(|| {
                AllocationSplitError::new(
                    "ALLOCATION_SPLIT.REGION_METADATA",
                    Some(use_.site.block()),
                    Some(replacement),
                    Some(candidate.root),
                    "renamed semantic use has no register-representative metadata",
                )
            })?;
            let preferred_register =
                region_preferences
                    .get(&replacement)
                    .copied()
                    .ok_or_else(|| {
                        AllocationSplitError::new(
                            "ALLOCATION_SPLIT.REGION_METADATA",
                            Some(use_.site.block()),
                            Some(replacement),
                            Some(candidate.root),
                            "renamed semantic use has no representative preference",
                        )
                    })?;
            use_.source = ExpandedUseSource::RegisterRegion {
                region,
                preferred_register,
            };
        }
    }

    let mut constraint_blocks = edit.changed_blocks.clone();
    for rewritten in &edit.rewritten_uses {
        if let UseSite::PhiEdge { successor, .. } = rewritten.site {
            constraint_blocks.insert(successor);
        }
    }
    let liveness = allocation_expand::refresh(expanded, cfg, &edit.changed_blocks)
        .map_err(AllocationSplitError::expand)?;
    for representative in edit.representatives {
        if expanded
            .intervals
            .intervals
            .get(representative.0 as usize)
            .and_then(Option::as_ref)
            .is_none()
        {
            return Err(AllocationSplitError::new(
                "ALLOCATION_SPLIT.DEAD_REPRESENTATIVE",
                None,
                Some(representative),
                Some(candidate.root),
                "SplitEditor returned a representative without a live machine interval",
            ));
        }
    }
    Ok(AppliedSplit {
        root: candidate.root,
        constraint_blocks,
        changed_values: liveness.changed_values,
        range_changed_values: liveness.range_changed_values,
        live_lengths: liveness.live_lengths,
    })
}

fn insert_register_representative(
    expanded: &mut ExpandedAllocationProblem,
    root: LiveBundleId,
    value: VReg,
    preferred_register: Option<PhysReg>,
    entry: ExpandedRegisterEntry,
) -> Result<RegisterRegionId, AllocationSplitError> {
    if expanded.region_by_value.contains_key(&value) {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.REGION_VALUE_IDENTITY",
            None,
            Some(value),
            Some(root),
            "machine representative already has register-region metadata",
        ));
    }
    let id = RegisterRegionId(expanded.next_register_region);
    expanded.next_register_region =
        expanded
            .next_register_region
            .checked_add(1)
            .ok_or_else(|| {
                AllocationSplitError::new(
                    "ALLOCATION_SPLIT.REGION_ID_RANGE",
                    None,
                    Some(value),
                    Some(root),
                    "register-representative identity exceeds u32",
                )
            })?;
    let row = expanded.register_regions.len();
    if expanded.region_rows.insert(id, row).is_some() {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.REGION_IDENTITY",
            None,
            Some(value),
            Some(root),
            "fresh register-representative identity already exists",
        ));
    }
    if expanded.region_by_value.insert(value, id).is_some() {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.REGION_VALUE_IDENTITY",
            None,
            Some(value),
            Some(root),
            "fresh register representative duplicates an active machine value",
        ));
    }
    expanded.register_regions.push(ExpandedRegisterRegion {
        id,
        root,
        value,
        preferred_register,
        entry_use: None,
        entry,
    });
    Ok(id)
}

/// Rewrite one interval only after both split stages have failed to produce a
/// shorter register range.  The spiller owns the concrete home choices and
/// returns only short transition ranges to the allocation session.
fn apply_spill(
    expanded: &mut ExpandedAllocationProblem,
    graph: &HomeGraph,
    joint: &JointAllocationProblem,
    request: &RegionSpillRequest,
    cfg: &NormalizedCfg,
    spiller: &Spiller,
) -> Result<AppliedSplit, AllocationSplitError> {
    let value = joint.value(request.value).ok_or_else(|| {
        AllocationSplitError::new(
            "ALLOCATION_SPILL.VALUE_RANGE",
            None,
            Some(request.value),
            Some(request.root),
            "spill request references a value outside joint allocation",
        )
    })?;
    let AllocationValueClass::Region { root, uses } = &value.class else {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPILL.VALUE_CLASS",
            Some(value.interval.definition.block()),
            Some(request.value),
            Some(request.root),
            "spill request no longer references a register live interval",
        ));
    };
    if *root != request.root || *uses != request.uses {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPILL.REQUEST_IDENTITY",
            Some(value.interval.definition.block()),
            Some(request.value),
            Some(request.root),
            "spill request and current live interval own different root uses",
        ));
    }
    if requires_machine_interval_spill(expanded, request, &value.interval)? {
        let interval = value.interval.clone();
        expanded
            .ir
            .begin_instruction_transaction()
            .map_err(|error| AllocationSplitError::ir(error, request.root))?;
        let SpillEdit {
            liveness_blocks,
            constraint_blocks,
        } = spiller
            .materialize_machine_interval(expanded, request.root, &interval)
            .map_err(AllocationSplitError::spill)?;
        let mut journal = SplitMutationJournal {
            liveness_blocks,
            constraint_blocks,
        };
        let replacement = (request.value, request.root);
        let liveness = finish_split_mutations(
            expanded,
            cfg,
            &mut journal,
            std::slice::from_ref(&replacement),
        )?;
        return Ok(AppliedSplit {
            root: request.root,
            constraint_blocks: journal.constraint_blocks,
            changed_values: liveness.changed_values,
            range_changed_values: liveness.range_changed_values,
            live_lengths: liveness.live_lengths,
        });
    }
    if request.uses.is_empty() {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPILL.EMPTY_MACHINE_INTERVAL",
            Some(value.interval.definition.block()),
            Some(request.value),
            Some(request.root),
            "an empty semantic region did not contain a machine transition use",
        ));
    }
    let candidate = RegionSplitCandidate {
        value: request.value,
        root: request.root,
        uses: request.uses.clone(),
        frontiers: Vec::new(),
    };
    let root_row = expanded_root(expanded, request.root)?;
    let source = region_source(expanded, root_row, &candidate, value)?;
    let entries = request
        .uses
        .iter()
        .copied()
        .map(|entry| SpillEntry {
            entry,
            uses: vec![entry],
            kind: SpillEntryKind::Materialized,
            preferred_register: None,
        })
        .collect::<Vec<_>>();
    let spill = spiller
        .plan(expanded, request.root, request.value, &entries)
        .map_err(AllocationSplitError::spill)?;
    expanded
        .ir
        .begin_instruction_transaction()
        .map_err(|error| {
            AllocationSplitError::new(
                error.rule,
                error.block,
                error.values.first().copied(),
                Some(request.root),
                error.message,
            )
        })?;
    let before = root_split_progress(expanded_root(expanded, request.root)?);
    let graph_root = graph_root(graph, request.root)?;
    let SpillEdit {
        liveness_blocks,
        constraint_blocks,
    } = spiller
        .materialize(
            expanded,
            graph,
            request.value,
            request.root,
            &entries,
            &spill,
            request.value == graph_root.origin,
        )
        .map_err(AllocationSplitError::spill)?;
    let mut journal = SplitMutationJournal {
        liveness_blocks,
        constraint_blocks,
    };
    prune_replaced_register_region(expanded, request.root, request.value, source.region)?;
    let after = root_split_progress(expanded_root(expanded, request.root)?);
    if after >= before {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPILL.NON_MONOTONIC",
            Some(value.interval.definition.block()),
            Some(request.value),
            Some(request.root),
            format!("spill progress {before:?} did not decrease: {after:?}"),
        ));
    }
    let replacement = (request.value, request.root);
    let liveness = finish_split_mutations(
        expanded,
        cfg,
        &mut journal,
        std::slice::from_ref(&replacement),
    )?;
    Ok(AppliedSplit {
        root: request.root,
        constraint_blocks: journal.constraint_blocks,
        changed_values: liveness.changed_values,
        range_changed_values: liveness.range_changed_values,
        live_lengths: liveness.live_lengths,
    })
}

/// A HomeGraph spill recipe owns only the immutable RTL-use subset. Once
/// SplitEditor has introduced copies or merge phis, an interval can also own
/// allocator-created transition uses. Such a representative must be spilled
/// as one exact machine interval; otherwise the semantic rewrite silently
/// leaves the pressure-causing suffix resident as an unspillable fixed value.
fn requires_machine_interval_spill(
    expanded: &ExpandedAllocationProblem,
    request: &RegionSpillRequest,
    interval: &super::live_interval::LiveInterval,
) -> Result<bool, AllocationSplitError> {
    let root = expanded_root(expanded, request.root)?;
    let mut recipe_owned = BTreeSet::new();
    for &use_id in &request.uses {
        let use_ = root.uses.get(use_id.0 as usize).ok_or_else(|| {
            AllocationSplitError::new(
                "ALLOCATION_SPILL.USE_RANGE",
                Some(interval.definition.block()),
                Some(request.value),
                Some(request.root),
                "spill request references a semantic use outside its root",
            )
        })?;
        if use_.id != use_id || use_.value != request.value {
            return Err(AllocationSplitError::new(
                "ALLOCATION_SPILL.USE_OWNERSHIP",
                Some(use_.site.block()),
                Some(request.value),
                Some(request.root),
                "spill request no longer owns one of its semantic uses",
            ));
        }
        recipe_owned.insert(use_.site);
    }

    // A logical-root home store is an effectful use deliberately retained by
    // the HDL-specific path. It does not by itself turn the value into a
    // SplitEditor product.
    let synthetic = expanded
        .ir
        .index_synthetic_instructions()
        .map_err(|error| AllocationSplitError::ir(error, request.root))?;
    for home in &expanded.stack_homes {
        let super::allocation_expand::ExpandedStackDefinition::Store { instruction, value } =
            home.definition
        else {
            continue;
        };
        if value != request.value {
            continue;
        }
        let site = expanded
            .ir
            .resolve_stack_store_use_site_indexed(
                instruction,
                home.id,
                value,
                &expanded.intervals,
                &synthetic,
            )
            .map_err(|error| AllocationSplitError::ir(error, request.root))?;
        recipe_owned.insert(site);
    }

    Ok(interval
        .uses
        .iter()
        .any(|site| !recipe_owned.contains(site)))
}

/// Mutate only semantic ownership and allocation IR. Liveness, constraints,
/// dead-materialization pruning, and physical allocation remain untouched
/// until every symbolic spill in the allocation round has been applied.
fn mutate_verified_split(
    expanded: &mut ExpandedAllocationProblem,
    graph: &HomeGraph,
    plan: &PlannedRegionSplit,
    journal: &mut SplitMutationJournal,
    spiller: &Spiller,
) -> Result<(), AllocationSplitError> {
    let before = root_split_progress(expanded_root(expanded, plan.root)?);
    let graph_root = graph_root(graph, plan.root)?;
    let replaces_complete_origin = plan.value == graph_root.origin && plan.retained.is_empty();
    let SpillEdit {
        liveness_blocks,
        constraint_blocks,
    } = spiller
        .materialize(
            expanded,
            graph,
            plan.value,
            plan.root,
            &plan.entries,
            &plan.spill,
            replaces_complete_origin,
        )
        .map_err(AllocationSplitError::spill)?;
    journal.liveness_blocks.extend(liveness_blocks);
    journal.constraint_blocks.extend(constraint_blocks);

    retarget_retained_fragment(expanded, &plan.split)?;
    prune_replaced_register_region(expanded, plan.root, plan.value, plan.source_region)?;
    let after = root_split_progress(expanded_root(expanded, plan.root)?);
    if after >= before {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.NON_MONOTONIC",
            Some(plan.primary_cut().block()),
            Some(plan.value),
            Some(plan.root),
            format!("split progress {before:?} did not decrease: {after:?}"),
        ));
    }
    Ok(())
}

/// Preserve the color decision made together with the split frontier.  Use
/// ownership and register-region metadata are one fact and must be changed
/// together; otherwise the rebuilt allocation row silently loses the
/// planner's proved prefix color.
fn retarget_retained_fragment(
    expanded: &mut ExpandedAllocationProblem,
    plan: &RegionSplitPlan,
) -> Result<(), AllocationSplitError> {
    if plan.retained.is_empty() {
        return Ok(());
    }
    let root_index = plan.root.0 as usize;
    let root_origin = expanded
        .roots
        .get(root_index)
        .ok_or_else(|| {
            AllocationSplitError::new(
                "ALLOCATION_SPLIT.ROOT_RANGE",
                None,
                Some(plan.value),
                Some(plan.root),
                "retained split fragment references a missing expanded root",
            )
        })?
        .origin;
    for &use_id in &plan.retained {
        let use_ = expanded
            .roots
            .get_mut(root_index)
            .and_then(|root| root.uses.get_mut(use_id.0 as usize))
            .ok_or_else(|| {
                AllocationSplitError::new(
                    "ALLOCATION_SPLIT.USE_RANGE",
                    None,
                    Some(plan.value),
                    Some(plan.root),
                    format!("retained split use {use_id:?} is outside its root"),
                )
            })?;
        if use_.value != plan.value {
            return Err(AllocationSplitError::new(
                "ALLOCATION_SPLIT.RETAINED_OWNERSHIP",
                Some(use_.site.block()),
                Some(plan.value),
                Some(plan.root),
                "retained use was rewritten while materializing the moved suffix",
            ));
        }
        match (&mut use_.source, plan.source_region) {
            (ExpandedUseSource::OriginalRegister { preferred_register }, None)
                if plan.value == root_origin =>
            {
                *preferred_register = Some(plan.register);
            }
            (
                ExpandedUseSource::RegisterRegion {
                    region,
                    preferred_register,
                },
                Some(source_region),
            ) if *region == source_region => {
                *preferred_register = Some(plan.register);
            }
            _ => {
                return Err(AllocationSplitError::new(
                    "ALLOCATION_SPLIT.RETAINED_SOURCE",
                    Some(use_.site.block()),
                    Some(plan.value),
                    Some(plan.root),
                    "retained use no longer belongs to the planned source region",
                ));
            }
        }
    }
    if let Some(region) = plan.source_region {
        let metadata = expanded
            .region_rows
            .get(&region)
            .and_then(|row| expanded.register_regions.get_mut(*row))
            .ok_or_else(|| {
                AllocationSplitError::new(
                    "ALLOCATION_SPLIT.REGION_METADATA",
                    None,
                    Some(plan.value),
                    Some(plan.root),
                    "retained fragment references missing register-region metadata",
                )
            })?;
        if metadata.root != plan.root || metadata.value != plan.value {
            return Err(AllocationSplitError::new(
                "ALLOCATION_SPLIT.REGION_METADATA",
                None,
                Some(plan.value),
                Some(plan.root),
                "retained register-region metadata has incompatible ownership",
            ));
        }
        metadata.preferred_register = Some(plan.register);
    }
    Ok(())
}

fn finish_split_mutations(
    expanded: &mut ExpandedAllocationProblem,
    cfg: &NormalizedCfg,
    journal: &mut SplitMutationJournal,
    replacements: &[(VReg, LiveBundleId)],
) -> Result<IncrementalLivenessUpdate, AllocationSplitError> {
    let mut liveness = allocation_expand::refresh(expanded, cfg, &journal.liveness_blocks)
        .map_err(AllocationSplitError::expand)?;
    let replaced = replacements
        .iter()
        .map(|(value, _)| *value)
        .collect::<Vec<_>>();
    let pruned_blocks = expanded
        .ir
        .prune_dead_materializations_from(&expanded.intervals, replaced)
        .map_err(|error| {
            let root = error.values.first().and_then(|value| {
                replacements
                    .iter()
                    .find(|(replaced, _)| replaced == value)
                    .map(|(_, root)| *root)
            });
            AllocationSplitError::new(
                error.rule,
                error.block,
                error.values.first().copied(),
                root,
                error.message,
            )
        })?;
    if !pruned_blocks.is_empty() {
        for &block in &pruned_blocks {
            journal.record_block(block);
        }
        liveness.extend(
            allocation_expand::refresh(expanded, cfg, &pruned_blocks)
                .map_err(AllocationSplitError::expand)?,
        );
    }
    prune_dead_register_representatives(expanded, replacements, &liveness.range_changed_values)?;
    Ok(liveness)
}

/// Incremental DCE can delete an older split copy while pruning the operand
/// cone of the value currently being spilled. Representative metadata is an
/// allocation fact, so it must be retired in the same root transaction once
/// exact liveness proves that its machine definition is dead.
fn prune_dead_register_representatives(
    expanded: &mut ExpandedAllocationProblem,
    replacements: &[(VReg, LiveBundleId)],
    range_changed_values: &[VReg],
) -> Result<(), AllocationSplitError> {
    let changed_roots = replacements
        .iter()
        .map(|(_, root)| *root)
        .collect::<BTreeSet<_>>();
    for &value in range_changed_values {
        if expanded
            .intervals
            .intervals
            .get(value.0 as usize)
            .and_then(Option::as_ref)
            .is_some()
        {
            continue;
        }
        let Some(region) = expanded.region_by_value.remove(&value) else {
            continue;
        };
        let row = expanded.region_rows.remove(&region).ok_or_else(|| {
            AllocationSplitError::new(
                "ALLOCATION_SPLIT.REGION_IDENTITY",
                None,
                Some(value),
                None,
                "dead register representative is absent from the stable region index",
            )
        })?;
        let metadata = expanded.register_regions.swap_remove(row);
        if metadata.id != region || metadata.value != value {
            return Err(AllocationSplitError::new(
                "ALLOCATION_SPLIT.REGION_VALUE_IDENTITY",
                None,
                Some(value),
                Some(metadata.root),
                "dead register representative disagrees with its machine-value index",
            ));
        }
        if !changed_roots.contains(&metadata.root) {
            return Err(AllocationSplitError::new(
                "ALLOCATION_SPLIT.DEAD_REGION_ROOT",
                None,
                Some(metadata.value),
                Some(metadata.root),
                "incremental DCE removed a representative outside the changed-root transaction",
            ));
        }
        let root = expanded_root(expanded, metadata.root)?;
        if root.uses.iter().any(|use_| {
            use_.value == metadata.value
                || matches!(
                    use_.source,
                    ExpandedUseSource::RegisterRegion { region, .. } if region == metadata.id
                )
        }) {
            return Err(AllocationSplitError::new(
                "ALLOCATION_SPLIT.DEAD_REGION_USE",
                root.uses
                    .iter()
                    .find(|use_| use_.value == metadata.value)
                    .map(|use_| use_.site.block()),
                Some(metadata.value),
                Some(metadata.root),
                "dead machine representative is still claimed by a semantic root use",
            ));
        }
        if let Some(moved) = expanded.register_regions.get(row) {
            expanded.region_rows.insert(moved.id, row);
        }
    }
    Ok(())
}

fn cheapest_spill_candidate(
    joint: &JointAllocationProblem,
    request: &RegionSplitRequest,
) -> Result<VReg, AllocationSplitError> {
    let mut best = None::<(VReg, u64, u64, usize)>;
    for candidate in &request.candidates {
        let value = joint.value(candidate.value).ok_or_else(|| {
            AllocationSplitError::new(
                "ALLOCATION_SPILL.CANDIDATE_RANGE",
                Some(request.definition.block()),
                Some(candidate.value),
                Some(candidate.root),
                "spill candidate is outside joint allocation",
            )
        })?;
        let cost = value.spill_cost.ok_or_else(|| {
            AllocationSplitError::new(
                "ALLOCATION_SPILL.CANDIDATE_CLASS",
                Some(value.interval.definition.block()),
                Some(candidate.value),
                Some(candidate.root),
                "fixed transition was selected as a spill candidate",
            )
        })?;
        let candidate_key = (
            candidate.value,
            cost,
            value.live_length,
            candidate.uses.len(),
        );
        let replace = best.is_none_or(|(best_value, best_cost, best_length, best_uses)| {
            let candidate_density = u128::from(cost) * u128::from(best_length);
            let best_density = u128::from(best_cost) * u128::from(value.live_length);
            candidate_density < best_density
                || (candidate_density == best_density
                    && (candidate.uses.len() < best_uses
                        || (candidate.uses.len() == best_uses && candidate.value < best_value)))
        });
        if replace {
            best = Some(candidate_key);
        }
    }
    best.map(|(value, _, _, _)| value).ok_or_else(|| {
        AllocationSplitError::new(
            "ALLOCATION_SPILL.NO_CANDIDATE",
            Some(request.definition.block()),
            Some(request.blocked_value),
            None,
            "failed split request contains no interval that can advance to Spill",
        )
    })
}

/// Iterate conventional live-range splitting, spilling, and greedy coloring
/// to a fixed point. Per-value allocator stages and legal machine boundaries
/// provide progress; semantic root-use counts are not an iteration bound once
/// strict-SSA edits create real copy and phi representatives.
pub(super) fn allocate_with_splitting(
    expanded: &mut ExpandedAllocationProblem,
    graph: &HomeGraph,
    cfg: &NormalizedCfg,
    registers: &[PhysReg],
) -> Result<JointAllocation, AllocationSplitError> {
    let timing = std::env::var_os("CELOX_REGALLOC_TIMING").is_some()
        || std::env::var_os("CELOX_PHASE_TIMING").is_some();
    let mut allocate_elapsed = std::time::Duration::ZERO;
    let mut select_elapsed = std::time::Duration::ZERO;
    let mut edit_elapsed = std::time::Duration::ZERO;
    let mut update_elapsed = std::time::Duration::ZERO;
    let mut allocate_calls = 0_u64;
    let mut split_edits = 0_u64;
    let mut spill_edits = 0_u64;
    let spiller = Spiller::build(graph).map_err(AllocationSplitError::spill)?;
    let mut allocator =
        GreedyAllocator::for_function(expanded, cfg, graph, registers, spiller.home_plans())
            .map_err(AllocationSplitError::joint)?;
    loop {
        let allocate_start = timing.then(crate::timing::now);
        let outcome = allocator
            .allocate(cfg, registers)
            .map_err(AllocationSplitError::joint)?;
        allocate_calls += 1;
        if let Some(start) = allocate_start {
            allocate_elapsed += start.elapsed();
        }
        match outcome {
            JointAllocationOutcome::Complete(allocation) => {
                if timing {
                    eprintln!(
                        "[regalloc-timing] interval_split_session blocks={} calls={allocate_calls} splits={split_edits} spills={spill_edits} allocate={allocate_elapsed:?} select={select_elapsed:?} edit={edit_elapsed:?} update={update_elapsed:?}",
                        cfg.successors.len(),
                    );
                }
                return Ok(allocation);
            }
            JointAllocationOutcome::NeedsSplit(request) => {
                let select_start = timing.then(crate::timing::now);
                let (candidate, frontier) = match select_live_range_frontier(
                    expanded,
                    allocator.live_intervals(),
                    &request,
                ) {
                    Ok(selection) => selection,
                    Err(error) if error.rule == "ALLOCATION_SPLIT.NO_PROGRESS" => {
                        let value = cheapest_spill_candidate(allocator.live_intervals(), &request)?;
                        allocator
                            .require_spill(value)
                            .map_err(AllocationSplitError::joint)?;
                        if let Some(start) = select_start {
                            select_elapsed += start.elapsed();
                        }
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                if let Some(start) = select_start {
                    select_elapsed += start.elapsed();
                }
                let edit_start = timing.then(crate::timing::now);
                allocator
                    .release_for_split(candidate.value)
                    .map_err(AllocationSplitError::joint)?;
                let applied = apply_live_range_edit(
                    expanded,
                    allocator.live_intervals(),
                    &candidate,
                    &frontier,
                    cfg,
                )?;
                split_edits += 1;
                if let Some(start) = edit_start {
                    edit_elapsed += start.elapsed();
                }
                let update_start = timing.then(crate::timing::now);
                allocator
                    .update_after_edit(
                        expanded,
                        cfg,
                        graph,
                        registers,
                        &applied.constraint_blocks,
                        &applied.changed_values,
                        &applied.range_changed_values,
                        &applied.live_lengths,
                        std::slice::from_ref(&applied.root),
                        spiller.home_plans(),
                    )
                    .map_err(AllocationSplitError::joint)?;
                if let Some(start) = update_start {
                    update_elapsed += start.elapsed();
                }
            }
            JointAllocationOutcome::NeedsSpill(request) => {
                let edit_start = timing.then(crate::timing::now);
                let applied = apply_spill(
                    expanded,
                    graph,
                    allocator.live_intervals(),
                    &request,
                    cfg,
                    &spiller,
                )?;
                spill_edits += 1;
                if let Some(start) = edit_start {
                    edit_elapsed += start.elapsed();
                }
                let update_start = timing.then(crate::timing::now);
                allocator
                    .update_after_edit(
                        expanded,
                        cfg,
                        graph,
                        registers,
                        &applied.constraint_blocks,
                        &applied.changed_values,
                        &applied.range_changed_values,
                        &applied.live_lengths,
                        std::slice::from_ref(&applied.root),
                        spiller.home_plans(),
                    )
                    .map_err(AllocationSplitError::joint)?;
                if let Some(start) = update_start {
                    update_elapsed += start.elapsed();
                }
            }
        }
    }
}

fn verify_candidate<'a>(
    joint: &'a JointAllocationProblem,
    candidate: &RegionSplitCandidate,
    cut: AllocationPressurePoint,
) -> Result<&'a AllocationValue, AllocationSplitError> {
    let value = joint.value(candidate.value).ok_or_else(|| {
        AllocationSplitError::new(
            "ALLOCATION_SPLIT.CANDIDATE_RANGE",
            Some(cut.block()),
            Some(candidate.value),
            Some(candidate.root),
            "split candidate is outside joint allocation",
        )
    })?;
    let AllocationValueClass::Region { root, uses } = &value.class else {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.CANDIDATE_CLASS",
            Some(value.interval.definition.block()),
            Some(candidate.value),
            Some(candidate.root),
            "split candidate is not a retained register region",
        ));
    };
    if *root != candidate.root || *uses != candidate.uses {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.CANDIDATE_IDENTITY",
            Some(value.interval.definition.block()),
            Some(candidate.value),
            Some(candidate.root),
            "split request and joint region ownership disagree",
        ));
    }
    verify_pressure_point(value, candidate, cut)?;
    Ok(value)
}

fn verify_pressure_point(
    value: &AllocationValue,
    candidate: &RegionSplitCandidate,
    cut: AllocationPressurePoint,
) -> Result<(), AllocationSplitError> {
    let Some(frontier) = candidate
        .frontiers
        .iter()
        .find(|frontier| frontier.register == cut.register())
    else {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.PRESSURE_REGISTER",
            Some(cut.block()),
            Some(candidate.value),
            Some(candidate.root),
            "split cut register has no candidate frontier",
        ));
    };
    if frontier.points.binary_search(&cut).is_err() {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.PRESSURE_POINT_IDENTITY",
            Some(cut.block()),
            Some(candidate.value),
            Some(candidate.root),
            "split cut is not one of the owner-qualified interference points",
        ));
    }
    if !value.allowed_registers.contains(cut.register()) {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.PRESSURE_REGISTER",
            Some(cut.block()),
            Some(candidate.value),
            Some(candidate.root),
            "split cut names a register forbidden to the candidate value",
        ));
    }
    if !value.interval.covers(cut.block(), cut.slot()) {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.PRESSURE_POINT",
            Some(cut.block()),
            Some(candidate.value),
            Some(candidate.root),
            "candidate live range does not cover the exact pressure point",
        ));
    }
    Ok(())
}

fn candidate_from_plan(
    joint: &JointAllocationProblem,
    plan: &RegionSplitPlan,
) -> Result<RegionSplitCandidate, AllocationSplitError> {
    let value = joint.value(plan.value).ok_or_else(|| {
        AllocationSplitError::new(
            "ALLOCATION_SPLIT.CANDIDATE_RANGE",
            Some(plan.primary_cut().block()),
            Some(plan.value),
            Some(plan.root),
            "split plan references a value outside joint allocation",
        )
    })?;
    let AllocationValueClass::Region { root, uses } = &value.class else {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.CANDIDATE_CLASS",
            Some(plan.primary_cut().block()),
            Some(plan.value),
            Some(plan.root),
            "split plan no longer references a register region",
        ));
    };
    if *root != plan.root {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.CANDIDATE_IDENTITY",
            Some(plan.primary_cut().block()),
            Some(plan.value),
            Some(plan.root),
            "split plan root differs from current joint allocation",
        ));
    }
    Ok(RegionSplitCandidate {
        value: plan.value,
        root: plan.root,
        uses: uses.clone(),
        frontiers: vec![RegisterPressureFrontier {
            register: plan.register,
            points: plan.cuts.clone(),
        }],
    })
}

fn expanded_root(
    expanded: &ExpandedAllocationProblem,
    root: LiveBundleId,
) -> Result<&super::allocation_expand::ExpandedRoot, AllocationSplitError> {
    let row = expanded.roots.get(root.0 as usize).ok_or_else(|| {
        AllocationSplitError::new(
            "ALLOCATION_SPLIT.ROOT_RANGE",
            None,
            None,
            Some(root),
            "split root is outside the expanded allocation problem",
        )
    })?;
    if row.id != root {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.ROOT_IDENTITY",
            None,
            Some(row.origin),
            Some(root),
            "expanded root differs from its dense identity",
        ));
    }
    Ok(row)
}

fn graph_root(graph: &HomeGraph, root: LiveBundleId) -> Result<&LiveBundle, AllocationSplitError> {
    let row = graph.bundles.get(root.0 as usize).ok_or_else(|| {
        AllocationSplitError::new(
            "ALLOCATION_SPLIT.ROOT_RANGE",
            None,
            None,
            Some(root),
            "split root is outside the immutable HomeGraph",
        )
    })?;
    if row.id != root {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.ROOT_IDENTITY",
            Some(row.definition.block()),
            Some(row.origin),
            Some(root),
            "HomeGraph root differs from its dense identity",
        ));
    }
    Ok(row)
}

fn expanded_use(
    expanded: &ExpandedAllocationProblem,
    root: LiveBundleId,
    use_id: BundleUseId,
) -> Result<&super::allocation_expand::ExpandedUse, AllocationSplitError> {
    let root = expanded_root(expanded, root)?;
    root.uses.get(use_id.0 as usize).ok_or_else(|| {
        AllocationSplitError::new(
            "ALLOCATION_SPLIT.USE_RANGE",
            None,
            Some(root.origin),
            Some(root.id),
            format!("split use {use_id:?} is outside its root"),
        )
    })
}

fn region_source(
    expanded: &ExpandedAllocationProblem,
    root: &super::allocation_expand::ExpandedRoot,
    candidate: &RegionSplitCandidate,
    value: &AllocationValue,
) -> Result<RegionSource, AllocationSplitError> {
    let preferred_register = value.preferred_register;
    let mut source_region = None::<RegisterRegionId>;
    let mut original = false;
    for &use_id in &candidate.uses {
        let use_ = root.uses.get(use_id.0 as usize).ok_or_else(|| {
            AllocationSplitError::new(
                "ALLOCATION_SPLIT.USE_RANGE",
                Some(value.interval.definition.block()),
                Some(candidate.value),
                Some(candidate.root),
                format!("candidate use {use_id:?} is outside its expanded root"),
            )
        })?;
        if use_.value != candidate.value {
            return Err(AllocationSplitError::new(
                "ALLOCATION_SPLIT.USE_OWNERSHIP",
                Some(use_.site.block()),
                Some(candidate.value),
                Some(candidate.root),
                "candidate use is owned by a different expanded machine value",
            ));
        }
        match use_.source {
            ExpandedUseSource::OriginalRegister {
                preferred_register: use_preference,
            } => {
                if use_preference != preferred_register || candidate.value != root.origin {
                    return Err(AllocationSplitError::new(
                        "ALLOCATION_SPLIT.REGION_SOURCE",
                        Some(use_.site.block()),
                        Some(candidate.value),
                        Some(candidate.root),
                        "original register source has inconsistent identity or preference",
                    ));
                }
                original = true;
            }
            ExpandedUseSource::RegisterRegion {
                region,
                preferred_register: use_preference,
            } => {
                if use_preference != preferred_register
                    || source_region.is_some_and(|existing| existing != region)
                {
                    return Err(AllocationSplitError::new(
                        "ALLOCATION_SPLIT.REGION_SOURCE",
                        Some(use_.site.block()),
                        Some(candidate.value),
                        Some(candidate.root),
                        "expanded region uses disagree on identity or preference",
                    ));
                }
                source_region = Some(region);
            }
            ExpandedUseSource::Materialized(_) | ExpandedUseSource::Edge(_) => {
                return Err(AllocationSplitError::new(
                    "ALLOCATION_SPLIT.REGION_SOURCE",
                    Some(use_.site.block()),
                    Some(candidate.value),
                    Some(candidate.root),
                    "materialized singleton was selected as a register region",
                ));
            }
        }
    }
    if original == source_region.is_some() {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.REGION_SOURCE",
            Some(value.interval.definition.block()),
            Some(candidate.value),
            Some(candidate.root),
            "candidate mixes original and synthetic region ownership",
        ));
    }
    let entry_use = if let Some(region) = source_region {
        let metadata = expanded
            .region_rows
            .get(&region)
            .and_then(|row| expanded.register_regions.get(*row))
            .ok_or_else(|| {
                AllocationSplitError::new(
                    "ALLOCATION_SPLIT.REGION_METADATA",
                    Some(value.interval.definition.block()),
                    Some(candidate.value),
                    Some(candidate.root),
                    "candidate references missing expanded-region metadata",
                )
            })?;
        if metadata.root != candidate.root
            || metadata.value != candidate.value
            || metadata.preferred_register != preferred_register
            || metadata
                .entry_use
                .is_some_and(|entry_use| !candidate.uses.contains(&entry_use))
        {
            return Err(AllocationSplitError::new(
                "ALLOCATION_SPLIT.REGION_METADATA",
                Some(value.interval.definition.block()),
                Some(candidate.value),
                Some(candidate.root),
                "expanded-region metadata disagrees with candidate ownership",
            ));
        }
        metadata.entry_use
    } else {
        None
    };
    Ok(RegionSource {
        preferred_register,
        region: source_region,
        entry_use,
    })
}

/// Project every branch cut of one physical-register frontier through a
/// single sparse live-graph traversal. We need the union of displaced uses,
/// not one independent result per cut; a block therefore carries only a
/// reached bit and the earliest local cut slot.
fn reachable_uses_at_frontier(
    expanded: &ExpandedAllocationProblem,
    root: &super::allocation_expand::ExpandedRoot,
    candidate: &RegionSplitCandidate,
    value: &AllocationValue,
    cuts: &[AllocationPressurePoint],
    cfg: &NormalizedCfg,
) -> Result<Option<Vec<BundleUseId>>, AllocationSplitError> {
    let block_count = cfg.idom.len();
    if cuts.is_empty() || expanded.intervals.block_slots.len() != block_count {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.LIVE_GRAPH",
            cuts.first().map(|cut| cut.block()),
            Some(candidate.value),
            Some(candidate.root),
            "register-frontier projection requires cuts and complete block slots",
        ));
    }
    let mut segments = vec![None::<LiveSegment>; block_count];
    for &segment in &value.interval.segments {
        let block = cfg
            .block_index
            .get(&segment.block)
            .copied()
            .ok_or_else(|| {
                AllocationSplitError::new(
                    "ALLOCATION_SPLIT.LIVE_GRAPH",
                    Some(segment.block),
                    Some(candidate.value),
                    Some(candidate.root),
                    "candidate segment references a block outside the CFG",
                )
            })?;
        if segments[block].replace(segment).is_some() {
            return Err(AllocationSplitError::new(
                "ALLOCATION_SPLIT.LIVE_GRAPH",
                Some(segment.block),
                Some(candidate.value),
                Some(candidate.root),
                "candidate has more than one canonical segment in a CFG block",
            ));
        }
    }

    let register = cuts[0].register();
    let mut reached = vec![false; block_count];
    let mut starts = vec![None::<SlotIndex>; block_count];
    let mut reentered = vec![false; block_count];
    let mut queue = VecDeque::new();
    for &cut in cuts {
        if cut.register() != register {
            return Err(AllocationSplitError::new(
                "ALLOCATION_SPLIT.PRESSURE_REGISTER",
                Some(cut.block()),
                Some(candidate.value),
                Some(candidate.root),
                "one split frontier mixes physical registers",
            ));
        }
        let block = cfg.block_index.get(&cut.block()).copied().ok_or_else(|| {
            AllocationSplitError::new(
                "ALLOCATION_SPLIT.PRESSURE_POINT",
                Some(cut.block()),
                Some(candidate.value),
                Some(candidate.root),
                "exact pressure point is outside the normalized CFG",
            )
        })?;
        if !segments[block].is_some_and(|segment| segment.contains(cut.slot())) {
            return Err(AllocationSplitError::new(
                "ALLOCATION_SPLIT.PRESSURE_POINT",
                Some(cut.block()),
                Some(candidate.value),
                Some(candidate.root),
                "candidate segment does not contain an exact pressure slot",
            ));
        }
        starts[block] = Some(starts[block].map_or(cut.slot(), |slot| slot.min(cut.slot())));
        if !reached[block] {
            reached[block] = true;
            queue.push_back(block);
        }
    }

    while let Some(block) = queue.pop_front() {
        let Some(source) = segments[block] else {
            continue;
        };
        if !source.contains(expanded.intervals.block_slots[block].exit) {
            continue;
        }
        for &successor in &cfg.successors[block] {
            let Some(target) = segments[successor] else {
                continue;
            };
            if !target.contains(expanded.intervals.block_slots[successor].entry) {
                continue;
            }
            if starts[successor].is_some() {
                reentered[successor] = true;
            }
            if !reached[successor] {
                reached[successor] = true;
                queue.push_back(successor);
            }
        }
    }

    let mut moved = Vec::new();
    for &use_id in &candidate.uses {
        let use_ = root.uses.get(use_id.0 as usize).ok_or_else(|| {
            AllocationSplitError::new(
                "ALLOCATION_SPLIT.USE_RANGE",
                cuts.first().map(|cut| cut.block()),
                Some(candidate.value),
                Some(candidate.root),
                format!("candidate use {use_id:?} is outside its expanded root"),
            )
        })?;
        if use_.value != candidate.value || !value.interval.contains_use_coordinate(use_.site) {
            return Err(AllocationSplitError::new(
                "ALLOCATION_SPLIT.USE_OWNERSHIP",
                Some(use_.site.block()),
                Some(candidate.value),
                Some(candidate.root),
                "candidate interval does not own an exact expanded use",
            ));
        }
        let block = cfg
            .block_index
            .get(&use_.site.block())
            .copied()
            .ok_or_else(|| {
                AllocationSplitError::new(
                    "ALLOCATION_SPLIT.USE_BLOCK",
                    Some(use_.site.block()),
                    Some(candidate.value),
                    Some(candidate.root),
                    "candidate use is outside the normalized CFG",
                )
            })?;
        if reached[block]
            && (starts[block].is_none()
                || reentered[block]
                || use_.site.slot() >= starts[block].unwrap())
        {
            moved.push(use_id);
        }
    }
    moved.sort_unstable();
    Ok((!moved.is_empty()).then_some(moved))
}

fn reachable_uses(
    expanded: &ExpandedAllocationProblem,
    root: &super::allocation_expand::ExpandedRoot,
    candidate: &RegionSplitCandidate,
    value: &AllocationValue,
    cut: AllocationPressurePoint,
    cfg: &NormalizedCfg,
) -> Result<Vec<BundleUseId>, AllocationSplitError> {
    let block_count = cfg.idom.len();
    if expanded.intervals.block_slots.len() != block_count {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.LIVE_GRAPH",
            Some(cut.block()),
            Some(candidate.value),
            Some(candidate.root),
            "expanded block slots do not cover the normalized CFG",
        ));
    }
    let mut segments = vec![None::<LiveSegment>; block_count];
    for &segment in &value.interval.segments {
        let block = cfg
            .block_index
            .get(&segment.block)
            .copied()
            .ok_or_else(|| {
                AllocationSplitError::new(
                    "ALLOCATION_SPLIT.LIVE_GRAPH",
                    Some(segment.block),
                    Some(candidate.value),
                    Some(candidate.root),
                    "candidate segment references a block outside the CFG",
                )
            })?;
        if segments[block].replace(segment).is_some() {
            return Err(AllocationSplitError::new(
                "ALLOCATION_SPLIT.LIVE_GRAPH",
                Some(segment.block),
                Some(candidate.value),
                Some(candidate.root),
                "candidate has more than one canonical segment in a CFG block",
            ));
        }
    }
    let start = cfg.block_index.get(&cut.block()).copied().ok_or_else(|| {
        AllocationSplitError::new(
            "ALLOCATION_SPLIT.PRESSURE_POINT",
            Some(cut.block()),
            Some(candidate.value),
            Some(candidate.root),
            "exact pressure point is outside the normalized CFG",
        )
    })?;
    if !segments[start].is_some_and(|segment| segment.contains(cut.slot())) {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.PRESSURE_POINT",
            Some(cut.block()),
            Some(candidate.value),
            Some(candidate.root),
            "candidate segment does not contain the exact pressure slot",
        ));
    }

    let mut reached = vec![false; block_count];
    reached[start] = true;
    let mut queue = VecDeque::from([start]);
    let mut reentered_start = false;
    while let Some(block) = queue.pop_front() {
        let Some(source) = segments[block] else {
            continue;
        };
        if !source.contains(expanded.intervals.block_slots[block].exit) {
            continue;
        }
        for &successor in &cfg.successors[block] {
            let Some(target) = segments[successor] else {
                continue;
            };
            if !target.contains(expanded.intervals.block_slots[successor].entry) {
                continue;
            }
            if successor == start {
                reentered_start = true;
            }
            if !reached[successor] {
                reached[successor] = true;
                queue.push_back(successor);
            }
        }
    }

    let mut moved = Vec::new();
    for &use_id in &candidate.uses {
        let use_ = root.uses.get(use_id.0 as usize).ok_or_else(|| {
            AllocationSplitError::new(
                "ALLOCATION_SPLIT.USE_RANGE",
                Some(cut.block()),
                Some(candidate.value),
                Some(candidate.root),
                format!("candidate use {use_id:?} is outside its expanded root"),
            )
        })?;
        if use_.value != candidate.value || !value.interval.contains_use_coordinate(use_.site) {
            return Err(AllocationSplitError::new(
                "ALLOCATION_SPLIT.USE_OWNERSHIP",
                Some(use_.site.block()),
                Some(candidate.value),
                Some(candidate.root),
                "candidate interval does not own an exact expanded use",
            ));
        }
        let block = cfg
            .block_index
            .get(&use_.site.block())
            .copied()
            .ok_or_else(|| {
                AllocationSplitError::new(
                    "ALLOCATION_SPLIT.USE_BLOCK",
                    Some(use_.site.block()),
                    Some(candidate.value),
                    Some(candidate.root),
                    "candidate use is outside the normalized CFG",
                )
            })?;
        if reached[block] && (block != start || reentered_start || use_.site.slot() >= cut.slot()) {
            moved.push(use_id);
        }
    }
    moved.sort_unstable();
    if moved.is_empty() {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.NO_REACHABLE_USE",
            Some(cut.block()),
            Some(candidate.value),
            Some(candidate.root),
            "candidate covers the pressure point but has no reachable owned use to split",
        ));
    }
    Ok(moved)
}

fn partition_moved_uses(
    root: &super::allocation_expand::ExpandedRoot,
    candidate: &RegionSplitCandidate,
    moved: &[BundleUseId],
    previous_entry: Option<BundleUseId>,
    cuts: &[AllocationPressurePoint],
    cfg: &NormalizedCfg,
    dominance: &Dominance,
    topology: &RootUseTopology,
) -> Result<Vec<EntryCluster>, AllocationSplitError> {
    let cut = cuts[0];
    if topology.root != candidate.root || root.id != candidate.root {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.USE_TOPOLOGY_ROOT",
            Some(cut.block()),
            Some(candidate.value),
            Some(candidate.root),
            "split candidate and function-lifetime use topology have different roots",
        ));
    }
    let ordered = topology.ordered_subset(moved)?;
    let full_existing_region = previous_entry.is_some() && moved == candidate.uses;
    let mut clusters = Vec::new();
    let mut cursor = 0usize;
    while cursor < ordered.len() {
        let seed = ordered[cursor];
        let seed_site = root.uses[seed.0 as usize].site;
        let unsafe_loop_entry = cuts
            .iter()
            .copied()
            .any(|cut| dominance.use_dominates_point(cfg, seed_site, cut));
        let repeats_same_boundary = full_existing_region && previous_entry == Some(seed);
        let end = if matches!(seed_site, UseSite::PhiEdge { .. })
            || unsafe_loop_entry
            || repeats_same_boundary
        {
            cursor + 1
        } else {
            let mut end = cursor + 1;
            while end < ordered.len()
                && dominance.use_dominates_use(
                    cfg,
                    seed_site,
                    root.uses[ordered[end].0 as usize].site,
                )
            {
                end += 1;
            }
            end
        };
        let mut uses = ordered[cursor..end].to_vec();
        uses.sort_unstable();
        if uses.binary_search(&seed).is_err() {
            return Err(AllocationSplitError::new(
                "ALLOCATION_SPLIT.CLUSTER_ENTRY",
                Some(seed_site.block()),
                Some(candidate.value),
                Some(candidate.root),
                "dominance cluster does not contain its own entry use",
            ));
        }
        let kind = if uses.len() == 1 {
            SplitEntryKind::Materialized
        } else {
            SplitEntryKind::RegisterRegion
        };
        clusters.push(EntryCluster {
            entry: seed,
            uses,
            kind,
        });
        cursor = end;
    }
    let clustered_use_count = clusters
        .iter()
        .map(|cluster| cluster.uses.len())
        .sum::<usize>();
    if clustered_use_count != moved.len() {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.CLUSTER_COVERAGE",
            Some(cut.block()),
            Some(candidate.value),
            Some(candidate.root),
            "dominance partition did not cover every reachable moved use",
        ));
    }
    clusters.sort_unstable_by_key(|cluster| cluster.entry);
    Ok(clusters)
}

fn sorted_difference(left: &[BundleUseId], removed: &[BundleUseId]) -> Vec<BundleUseId> {
    let mut result = Vec::with_capacity(left.len().saturating_sub(removed.len()));
    let mut removed_cursor = 0usize;
    for &use_id in left {
        while removed_cursor < removed.len() && removed[removed_cursor] < use_id {
            removed_cursor += 1;
        }
        if removed.get(removed_cursor).copied() != Some(use_id) {
            result.push(use_id);
        }
    }
    result
}

fn verify_plan_structure(
    expanded: &ExpandedAllocationProblem,
    joint: &JointAllocationProblem,
    candidate: &RegionSplitCandidate,
    plan: &RegionSplitPlan,
    cfg: &NormalizedCfg,
    dominance: &Dominance,
) -> Result<(), AllocationSplitError> {
    if plan.value != candidate.value || plan.root != candidate.root {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.PLAN_IDENTITY",
            Some(plan.primary_cut().block()),
            Some(plan.value),
            Some(plan.root),
            "split plan and candidate identities differ",
        ));
    }
    if plan.cuts.is_empty()
        || plan.cuts.iter().any(|cut| cut.register() != plan.register)
        || plan.cuts.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.PLAN_FRONTIER",
            None,
            Some(plan.value),
            Some(plan.root),
            "split plan has an empty, mixed, duplicated, or unordered register frontier",
        ));
    }
    let value = verify_candidate(joint, candidate, plan.primary_cut())?;
    for &cut in &plan.cuts[1..] {
        verify_pressure_point(value, candidate, cut)?;
    }
    let root = expanded_root(expanded, plan.root)?;
    let source = region_source(expanded, root, candidate, value)?;
    if source.preferred_register != plan.preferred_register || source.region != plan.source_region {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.PLAN_PREFERENCE",
            Some(plan.primary_cut().block()),
            Some(plan.value),
            Some(plan.root),
            "split plan changed the candidate's register affinity",
        ));
    }
    let mut expected_moved = Vec::new();
    for &cut in &plan.cuts {
        expected_moved.extend(reachable_uses(expanded, root, candidate, value, cut, cfg)?);
    }
    expected_moved.sort_unstable();
    expected_moved.dedup();
    if expected_moved != plan.moved {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.MOVED_SET",
            Some(plan.primary_cut().block()),
            Some(plan.value),
            Some(plan.root),
            "split plan does not move exactly the uses reachable across the pressure point",
        ));
    }
    let mut ownership = BTreeMap::<BundleUseId, usize>::new();
    for entry in &plan.entries {
        if entry.uses.is_empty() || !entry.uses.contains(&entry.entry) {
            return Err(AllocationSplitError::new(
                "ALLOCATION_SPLIT.CLUSTER_ENTRY",
                Some(plan.primary_cut().block()),
                Some(plan.value),
                Some(plan.root),
                "split entry does not own its entry use",
            ));
        }
        match entry.kind {
            SplitEntryKind::Materialized if entry.uses.as_slice() != [entry.entry] => {
                return Err(AllocationSplitError::new(
                    "ALLOCATION_SPLIT.SINGLETON_SHAPE",
                    Some(plan.primary_cut().block()),
                    Some(plan.value),
                    Some(plan.root),
                    "materialized entry is not an exact singleton",
                ));
            }
            SplitEntryKind::RegisterRegion => {
                if entry.uses.len() < 2 {
                    return Err(AllocationSplitError::new(
                        "ALLOCATION_SPLIT.REGION_SHAPE",
                        Some(plan.primary_cut().block()),
                        Some(plan.value),
                        Some(plan.root),
                        "register region has fewer than two uses",
                    ));
                }
                let entry_site = root.uses[entry.entry.0 as usize].site;
                if !matches!(entry_site, UseSite::Instruction { .. })
                    || entry.uses.iter().any(|use_id| {
                        !dominance.use_dominates_use(
                            cfg,
                            entry_site,
                            root.uses[use_id.0 as usize].site,
                        )
                    })
                {
                    return Err(AllocationSplitError::new(
                        "ALLOCATION_SPLIT.REGION_DOMINANCE",
                        Some(entry_site.block()),
                        Some(plan.value),
                        Some(plan.root),
                        "register-region entry does not dominate every owned use",
                    ));
                }
                if plan.moved == candidate.uses && source.entry_use == Some(entry.entry) {
                    return Err(AllocationSplitError::new(
                        "ALLOCATION_SPLIT.REPEATED_BOUNDARY",
                        Some(entry_site.block()),
                        Some(plan.value),
                        Some(plan.root),
                        "split recreates the same region at the same immutable use boundary",
                    ));
                }
                if entry
                    .preferred_register
                    .is_some_and(|register| !value.allowed_registers.contains(register))
                {
                    return Err(AllocationSplitError::new(
                        "ALLOCATION_SPLIT.SYMBOLIC_CONSTRAINT",
                        Some(entry_site.block()),
                        Some(plan.value),
                        Some(plan.root),
                        "symbolic register-region color violates source constraints",
                    ));
                }
            }
            SplitEntryKind::Materialized => {
                if entry.preferred_register.is_some() {
                    return Err(AllocationSplitError::new(
                        "ALLOCATION_SPLIT.SYMBOLIC_SINGLETON",
                        Some(plan.primary_cut().block()),
                        Some(plan.value),
                        Some(plan.root),
                        "singleton materialization carries a nonexistent persistent color",
                    ));
                }
            }
        }
        for &use_id in &entry.uses {
            *ownership.entry(use_id).or_default() += 1;
        }
    }
    if plan
        .moved
        .iter()
        .any(|use_id| ownership.get(use_id) != Some(&1))
        || ownership.keys().copied().collect::<Vec<_>>() != plan.moved
    {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.CLUSTER_COVERAGE",
            Some(plan.primary_cut().block()),
            Some(plan.value),
            Some(plan.root),
            "split entries do not partition the moved use set exactly once",
        ));
    }
    let moved = plan.moved.iter().copied().collect::<BTreeSet<_>>();
    let expected_retained = candidate
        .uses
        .iter()
        .copied()
        .filter(|use_id| !moved.contains(use_id))
        .collect::<Vec<_>>();
    if plan.retained != expected_retained {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.RETAINED_SET",
            Some(plan.primary_cut().block()),
            Some(plan.value),
            Some(plan.root),
            "retained and moved uses do not partition the candidate region",
        ));
    }

    Ok(())
}

fn verify_plan(
    expanded: &ExpandedAllocationProblem,
    joint: &JointAllocationProblem,
    candidate: &RegionSplitCandidate,
    plan: &PlannedRegionSplit,
    cfg: &NormalizedCfg,
    dominance: &Dominance,
    spiller: &Spiller,
) -> Result<(), AllocationSplitError> {
    verify_plan_structure(expanded, joint, candidate, &plan.split, cfg, dominance)?;
    spiller
        .verify(expanded, &plan.entries, &plan.spill)
        .map_err(AllocationSplitError::spill)?;
    Ok(())
}

fn prune_replaced_register_region(
    expanded: &mut ExpandedAllocationProblem,
    root: LiveBundleId,
    value: VReg,
    region: Option<RegisterRegionId>,
) -> Result<(), AllocationSplitError> {
    let Some(region) = region else {
        return Ok(());
    };
    let root_row = expanded_root(expanded, root)?;
    if root_row.uses.iter().any(|use_| {
        matches!(
            use_.source,
            ExpandedUseSource::RegisterRegion {
                region: use_region,
                ..
            } if use_region == region
        )
    }) {
        return Ok(());
    }
    let row = expanded.region_rows.remove(&region).ok_or_else(|| {
        AllocationSplitError::new(
            "ALLOCATION_SPLIT.REGION_IDENTITY",
            None,
            Some(value),
            Some(root),
            "replaced register region is absent from the stable metadata index",
        )
    })?;
    if expanded.region_by_value.remove(&value) != Some(region) {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.REGION_VALUE_IDENTITY",
            None,
            Some(value),
            Some(root),
            "replaced register region is absent from the machine-value index",
        ));
    }
    let removed = expanded.register_regions.swap_remove(row);
    if removed.id != region || removed.root != root || removed.value != value {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.REGION_IDENTITY",
            None,
            Some(value),
            Some(root),
            "removed register-region metadata differs from the split source",
        ));
    }
    if let Some(moved) = expanded.register_regions.get(row) {
        expanded.region_rows.insert(moved.id, row);
    }
    Ok(())
}

fn split_progress(expanded: &ExpandedAllocationProblem) -> SplitProgress {
    expanded.roots.iter().map(root_split_progress).fold(
        SplitProgress {
            paired_uses: 0,
            original_uses: 0,
            register_uses: 0,
        },
        |mut total, root| {
            total.paired_uses = total.paired_uses.saturating_add(root.paired_uses);
            total.original_uses = total.original_uses.saturating_add(root.original_uses);
            total.register_uses = total.register_uses.saturating_add(root.register_uses);
            total
        },
    )
}

fn root_split_progress(root: &super::allocation_expand::ExpandedRoot) -> SplitProgress {
    let mut regions = BTreeMap::<VReg, (u128, bool)>::new();
    for use_ in &root.uses {
        let original = match use_.source {
            ExpandedUseSource::OriginalRegister { .. } => true,
            ExpandedUseSource::RegisterRegion { .. } => false,
            ExpandedUseSource::Materialized(_) | ExpandedUseSource::Edge(_) => continue,
        };
        let region = regions.entry(use_.value).or_insert((0, original));
        region.0 += 1;
        region.1 |= original;
    }
    let mut progress = SplitProgress {
        paired_uses: 0,
        original_uses: 0,
        register_uses: 0,
    };
    for (_, (uses, original)) in regions {
        progress.paired_uses += uses.saturating_mul(uses.saturating_sub(1)) / 2;
        progress.register_uses += uses;
        if original {
            progress.original_uses += uses;
        }
    }
    progress
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::native::mir::{
        BaseReg, MBlock, MFunction, MInst, OpSize, PhiNode, SpillDesc, VRegAllocator,
    };

    use super::super::allocation_expand::{expand, expand_unallocated};
    use super::super::home_graph;
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

    fn model(
        function: &mut MFunction,
        registers: &[PhysReg],
    ) -> (NormalizedCfg, HomeGraph, ExpandedAllocationProblem) {
        let cfg = super::super::cfg::normalize(function).unwrap();
        let graph = home_graph::build(function, &cfg).unwrap();
        let allocation = allocate_roots(&graph, &cfg, registers).unwrap();
        let expanded = expand(function, &cfg, &graph, &allocation, registers).unwrap();
        (cfg, graph, expanded)
    }

    fn candidate(joint: &JointAllocationProblem, value: VReg) -> RegionSplitCandidate {
        let value = joint.value(value).unwrap();
        let AllocationValueClass::Region { root, uses } = &value.class else {
            panic!("requested test value is not a register region");
        };
        RegionSplitCandidate {
            value: value.value,
            root: *root,
            uses: uses.clone(),
            frontiers: Vec::new(),
        }
    }

    fn request(
        joint: &JointAllocationProblem,
        blocked: VReg,
        mut candidate: RegionSplitCandidate,
    ) -> RegionSplitRequest {
        let definition = joint.value(blocked).unwrap().interval.definition;
        candidate.frontiers = vec![RegisterPressureFrontier {
            register: PhysReg::RAX,
            points: vec![AllocationPressurePoint {
                register: PhysReg::RAX,
                block: definition.block(),
                slot: definition.slot(),
            }],
        }];
        RegionSplitRequest {
            blocked_value: blocked,
            definition,
            conflicts: Vec::new(),
            candidates: vec![candidate],
        }
    }

    fn root_for(
        expanded: &ExpandedAllocationProblem,
        origin: VReg,
    ) -> &super::super::allocation_expand::ExpandedRoot {
        expanded
            .roots
            .iter()
            .find(|root| root.origin == origin)
            .unwrap()
    }

    #[test]
    fn split_editor_children_own_machine_intervals_even_without_direct_root_uses() {
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: VReg(0),
            value: 7,
        });
        entry.push(MInst::LoadImm {
            dst: VReg(1),
            value: 1,
        });
        entry.push(MInst::Branch {
            cond: VReg(1),
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });
        let mut left = MBlock::new(BlockId(1));
        left.push(MInst::Jump { target: BlockId(3) });
        let mut right = MBlock::new(BlockId(2));
        right.push(MInst::Jump { target: BlockId(3) });
        let mut merge = MBlock::new(BlockId(3));
        merge.push(MInst::Mov {
            dst: VReg(2),
            src: VReg(0),
        });
        merge.push(MInst::Return);
        let mut function = function(3, Vec::new());
        function.blocks = vec![entry, left, right, merge];
        let cfg = super::super::cfg::normalize(&mut function).unwrap();
        let graph = home_graph::build(&function, &cfg).unwrap();
        let registers = [PhysReg::RAX, PhysReg::RDX, PhysReg::RCX];
        let mut expanded = expand_unallocated(&function, &cfg, &graph).unwrap();
        let joint = JointAllocationProblem::build(&expanded, &cfg, &graph, &registers).unwrap();
        let mut candidate = candidate(&joint, VReg(0));
        let points = [BlockId(1), BlockId(2)]
            .into_iter()
            .map(|block| {
                let row = cfg.block_index[&block];
                AllocationPressurePoint {
                    register: PhysReg::RAX,
                    block,
                    slot: expanded.intervals.block_slots[row]
                        .instruction_use(0)
                        .unwrap(),
                }
            })
            .collect::<Vec<_>>();
        candidate.frontiers = vec![RegisterPressureFrontier {
            register: PhysReg::RAX,
            points: points.clone(),
        }];

        let applied = apply_live_range_edit(
            &mut expanded,
            &joint,
            &candidate,
            &candidate.frontiers[0],
            &cfg,
        )
        .unwrap();
        assert_eq!(applied.root, candidate.root);
        assert_eq!(expanded.register_regions.len(), 4);
        assert_eq!(
            expanded
                .register_regions
                .iter()
                .filter(|region| matches!(region.entry, ExpandedRegisterEntry::SplitCopy { .. }))
                .count(),
            2
        );
        assert_eq!(
            expanded
                .register_regions
                .iter()
                .filter(|region| matches!(region.entry, ExpandedRegisterEntry::SplitPhi { .. }))
                .count(),
            1
        );

        let rebuilt = JointAllocationProblem::build(&expanded, &cfg, &graph, &registers).unwrap();
        let semantic_owner = root_for(&expanded, VReg(0)).uses[0].value;
        for metadata in &expanded.register_regions {
            let value = rebuilt.value(metadata.value).unwrap();
            let AllocationValueClass::Region { root, uses } = &value.class else {
                panic!("SplitEditor representative is not an allocatable region");
            };
            assert_eq!(*root, candidate.root);
            if metadata.value == semantic_owner {
                assert_eq!(uses.as_slice(), candidate.uses.as_slice());
            } else {
                assert!(uses.is_empty());
            }
            assert!(value.spill_cost.is_some());
        }
    }

    #[test]
    fn split_editor_transition_is_spilled_as_one_exact_machine_interval() {
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: VReg(0),
            value: 7,
        });
        entry.push(MInst::LoadImm {
            dst: VReg(1),
            value: 1,
        });
        entry.push(MInst::Branch {
            cond: VReg(1),
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });
        let mut left = MBlock::new(BlockId(1));
        left.push(MInst::Jump { target: BlockId(3) });
        let mut right = MBlock::new(BlockId(2));
        right.push(MInst::Jump { target: BlockId(3) });
        let mut merge = MBlock::new(BlockId(3));
        merge.push(MInst::Mov {
            dst: VReg(2),
            src: VReg(0),
        });
        merge.push(MInst::Return);
        let mut function = function(3, Vec::new());
        function.blocks = vec![entry, left, right, merge];
        let cfg = super::super::cfg::normalize(&mut function).unwrap();
        let graph = home_graph::build(&function, &cfg).unwrap();
        let registers = [PhysReg::RAX, PhysReg::RDX, PhysReg::RCX];
        let mut expanded = expand_unallocated(&function, &cfg, &graph).unwrap();
        let joint = JointAllocationProblem::build(&expanded, &cfg, &graph, &registers).unwrap();
        let mut candidate = candidate(&joint, VReg(0));
        let points = [BlockId(1), BlockId(2)]
            .into_iter()
            .map(|block| {
                let row = cfg.block_index[&block];
                AllocationPressurePoint {
                    register: PhysReg::RAX,
                    block,
                    slot: expanded.intervals.block_slots[row]
                        .instruction_use(0)
                        .unwrap(),
                }
            })
            .collect::<Vec<_>>();
        candidate.frontiers = vec![RegisterPressureFrontier {
            register: PhysReg::RAX,
            points,
        }];
        apply_live_range_edit(
            &mut expanded,
            &joint,
            &candidate,
            &candidate.frontiers[0],
            &cfg,
        )
        .unwrap();

        let source = expanded
            .register_regions
            .iter()
            .find_map(|region| {
                matches!(region.entry, ExpandedRegisterEntry::SplitCopy { .. })
                    .then_some(region.value)
            })
            .unwrap();
        let joint = JointAllocationProblem::build(&expanded, &cfg, &graph, &registers).unwrap();
        let value = joint.value(source).unwrap();
        let AllocationValueClass::Region { root, uses } = &value.class else {
            panic!("split-copy product is not a spillable machine interval");
        };
        assert!(uses.is_empty());
        let request = RegionSpillRequest {
            value: source,
            root: *root,
            uses: uses.clone(),
        };
        let context = SplitPlanningContext::build(&graph, &cfg).unwrap();
        apply_spill(
            &mut expanded,
            &graph,
            &joint,
            &request,
            &cfg,
            &context.spiller,
        )
        .unwrap();

        assert!(
            expanded
                .register_regions
                .iter()
                .all(|row| row.value != source)
        );
        assert!(expanded.stack_homes.iter().any(|home| {
            home.root == request.root
                && home.kind
                    == super::super::allocation_expand::ExpandedStackHomeKind::Machine {
                        value: source,
                    }
        }));

        // A strict-SSA merge is also an ordinary machine definition. Spilling
        // it assigns the synthetic phi a real stack destination and inserts a
        // reload for its exact downstream use.
        let merge = expanded
            .register_regions
            .iter()
            .find_map(|region| {
                matches!(region.entry, ExpandedRegisterEntry::SplitPhi { .. })
                    .then_some(region.value)
            })
            .unwrap();
        let interval = expanded.intervals.intervals[merge.0 as usize]
            .as_ref()
            .unwrap()
            .clone();
        expanded.ir.begin_instruction_transaction().unwrap();
        let SpillEdit {
            liveness_blocks,
            constraint_blocks,
        } = context
            .spiller
            .materialize_machine_interval(&mut expanded, request.root, &interval)
            .unwrap();
        let mut journal = SplitMutationJournal {
            liveness_blocks,
            constraint_blocks,
        };
        finish_split_mutations(&mut expanded, &cfg, &mut journal, &[(merge, request.root)])
            .unwrap();
        assert!(expanded.stack_homes.iter().any(|home| {
            home.root == request.root
                && home.kind
                    == super::super::allocation_expand::ExpandedStackHomeKind::Machine {
                        value: merge,
                    }
                && matches!(
                    home.definition,
                    super::super::allocation_expand::ExpandedStackDefinition::Phi {
                        destination,
                        ..
                    } if destination == merge
                )
        }));
        expanded.ir.verify_stack_homes(&cfg).unwrap();
        let rebuilt = JointAllocationProblem::build(&expanded, &cfg, &graph, &registers).unwrap();
        assert!(matches!(
            rebuilt.value(source).map(|value| &value.class),
            Some(AllocationValueClass::Fixed)
        ));
        assert!(!matches!(
            rebuilt.value(merge).map(|value| &value.class),
            Some(AllocationValueClass::Region { .. })
        ));
        let allocation = allocate_with_splitting(&mut expanded, &graph, &cfg, &registers).unwrap();
        let lowered = super::super::allocation_lower::lower(
            &function,
            &cfg,
            &graph,
            &expanded,
            &allocation,
            &registers,
        )
        .unwrap();
        lowered.function.verify_result().unwrap();
    }

    #[test]
    fn unallocated_ssa_pressure_drives_split_and_materialization_in_one_session() {
        let instructions = vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
            },
            MInst::Load {
                dst: VReg(1),
                base: BaseReg::SimState,
                offset: 8,
                size: OpSize::S64,
            },
            MInst::Load {
                dst: VReg(2),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S64,
            },
            MInst::Add {
                dst: VReg(3),
                lhs: VReg(0),
                rhs: VReg(1),
            },
            MInst::Add {
                dst: VReg(4),
                lhs: VReg(3),
                rhs: VReg(2),
            },
            MInst::Return,
        ];
        let registers = [PhysReg::RAX, PhysReg::RDX];
        let mut function = function(5, instructions);
        let cfg = super::super::cfg::normalize(&mut function).unwrap();
        let graph = home_graph::build(&function, &cfg).unwrap();
        let mut expanded = expand_unallocated(&function, &cfg, &graph).unwrap();
        let original_value_count = expanded.ir.value_count();
        assert!(expanded.stack_homes.is_empty());
        assert!(expanded.register_regions.is_empty());

        let joint = JointAllocationProblem::build(&expanded, &cfg, &graph, &registers).unwrap();
        let JointAllocationOutcome::NeedsSplit(request) = joint.allocate(&cfg, &registers).unwrap()
        else {
            panic!("unallocated root pressure should request a use-frontier split");
        };
        assert!(!request.conflicts.is_empty());
        let plan = plan_split(&expanded, &graph, &joint, &request, &cfg).unwrap();
        let candidate = request
            .candidates
            .iter()
            .find(|candidate| candidate.value == plan.value)
            .unwrap();
        let frontier = candidate
            .frontiers
            .iter()
            .find(|frontier| frontier.register == plan.register)
            .unwrap();
        assert_eq!(frontier.points, plan.cuts);
        assert!(!plan.moved.is_empty());

        let allocation = allocate_with_splitting(&mut expanded, &graph, &cfg, &registers).unwrap();
        JointAllocationProblem::build(&expanded, &cfg, &graph, &registers)
            .unwrap()
            .verify(&cfg, &registers, &allocation)
            .unwrap();
        assert!(expanded.ir.value_count() > original_value_count);
        assert!(
            expanded
                .roots
                .iter()
                .flat_map(|root| &root.uses)
                .any(|use_| {
                    matches!(use_.source, ExpandedUseSource::Materialized(_))
                        || matches!(use_.source, ExpandedUseSource::RegisterRegion { .. })
                })
        );
    }

    #[test]
    fn synthetic_pressure_is_materialized_and_reallocated_to_completion() {
        let instructions = vec![
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
            MInst::Mov {
                dst: VReg(3),
                src: VReg(0),
            },
            MInst::Mov {
                dst: VReg(4),
                src: VReg(1),
            },
            MInst::Mov {
                dst: VReg(5),
                src: VReg(2),
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
                src: VReg(1),
            },
            MInst::Mov {
                dst: VReg(8),
                src: VReg(2),
            },
            MInst::Mov {
                dst: VReg(9),
                src: VReg(1),
            },
            MInst::Mov {
                dst: VReg(10),
                src: VReg(2),
            },
            MInst::Mov {
                dst: VReg(11),
                src: VReg(0),
            },
            MInst::Return,
        ];
        let registers = [PhysReg::RAX, PhysReg::RDX];
        let mut function = function(12, instructions);
        let (cfg, graph, expanded) = model(&mut function, &registers);
        let joint = JointAllocationProblem::build(&expanded, &cfg, &graph, &registers).unwrap();
        let JointAllocationOutcome::NeedsSplit(request) = joint.allocate(&cfg, &registers).unwrap()
        else {
            panic!("explicit state materialization should expose root pressure");
        };
        let plan = plan_split(&expanded, &graph, &joint, &request, &cfg).unwrap();
        assert!(!plan.moved.is_empty());
        assert!(plan.entries.iter().any(|entry| {
            entry.kind == SplitEntryKind::RegisterRegion && entry.uses.len() >= 2
        }));

        let mut solved = expanded;
        let allocation = allocate_with_splitting(&mut solved, &graph, &cfg, &registers).unwrap();
        JointAllocationProblem::build(&solved, &cfg, &graph, &registers)
            .unwrap()
            .verify(&cfg, &registers, &allocation)
            .unwrap();
        assert!(solved.roots.iter().flat_map(|root| &root.uses).any(|use_| {
            matches!(
                use_.source,
                ExpandedUseSource::Materialized(_) | ExpandedUseSource::RegisterRegion { .. }
            )
        }));
    }

    #[test]
    fn exact_movable_cut_retains_the_noninterfering_prefix() {
        let instructions = vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 24,
                src: VReg(0),
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
            MInst::Store {
                base: BaseReg::SimState,
                offset: 32,
                src: VReg(1),
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 40,
                src: VReg(2),
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 48,
                src: VReg(1),
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 56,
                src: VReg(2),
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 64,
                src: VReg(1),
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 72,
                src: VReg(2),
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 80,
                src: VReg(1),
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 88,
                src: VReg(0),
                size: OpSize::S64,
            },
            MInst::Return,
        ];
        let registers = [PhysReg::RAX, PhysReg::RDX];
        let mut function = function(3, instructions);
        let cfg = super::super::cfg::normalize(&mut function).unwrap();
        let graph = home_graph::build(&function, &cfg).unwrap();
        let mut expanded = expand_unallocated(&function, &cfg, &graph).unwrap();
        let mut joint = JointAllocationProblem::build(&expanded, &cfg, &graph, &registers).unwrap();
        for value in &mut joint.values {
            match value.value {
                VReg(0) => value.spill_cost = Some(0),
                VReg(1) | VReg(2) => value.spill_cost = Some(1_000),
                _ => {}
            }
        }
        let JointAllocationOutcome::NeedsSplit(request) = joint.allocate(&cfg, &registers).unwrap()
        else {
            panic!("two long-lived residents should block the lower-priority root");
        };
        assert_eq!(request.blocked_value, VReg(0));
        let candidate = request
            .candidates
            .iter()
            .find(|candidate| candidate.value == VReg(0))
            .unwrap()
            .clone();
        let candidate_definition = joint.value(candidate.value).unwrap().interval.definition;
        assert!(
            candidate
                .frontiers
                .iter()
                .flat_map(|frontier| &frontier.points)
                .all(|point| point.block() != candidate_definition.block()
                    || point.slot() != candidate_definition.slot())
        );
        let restricted = RegionSplitRequest {
            blocked_value: request.blocked_value,
            definition: request.definition,
            conflicts: request.conflicts,
            candidates: vec![candidate],
        };
        let plan = plan_split(&expanded, &graph, &joint, &restricted, &cfg).unwrap();
        assert!(!plan.retained.is_empty());
        assert!(!plan.moved.is_empty());
        assert_eq!(plan.retained.len() + plan.moved.len(), 2);

        apply_split(&mut expanded, &graph, &joint, &plan, &cfg).unwrap();
        let root = expanded_root(&expanded, plan.root).unwrap();
        for &use_id in &plan.retained {
            let use_ = &root.uses[use_id.0 as usize];
            assert_eq!(use_.value, plan.value);
            assert_eq!(
                use_.source,
                ExpandedUseSource::OriginalRegister {
                    preferred_register: Some(plan.register),
                }
            );
        }
        let rebuilt = JointAllocationProblem::build(&expanded, &cfg, &graph, &registers).unwrap();
        assert_eq!(
            rebuilt.value(plan.value).unwrap().preferred_register,
            Some(plan.register)
        );
    }

    #[test]
    fn equal_cost_frontiers_keep_the_largest_register_prefix() {
        let instructions = vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 7,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 8,
                src: VReg(0),
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 16,
                src: VReg(0),
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 24,
                src: VReg(0),
                size: OpSize::S64,
            },
            MInst::Return,
        ];
        let registers = [PhysReg::RAX, PhysReg::RDX];
        let mut function = function(1, instructions);
        let cfg = super::super::cfg::normalize(&mut function).unwrap();
        let graph = home_graph::build(&function, &cfg).unwrap();
        let expanded = expand_unallocated(&function, &cfg, &graph).unwrap();
        let joint = JointAllocationProblem::build(&expanded, &cfg, &graph, &registers).unwrap();
        let mut candidate = candidate(&joint, VReg(0));
        let slots = &expanded.intervals.block_slots[0];
        let earlier = AllocationPressurePoint {
            register: PhysReg::RDX,
            block: BlockId(0),
            slot: slots.instruction_use(2).unwrap(),
        };
        let later = AllocationPressurePoint {
            register: PhysReg::RAX,
            block: BlockId(0),
            slot: slots.instruction_use(3).unwrap(),
        };
        candidate.frontiers = vec![
            RegisterPressureFrontier {
                register: earlier.register(),
                points: vec![earlier],
            },
            RegisterPressureFrontier {
                register: later.register(),
                points: vec![later],
            },
        ];
        candidate
            .frontiers
            .sort_unstable_by_key(|frontier| frontier.register);
        let request = RegionSplitRequest {
            blocked_value: VReg(0),
            definition: joint.value(VReg(0)).unwrap().interval.definition,
            conflicts: Vec::new(),
            candidates: vec![candidate],
        };

        let plan = plan_split(&expanded, &graph, &joint, &request, &cfg).unwrap();
        assert_eq!(plan.cuts, vec![later]);
        assert_eq!(plan.retained.len(), 2);
        assert_eq!(plan.moved.len(), 1);
    }

    #[test]
    fn multi_cut_children_reenter_ordinary_allocation_without_hard_colors() {
        let mut values = VRegAllocator::new();
        for _ in 0..6 {
            values.alloc();
        }
        let mut function = MFunction::new(values, vec![SpillDesc::transient(); 6]);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: VReg(0),
            value: 7,
        });
        entry.push(MInst::LoadImm {
            dst: VReg(1),
            value: 1,
        });
        entry.push(MInst::Branch {
            cond: VReg(1),
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });
        let mut left = MBlock::new(BlockId(1));
        left.push(MInst::Mov {
            dst: VReg(2),
            src: VReg(0),
        });
        left.push(MInst::Mov {
            dst: VReg(3),
            src: VReg(0),
        });
        left.push(MInst::Jump { target: BlockId(3) });
        let mut right = MBlock::new(BlockId(2));
        right.push(MInst::Mov {
            dst: VReg(4),
            src: VReg(0),
        });
        right.push(MInst::Mov {
            dst: VReg(5),
            src: VReg(0),
        });
        right.push(MInst::Jump { target: BlockId(3) });
        let mut merge = MBlock::new(BlockId(3));
        merge.push(MInst::Return);
        function.blocks = vec![entry, left, right, merge];

        let registers = [PhysReg::RAX, PhysReg::RDX, PhysReg::RCX, PhysReg::RBX];
        let (cfg, graph, mut expanded) = model(&mut function, &registers);
        let joint = JointAllocationProblem::build(&expanded, &cfg, &graph, &registers).unwrap();
        let mut candidate = candidate(&joint, VReg(0));
        let root = root_for(&expanded, VReg(0));
        let mut arm_blocks = BTreeSet::new();
        for use_id in &candidate.uses {
            let site = root.uses[use_id.0 as usize].site;
            arm_blocks.insert(site.block());
        }
        let mut points = arm_blocks
            .into_iter()
            .map(|block| AllocationPressurePoint {
                register: PhysReg::RAX,
                block,
                slot: expanded.intervals.block_slots[cfg.block_index[&block]].entry,
            })
            .collect::<Vec<_>>();
        points.sort_unstable();
        candidate.frontiers = vec![RegisterPressureFrontier {
            register: PhysReg::RAX,
            points: points.clone(),
        }];
        let request = RegionSplitRequest {
            blocked_value: VReg(2),
            definition: joint.value(VReg(2)).unwrap().interval.definition,
            conflicts: Vec::new(),
            candidates: vec![candidate.clone()],
        };

        let plan = plan_split(&expanded, &graph, &joint, &request, &cfg).unwrap();
        assert_eq!(plan.register, PhysReg::RAX);
        assert_eq!(plan.cuts, points);
        assert_eq!(plan.moved, candidate.uses);
        assert!(plan.retained.is_empty());
        assert_eq!(plan.entries.len(), 2);
        assert!(plan.entries.iter().all(|entry| {
            entry.kind == SplitEntryKind::RegisterRegion && entry.uses.len() == 2
        }));
        assert!(
            plan.entries
                .iter()
                .all(|entry| entry.preferred_register == Some(PhysReg::RAX))
        );

        let mut session =
            GreedyAllocator::new_with_incremental_state(joint, &expanded, &cfg, &graph, &registers)
                .unwrap();
        session.release_for_split(plan.value).unwrap();
        let update =
            apply_split(&mut expanded, &graph, session.live_intervals(), &plan, &cfg).unwrap();
        session
            .update_after_test_edit(
                &expanded,
                &cfg,
                &graph,
                &registers,
                &update.constraint_blocks,
                &update.changed_values,
                &update.range_changed_values,
                &update.live_lengths,
                update.root,
            )
            .unwrap();
        assert!(
            root_for(&expanded, VReg(0))
                .uses
                .iter()
                .all(|use_| use_.value != VReg(0))
        );
        for entry in &plan.entries {
            let region = expanded
                .register_regions
                .iter()
                .find(|region| region.root == plan.root && region.entry_use == Some(entry.entry))
                .unwrap();
            assert_eq!(region.preferred_register, Some(PhysReg::RAX));
            assert_eq!(session.assigned_register(region.value), None);
        }

        let JointAllocationOutcome::Complete(allocation) =
            session.allocate(&cfg, &registers).unwrap()
        else {
            panic!("published child regions should complete ordinary allocation");
        };
        session
            .live_intervals()
            .verify(&cfg, &registers, &allocation)
            .unwrap();
    }

    #[test]
    fn split_rewrites_only_the_reachable_cfg_arm() {
        let mut values = VRegAllocator::new();
        for _ in 0..8 {
            values.alloc();
        }
        let mut function = MFunction::new(values, vec![SpillDesc::transient(); 8]);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: VReg(0),
            value: 7,
        });
        entry.push(MInst::LoadImm {
            dst: VReg(1),
            value: 1,
        });
        entry.push(MInst::Branch {
            cond: VReg(1),
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });
        let mut left = MBlock::new(BlockId(1));
        left.push(MInst::Mov {
            dst: VReg(2),
            src: VReg(0),
        });
        left.push(MInst::LoadImm {
            dst: VReg(3),
            value: 19,
        });
        left.push(MInst::Mov {
            dst: VReg(4),
            src: VReg(0),
        });
        left.push(MInst::Mov {
            dst: VReg(5),
            src: VReg(0),
        });
        left.push(MInst::Jump { target: BlockId(3) });
        let mut right = MBlock::new(BlockId(2));
        right.push(MInst::Mov {
            dst: VReg(6),
            src: VReg(0),
        });
        right.push(MInst::Mov {
            dst: VReg(7),
            src: VReg(0),
        });
        right.push(MInst::Jump { target: BlockId(3) });
        let mut merge = MBlock::new(BlockId(3));
        merge.push(MInst::Return);
        function.blocks = vec![entry, left, right, merge];
        let registers = [PhysReg::RAX, PhysReg::RDX, PhysReg::RCX, PhysReg::RBX];
        let (cfg, graph, mut expanded) = model(&mut function, &registers);
        let joint = JointAllocationProblem::build(&expanded, &cfg, &graph, &registers).unwrap();
        let candidate = candidate(&joint, VReg(0));
        let request = request(&joint, VReg(3), candidate.clone());
        let plan = plan_split(&expanded, &graph, &joint, &request, &cfg).unwrap();
        let root = root_for(&expanded, VReg(0));
        assert_eq!(
            plan.moved
                .iter()
                .map(|use_id| root.uses[use_id.0 as usize].site.block())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([BlockId(1)])
        );
        let right_uses = root
            .uses
            .iter()
            .filter(|use_| use_.site.block() == BlockId(2))
            .map(|use_| (use_.id, use_.value))
            .collect::<Vec<_>>();

        apply_split(&mut expanded, &graph, &joint, &plan, &cfg).unwrap();
        let root = root_for(&expanded, VReg(0));
        for (use_id, value) in right_uses {
            let use_ = &root.uses[use_id.0 as usize];
            assert_eq!(use_.value, value);
            assert_eq!(
                use_.source,
                ExpandedUseSource::OriginalRegister {
                    preferred_register: Some(plan.register),
                }
            );
        }
        assert!(plan.entries.iter().any(|entry| {
            entry.kind == SplitEntryKind::RegisterRegion && entry.uses.len() == 2
        }));
    }

    #[test]
    fn cross_block_region_updates_liveness_and_ownership_from_one_journal() {
        let mut values = VRegAllocator::new();
        for _ in 0..8 {
            values.alloc();
        }
        let mut function = MFunction::new(values, vec![SpillDesc::transient(); 8]);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: VReg(0),
            value: 7,
        });
        entry.push(MInst::LoadImm {
            dst: VReg(1),
            value: 1,
        });
        entry.push(MInst::Branch {
            cond: VReg(1),
            true_bb: BlockId(1),
            false_bb: BlockId(3),
        });
        let mut left = MBlock::new(BlockId(1));
        left.push(MInst::Mov {
            dst: VReg(2),
            src: VReg(0),
        });
        left.push(MInst::LoadImm {
            dst: VReg(3),
            value: 19,
        });
        left.push(MInst::Mov {
            dst: VReg(4),
            src: VReg(0),
        });
        left.push(MInst::Jump { target: BlockId(2) });
        let mut left_tail = MBlock::new(BlockId(2));
        left_tail.push(MInst::Mov {
            dst: VReg(5),
            src: VReg(0),
        });
        left_tail.push(MInst::Jump { target: BlockId(4) });
        let mut right = MBlock::new(BlockId(3));
        right.push(MInst::Mov {
            dst: VReg(6),
            src: VReg(0),
        });
        right.push(MInst::Mov {
            dst: VReg(7),
            src: VReg(0),
        });
        right.push(MInst::Jump { target: BlockId(4) });
        let mut merge = MBlock::new(BlockId(4));
        merge.push(MInst::Return);
        function.blocks = vec![entry, left, left_tail, right, merge];

        let registers = [PhysReg::RAX, PhysReg::RDX, PhysReg::RCX, PhysReg::RBX];
        let (cfg, graph, mut expanded) = model(&mut function, &registers);
        let joint = JointAllocationProblem::build(&expanded, &cfg, &graph, &registers).unwrap();
        let request = request(&joint, VReg(3), candidate(&joint, VReg(0)));
        let plan = plan_split(&expanded, &graph, &joint, &request, &cfg).unwrap();
        assert!(!plan.retained.is_empty());
        assert!(plan.entries.iter().any(|entry| {
            entry.kind == SplitEntryKind::RegisterRegion
                && entry
                    .uses
                    .iter()
                    .map(|use_id| {
                        root_for(&expanded, VReg(0)).uses[use_id.0 as usize]
                            .site
                            .block()
                    })
                    .collect::<BTreeSet<_>>()
                    == BTreeSet::from([BlockId(1), BlockId(2)])
        }));

        let mut session =
            GreedyAllocator::new_with_incremental_state(joint, &expanded, &cfg, &graph, &registers)
                .unwrap();
        let update =
            apply_split(&mut expanded, &graph, session.live_intervals(), &plan, &cfg).unwrap();
        assert!(update.constraint_blocks.contains(&BlockId(1)));
        assert!(update.constraint_blocks.contains(&BlockId(2)));
        session
            .update_after_test_edit(
                &expanded,
                &cfg,
                &graph,
                &registers,
                &update.constraint_blocks,
                &update.changed_values,
                &update.range_changed_values,
                &update.live_lengths,
                update.root,
            )
            .unwrap();
        let rebuilt = JointAllocationProblem::build(&expanded, &cfg, &graph, &registers).unwrap();
        assert_eq!(session.live_intervals(), &rebuilt);
        assert_eq!(session.assigned_register(plan.value), None);
        let JointAllocationOutcome::Complete(allocation) =
            session.allocate(&cfg, &registers).unwrap()
        else {
            panic!("updated split children should reenter ordinary allocation");
        };
        session
            .live_intervals()
            .verify(&cfg, &registers, &allocation)
            .unwrap();
    }

    #[test]
    fn loop_reentry_materializes_a_pre_cut_use_instead_of_extending_a_region_across_pressure() {
        let mut values = VRegAllocator::new();
        for _ in 0..5 {
            values.alloc();
        }
        let mut function = MFunction::new(values, vec![SpillDesc::transient(); 5]);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: VReg(0),
            value: 7,
        });
        entry.push(MInst::LoadImm {
            dst: VReg(1),
            value: 1,
        });
        entry.push(MInst::Jump { target: BlockId(1) });
        let mut loop_block = MBlock::new(BlockId(1));
        loop_block.push(MInst::Mov {
            dst: VReg(2),
            src: VReg(0),
        });
        loop_block.push(MInst::LoadImm {
            dst: VReg(3),
            value: 19,
        });
        loop_block.push(MInst::Mov {
            dst: VReg(4),
            src: VReg(0),
        });
        loop_block.push(MInst::Branch {
            cond: VReg(1),
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });
        let mut exit = MBlock::new(BlockId(2));
        exit.push(MInst::Return);
        function.blocks = vec![entry, loop_block, exit];
        let registers = [PhysReg::RAX, PhysReg::RDX, PhysReg::RCX, PhysReg::RBX];
        let (cfg, graph, mut expanded) = model(&mut function, &registers);
        let joint = JointAllocationProblem::build(&expanded, &cfg, &graph, &registers).unwrap();
        let candidate = candidate(&joint, VReg(0));
        let request = request(&joint, VReg(3), candidate.clone());
        let plan = plan_split(&expanded, &graph, &joint, &request, &cfg).unwrap();
        assert_eq!(plan.moved, candidate.uses);
        assert!(
            plan.entries
                .iter()
                .all(|entry| entry.kind == SplitEntryKind::Materialized)
        );
        apply_split(&mut expanded, &graph, &joint, &plan, &cfg).unwrap();
        assert!(
            root_for(&expanded, VReg(0))
                .uses
                .iter()
                .all(|use_| matches!(use_.source, ExpandedUseSource::Materialized(_)))
        );
    }

    #[test]
    fn complete_phi_stack_split_defines_the_home_without_a_fixed_store_range() {
        let mut values = VRegAllocator::new();
        for _ in 0..3 {
            values.alloc();
        }
        let mut function = MFunction::new(values, vec![SpillDesc::transient(); 3]);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: VReg(0),
            value: 7,
        });
        entry.push(MInst::Jump { target: BlockId(1) });
        let mut body = MBlock::new(BlockId(1));
        body.phis.push(PhiNode {
            dst: VReg(1),
            sources: vec![(BlockId(0), VReg(0))],
        });
        body.push(MInst::Mov {
            dst: VReg(2),
            src: VReg(1),
        });
        body.push(MInst::Return);
        function.blocks = vec![entry, body];

        let registers = [PhysReg::RAX, PhysReg::RDX];
        let cfg = super::super::cfg::normalize(&mut function).unwrap();
        let graph = home_graph::build(&function, &cfg).unwrap();
        let mut expanded = expand_unallocated(&function, &cfg, &graph).unwrap();
        let joint = JointAllocationProblem::build(&expanded, &cfg, &graph, &registers).unwrap();
        let candidate = candidate(&joint, VReg(1));
        let request = request(&joint, VReg(1), candidate);
        let plan = plan_split(&expanded, &graph, &joint, &request, &cfg).unwrap();
        assert!(plan.retained.is_empty());
        assert!(
            plan.spill
                .selections()
                .values()
                .all(|selection| selection.kind == super::super::home_graph::HomeKind::Stack)
        );

        apply_split(&mut expanded, &graph, &joint, &plan, &cfg).unwrap();
        let home = expanded
            .stack_homes
            .iter()
            .find(|home| home.root == plan.root)
            .unwrap();
        assert!(matches!(
            home.definition,
            super::super::allocation_expand::ExpandedStackDefinition::Phi {
                destination: VReg(1),
                ..
            }
        ));
        assert!(expanded.intervals.intervals[VReg(1).0 as usize].is_none());
        assert!(
            expanded
                .ir
                .stack_facts()
                .unwrap()
                .operations
                .iter()
                .all(|operation| operation.kind
                    != super::super::allocation_ir::AllocationStackOperationKind::Store)
        );
    }

    #[test]
    fn partial_stack_split_keeps_the_store_as_a_fixed_use_and_the_prefix_as_a_region() {
        let instructions = vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 2,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 3,
            },
            MInst::Add {
                dst: VReg(2),
                lhs: VReg(0),
                rhs: VReg(1),
            },
            MInst::Mov {
                dst: VReg(3),
                src: VReg(2),
            },
            MInst::LoadImm {
                dst: VReg(4),
                value: 19,
            },
            MInst::Mov {
                dst: VReg(5),
                src: VReg(2),
            },
            MInst::Mov {
                dst: VReg(6),
                src: VReg(2),
            },
            MInst::Return,
        ];
        let registers = [
            PhysReg::RAX,
            PhysReg::RDX,
            PhysReg::RCX,
            PhysReg::RBX,
            PhysReg::RSI,
        ];
        let mut function = function(7, instructions);
        let (cfg, graph, mut expanded) = model(&mut function, &registers);
        let joint = JointAllocationProblem::build(&expanded, &cfg, &graph, &registers).unwrap();
        let candidate = candidate(&joint, VReg(2));
        let request = request(&joint, VReg(4), candidate);
        let plan = plan_split(&expanded, &graph, &joint, &request, &cfg).unwrap();
        assert_eq!(plan.retained.len(), 1);
        assert_eq!(plan.moved.len(), 2);
        assert!(
            plan.spill
                .selections()
                .values()
                .all(|selection| selection.kind == super::super::home_graph::HomeKind::Stack)
        );

        let mut session = GreedyAllocator::new_with_incremental_state(
            joint.clone(),
            &expanded,
            &cfg,
            &graph,
            &registers,
        )
        .unwrap();
        let update =
            apply_split(&mut expanded, &graph, session.live_intervals(), &plan, &cfg).unwrap();
        session
            .update_after_test_edit(
                &expanded,
                &cfg,
                &graph,
                &registers,
                &update.constraint_blocks,
                &update.changed_values,
                &update.range_changed_values,
                &update.live_lengths,
                update.root,
            )
            .unwrap();
        let root = root_for(&expanded, VReg(2));
        assert!(matches!(
            root.uses[plan.retained[0].0 as usize].source,
            ExpandedUseSource::OriginalRegister { .. }
        ));
        assert!(
            expanded
                .stack_homes
                .iter()
                .any(|home| home.root == plan.root)
        );
        let rebuilt = JointAllocationProblem::build(&expanded, &cfg, &graph, &registers).unwrap();
        assert_eq!(session.live_intervals(), &rebuilt);
        assert!(matches!(
            rebuilt.value(VReg(2)).unwrap().class,
            AllocationValueClass::Region { .. }
        ));
    }

    #[test]
    fn an_existing_region_is_not_recreated_at_the_same_entry_boundary() {
        let instructions = vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 7,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 19,
            },
            MInst::Mov {
                dst: VReg(2),
                src: VReg(0),
            },
            MInst::Mov {
                dst: VReg(3),
                src: VReg(0),
            },
            MInst::Mov {
                dst: VReg(4),
                src: VReg(0),
            },
            MInst::Return,
        ];
        let registers = [PhysReg::RAX, PhysReg::RDX, PhysReg::RCX];
        let mut function = function(5, instructions);
        let (cfg, graph, mut expanded) = model(&mut function, &registers);
        let joint = JointAllocationProblem::build(&expanded, &cfg, &graph, &registers).unwrap();
        let first_candidate = candidate(&joint, VReg(0));
        let first_request = request(&joint, VReg(1), first_candidate);
        let first_plan = plan_split(&expanded, &graph, &joint, &first_request, &cfg).unwrap();
        assert_eq!(first_plan.entries.len(), 1);
        assert_eq!(first_plan.entries[0].kind, SplitEntryKind::RegisterRegion);
        apply_split(&mut expanded, &graph, &joint, &first_plan, &cfg).unwrap();

        let metadata = expanded
            .register_regions
            .iter()
            .find(|region| region.root == first_plan.root)
            .unwrap()
            .clone();
        let joint = JointAllocationProblem::build(&expanded, &cfg, &graph, &registers).unwrap();
        let second_candidate = candidate(&joint, metadata.value);
        let second_request = request(&joint, metadata.value, second_candidate.clone());
        let second_plan = plan_split(&expanded, &graph, &joint, &second_request, &cfg).unwrap();
        assert_eq!(second_plan.moved, second_candidate.uses);
        assert!(second_plan.entries.iter().all(|entry| {
            entry.kind != SplitEntryKind::RegisterRegion
                || entry.uses != second_candidate.uses
                || Some(entry.entry) != metadata.entry_use
        }));
        assert!(second_plan.entries.iter().any(|entry| {
            Some(entry.entry) == metadata.entry_use && entry.kind == SplitEntryKind::Materialized
        }));
        let values_before_replacement = expanded.ir.value_count();
        apply_split(&mut expanded, &graph, &joint, &second_plan, &cfg).unwrap();
        assert_eq!(
            expanded.ir.value_count(),
            values_before_replacement + 2,
            "replacement recipes receive fresh stable session identities"
        );
        assert!(
            expanded.intervals.intervals[metadata.value.0 as usize].is_none(),
            "the replaced entry recipe must be dead without renumbering later values"
        );
        assert_eq!(
            expanded.intervals.intervals[values_before_replacement as usize..]
                .iter()
                .filter(|interval| interval.is_some())
                .count(),
            2,
            "both replacement recipes must remain live"
        );
    }

    #[test]
    fn spill_stage_materializes_every_use_and_removes_the_register_interval() {
        let instructions = vec![
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
            MInst::Mov {
                dst: VReg(2),
                src: VReg(0),
            },
            MInst::Return,
        ];
        let registers = [PhysReg::RAX, PhysReg::RDX];
        let mut function = function(3, instructions);
        let (cfg, graph, mut expanded) = model(&mut function, &registers);
        let joint = JointAllocationProblem::build(&expanded, &cfg, &graph, &registers).unwrap();
        let value = joint.value(VReg(0)).unwrap();
        let AllocationValueClass::Region { root, uses } = &value.class else {
            panic!("original SSA value should start as one register interval");
        };
        let request = RegionSpillRequest {
            value: VReg(0),
            root: *root,
            uses: uses.clone(),
        };
        let context = SplitPlanningContext::build(&graph, &cfg).unwrap();
        apply_spill(
            &mut expanded,
            &graph,
            &joint,
            &request,
            &cfg,
            &context.spiller,
        )
        .unwrap();

        let root = root_for(&expanded, VReg(0));
        assert!(root.uses.iter().all(|use_| {
            use_.value != VReg(0)
                && matches!(
                    use_.source,
                    ExpandedUseSource::Materialized(_) | ExpandedUseSource::Edge(_)
                )
        }));
        let rebuilt = JointAllocationProblem::build(&expanded, &cfg, &graph, &registers).unwrap();
        assert!(!matches!(
            rebuilt.value(VReg(0)).map(|value| &value.class),
            Some(AllocationValueClass::Region { .. })
        ));
        assert!(rebuilt.values.iter().all(|value| {
            matches!(value.class, AllocationValueClass::Fixed)
                || !matches!(value.class, AllocationValueClass::Region { root: value_root, .. } if value_root == request.root)
        }));
    }
}
