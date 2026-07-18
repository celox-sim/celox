//! Exact pressure-driven splitting for the expanded allocation problem.
//!
//! A coloring failure is resolved at the definition which is simultaneously
//! covered by every earlier interfering SSA range.  Only root uses reachable
//! from that point through the candidate's exact live-range graph are moved.
//! The moved uses are partitioned into dominance-connected regions, each
//! entered by one proved home materialization; isolated and loop-carried entry
//! uses are materialized independently.  The resulting machine values are fed
//! back into joint allocation instead of being assigned scratch registers.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

use crate::backend::native::mir::{BlockId, Uses, VReg};

use super::allocation_expand::{
    self, ExpandedAllocationProblem, ExpandedRegisterRegion, ExpandedStackHome, ExpandedUseSource,
    RegisterRegionId,
};
use super::allocation_ir::{StackHomeId, SyntheticOperation};
use super::allocation_reallocate::{
    AllocationValue, AllocationValueClass, JointAllocation, JointAllocationError,
    JointAllocationOutcome, JointAllocationProblem, JointAllocationSession, RegionSplitCandidate,
    RegionSplitRequest,
};
use super::assignment::PhysReg;
use super::cfg::NormalizedCfg;
use super::home_graph::{
    BundleUseId, HomeGraph, HomeKind, LiveBundle, LiveBundleId, STACK_HOME_CREATION_COST,
    STACK_HOME_MATERIALIZATION_COST,
};
use super::interval_allocator::{HomeSelection, IntervalAllocationError, RootHomePlan};
use super::live_interval::{DefinitionSite, LiveSegment, UseSite};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SplitEntryKind {
    Materialized,
    RegisterRegion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SplitEntry {
    pub entry: BundleUseId,
    pub uses: Vec<BundleUseId>,
    pub kind: SplitEntryKind,
    pub home: HomeSelection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RegionSplitPlan {
    pub blocked_value: VReg,
    pub cut: DefinitionSite,
    pub value: VReg,
    pub root: LiveBundleId,
    pub source_region: Option<RegisterRegionId>,
    pub preferred_register: PhysReg,
    pub retained: Vec<BundleUseId>,
    pub moved: Vec<BundleUseId>,
    pub entries: Vec<SplitEntry>,
    pub transition_cost: u64,
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

    fn home(error: IntervalAllocationError, root: LiveBundleId) -> Self {
        Self::new(error.rule, error.block, None, Some(root), error.message)
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

    fn use_dominates_definition(
        &self,
        cfg: &NormalizedCfg,
        use_: UseSite,
        definition: DefinitionSite,
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
        let Some(&definition_block) = cfg.block_index.get(&definition.block()) else {
            return false;
        };
        if use_block == definition_block {
            use_slot <= definition.slot()
        } else {
            self.block_dominates(use_block, definition_block)
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct RegionSource {
    preferred_register: PhysReg,
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
}

/// Select one exact resident region whose removal at `request.definition` has
/// minimum proved transition cost.  Physical assignments are deliberately not
/// considered final here; the returned values re-enter joint allocation.
pub(super) fn plan_split(
    expanded: &ExpandedAllocationProblem,
    graph: &HomeGraph,
    joint: &JointAllocationProblem,
    request: &RegionSplitRequest,
    cfg: &NormalizedCfg,
) -> Result<RegionSplitPlan, AllocationSplitError> {
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

    let dominance = Dominance::build(cfg)?;
    let mut best = None::<RegionSplitPlan>;
    for candidate in &request.candidates {
        let value = verify_candidate(joint, candidate, request.definition)?;
        let root = expanded_root(expanded, candidate.root)?;
        let source = region_source(expanded, root, candidate, value)?;
        let moved = match reachable_uses(expanded, root, candidate, value, request.definition, cfg)
        {
            Ok(moved) => moved,
            Err(error) if error.rule == "ALLOCATION_SPLIT.NO_REACHABLE_USE" => continue,
            Err(error) => return Err(error),
        };
        let clusters = partition_moved_uses(
            root,
            candidate,
            &moved,
            source.entry_use,
            request.definition,
            cfg,
            &dominance,
        )?;
        let entry_uses = clusters
            .iter()
            .map(|cluster| cluster.entry)
            .collect::<Vec<_>>();
        let stack_exists = stack_home(expanded, candidate.root)?.is_some();
        let home_plan = RootHomePlan::build(graph, graph_root(graph, candidate.root)?)
            .map_err(|error| AllocationSplitError::home(error, candidate.root))?;
        let (mut selections, transition_cost) =
            entry_selections(&home_plan, &entry_uses, stack_exists, candidate.root)?;
        let entries = clusters
            .into_iter()
            .map(|cluster| {
                let home = selections.remove(&cluster.entry).ok_or_else(|| {
                    AllocationSplitError::new(
                        "ALLOCATION_SPLIT.HOME_COVERAGE",
                        root.uses
                            .get(cluster.entry.0 as usize)
                            .map(|use_| use_.site.block()),
                        Some(candidate.value),
                        Some(candidate.root),
                        "home partition did not select a transition for a region entry",
                    )
                })?;
                Ok(SplitEntry {
                    entry: cluster.entry,
                    uses: cluster.uses,
                    kind: cluster.kind,
                    home,
                })
            })
            .collect::<Result<Vec<_>, AllocationSplitError>>()?;
        if !selections.is_empty() {
            return Err(AllocationSplitError::new(
                "ALLOCATION_SPLIT.HOME_COVERAGE",
                Some(request.definition.block()),
                Some(candidate.value),
                Some(candidate.root),
                "home partition selected an entry not owned by the split plan",
            ));
        }
        let moved_set = moved.iter().copied().collect::<BTreeSet<_>>();
        let retained = candidate
            .uses
            .iter()
            .copied()
            .filter(|use_id| !moved_set.contains(use_id))
            .collect::<Vec<_>>();
        let plan = RegionSplitPlan {
            blocked_value: request.blocked_value,
            cut: request.definition,
            value: candidate.value,
            root: candidate.root,
            source_region: source.region,
            preferred_register: source.preferred_register,
            retained,
            moved,
            entries,
            transition_cost,
        };
        verify_plan(expanded, graph, joint, candidate, &plan, cfg, &dominance)?;
        let key = (plan.transition_cost, plan.value, plan.root);
        if best
            .as_ref()
            .is_none_or(|current| key < (current.transition_cost, current.value, current.root))
        {
            best = Some(plan);
        }
    }
    best.ok_or_else(|| {
        AllocationSplitError::new(
            "ALLOCATION_SPLIT.NO_PROGRESS",
            Some(request.definition.block()),
            Some(request.blocked_value),
            None,
            "no requested root region can be shortened at the pressure point",
        )
    })
}

/// Apply a verified split inside the private allocation session. Every
/// fallible planning and dominance check runs before mutation; an invariant
/// failure after mutation aborts and discards the session without publishing
/// MIR. Independent whole-program proofs run once at atomic lowering.
fn apply_split(
    expanded: &mut ExpandedAllocationProblem,
    graph: &HomeGraph,
    joint: &JointAllocationProblem,
    plan: &RegionSplitPlan,
    cfg: &NormalizedCfg,
) -> Result<AppliedSplit, AllocationSplitError> {
    let candidate = candidate_from_plan(joint, plan)?;
    let dominance = Dominance::build(cfg)?;
    verify_plan(expanded, graph, joint, &candidate, plan, cfg, &dominance)?;
    let before = root_split_progress(expanded_root(expanded, plan.root)?);
    let graph_root = graph_root(graph, plan.root)?;
    let mut changed_blocks = BTreeSet::new();
    let mut constraint_blocks = BTreeSet::new();

    let needs_stack = plan
        .entries
        .iter()
        .any(|entry| entry.home.kind == HomeKind::Stack);
    let existing_stack_home = stack_home(expanded, plan.root)?;
    let stack_home = if needs_stack {
        if existing_stack_home.is_none() {
            changed_blocks.insert(graph_root.definition.block());
            constraint_blocks.insert(graph_root.definition.block());
        }
        Some(ensure_stack_home(expanded, graph_root)?)
    } else {
        existing_stack_home
    };

    for entry in &plan.entries {
        let entry_use = expanded_use(expanded, plan.root, entry.entry)?.clone();
        changed_blocks.insert(entry_use.original_site.block());
        constraint_blocks.insert(entry_use.original_site.block());
        if let UseSite::PhiEdge { successor, .. } = entry_use.original_site {
            constraint_blocks.insert(successor);
        }
        let lowered = allocation_expand::lower_use_materialization(
            &mut expanded.ir,
            graph,
            graph_root,
            plan.value,
            entry.entry,
            entry_use.original_site,
            &entry.home,
            stack_home,
            &mut expanded.stack_homes,
        )
        .map_err(AllocationSplitError::expand)?;
        match entry.kind {
            SplitEntryKind::Materialized => {
                if entry.uses.as_slice() != [entry.entry] {
                    return Err(AllocationSplitError::new(
                        "ALLOCATION_SPLIT.SINGLETON_SHAPE",
                        Some(entry_use.site.block()),
                        Some(plan.value),
                        Some(plan.root),
                        "materialized split entry owns more than its exact entry use",
                    ));
                }
                match lowered {
                    allocation_expand::LoweredUseMaterialization::Register(lowered) => {
                        rewrite_expanded_use(
                            expanded,
                            plan.root,
                            entry.entry,
                            plan.value,
                            lowered.value,
                            ExpandedUseSource::Materialized(lowered.source),
                        )?;
                    }
                    allocation_expand::LoweredUseMaterialization::Edge(location) => {
                        let target = expanded
                            .roots
                            .get_mut(plan.root.0 as usize)
                            .and_then(|root| root.uses.get_mut(entry.entry.0 as usize))
                            .ok_or_else(|| {
                                AllocationSplitError::new(
                                    "ALLOCATION_SPLIT.USE_RANGE",
                                    Some(entry_use.site.block()),
                                    Some(plan.value),
                                    Some(plan.root),
                                    "phi-edge home references a missing expanded use",
                                )
                            })?;
                        if target.value != plan.value {
                            return Err(AllocationSplitError::new(
                                "ALLOCATION_SPLIT.USE_OWNERSHIP",
                                Some(target.site.block()),
                                Some(target.value),
                                Some(plan.root),
                                "phi-edge home no longer belongs to the split register region",
                            ));
                        }
                        target.value = graph_root.origin;
                        target.source = ExpandedUseSource::Edge(location);
                    }
                }
            }
            SplitEntryKind::RegisterRegion => {
                let allocation_expand::LoweredUseMaterialization::Register(lowered) = lowered
                else {
                    return Err(AllocationSplitError::new(
                        "ALLOCATION_SPLIT.REGION_EDGE_ENTRY",
                        Some(entry_use.site.block()),
                        Some(plan.value),
                        Some(plan.root),
                        "multi-use register region cannot start from a non-register phi-edge location",
                    ));
                };
                let region = fresh_region_id(expanded)?;
                let region_row = expanded.register_regions.len();
                if expanded.region_rows.insert(region, region_row).is_some() {
                    return Err(AllocationSplitError::new(
                        "ALLOCATION_SPLIT.REGION_IDENTITY",
                        Some(entry_use.site.block()),
                        Some(lowered.value),
                        Some(plan.root),
                        "new register region duplicates an existing stable identity",
                    ));
                }
                expanded.register_regions.push(ExpandedRegisterRegion {
                    id: region,
                    root: plan.root,
                    value: lowered.value,
                    preferred_register: plan.preferred_register,
                    entry_use: entry.entry,
                    entry: lowered.source,
                });
                for &use_id in &entry.uses {
                    rewrite_expanded_use(
                        expanded,
                        plan.root,
                        use_id,
                        plan.value,
                        lowered.value,
                        ExpandedUseSource::RegisterRegion {
                            region,
                            preferred_register: plan.preferred_register,
                        },
                    )?;
                }
            }
        }
    }

    prune_replaced_register_region(expanded, plan.root, plan.value, plan.source_region)?;
    let mut changed_values = allocation_expand::refresh(expanded, cfg, &changed_blocks)
        .map_err(AllocationSplitError::expand)?
        .into_iter()
        .collect::<BTreeSet<_>>();
    let pruned_blocks = expanded
        .ir
        .prune_dead_materializations_from(&expanded.intervals, [plan.value])
        .map_err(|error| {
            AllocationSplitError::new(
                error.rule,
                error.block,
                error.values.first().copied(),
                Some(plan.root),
                error.message,
            )
        })?;
    if !pruned_blocks.is_empty() {
        changed_blocks.extend(pruned_blocks.iter().copied());
        constraint_blocks.extend(pruned_blocks.iter().copied());
        changed_values.extend(
            allocation_expand::refresh(expanded, cfg, &pruned_blocks)
                .map_err(AllocationSplitError::expand)?,
        );
    }
    let after = root_split_progress(expanded_root(expanded, plan.root)?);
    if after >= before {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.NON_MONOTONIC",
            Some(plan.cut.block()),
            Some(plan.value),
            Some(plan.root),
            format!("split progress {before:?} did not decrease: {after:?}"),
        ));
    }
    Ok(AppliedSplit {
        root: plan.root,
        constraint_blocks,
        changed_values: changed_values.into_iter().collect(),
    })
}

/// Iterate exact splitting and joint coloring to a fixed point.  The bound is
/// derived from use ownership: regions are never merged, and every accepted
/// split reduces pairwise co-residency, original residency, or register uses.
pub(super) fn allocate_with_splitting(
    expanded: &mut ExpandedAllocationProblem,
    graph: &HomeGraph,
    cfg: &NormalizedCfg,
    registers: &[PhysReg],
) -> Result<JointAllocation, AllocationSplitError> {
    let initial_register_uses = split_progress(expanded).register_uses;
    let root_count = u128::try_from(expanded.roots.len()).map_err(|_| {
        AllocationSplitError::new(
            "ALLOCATION_SPLIT.ITERATION_BOUND",
            None,
            None,
            None,
            "root count exceeds u128",
        )
    })?;
    let max_steps = initial_register_uses
        .checked_mul(3)
        .and_then(|steps| steps.checked_add(root_count))
        .and_then(|steps| steps.checked_add(1))
        .ok_or_else(|| {
            AllocationSplitError::new(
                "ALLOCATION_SPLIT.ITERATION_BOUND",
                None,
                None,
                None,
                "split iteration bound exceeds u128",
            )
        })?;
    let joint = JointAllocationProblem::build(expanded, cfg, graph, registers)
        .map_err(AllocationSplitError::joint)?;
    let mut session =
        JointAllocationSession::new_persistent(joint, expanded, cfg, graph, registers)
            .map_err(AllocationSplitError::joint)?;
    let mut steps = 0u128;
    loop {
        match session
            .allocate(cfg, registers)
            .map_err(AllocationSplitError::joint)?
        {
            JointAllocationOutcome::Complete(allocation) => return Ok(allocation),
            JointAllocationOutcome::NeedsSplit(request) => {
                if steps >= max_steps {
                    return Err(AllocationSplitError::new(
                        "ALLOCATION_SPLIT.ITERATION_BOUND",
                        Some(request.definition.block()),
                        Some(request.blocked_value),
                        None,
                        "monotonic split sequence exceeded its ownership-derived bound",
                    ));
                }
                let plan = plan_split(expanded, graph, session.problem(), &request, cfg)?;
                let update = apply_split(expanded, graph, session.problem(), &plan, cfg)?;
                session
                    .update_from_expanded(
                        expanded,
                        cfg,
                        graph,
                        registers,
                        &update.constraint_blocks,
                        &update.changed_values,
                        update.root,
                    )
                    .map_err(AllocationSplitError::joint)?;
                steps += 1;
            }
        }
    }
}

fn verify_candidate<'a>(
    joint: &'a JointAllocationProblem,
    candidate: &RegionSplitCandidate,
    cut: DefinitionSite,
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
    if *root != candidate.root || *uses != candidate.uses || candidate.uses.is_empty() {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.CANDIDATE_IDENTITY",
            Some(value.interval.definition.block()),
            Some(candidate.value),
            Some(candidate.root),
            "split request and joint region ownership disagree",
        ));
    }
    if !value.interval.covers(cut.block(), cut.slot()) {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.PRESSURE_POINT",
            Some(cut.block()),
            Some(candidate.value),
            Some(candidate.root),
            "candidate live range does not cover the blocked definition",
        ));
    }
    Ok(value)
}

fn candidate_from_plan(
    joint: &JointAllocationProblem,
    plan: &RegionSplitPlan,
) -> Result<RegionSplitCandidate, AllocationSplitError> {
    let value = joint.value(plan.value).ok_or_else(|| {
        AllocationSplitError::new(
            "ALLOCATION_SPLIT.CANDIDATE_RANGE",
            Some(plan.cut.block()),
            Some(plan.value),
            Some(plan.root),
            "split plan references a value outside joint allocation",
        )
    })?;
    let AllocationValueClass::Region { root, uses } = &value.class else {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.CANDIDATE_CLASS",
            Some(plan.cut.block()),
            Some(plan.value),
            Some(plan.root),
            "split plan no longer references a register region",
        ));
    };
    if *root != plan.root {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.CANDIDATE_IDENTITY",
            Some(plan.cut.block()),
            Some(plan.value),
            Some(plan.root),
            "split plan root differs from current joint allocation",
        ));
    }
    Ok(RegionSplitCandidate {
        value: plan.value,
        root: plan.root,
        uses: uses.clone(),
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
    let preferred_register = value.preferred_register.ok_or_else(|| {
        AllocationSplitError::new(
            "ALLOCATION_SPLIT.REGION_PREFERENCE",
            Some(value.interval.definition.block()),
            Some(candidate.value),
            Some(candidate.root),
            "register region has no previous-register affinity",
        )
    })?;
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
            || !candidate.uses.contains(&metadata.entry_use)
        {
            return Err(AllocationSplitError::new(
                "ALLOCATION_SPLIT.REGION_METADATA",
                Some(value.interval.definition.block()),
                Some(candidate.value),
                Some(candidate.root),
                "expanded-region metadata disagrees with candidate ownership",
            ));
        }
        Some(metadata.entry_use)
    } else {
        None
    };
    Ok(RegionSource {
        preferred_register,
        region: source_region,
        entry_use,
    })
}

fn reachable_uses(
    expanded: &ExpandedAllocationProblem,
    root: &super::allocation_expand::ExpandedRoot,
    candidate: &RegionSplitCandidate,
    value: &AllocationValue,
    cut: DefinitionSite,
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
            "blocked definition is outside the normalized CFG",
        )
    })?;
    if !segments[start].is_some_and(|segment| segment.contains(cut.slot())) {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.PRESSURE_POINT",
            Some(cut.block()),
            Some(candidate.value),
            Some(candidate.root),
            "candidate segment does not contain the blocked definition slot",
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
        if use_.value != candidate.value || !value.interval.uses.contains(&use_.site) {
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
    cut: DefinitionSite,
    cfg: &NormalizedCfg,
    dominance: &Dominance,
) -> Result<Vec<EntryCluster>, AllocationSplitError> {
    let mut ordered = moved.to_vec();
    ordered.sort_unstable_by_key(|use_id| {
        let site = root.uses[use_id.0 as usize].site;
        let block = cfg.block_index[&site.block()];
        (dominance.enter[block], site.slot(), *use_id)
    });
    let mut remaining = moved.iter().copied().collect::<BTreeSet<_>>();
    let full_existing_region = previous_entry.is_some() && moved == candidate.uses;
    let mut clusters = Vec::new();
    for seed in ordered {
        if !remaining.contains(&seed) {
            continue;
        }
        let seed_site = root.uses[seed.0 as usize].site;
        let unsafe_loop_entry = dominance.use_dominates_definition(cfg, seed_site, cut);
        let repeats_same_boundary = full_existing_region && previous_entry == Some(seed);
        let mut uses = if matches!(seed_site, UseSite::PhiEdge { .. })
            || unsafe_loop_entry
            || repeats_same_boundary
        {
            vec![seed]
        } else {
            remaining
                .iter()
                .copied()
                .filter(|use_id| {
                    dominance.use_dominates_use(cfg, seed_site, root.uses[use_id.0 as usize].site)
                })
                .collect::<Vec<_>>()
        };
        uses.sort_unstable();
        if uses.is_empty() || !uses.contains(&seed) {
            return Err(AllocationSplitError::new(
                "ALLOCATION_SPLIT.CLUSTER_ENTRY",
                Some(seed_site.block()),
                Some(candidate.value),
                Some(candidate.root),
                "dominance cluster does not contain its own entry use",
            ));
        }
        for use_id in &uses {
            if !remaining.remove(use_id) {
                return Err(AllocationSplitError::new(
                    "ALLOCATION_SPLIT.CLUSTER_OWNERSHIP",
                    Some(root.uses[use_id.0 as usize].site.block()),
                    Some(candidate.value),
                    Some(candidate.root),
                    "two split clusters claim the same moved use",
                ));
            }
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
    }
    if !remaining.is_empty() {
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

fn entry_selections(
    home_plan: &RootHomePlan,
    entries: &[BundleUseId],
    stack_exists: bool,
    root: LiveBundleId,
) -> Result<(BTreeMap<BundleUseId, HomeSelection>, u64), AllocationSplitError> {
    let mut ordered = entries.to_vec();
    ordered.sort_unstable();
    if ordered.is_empty() || ordered.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.ENTRY_ORDER",
            None,
            None,
            Some(root),
            "split entries are empty or contain duplicate use identities",
        ));
    }
    let partition = home_plan
        .partition_with_existing_stack(&ordered, stack_exists)
        .map_err(|error| AllocationSplitError::home(error, root))?;
    let mut selections = BTreeMap::new();
    let mut stack_creation_pending = !stack_exists;
    let mut materialized_total = 0u64;
    for piece in partition.pieces {
        for use_id in piece.uses {
            let selection = match piece.selection.kind {
                HomeKind::Stack => {
                    let creation_cost = if stack_creation_pending {
                        stack_creation_pending = false;
                        STACK_HOME_CREATION_COST
                    } else {
                        0
                    };
                    HomeSelection {
                        kind: HomeKind::Stack,
                        materializations: Vec::new(),
                        creation_cost,
                        materialization_cost: STACK_HOME_MATERIALIZATION_COST,
                    }
                }
                HomeKind::Rematerialize(_) | HomeKind::State(_) => {
                    let materialization = piece
                        .selection
                        .materializations
                        .iter()
                        .find(|materialization| materialization.use_id == use_id)
                        .copied()
                        .ok_or_else(|| {
                            AllocationSplitError::new(
                                "ALLOCATION_SPLIT.HOME_COVERAGE",
                                None,
                                None,
                                Some(root),
                                format!("non-stack home has no exact recipe for entry {use_id:?}"),
                            )
                        })?;
                    HomeSelection {
                        kind: piece.selection.kind,
                        materializations: vec![materialization],
                        creation_cost: 0,
                        materialization_cost: materialization.cost,
                    }
                }
                HomeKind::Register => {
                    return Err(AllocationSplitError::new(
                        "ALLOCATION_SPLIT.HOME_CLASS",
                        None,
                        None,
                        Some(root),
                        "home partition returned allocator-owned register residency",
                    ));
                }
            };
            materialized_total = materialized_total
                .checked_add(selection.total_cost())
                .ok_or_else(|| {
                    AllocationSplitError::new(
                        "ALLOCATION_SPLIT.HOME_COST_OVERFLOW",
                        None,
                        None,
                        Some(root),
                        "entry home cost exceeds u64",
                    )
                })?;
            if selections.insert(use_id, selection).is_some() {
                return Err(AllocationSplitError::new(
                    "ALLOCATION_SPLIT.HOME_COVERAGE",
                    None,
                    None,
                    Some(root),
                    "home partition selected the same entry more than once",
                ));
            }
        }
    }
    if selections.len() != ordered.len() || materialized_total != partition.total_cost {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.HOME_COST_IDENTITY",
            None,
            None,
            Some(root),
            format!(
                "materialized entries cost {materialized_total}, indexed partition cost {}",
                partition.total_cost
            ),
        ));
    }
    Ok((selections, partition.total_cost))
}

fn verify_plan(
    expanded: &ExpandedAllocationProblem,
    graph: &HomeGraph,
    joint: &JointAllocationProblem,
    candidate: &RegionSplitCandidate,
    plan: &RegionSplitPlan,
    cfg: &NormalizedCfg,
    dominance: &Dominance,
) -> Result<(), AllocationSplitError> {
    if plan.value != candidate.value || plan.root != candidate.root {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.PLAN_IDENTITY",
            Some(plan.cut.block()),
            Some(plan.value),
            Some(plan.root),
            "split plan and candidate identities differ",
        ));
    }
    let value = verify_candidate(joint, candidate, plan.cut)?;
    let root = expanded_root(expanded, plan.root)?;
    let source = region_source(expanded, root, candidate, value)?;
    if source.preferred_register != plan.preferred_register || source.region != plan.source_region {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.PLAN_PREFERENCE",
            Some(plan.cut.block()),
            Some(plan.value),
            Some(plan.root),
            "split plan changed the candidate's register affinity",
        ));
    }
    let expected_moved = reachable_uses(expanded, root, candidate, value, plan.cut, cfg)?;
    if expected_moved != plan.moved {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.MOVED_SET",
            Some(plan.cut.block()),
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
                Some(plan.cut.block()),
                Some(plan.value),
                Some(plan.root),
                "split entry does not own its entry use",
            ));
        }
        match entry.kind {
            SplitEntryKind::Materialized if entry.uses.as_slice() != [entry.entry] => {
                return Err(AllocationSplitError::new(
                    "ALLOCATION_SPLIT.SINGLETON_SHAPE",
                    Some(plan.cut.block()),
                    Some(plan.value),
                    Some(plan.root),
                    "materialized entry is not an exact singleton",
                ));
            }
            SplitEntryKind::RegisterRegion => {
                if entry.uses.len() < 2 {
                    return Err(AllocationSplitError::new(
                        "ALLOCATION_SPLIT.REGION_SHAPE",
                        Some(plan.cut.block()),
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
            }
            SplitEntryKind::Materialized => {}
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
            Some(plan.cut.block()),
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
            Some(plan.cut.block()),
            Some(plan.value),
            Some(plan.root),
            "retained and moved uses do not partition the candidate region",
        ));
    }

    let entry_ids = plan
        .entries
        .iter()
        .map(|entry| entry.entry)
        .collect::<Vec<_>>();
    let stack_exists = stack_home(expanded, plan.root)?.is_some();
    let home_plan = RootHomePlan::build(graph, graph_root(graph, plan.root)?)
        .map_err(|error| AllocationSplitError::home(error, plan.root))?;
    let (expected_homes, expected_cost) =
        entry_selections(&home_plan, &entry_ids, stack_exists, plan.root)?;
    if plan.transition_cost != expected_cost
        || plan.entries.iter().any(|entry| {
            expected_homes.get(&entry.entry) != Some(&entry.home)
                || entry.home.kind == HomeKind::Register
        })
    {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.HOME_IDENTITY",
            Some(plan.cut.block()),
            Some(plan.value),
            Some(plan.root),
            "split transitions differ from the exact HomeGraph partition",
        ));
    }
    Ok(())
}

fn stack_home(
    expanded: &ExpandedAllocationProblem,
    root: LiveBundleId,
) -> Result<Option<StackHomeId>, AllocationSplitError> {
    let homes = expanded
        .stack_homes
        .iter()
        .filter(|home| {
            home.root == root && home.kind == super::allocation_expand::ExpandedStackHomeKind::Root
        })
        .collect::<Vec<_>>();
    match homes.as_slice() {
        [] => Ok(None),
        [home] => Ok(Some(home.id)),
        _ => Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.STACK_HOME_IDENTITY",
            None,
            None,
            Some(root),
            "one root owns more than one explicit stack home",
        )),
    }
}

fn ensure_stack_home(
    expanded: &mut ExpandedAllocationProblem,
    root: &LiveBundle,
) -> Result<StackHomeId, AllocationSplitError> {
    if let Some(home) = stack_home(expanded, root.id)? {
        return Ok(home);
    }
    if expanded
        .stack_homes
        .iter()
        .enumerate()
        .any(|(index, home)| home.id.0 as usize != index)
    {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.STACK_HOME_IDENTITY",
            Some(root.definition.block()),
            Some(root.origin),
            Some(root.id),
            "expanded stack homes are not densely identified",
        ));
    }
    let id = StackHomeId(u32::try_from(expanded.stack_homes.len()).map_err(|_| {
        AllocationSplitError::new(
            "ALLOCATION_SPLIT.STACK_HOME_ID_RANGE",
            Some(root.definition.block()),
            Some(root.origin),
            Some(root.id),
            "expanded stack-home count exceeds u32",
        )
    })?);
    let store = expanded
        .ir
        .insert_after_definition(
            root.definition,
            SyntheticOperation::StackStore { home: id },
            Uses::one(root.origin),
            false,
        )
        .map_err(|error| {
            AllocationSplitError::new(
                error.rule,
                error.block,
                error.values.first().copied(),
                Some(root.id),
                error.message,
            )
        })?
        .instruction;
    expanded.stack_homes.push(ExpandedStackHome {
        id,
        root: root.id,
        definition: super::allocation_expand::ExpandedStackDefinition::Store {
            instruction: store,
            value: root.origin,
        },
        kind: super::allocation_expand::ExpandedStackHomeKind::Root,
    });
    Ok(id)
}

fn fresh_region_id(
    expanded: &mut ExpandedAllocationProblem,
) -> Result<RegisterRegionId, AllocationSplitError> {
    let id = RegisterRegionId(expanded.next_register_region);
    expanded.next_register_region =
        expanded
            .next_register_region
            .checked_add(1)
            .ok_or_else(|| {
                AllocationSplitError::new(
                    "ALLOCATION_SPLIT.REGION_ID_RANGE",
                    None,
                    None,
                    None,
                    "expanded register-region identity exceeds u32",
                )
            })?;
    Ok(id)
}

fn rewrite_expanded_use(
    expanded: &mut ExpandedAllocationProblem,
    root: LiveBundleId,
    use_id: BundleUseId,
    original: VReg,
    replacement: VReg,
    source: ExpandedUseSource,
) -> Result<(), AllocationSplitError> {
    let root_index = root.0 as usize;
    let use_index = use_id.0 as usize;
    let use_ = expanded
        .roots
        .get(root_index)
        .and_then(|root| root.uses.get(use_index))
        .ok_or_else(|| {
            AllocationSplitError::new(
                "ALLOCATION_SPLIT.USE_RANGE",
                None,
                Some(original),
                Some(root),
                format!("rewritten use {use_id:?} is outside its expanded root"),
            )
        })?
        .clone();
    if use_.value != original {
        return Err(AllocationSplitError::new(
            "ALLOCATION_SPLIT.USE_OWNERSHIP",
            Some(use_.site.block()),
            Some(original),
            Some(root),
            "rewritten use no longer belongs to the selected region",
        ));
    }
    expanded
        .ir
        .rewrite_use(use_.original_site, original, replacement)
        .map_err(|error| {
            AllocationSplitError::new(
                error.rule,
                error.block,
                error.values.first().copied(),
                Some(root),
                error.message,
            )
        })?;
    let target = &mut expanded.roots[root_index].uses[use_index];
    target.value = replacement;
    target.source = source;
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
        BaseReg, MBlock, MFunction, MInst, OpSize, SpillDesc, VRegAllocator,
    };

    use super::super::allocation_expand::expand;
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
        }
    }

    fn request(
        joint: &JointAllocationProblem,
        blocked: VReg,
        candidate: RegionSplitCandidate,
    ) -> RegionSplitRequest {
        RegionSplitRequest {
            blocked_value: blocked,
            definition: joint.value(blocked).unwrap().interval.definition,
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
    fn synthetic_pressure_is_split_into_machine_regions_and_reallocated_to_completion() {
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
        assert!(!solved.register_regions.is_empty());
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
            .map(|use_| (use_.id, use_.value, use_.source.clone()))
            .collect::<Vec<_>>();

        apply_split(&mut expanded, &graph, &joint, &plan, &cfg).unwrap();
        let root = root_for(&expanded, VReg(0));
        for (use_id, value, source) in right_uses {
            let use_ = &root.uses[use_id.0 as usize];
            assert_eq!((use_.value, &use_.source), (value, &source));
        }
        assert!(plan.entries.iter().any(|entry| {
            entry.kind == SplitEntryKind::RegisterRegion && entry.uses.len() == 2
        }));
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
            plan.entries
                .iter()
                .all(|entry| entry.home.kind == HomeKind::Stack)
        );

        let mut session = JointAllocationSession::new_persistent(
            joint.clone(),
            &expanded,
            &cfg,
            &graph,
            &registers,
        )
        .unwrap();
        let update = apply_split(&mut expanded, &graph, session.problem(), &plan, &cfg).unwrap();
        session
            .update_from_expanded(
                &expanded,
                &cfg,
                &graph,
                &registers,
                &update.constraint_blocks,
                &update.changed_values,
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
        assert_eq!(session.problem(), &rebuilt);
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
                || entry.entry != metadata.entry_use
        }));
        assert!(second_plan.entries.iter().any(|entry| {
            entry.entry == metadata.entry_use && entry.kind == SplitEntryKind::Materialized
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
}
