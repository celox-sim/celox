//! Joint physical allocation model for expanded original and synthetic values.
//!
//! Home expansion invalidates every physical assignment made before synthetic
//! stores, reloads, and recipe nodes existed. This module rebuilds one sparse
//! allocation problem from the expanded IR. Existing register numbers are
//! affinities only. A failed coloring returns exact resident conflicts and the
//! root regions which may be split; it never invents a scratch register or
//! silently finalizes a value to memory.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::backend::native::mir::{BlockId, VReg};

use super::allocation_constraints::{
    AllocationConstraintError, AllocationConstraintModel, IncrementalConstraintModel, RegisterMask,
    WeightedAffinity,
};
use super::allocation_expand::{
    ExpandedAllocationProblem, ExpandedEdgeLocation, ExpandedStackDefinition,
    ExpandedStackHomeKind, ExpandedUseSource,
};
use super::assignment::PhysReg;
use super::cfg::NormalizedCfg;
use super::home_graph::{BundleUseId, HomeGraph, LiveBundleId};
use super::interval_allocator::{IntervalAllocationError, RootHomePlan};
use super::interval_union::{
    AllocationBundleId, ConflictCollector, FixedRegisterReservation, IntervalUnionError,
    LiveIntervalMatrix, OccupancyCut, OccupancyOwner, SparseRange,
};
use super::live_interval::{DefinitionSite, LiveInterval, SlotIndex, UseSite};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum AllocationValueClass {
    /// A definition-to-store, reload-to-use, recipe intermediate, or other
    /// machine range which is already at the explicit transition boundary.
    Fixed,
    /// A retained root range which may be replaced by exact homes at a strict
    /// subset of its original uses.
    Region {
        root: LiveBundleId,
        uses: Vec<BundleUseId>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AllocationValue {
    pub id: AllocationBundleId,
    pub value: VReg,
    pub interval: LiveInterval,
    pub class: AllocationValueClass,
    /// Exact cost of displacing every owned root use to a proved home. Fixed
    /// synthetic transitions are unspillable and therefore carry `None`.
    pub spill_cost: Option<u64>,
    /// Exact half-open range length in current emitted instruction order.
    /// This is deliberately independent of the stable slot label namespace.
    pub live_length: u64,
    pub preferred_register: Option<PhysReg>,
    pub allowed_registers: RegisterMask,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct JointAllocationProblem {
    value_count: u32,
    pub values: Vec<AllocationValue>,
    /// Dense active-row lookup indexed by the stable allocation-session VReg.
    /// Physical interval-union bundle IDs are the VReg identity itself; they
    /// never shift when another synthetic value becomes dead.
    value_rows: Vec<Option<usize>>,
    definition_order: BTreeSet<DefinitionOrderEntry>,
    target_registers: Vec<PhysReg>,
    affinities: Vec<WeightedAffinity>,
    /// Immutable value-to-affinity CSR. Coloring and recoloring query only
    /// incident edges; scanning the whole edge list for every candidate color
    /// makes allocation quadratic on large RTL dataflow graphs.
    affinity_index: AffinityIndex,
    fixed_reservations: Vec<FixedRegisterReservation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AffinityNeighbor {
    value: VReg,
    weight: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AffinityIndex {
    offsets: Vec<usize>,
    neighbors: Vec<AffinityNeighbor>,
}

impl AffinityIndex {
    fn build(
        value_count: u32,
        affinities: &[WeightedAffinity],
    ) -> Result<Self, JointAllocationError> {
        let value_count = value_count as usize;
        let mut offsets = vec![0usize; value_count + 1];
        for affinity in affinities {
            let left = affinity.left.0 as usize;
            let right = affinity.right.0 as usize;
            if left >= value_count || right >= value_count || left == right {
                return Err(JointAllocationError::new(
                    "JOINT_ALLOC.AFFINITY_INDEX",
                    None,
                    Some(affinity.left),
                    "affinity edge has an out-of-range or self-referential endpoint",
                ));
            }
            offsets[left + 1] = offsets[left + 1].checked_add(1).ok_or_else(|| {
                JointAllocationError::new(
                    "JOINT_ALLOC.AFFINITY_INDEX",
                    None,
                    Some(affinity.left),
                    "left affinity degree exceeds usize",
                )
            })?;
            offsets[right + 1] = offsets[right + 1].checked_add(1).ok_or_else(|| {
                JointAllocationError::new(
                    "JOINT_ALLOC.AFFINITY_INDEX",
                    None,
                    Some(affinity.right),
                    "right affinity degree exceeds usize",
                )
            })?;
        }
        for value in 0..value_count {
            offsets[value + 1] =
                offsets[value + 1]
                    .checked_add(offsets[value])
                    .ok_or_else(|| {
                        JointAllocationError::new(
                            "JOINT_ALLOC.AFFINITY_INDEX",
                            None,
                            None,
                            "affinity adjacency size exceeds usize",
                        )
                    })?;
        }

        let mut cursors = offsets[..value_count].to_vec();
        let mut neighbors = vec![
            AffinityNeighbor {
                value: VReg(0),
                weight: 0,
            };
            offsets[value_count]
        ];
        for affinity in affinities {
            let left = affinity.left.0 as usize;
            let right = affinity.right.0 as usize;
            neighbors[cursors[left]] = AffinityNeighbor {
                value: affinity.right,
                weight: affinity.weight,
            };
            cursors[left] += 1;
            neighbors[cursors[right]] = AffinityNeighbor {
                value: affinity.left,
                weight: affinity.weight,
            };
            cursors[right] += 1;
        }
        Ok(Self { offsets, neighbors })
    }

    fn neighbors(&self, value: VReg) -> &[AffinityNeighbor] {
        let value = value.0 as usize;
        let Some((&start, &end)) = self.offsets.get(value).zip(self.offsets.get(value + 1)) else {
            return &[];
        };
        &self.neighbors[start..end]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct DefinitionOrderEntry {
    key: (usize, SlotIndex, VReg),
    id: AllocationBundleId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RegisterConflicts {
    pub register: PhysReg,
    pub values: Vec<VReg>,
    pub cuts: Vec<OccupancyCut>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct AllocationPressurePoint {
    pub block: BlockId,
    pub slot: SlotIndex,
}

impl AllocationPressurePoint {
    pub(super) fn block(self) -> BlockId {
        self.block
    }

    pub(super) fn slot(self) -> SlotIndex {
        self.slot
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RegionSplitCandidate {
    pub value: VReg,
    pub root: LiveBundleId,
    pub uses: Vec<BundleUseId>,
    pub pressure_points: Vec<AllocationPressurePoint>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RegionSplitRequest {
    pub blocked_value: VReg,
    pub definition: DefinitionSite,
    pub conflicts: Vec<RegisterConflicts>,
    pub candidates: Vec<RegionSplitCandidate>,
    /// For pure movable pressure, split the lowest-priority blocked region at
    /// its definition into earliest dominating-use fragments in one
    /// transaction. Fixed reservations never set this frontier.
    pub preferred_frontier: Option<(VReg, AllocationPressurePoint)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct JointAllocation {
    pub assignments: Vec<Option<PhysReg>>,
}

/// Persistent physical allocation state across exact region splits.
/// Unchanged session VRegs retain both their sparse-range token and matrix
/// membership; only dead, rewritten, or newly materialized values re-enter
/// the allocation queue.
#[derive(Debug)]
pub(super) struct JointAllocationSession {
    problem: JointAllocationProblem,
    constraints: Option<IncrementalConstraintModel>,
    ownership: Option<RegionOwnershipIndex>,
    definition_rank: Vec<usize>,
    matrix: LiveIntervalMatrix,
    ranges: Vec<Option<SparseRange>>,
    assignments: Vec<Option<PhysReg>>,
    /// Static spill-priority order for the current semantic problem. Priority
    /// keys do not change while coloring, so sorting once and popping from the
    /// end avoids an O(log N) heap repair for every RTL value.
    pending: Vec<AllocationQueueItem>,
    home_plans: Option<Vec<RootHomePlan>>,
    deferred: BTreeSet<VReg>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AllocationQueueItem {
    id: AllocationBundleId,
    spill_cost: Option<u64>,
    live_length: u64,
    use_count: usize,
}

impl Ord for AllocationQueueItem {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.spill_cost, other.spill_cost) {
            // Explicit transitions cannot be displaced. Coloring them first
            // ensures pressure is paid by a splittable root region.
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (None, None) => self
                .use_count
                .cmp(&other.use_count)
                .then_with(|| other.live_length.cmp(&self.live_length))
                .then_with(|| other.id.cmp(&self.id)),
            (Some(left_cost), Some(right_cost)) => {
                let left = u128::from(left_cost) * u128::from(other.live_length);
                let right = u128::from(right_cost) * u128::from(self.live_length);
                left.cmp(&right)
                    .then_with(|| left_cost.cmp(&right_cost))
                    .then_with(|| self.use_count.cmp(&other.use_count))
                    .then_with(|| other.id.cmp(&self.id))
            }
        }
    }
}

impl PartialOrd for AllocationQueueItem {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum JointAllocationOutcome {
    Complete(JointAllocation),
    NeedsSplit(RegionSplitRequest),
    /// Every non-deferred value has a color. The caller must atomically
    /// materialize the accumulated symbolic spill plans and start the next
    /// allocation round before publication.
    DeferredRound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct JointAllocationError {
    pub rule: &'static str,
    pub block: Option<BlockId>,
    pub value: Option<VReg>,
    pub message: String,
}

impl JointAllocationError {
    fn new(
        rule: &'static str,
        block: Option<BlockId>,
        value: Option<VReg>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            rule,
            block,
            value,
            message: message.into(),
        }
    }

    fn union(error: IntervalUnionError) -> Self {
        Self::new(error.rule, error.block, None, error.message)
    }

    fn ir(error: super::allocation_ir::AllocationIrError) -> Self {
        Self::new(
            error.rule,
            error.block,
            error.values.first().copied(),
            error.message,
        )
    }

    fn constraints(error: AllocationConstraintError) -> Self {
        Self::new(
            error.rule,
            error.block,
            error.values.first().copied(),
            error.message,
        )
    }

    fn home(error: IntervalAllocationError) -> Self {
        Self::new(error.rule, error.block, None, error.message)
    }
}

fn build_home_plans(graph: &HomeGraph) -> Result<Vec<RootHomePlan>, JointAllocationError> {
    graph
        .bundles
        .iter()
        .enumerate()
        .map(|(row, root)| {
            if root.id.0 as usize != row {
                return Err(JointAllocationError::new(
                    "JOINT_ALLOC.HOME_ROOT_IDENTITY",
                    Some(root.definition.block()),
                    Some(root.origin),
                    "HomeGraph root differs from its physical-allocation cost row",
                ));
            }
            RootHomePlan::build(graph, root).map_err(JointAllocationError::home)
        })
        .collect()
}

impl fmt::Display for JointAllocationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.rule)?;
        if let Some(block) = self.block {
            write!(formatter, " at {block}")?;
        }
        if let Some(value) = self.value {
            write!(formatter, " value={value}")?;
        }
        write!(formatter, ": {}", self.message)
    }
}

impl std::error::Error for JointAllocationError {}

#[derive(Debug)]
struct RegionBuilder {
    root: LiveBundleId,
    uses: Vec<BundleUseId>,
    sites: Vec<UseSite>,
    preferred_register: Option<PhysReg>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct IndexedRegion {
    root: LiveBundleId,
    uses: Vec<BundleUseId>,
    preferred_register: Option<PhysReg>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RegionOwnershipIndex {
    owners: Vec<Option<IndexedRegion>>,
    values_by_root: Vec<Vec<VReg>>,
}

impl JointAllocationProblem {
    /// Build at an external verification boundary. This deliberately performs
    /// an independent liveness and target-constraint reconstruction.
    pub(super) fn build(
        expanded: &ExpandedAllocationProblem,
        cfg: &NormalizedCfg,
        graph: &HomeGraph,
        registers: &[PhysReg],
    ) -> Result<Self, JointAllocationError> {
        Self::build_internal(expanded, cfg, graph, registers, true)
    }

    pub(super) fn build_session_with_constraints(
        expanded: &ExpandedAllocationProblem,
        cfg: &NormalizedCfg,
        registers: &[PhysReg],
        constraints: &AllocationConstraintModel,
        home_plans: &[RootHomePlan],
    ) -> Result<Self, JointAllocationError> {
        Self::build_from_constraints(expanded, cfg, registers, constraints, home_plans)
    }

    fn build_internal(
        expanded: &ExpandedAllocationProblem,
        cfg: &NormalizedCfg,
        graph: &HomeGraph,
        registers: &[PhysReg],
        verify_independently: bool,
    ) -> Result<Self, JointAllocationError> {
        let constraints = if verify_independently {
            AllocationConstraintModel::build_verified(expanded, cfg, graph, registers)
        } else {
            AllocationConstraintModel::build(expanded, cfg, graph, registers)
        }
        .map_err(JointAllocationError::constraints)?;
        if verify_independently {
            let recomputed = expanded.ir.analyze(cfg).map_err(|error| {
                JointAllocationError::new(
                    error.rule,
                    error.block,
                    error.values.first().copied(),
                    error.message,
                )
            })?;
            if recomputed != expanded.intervals
                || expanded.ir.value_count() as usize != expanded.intervals.intervals.len()
            {
                return Err(JointAllocationError::new(
                    "JOINT_ALLOC.STALE_INTERVALS",
                    None,
                    None,
                    "expanded intervals do not exactly describe the current allocation IR",
                ));
            }
        }

        let home_plans = build_home_plans(graph)?;
        Self::build_from_constraints(expanded, cfg, registers, &constraints, &home_plans)
    }

    fn build_from_constraints(
        expanded: &ExpandedAllocationProblem,
        cfg: &NormalizedCfg,
        registers: &[PhysReg],
        constraints: &AllocationConstraintModel,
        home_plans: &[RootHomePlan],
    ) -> Result<Self, JointAllocationError> {
        if home_plans.len() != expanded.roots.len() {
            return Err(JointAllocationError::new(
                "JOINT_ALLOC.HOME_COST_SHAPE",
                None,
                None,
                "physical-allocation home-cost rows do not cover every expanded root",
            ));
        }
        let synthetic_instruction_index = expanded
            .ir
            .index_synthetic_instructions()
            .map_err(JointAllocationError::ir)?;
        let mut fixed_region_uses = BTreeMap::<VReg, Vec<UseSite>>::new();
        let mut stack_roots = BTreeSet::new();
        for (home_index, home) in expanded.stack_homes.iter().enumerate() {
            if home.id.0 as usize != home_index {
                return Err(JointAllocationError::new(
                    "JOINT_ALLOC.STACK_HOME_IDENTITY",
                    None,
                    None,
                    "expanded stack homes are not a dense identity-ordered domain",
                ));
            }
            let root = expanded.roots.get(home.root.0 as usize).ok_or_else(|| {
                JointAllocationError::new(
                    "JOINT_ALLOC.STACK_HOME_ROOT",
                    None,
                    None,
                    "expanded stack home references a missing root",
                )
            })?;
            if root.id != home.root {
                return Err(JointAllocationError::new(
                    "JOINT_ALLOC.STACK_HOME_ROOT",
                    None,
                    Some(root.origin),
                    "expanded stack-home root differs from its dense row",
                ));
            }
            match home.kind {
                ExpandedStackHomeKind::Root => {
                    if !stack_roots.insert(home.root) {
                        return Err(JointAllocationError::new(
                            "JOINT_ALLOC.STACK_HOME_IDENTITY",
                            None,
                            Some(root.origin),
                            "one expanded root owns more than one persistent stack home",
                        ));
                    }
                    match home.definition {
                        ExpandedStackDefinition::Store { instruction, value }
                            if value == root.origin =>
                        {
                            let site = expanded
                                .ir
                                .resolve_stack_store_use_site_indexed(
                                    instruction,
                                    home.id,
                                    value,
                                    &expanded.intervals,
                                    &synthetic_instruction_index,
                                )
                                .map_err(JointAllocationError::ir)?;
                            fixed_region_uses.entry(root.origin).or_default().push(site);
                        }
                        ExpandedStackDefinition::Phi {
                            block,
                            phi,
                            destination,
                        } if destination == root.origin => {
                            expanded
                                .ir
                                .verify_phi_stack_definition(block, phi, destination, home.id)
                                .map_err(JointAllocationError::ir)?;
                        }
                        _ => {
                            return Err(JointAllocationError::new(
                                "JOINT_ALLOC.STACK_HOME_DEFINITION",
                                None,
                                Some(root.origin),
                                "persistent stack home has an incompatible definition",
                            ));
                        }
                    }
                }
                ExpandedStackHomeKind::EdgeRecipe { use_id } => {
                    let ExpandedStackDefinition::Store { instruction, value } = home.definition
                    else {
                        return Err(JointAllocationError::new(
                            "JOINT_ALLOC.EDGE_HOME_DEFINITION",
                            None,
                            Some(root.origin),
                            "edge recipe stack home is not defined by an explicit store",
                        ));
                    };
                    let use_ = root.uses.get(use_id.0 as usize).ok_or_else(|| {
                        JointAllocationError::new(
                            "JOINT_ALLOC.EDGE_HOME_USE",
                            None,
                            Some(value),
                            "edge recipe stack home references a missing root use",
                        )
                    })?;
                    let ExpandedUseSource::Edge(ExpandedEdgeLocation::Stack { home: use_home }) =
                        &use_.source
                    else {
                        return Err(JointAllocationError::new(
                            "JOINT_ALLOC.EDGE_HOME_USE",
                            Some(use_.site.block()),
                            Some(value),
                            "edge recipe stack home is not owned by its exact phi use",
                        ));
                    };
                    let UseSite::PhiEdge { predecessor, .. } = use_.site else {
                        return Err(JointAllocationError::new(
                            "JOINT_ALLOC.EDGE_HOME_USE",
                            Some(use_.site.block()),
                            Some(value),
                            "edge recipe stack home is attached to an instruction use",
                        ));
                    };
                    if *use_home != home.id || use_.value != root.origin {
                        return Err(JointAllocationError::new(
                            "JOINT_ALLOC.EDGE_HOME_USE",
                            Some(predecessor),
                            Some(value),
                            "edge stack metadata and expanded root use disagree",
                        ));
                    }
                    let store = expanded
                        .ir
                        .resolve_stack_store_use_site_indexed(
                            instruction,
                            home.id,
                            value,
                            &expanded.intervals,
                            &synthetic_instruction_index,
                        )
                        .map_err(JointAllocationError::ir)?;
                    if store.block() != predecessor || store.slot() >= use_.site.slot() {
                        return Err(JointAllocationError::new(
                            "JOINT_ALLOC.EDGE_HOME_ORDER",
                            Some(predecessor),
                            Some(value),
                            "edge recipe store is not ordered before its exact phi-edge location",
                        ));
                    }
                }
            }
        }

        let region_metadata = expanded
            .register_regions
            .iter()
            .map(|region| (region.id, region))
            .collect::<BTreeMap<_, _>>();
        if region_metadata.len() != expanded.register_regions.len() {
            return Err(JointAllocationError::new(
                "JOINT_ALLOC.REGION_IDENTITY",
                None,
                None,
                "two expanded register regions share one identity",
            ));
        }
        if expanded.region_rows.len() != expanded.register_regions.len()
            || expanded
                .register_regions
                .iter()
                .enumerate()
                .any(|(row, region)| {
                    expanded.region_rows.get(&region.id) != Some(&row)
                        || region.id.0 >= expanded.next_register_region
                })
        {
            return Err(JointAllocationError::new(
                "JOINT_ALLOC.REGION_INDEX",
                None,
                None,
                "stable register-region index differs from active metadata",
            ));
        }

        let mut regions = BTreeMap::<VReg, RegionBuilder>::new();
        let mut referenced_regions = BTreeSet::new();
        for root in &expanded.roots {
            for use_ in &root.uses {
                let preferred_register = match use_.source {
                    ExpandedUseSource::OriginalRegister { preferred_register } => {
                        if use_.value != root.origin {
                            return Err(JointAllocationError::new(
                                "JOINT_ALLOC.ORIGINAL_REGION",
                                Some(use_.site.block()),
                                Some(use_.value),
                                "original register use is not owned by its root value",
                            ));
                        }
                        preferred_register
                    }
                    ExpandedUseSource::RegisterRegion {
                        region,
                        preferred_register,
                    } => {
                        let metadata = region_metadata.get(&region).ok_or_else(|| {
                            JointAllocationError::new(
                                "JOINT_ALLOC.REGION_IDENTITY",
                                Some(use_.site.block()),
                                Some(use_.value),
                                "register use references missing expanded-region metadata",
                            )
                        })?;
                        if metadata.root != root.id
                            || metadata.value != use_.value
                            || metadata.preferred_register != preferred_register
                        {
                            return Err(JointAllocationError::new(
                                "JOINT_ALLOC.REGION_IDENTITY",
                                Some(use_.site.block()),
                                Some(use_.value),
                                "register use and expanded-region metadata disagree",
                            ));
                        }
                        referenced_regions.insert(region);
                        preferred_register
                    }
                    ExpandedUseSource::Materialized(_) => continue,
                    ExpandedUseSource::Edge(_) => continue,
                };
                let region = regions.entry(use_.value).or_insert_with(|| RegionBuilder {
                    root: root.id,
                    uses: Vec::new(),
                    sites: Vec::new(),
                    preferred_register,
                });
                if region.root != root.id || region.preferred_register != preferred_register {
                    return Err(JointAllocationError::new(
                        "JOINT_ALLOC.REGION_OWNERSHIP",
                        Some(use_.site.block()),
                        Some(use_.value),
                        "one machine value is claimed by incompatible register regions",
                    ));
                }
                region.uses.push(use_.id);
                region.sites.push(use_.site);
            }
        }
        if referenced_regions != region_metadata.keys().copied().collect() {
            return Err(JointAllocationError::new(
                "JOINT_ALLOC.REGION_COVERAGE",
                None,
                None,
                "expanded register-region metadata is not referenced by its region uses",
            ));
        }

        let mut values = Vec::new();
        let mut value_rows = vec![None; expanded.ir.value_count() as usize];
        for (value_index, interval) in expanded.intervals.intervals.iter().enumerate() {
            let Some(interval) = interval else {
                continue;
            };
            let value = VReg(u32::try_from(value_index).map_err(|_| {
                JointAllocationError::new(
                    "JOINT_ALLOC.VALUE_ID_RANGE",
                    None,
                    None,
                    "allocation value index exceeds u32",
                )
            })?);
            if interval.value != value || interval.segments.is_empty() {
                return Err(JointAllocationError::new(
                    "JOINT_ALLOC.INTERVAL_IDENTITY",
                    Some(interval.definition.block()),
                    Some(value),
                    "live interval identity or sparse range is malformed",
                ));
            }
            let (class, spill_cost, preferred_register) = if let Some(mut region) =
                regions.remove(&value)
            {
                region.uses.sort_unstable();
                region.sites.sort_unstable();
                region.sites.dedup();
                let mut owned_sites = region.sites.clone();
                owned_sites.extend(fixed_region_uses.get(&value).into_iter().flatten().copied());
                owned_sites.sort_unstable();
                owned_sites.dedup();
                if region.uses.is_empty()
                    || region.uses.windows(2).any(|pair| pair[0] >= pair[1])
                    || interval.uses.len() != owned_sites.len()
                    || owned_sites
                        .iter()
                        .any(|site| !interval.contains_use_coordinate(*site))
                {
                    return Err(JointAllocationError::new(
                        "JOINT_ALLOC.REGION_USES",
                        Some(interval.definition.block()),
                        Some(value),
                        "register region plus its identified fixed stack-store use do not own the exact expanded interval uses",
                    ));
                }
                let home_plan = home_plans.get(region.root.0 as usize).ok_or_else(|| {
                    JointAllocationError::new(
                        "JOINT_ALLOC.HOME_COST_ROOT",
                        Some(interval.definition.block()),
                        Some(value),
                        "register region has no physical-allocation home-cost row",
                    )
                })?;
                let spill_cost = home_plan
                    .spill_cost(&region.uses, stack_roots.contains(&region.root))
                    .map_err(JointAllocationError::home)?;
                (
                    AllocationValueClass::Region {
                        root: region.root,
                        uses: region.uses,
                    },
                    Some(spill_cost),
                    region.preferred_register,
                )
            } else {
                (AllocationValueClass::Fixed, None, None)
            };
            let id = AllocationBundleId(value.0);
            let live_length = cached_program_order_length(expanded, interval)?;
            value_rows[value.0 as usize] = Some(values.len());
            values.push(AllocationValue {
                id,
                value,
                interval: interval.clone(),
                class,
                spill_cost,
                live_length,
                preferred_register,
                allowed_registers: constraints.allowed(value).ok_or_else(|| {
                    JointAllocationError::new(
                        "JOINT_ALLOC.CONSTRAINT_VALUE",
                        Some(interval.definition.block()),
                        Some(value),
                        "machine value has no target-register constraint row",
                    )
                })?,
            });
        }
        if let Some((&value, region)) = regions.first_key_value() {
            return Err(JointAllocationError::new(
                "JOINT_ALLOC.REGION_INTERVAL",
                region.sites.first().map(|site| site.block()),
                Some(value),
                "expanded register region has no live interval",
            ));
        }

        let definition_order = definition_order(&values, cfg)?;
        let affinities = constraints.affinities.clone();
        let affinity_index = AffinityIndex::build(expanded.ir.value_count(), &affinities)?;
        Ok(Self {
            value_count: expanded.ir.value_count(),
            values,
            value_rows,
            definition_order,
            target_registers: registers.to_vec(),
            affinities,
            affinity_index,
            fixed_reservations: constraints.fixed_reservations.clone(),
        })
    }

    pub(super) fn value(&self, value: VReg) -> Option<&AllocationValue> {
        let row = self.value_rows.get(value.0 as usize).copied().flatten()?;
        self.values.get(row)
    }

    fn value_mut(&mut self, value: VReg) -> Option<&mut AllocationValue> {
        let row = self.value_rows.get(value.0 as usize).copied().flatten()?;
        self.values.get_mut(row)
    }

    fn replace_value(
        &mut self,
        value: VReg,
        replacement: Option<AllocationValue>,
    ) -> Result<(), JointAllocationError> {
        let index = value.0 as usize;
        if index >= self.value_count as usize || index >= self.value_rows.len() {
            return Err(JointAllocationError::new(
                "JOINT_ALLOC.SESSION_VALUE_RANGE",
                None,
                Some(value),
                "session value replacement is outside the stable VReg table",
            ));
        }
        match (self.value_rows[index], replacement) {
            (Some(row), Some(replacement)) => {
                if replacement.value != value || replacement.id != AllocationBundleId(value.0) {
                    return Err(JointAllocationError::new(
                        "JOINT_ALLOC.SESSION_VALUE_IDENTITY",
                        Some(replacement.interval.definition.block()),
                        Some(value),
                        "replacement allocation row has a different stable identity",
                    ));
                }
                self.values[row] = replacement;
            }
            (Some(row), None) => {
                self.value_rows[index] = None;
                self.values.swap_remove(row);
                if let Some(moved) = self.values.get(row) {
                    self.value_rows[moved.value.0 as usize] = Some(row);
                }
            }
            (None, Some(replacement)) => {
                if replacement.value != value || replacement.id != AllocationBundleId(value.0) {
                    return Err(JointAllocationError::new(
                        "JOINT_ALLOC.SESSION_VALUE_IDENTITY",
                        Some(replacement.interval.definition.block()),
                        Some(value),
                        "new allocation row has a different stable identity",
                    ));
                }
                self.value_rows[index] = Some(self.values.len());
                self.values.push(replacement);
            }
            (None, None) => {}
        }
        Ok(())
    }

    fn bundle(&self, bundle: AllocationBundleId) -> Option<&AllocationValue> {
        self.value(VReg(bundle.0))
    }

    pub(super) fn allocate(
        &self,
        cfg: &NormalizedCfg,
        registers: &[PhysReg],
    ) -> Result<JointAllocationOutcome, JointAllocationError> {
        JointAllocationSession::new(self.clone(), cfg, registers)?.allocate(cfg, registers)
    }
    fn assigned_affinity_score(
        &self,
        value: VReg,
        register: PhysReg,
        assignments: &[Option<PhysReg>],
    ) -> u64 {
        self.affinity_index
            .neighbors(value)
            .iter()
            .filter_map(|neighbor| {
                (assignments
                    .get(neighbor.value.0 as usize)
                    .copied()
                    .flatten()
                    == Some(register))
                .then_some(u64::from(neighbor.weight))
            })
            .sum()
    }

    /// Conservative post-color coalescing. Every attempted recolor removes
    /// both endpoints from the exact sparse matrix, requires a common allowed
    /// color, proves the union interference-free, and publishes only when the
    /// total satisfied incident affinity weight strictly increases.
    fn coalesce_affinities(
        &self,
        matrix: &mut LiveIntervalMatrix,
        ranges: &[Option<SparseRange>],
        assignments: &mut [Option<PhysReg>],
    ) -> Result<(), JointAllocationError> {
        let mut affinities = self.affinities.clone();
        affinities.sort_unstable_by(|left, right| {
            right
                .weight
                .cmp(&left.weight)
                .then_with(|| (left.left, left.right).cmp(&(right.left, right.right)))
        });
        for affinity in affinities {
            let Some(left) = self.value(affinity.left) else {
                continue;
            };
            let Some(right) = self.value(affinity.right) else {
                continue;
            };
            if left.interval.interferes(&right.interval) {
                continue;
            }
            let left_register = assignments[left.value.0 as usize].ok_or_else(|| {
                JointAllocationError::new(
                    "JOINT_ALLOC.COALESCE_ASSIGNMENT",
                    Some(left.interval.definition.block()),
                    Some(left.value),
                    "affinity endpoint has no physical assignment",
                )
            })?;
            let right_register = assignments[right.value.0 as usize].ok_or_else(|| {
                JointAllocationError::new(
                    "JOINT_ALLOC.COALESCE_ASSIGNMENT",
                    Some(right.interval.definition.block()),
                    Some(right.value),
                    "affinity endpoint has no physical assignment",
                )
            })?;
            if left_register == right_register {
                continue;
            }
            let before = self.incident_affinity_score(left.value, right.value, None, assignments);
            let mut candidates = self
                .target_registers
                .iter()
                .copied()
                .enumerate()
                .filter(|(_, register)| {
                    left.allowed_registers.contains(*register)
                        && right.allowed_registers.contains(*register)
                })
                .map(|(order, register)| {
                    let score = self.incident_affinity_score(
                        left.value,
                        right.value,
                        Some(register),
                        assignments,
                    );
                    (order, register, score)
                })
                .collect::<Vec<_>>();
            candidates.sort_by(
                |(left_order, _, left_score), (right_order, _, right_score)| {
                    right_score
                        .cmp(left_score)
                        .then_with(|| left_order.cmp(right_order))
                },
            );

            let left_range = ranges
                .get(left.value.0 as usize)
                .and_then(Option::as_ref)
                .ok_or_else(|| {
                    JointAllocationError::new(
                        "JOINT_ALLOC.COALESCE_RANGE",
                        Some(left.interval.definition.block()),
                        Some(left.value),
                        "left affinity endpoint has no validated sparse range",
                    )
                })?;
            let right_range = ranges
                .get(right.value.0 as usize)
                .and_then(Option::as_ref)
                .ok_or_else(|| {
                    JointAllocationError::new(
                        "JOINT_ALLOC.COALESCE_RANGE",
                        Some(right.interval.definition.block()),
                        Some(right.value),
                        "right affinity endpoint has no validated sparse range",
                    )
                })?;
            matrix
                .unassign_validated(left.id, left_range.validated())
                .map_err(JointAllocationError::union)?;
            if let Err(error) = matrix.unassign_validated(right.id, right_range.validated()) {
                matrix
                    .assign_validated(left.id, left_register, left_range.validated())
                    .map_err(JointAllocationError::union)?;
                return Err(JointAllocationError::union(error));
            }

            let mut selected = None;
            for (_, candidate, after) in candidates {
                if after <= before
                    || matrix
                        .interferes_validated(candidate, left_range.validated())
                        .map_err(JointAllocationError::union)?
                    || matrix
                        .interferes_validated(candidate, right_range.validated())
                        .map_err(JointAllocationError::union)?
                {
                    continue;
                }
                selected = Some(candidate);
                break;
            }
            if let Some(register) = selected {
                matrix
                    .assign_validated(left.id, register, left_range.validated())
                    .map_err(JointAllocationError::union)?;
                matrix
                    .assign_validated(right.id, register, right_range.validated())
                    .map_err(JointAllocationError::union)?;
                assignments[left.value.0 as usize] = Some(register);
                assignments[right.value.0 as usize] = Some(register);
            } else {
                matrix
                    .assign_validated(left.id, left_register, left_range.validated())
                    .map_err(JointAllocationError::union)?;
                matrix
                    .assign_validated(right.id, right_register, right_range.validated())
                    .map_err(JointAllocationError::union)?;
            }
        }
        matrix.verify().map_err(JointAllocationError::union)
    }

    fn incident_affinity_score(
        &self,
        left: VReg,
        right: VReg,
        override_register: Option<PhysReg>,
        assignments: &[Option<PhysReg>],
    ) -> u64 {
        let assigned = |value: VReg| {
            if matches!(value, candidate if candidate == left || candidate == right) {
                override_register.or_else(|| assignments[value.0 as usize])
            } else {
                assignments[value.0 as usize]
            }
        };
        let left_score = self
            .affinity_index
            .neighbors(left)
            .iter()
            .filter(|neighbor| assigned(left) == assigned(neighbor.value))
            .map(|neighbor| u64::from(neighbor.weight))
            .sum::<u64>();
        if left == right {
            return left_score;
        }
        left_score
            + self
                .affinity_index
                .neighbors(right)
                .iter()
                // The left-right edge was already counted from the left row.
                .filter(|neighbor| neighbor.value != left)
                .filter(|neighbor| assigned(right) == assigned(neighbor.value))
                .map(|neighbor| u64::from(neighbor.weight))
                .sum::<u64>()
    }

    pub(super) fn verify(
        &self,
        cfg: &NormalizedCfg,
        registers: &[PhysReg],
        allocation: &JointAllocation,
    ) -> Result<(), JointAllocationError> {
        if registers != self.target_registers {
            return Err(JointAllocationError::new(
                "JOINT_ALLOC.TARGET_REGISTER_SET",
                None,
                None,
                "verification register order differs from the constraint model",
            ));
        }
        if allocation.assignments.len() != self.value_count as usize {
            return Err(JointAllocationError::new(
                "JOINT_ALLOC.ASSIGNMENT_SHAPE",
                None,
                None,
                "physical assignment table does not cover the allocation value domain",
            ));
        }
        if self.value_rows.len() != self.value_count as usize
            || self.values.iter().enumerate().any(|(row, value)| {
                self.value_rows
                    .get(value.value.0 as usize)
                    .copied()
                    .flatten()
                    != Some(row)
                    || value.id != AllocationBundleId(value.value.0)
            })
        {
            return Err(JointAllocationError::new(
                "JOINT_ALLOC.VALUE_INDEX",
                None,
                None,
                "VReg-to-allocation-value index is not a bijection",
            ));
        }
        let mut rebuilt =
            LiveIntervalMatrix::new(cfg, registers).map_err(JointAllocationError::union)?;
        rebuilt
            .replace_fixed_reservations(&self.fixed_reservations)
            .map_err(JointAllocationError::union)?;
        for value in &self.values {
            let register = allocation.assignments[value.value.0 as usize].ok_or_else(|| {
                JointAllocationError::new(
                    "JOINT_ALLOC.MISSING_ASSIGNMENT",
                    Some(value.interval.definition.block()),
                    Some(value.value),
                    "machine definition has no physical register",
                )
            })?;
            if !value.allowed_registers.contains(register) {
                return Err(JointAllocationError::new(
                    "JOINT_ALLOC.CONSTRAINT_ASSIGNMENT",
                    Some(value.interval.definition.block()),
                    Some(value.value),
                    format!("assigned register {register} violates the machine-value mask"),
                ));
            }
            rebuilt
                .assign(value.id, register, &value.interval.segments)
                .map_err(JointAllocationError::union)?;
        }
        for (value, assignment) in allocation.assignments.iter().enumerate() {
            if self.value_rows[value].is_none() && assignment.is_some() {
                return Err(JointAllocationError::new(
                    "JOINT_ALLOC.EXTRA_ASSIGNMENT",
                    None,
                    Some(VReg(value as u32)),
                    "physical assignment exists for a value without a machine definition",
                ));
            }
        }
        rebuilt.verify().map_err(JointAllocationError::union)
    }
}

impl RegionOwnershipIndex {
    fn build(expanded: &ExpandedAllocationProblem) -> Result<Self, JointAllocationError> {
        let mut result = Self {
            owners: vec![None; expanded.ir.value_count() as usize],
            values_by_root: vec![Vec::new(); expanded.roots.len()],
        };
        for root in &expanded.roots {
            result.update_root(expanded, root.id)?;
        }
        Ok(result)
    }

    fn owner(&self, value: VReg) -> Option<&IndexedRegion> {
        self.owners.get(value.0 as usize).and_then(Option::as_ref)
    }

    fn update_root(
        &mut self,
        expanded: &ExpandedAllocationProblem,
        root_id: LiveBundleId,
    ) -> Result<Vec<VReg>, JointAllocationError> {
        let root = expanded.roots.get(root_id.0 as usize).ok_or_else(|| {
            JointAllocationError::new(
                "JOINT_ALLOC.SESSION_ROOT_RANGE",
                None,
                None,
                "changed root is outside the expanded allocation problem",
            )
        })?;
        if root.id != root_id || root.id.0 as usize >= self.values_by_root.len() {
            return Err(JointAllocationError::new(
                "JOINT_ALLOC.SESSION_ROOT_IDENTITY",
                None,
                Some(root.origin),
                "changed root differs from the stable root index",
            ));
        }
        self.owners
            .resize_with(expanded.ir.value_count() as usize, || None);
        let mut affected = BTreeSet::new();
        for value in std::mem::take(&mut self.values_by_root[root_id.0 as usize]) {
            let row = self.owners.get_mut(value.0 as usize).ok_or_else(|| {
                JointAllocationError::new(
                    "JOINT_ALLOC.SESSION_REGION_RANGE",
                    None,
                    Some(value),
                    "previous root region is outside the stable VReg table",
                )
            })?;
            if row.as_ref().is_none_or(|owner| owner.root != root_id) {
                return Err(JointAllocationError::new(
                    "JOINT_ALLOC.SESSION_REGION_IDENTITY",
                    None,
                    Some(value),
                    "previous region owner differs from its root index",
                ));
            }
            *row = None;
            affected.insert(value);
        }

        let mut grouped = BTreeMap::<VReg, (Option<PhysReg>, Vec<BundleUseId>)>::new();
        for (use_index, use_) in root.uses.iter().enumerate() {
            if use_.id.0 as usize != use_index {
                return Err(JointAllocationError::new(
                    "JOINT_ALLOC.SESSION_USE_IDENTITY",
                    Some(use_.site.block()),
                    Some(use_.value),
                    "root use differs from its stable use row",
                ));
            }
            let preferred_register = match use_.source {
                ExpandedUseSource::OriginalRegister { preferred_register } => {
                    if use_.value != root.origin {
                        return Err(JointAllocationError::new(
                            "JOINT_ALLOC.SESSION_ORIGINAL_REGION",
                            Some(use_.site.block()),
                            Some(use_.value),
                            "original register use is not owned by its root value",
                        ));
                    }
                    preferred_register
                }
                ExpandedUseSource::RegisterRegion {
                    preferred_register, ..
                } => preferred_register,
                ExpandedUseSource::Materialized(_) | ExpandedUseSource::Edge(_) => continue,
            };
            let entry = grouped
                .entry(use_.value)
                .or_insert_with(|| (preferred_register, Vec::new()));
            if entry.0 != preferred_register {
                return Err(JointAllocationError::new(
                    "JOINT_ALLOC.SESSION_REGION_PREFERENCE",
                    Some(use_.site.block()),
                    Some(use_.value),
                    "one session region has incompatible register preferences",
                ));
            }
            entry.1.push(use_.id);
        }
        for (value, (preferred_register, mut uses)) in grouped {
            uses.sort_unstable();
            uses.dedup();
            let row = self.owners.get_mut(value.0 as usize).ok_or_else(|| {
                JointAllocationError::new(
                    "JOINT_ALLOC.SESSION_REGION_RANGE",
                    None,
                    Some(value),
                    "new root region is outside the stable VReg table",
                )
            })?;
            if let Some(existing) = row {
                return Err(JointAllocationError::new(
                    "JOINT_ALLOC.SESSION_REGION_OWNERSHIP",
                    None,
                    Some(value),
                    format!(
                        "machine value is owned by roots {:?} and {:?}",
                        existing.root, root_id
                    ),
                ));
            }
            *row = Some(IndexedRegion {
                root: root_id,
                uses,
                preferred_register,
            });
            self.values_by_root[root_id.0 as usize].push(value);
            affected.insert(value);
        }
        self.values_by_root[root_id.0 as usize].sort_unstable();
        Ok(affected.into_iter().collect())
    }
}

fn allocation_queue_item(
    value: &AllocationValue,
) -> Result<AllocationQueueItem, JointAllocationError> {
    if value.live_length == 0 {
        return Err(JointAllocationError::new(
            "JOINT_ALLOC.LIVE_LENGTH",
            Some(value.interval.definition.block()),
            Some(value.value),
            "active allocation value has an empty sparse range",
        ));
    }
    Ok(AllocationQueueItem {
        id: value.id,
        spill_cost: value.spill_cost,
        live_length: value.live_length,
        use_count: value.interval.uses.len(),
    })
}

impl JointAllocationSession {
    pub(super) fn new(
        problem: JointAllocationProblem,
        cfg: &NormalizedCfg,
        registers: &[PhysReg],
    ) -> Result<Self, JointAllocationError> {
        if registers != problem.target_registers {
            return Err(JointAllocationError::new(
                "JOINT_ALLOC.TARGET_REGISTER_SET",
                None,
                None,
                "allocation register order differs from the verified constraint model",
            ));
        }
        let mut matrix =
            LiveIntervalMatrix::new(cfg, registers).map_err(JointAllocationError::union)?;
        matrix
            .replace_fixed_reservations(&problem.fixed_reservations)
            .map_err(JointAllocationError::union)?;
        let mut ranges = (0..problem.value_count)
            .map(|_| None::<SparseRange>)
            .collect::<Vec<_>>();
        for value in &problem.values {
            validate_allocatable_value(value, registers)?;
            ranges[value.value.0 as usize] = Some(
                matrix
                    .make_range(value.interval.segments.clone())
                    .map_err(JointAllocationError::union)?,
            );
        }
        let assignments = vec![None; problem.value_count as usize];
        let mut pending = problem
            .values
            .iter()
            .map(allocation_queue_item)
            .collect::<Result<Vec<_>, _>>()?;
        pending.sort_unstable();
        let definition_rank = dominator_rank(cfg)?;
        Ok(Self {
            problem,
            constraints: None,
            ownership: None,
            definition_rank,
            matrix,
            ranges,
            assignments,
            pending,
            home_plans: None,
            deferred: BTreeSet::new(),
        })
    }

    pub(super) fn problem(&self) -> &JointAllocationProblem {
        &self.problem
    }

    pub(super) fn new_persistent(
        problem: JointAllocationProblem,
        expanded: &ExpandedAllocationProblem,
        cfg: &NormalizedCfg,
        graph: &HomeGraph,
        registers: &[PhysReg],
    ) -> Result<Self, JointAllocationError> {
        let constraints = IncrementalConstraintModel::build(expanded, cfg, graph, registers)
            .map_err(JointAllocationError::constraints)?;
        let ownership = RegionOwnershipIndex::build(expanded)?;
        let home_plans = build_home_plans(graph)?;
        let rebuilt = JointAllocationProblem::build_session_with_constraints(
            expanded,
            cfg,
            registers,
            constraints.model(),
            &home_plans,
        )?;
        if rebuilt != problem {
            return Err(JointAllocationError::new(
                "JOINT_ALLOC.SESSION_CONSTRAINT_IDENTITY",
                None,
                None,
                "block-indexed target constraints differ from the initial joint problem",
            ));
        }
        let mut result = Self::new(problem, cfg, registers)?;
        result.constraints = Some(constraints);
        result.ownership = Some(ownership);
        result.home_plans = Some(home_plans);
        Ok(result)
    }

    /// Start the production allocation session directly from the incremental
    /// constraint owner. The caller already owns function-lifetime home plans,
    /// so this constructs neither a second constraint model nor an independent
    /// whole-function liveness proof.
    pub(super) fn new_cached_persistent(
        expanded: &ExpandedAllocationProblem,
        cfg: &NormalizedCfg,
        graph: &HomeGraph,
        registers: &[PhysReg],
        home_plans: &[RootHomePlan],
    ) -> Result<Self, JointAllocationError> {
        let constraints = IncrementalConstraintModel::build(expanded, cfg, graph, registers)
            .map_err(JointAllocationError::constraints)?;
        let ownership = RegionOwnershipIndex::build(expanded)?;
        let problem = JointAllocationProblem::build_session_with_constraints(
            expanded,
            cfg,
            registers,
            constraints.model(),
            home_plans,
        )?;
        let mut result = Self::new(problem, cfg, registers)?;
        result.constraints = Some(constraints);
        result.ownership = Some(ownership);
        Ok(result)
    }

    pub(super) fn update_from_expanded(
        &mut self,
        expanded: &ExpandedAllocationProblem,
        cfg: &NormalizedCfg,
        graph: &HomeGraph,
        registers: &[PhysReg],
        changed_blocks: &BTreeSet<BlockId>,
        changed_values: &[VReg],
        range_changed_values: &[VReg],
        live_lengths: &[(VReg, Option<u64>)],
        changed_root: LiveBundleId,
    ) -> Result<(), JointAllocationError> {
        let home_plans = self.home_plans.take().ok_or_else(|| {
            JointAllocationError::new(
                "JOINT_ALLOC.SESSION_HOME_COST_STATE",
                None,
                None,
                "persistent joint update has no function-lifetime home-cost model",
            )
        })?;
        let result = self.update_from_expanded_round(
            expanded,
            cfg,
            graph,
            registers,
            changed_blocks,
            changed_values,
            range_changed_values,
            live_lengths,
            std::slice::from_ref(&changed_root),
            &home_plans,
        );
        self.home_plans = Some(home_plans);
        result
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn update_from_expanded_round(
        &mut self,
        expanded: &ExpandedAllocationProblem,
        cfg: &NormalizedCfg,
        graph: &HomeGraph,
        registers: &[PhysReg],
        changed_blocks: &BTreeSet<BlockId>,
        changed_values: &[VReg],
        range_changed_values: &[VReg],
        live_lengths: &[(VReg, Option<u64>)],
        changed_roots: &[LiveBundleId],
        home_plans: &[RootHomePlan],
    ) -> Result<(), JointAllocationError> {
        if registers != self.problem.target_registers {
            return Err(JointAllocationError::new(
                "JOINT_ALLOC.TARGET_REGISTER_SET",
                None,
                None,
                "incremental joint update uses a different physical register set",
            ));
        }
        let constraint_update = self
            .constraints
            .as_mut()
            .ok_or_else(|| {
                JointAllocationError::new(
                    "JOINT_ALLOC.SESSION_CONSTRAINT_STATE",
                    None,
                    None,
                    "persistent joint update has no block-indexed constraint state",
                )
            })?
            .update(expanded, cfg, graph, changed_blocks, range_changed_values)
            .map_err(JointAllocationError::constraints)?;
        if changed_roots.is_empty() || changed_roots.windows(2).any(|roots| roots[0] >= roots[1]) {
            return Err(JointAllocationError::new(
                "JOINT_ALLOC.SESSION_ROOT_SET",
                None,
                None,
                "incremental allocation round has no roots or duplicate/out-of-order roots",
            ));
        }
        let ownership = self.ownership.as_mut().ok_or_else(|| {
            JointAllocationError::new(
                "JOINT_ALLOC.SESSION_REGION_STATE",
                None,
                None,
                "persistent joint update has no region-ownership index",
            )
        })?;
        let mut ownership_changes = BTreeSet::new();
        for &root in changed_roots {
            ownership_changes.extend(ownership.update_root(expanded, root)?);
        }
        self.apply_live_lengths(live_lengths, range_changed_values)?;
        let mut affected = range_changed_values
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        affected.extend(constraint_update.changed_values.iter().copied());
        affected.extend(ownership_changes);

        let constraints = self.constraints.as_ref().unwrap().model();
        let ownership = self.ownership.as_ref().unwrap();
        let mut replacements = Vec::with_capacity(affected.len());
        for &value in &affected {
            replacements.push((
                value,
                session_allocation_value(
                    expanded,
                    constraints,
                    ownership,
                    home_plans,
                    value,
                    registers,
                )?,
            ));
        }
        let affinities = constraint_update
            .affinities_changed
            .then(|| constraints.affinities.clone());
        let fixed_reservations = constraint_update
            .fixed_reservations_changed
            .then(|| constraints.fixed_reservations.clone());
        self.apply_value_delta(
            expanded.ir.value_count(),
            replacements,
            affinities,
            fixed_reservations,
            cfg,
            registers,
        )?;
        self.relabel_intervals(expanded, changed_values)?;
        self.deferred.clear();
        Ok(())
    }

    fn apply_live_lengths(
        &mut self,
        lengths: &[(VReg, Option<u64>)],
        range_changed_values: &[VReg],
    ) -> Result<(), JointAllocationError> {
        for &(value, length) in lengths {
            if range_changed_values.binary_search(&value).is_ok() {
                continue;
            }
            let Some(length) = length else {
                if self.problem.value(value).is_some() {
                    return Err(JointAllocationError::new(
                        "JOINT_ALLOC.LIVE_LENGTH_CACHE",
                        self.problem
                            .value(value)
                            .map(|value| value.interval.definition.block()),
                        Some(value),
                        "removed interval retained a non-rebuilt allocation row",
                    ));
                }
                continue;
            };
            if length == 0 {
                return Err(JointAllocationError::new(
                    "JOINT_ALLOC.LIVE_LENGTH_CACHE",
                    self.problem
                        .value(value)
                        .map(|value| value.interval.definition.block()),
                    Some(value),
                    "active cached program-order length is zero",
                ));
            }
            if let Some(current) = self.problem.value_mut(value) {
                current.live_length = length;
            }
        }
        Ok(())
    }

    fn relabel_intervals(
        &mut self,
        expanded: &ExpandedAllocationProblem,
        changed_values: &[VReg],
    ) -> Result<(), JointAllocationError> {
        for &value in changed_values {
            let next = expanded
                .intervals
                .intervals
                .get(value.0 as usize)
                .and_then(Option::as_ref);
            match (self.problem.value_mut(value), next) {
                (None, None) => {}
                (Some(current), Some(next)) => {
                    if current.interval != *next && !current.interval.relabel_from(next) {
                        return Err(JointAllocationError::new(
                            "JOINT_ALLOC.SESSION_RELABEL_GEOMETRY",
                            Some(current.interval.definition.block()),
                            Some(value),
                            "metadata-only liveness update changed physical allocation geometry",
                        ));
                    }
                }
                (None, Some(next)) => {
                    return Err(JointAllocationError::new(
                        "JOINT_ALLOC.SESSION_RELABEL_VALUE",
                        Some(next.definition.block()),
                        Some(value),
                        "active relabeled interval has no persistent allocation row",
                    ));
                }
                (Some(current), None) => {
                    return Err(JointAllocationError::new(
                        "JOINT_ALLOC.SESSION_RELABEL_VALUE",
                        Some(current.interval.definition.block()),
                        Some(value),
                        "dead relabeled interval retained a persistent allocation row",
                    ));
                }
            }
        }
        Ok(())
    }

    fn apply_value_delta(
        &mut self,
        value_count: u32,
        replacements: Vec<(VReg, Option<AllocationValue>)>,
        affinities: Option<Vec<WeightedAffinity>>,
        fixed_reservations: Option<Vec<FixedRegisterReservation>>,
        cfg: &NormalizedCfg,
        registers: &[PhysReg],
    ) -> Result<(), JointAllocationError> {
        if value_count < self.problem.value_count {
            return Err(JointAllocationError::new(
                "JOINT_ALLOC.SESSION_ID_RANGE",
                None,
                None,
                "allocation-session VReg bound decreased after a split",
            ));
        }
        self.problem.value_count = value_count;
        self.problem.value_rows.resize(value_count as usize, None);
        self.assignments.resize(value_count as usize, None);
        self.ranges.resize_with(value_count as usize, || None);

        let mut changed = BTreeSet::new();
        for (value, replacement) in replacements {
            let previous = self.problem.value(value).cloned();
            if previous == replacement {
                continue;
            }
            if matches!(
                (previous.as_ref(), replacement.as_ref()),
                (Some(previous), Some(replacement))
                    if same_allocation_geometry(previous, replacement)
            ) {
                // Stable program points distinguish an instruction's physical
                // range from its current dense lowering position. Updating
                // only that metadata must not evict a valid color or rebuild
                // its interval-union token.
                self.problem.replace_value(value, replacement)?;
                continue;
            }
            let index = value.0 as usize;
            if self.assignments[index].is_some() {
                let previous = previous.as_ref().ok_or_else(|| {
                    JointAllocationError::new(
                        "JOINT_ALLOC.SESSION_ASSIGNMENT",
                        None,
                        Some(value),
                        "assigned value has no previous semantic allocation row",
                    )
                })?;
                let range = self.ranges[index].as_ref().ok_or_else(|| {
                    JointAllocationError::new(
                        "JOINT_ALLOC.SESSION_RANGE",
                        Some(previous.interval.definition.block()),
                        Some(value),
                        "assigned changed value has no retained sparse range",
                    )
                })?;
                self.matrix
                    .unassign_validated(previous.id, range.validated())
                    .map_err(JointAllocationError::union)?;
            }
            self.assignments[index] = None;
            self.ranges[index] = None;
            if let Some(previous) = &previous {
                let entry = DefinitionOrderEntry {
                    key: definition_key(previous, cfg, &self.definition_rank)?,
                    id: previous.id,
                };
                if !self.problem.definition_order.remove(&entry) {
                    return Err(JointAllocationError::new(
                        "JOINT_ALLOC.SESSION_DEFINITION_ORDER",
                        Some(previous.interval.definition.block()),
                        Some(value),
                        "changed value is absent from the persistent definition order",
                    ));
                }
            }
            self.problem.replace_value(value, replacement.clone())?;
            if let Some(replacement) = replacement {
                validate_allocatable_value(&replacement, registers)?;
                let entry = DefinitionOrderEntry {
                    key: definition_key(&replacement, cfg, &self.definition_rank)?,
                    id: replacement.id,
                };
                if !self.problem.definition_order.insert(entry) {
                    return Err(JointAllocationError::new(
                        "JOINT_ALLOC.SESSION_DEFINITION_ORDER",
                        Some(replacement.interval.definition.block()),
                        Some(value),
                        "changed value duplicates a persistent definition-order entry",
                    ));
                }
                self.ranges[index] = Some(
                    self.matrix
                        .make_range(replacement.interval.segments.clone())
                        .map_err(JointAllocationError::union)?,
                );
            }
            changed.insert(value);
        }
        if let Some(affinities) = affinities {
            self.problem.affinities = affinities;
            self.problem.affinity_index =
                AffinityIndex::build(self.problem.value_count, &self.problem.affinities)?;
        }
        if let Some(fixed_reservations) = fixed_reservations {
            self.matrix
                .replace_fixed_reservations(&fixed_reservations)
                .map_err(JointAllocationError::union)?;
            self.problem.fixed_reservations = fixed_reservations;
        }

        let mut pending = self
            .pending
            .drain(..)
            .map(|item| item.id)
            .collect::<BTreeSet<_>>();
        pending.retain(|id| {
            self.problem.value(VReg(id.0)).is_some() && self.assignments[id.0 as usize].is_none()
        });
        pending.extend(changed.into_iter().filter_map(|value| {
            (self.problem.value(value).is_some() && self.assignments[value.0 as usize].is_none())
                .then_some(AllocationBundleId(value.0))
        }));
        self.pending = pending
            .into_iter()
            .map(|id| {
                let value = self.problem.bundle(id).ok_or_else(|| {
                    JointAllocationError::new(
                        "JOINT_ALLOC.SESSION_PENDING_VALUE",
                        None,
                        Some(VReg(id.0)),
                        "pending session value has no semantic allocation row",
                    )
                })?;
                allocation_queue_item(value)
            })
            .collect::<Result<Vec<_>, JointAllocationError>>()?;
        self.pending.sort_unstable();
        Ok(())
    }

    /// Replace the semantic problem while retaining every byte-identical
    /// interval's physical assignment. Stable VReg/bundle identities make the
    /// comparison independent of active-row compaction.
    pub(super) fn update(
        &mut self,
        next: JointAllocationProblem,
        registers: &[PhysReg],
    ) -> Result<(), JointAllocationError> {
        if registers != self.problem.target_registers || registers != next.target_registers {
            return Err(JointAllocationError::new(
                "JOINT_ALLOC.TARGET_REGISTER_SET",
                None,
                None,
                "updated allocation problem uses a different physical register set",
            ));
        }
        if next.value_count < self.problem.value_count {
            return Err(JointAllocationError::new(
                "JOINT_ALLOC.SESSION_ID_RANGE",
                None,
                None,
                "allocation-session VReg bound decreased after a split",
            ));
        }

        for old in &self.problem.values {
            if next.value(old.value) == Some(old) {
                continue;
            }
            let value = old.value.0 as usize;
            if self.assignments[value].is_some() {
                let range = self.ranges[value].as_ref().ok_or_else(|| {
                    JointAllocationError::new(
                        "JOINT_ALLOC.SESSION_RANGE",
                        Some(old.interval.definition.block()),
                        Some(old.value),
                        "assigned session value has no retained sparse range",
                    )
                })?;
                self.matrix
                    .unassign_validated(old.id, range.validated())
                    .map_err(JointAllocationError::union)?;
            }
            self.assignments[value] = None;
            self.ranges[value] = None;
        }

        self.assignments.resize(next.value_count as usize, None);
        self.ranges.resize_with(next.value_count as usize, || None);
        for value in &next.values {
            let index = value.value.0 as usize;
            if self.problem.value(value.value) == Some(value) {
                if self.ranges[index].is_none() {
                    return Err(JointAllocationError::new(
                        "JOINT_ALLOC.SESSION_RANGE",
                        Some(value.interval.definition.block()),
                        Some(value.value),
                        "unchanged session value lost its sparse range",
                    ));
                }
                continue;
            }
            validate_allocatable_value(value, registers)?;
            self.ranges[index] = Some(
                self.matrix
                    .make_range(value.interval.segments.clone())
                    .map_err(JointAllocationError::union)?,
            );
        }

        self.matrix
            .replace_fixed_reservations(&next.fixed_reservations)
            .map_err(JointAllocationError::union)?;

        self.pending = next
            .values
            .iter()
            .filter(|value| self.assignments[value.value.0 as usize].is_none())
            .map(allocation_queue_item)
            .collect::<Result<Vec<_>, _>>()?;
        self.pending.sort_unstable();
        self.problem = next;
        Ok(())
    }

    /// Remove one verified splittable region from the current coloring round
    /// without mutating allocation IR. The caller retains its semantic split
    /// plan and materializes all deferred regions together at the round
    /// boundary.
    pub(super) fn defer_split(&mut self, value: VReg) -> Result<(), JointAllocationError> {
        let allocation = self.problem.value(value).cloned().ok_or_else(|| {
            JointAllocationError::new(
                "JOINT_ALLOC.DEFER_RANGE",
                None,
                Some(value),
                "deferred split value is outside the allocation problem",
            )
        })?;
        if !matches!(allocation.class, AllocationValueClass::Region { .. }) {
            return Err(JointAllocationError::new(
                "JOINT_ALLOC.DEFER_CLASS",
                Some(allocation.interval.definition.block()),
                Some(value),
                "fixed transition cannot be deferred as a spill region",
            ));
        }
        if !self.deferred.insert(value) {
            return Err(JointAllocationError::new(
                "JOINT_ALLOC.DEFER_IDENTITY",
                Some(allocation.interval.definition.block()),
                Some(value),
                "register region was deferred twice in one allocation round",
            ));
        }

        let index = value.0 as usize;
        if self.assignments[index].is_some() {
            let range = self.ranges[index].as_ref().ok_or_else(|| {
                JointAllocationError::new(
                    "JOINT_ALLOC.DEFER_RANGE",
                    Some(allocation.interval.definition.block()),
                    Some(value),
                    "assigned deferred region has no validated sparse range",
                )
            })?;
            self.matrix
                .unassign_validated(allocation.id, range.validated())
                .map_err(JointAllocationError::union)?;
            self.assignments[index] = None;
        }
        Ok(())
    }

    pub(super) fn allocate(
        &mut self,
        cfg: &NormalizedCfg,
        registers: &[PhysReg],
    ) -> Result<JointAllocationOutcome, JointAllocationError> {
        if registers != self.problem.target_registers {
            return Err(JointAllocationError::new(
                "JOINT_ALLOC.TARGET_REGISTER_SET",
                None,
                None,
                "allocation register order differs from the persistent session",
            ));
        }
        while let Some(item) = self.pending.pop() {
            let id = item.id;
            if self.deferred.contains(&VReg(id.0)) {
                continue;
            }
            let value = self.problem.bundle(id).cloned().ok_or_else(|| {
                JointAllocationError::new(
                    "JOINT_ALLOC.ORDER_RANGE",
                    None,
                    Some(VReg(id.0)),
                    "definition order references a missing allocation value",
                )
            })?;
            if allocation_queue_item(&value)? != item {
                return Err(JointAllocationError::new(
                    "JOINT_ALLOC.QUEUE_PRIORITY",
                    Some(value.interval.definition.block()),
                    Some(value.value),
                    "pending allocation priority differs from the current semantic range",
                ));
            }
            if self.assignments[value.value.0 as usize].is_some() {
                continue;
            }
            let range = self.ranges[value.value.0 as usize]
                .as_ref()
                .ok_or_else(|| {
                    JointAllocationError::new(
                        "JOINT_ALLOC.RANGE",
                        Some(value.interval.definition.block()),
                        Some(value.value),
                        "allocation value has no validated sparse range",
                    )
                })?;
            let mut register_order = registers
                .iter()
                .copied()
                .enumerate()
                .filter(|(_, register)| value.allowed_registers.contains(*register))
                .map(|(order, register)| {
                    let affinity_score = self.problem.assigned_affinity_score(
                        value.value,
                        register,
                        &self.assignments,
                    );
                    (order, register, affinity_score)
                })
                .collect::<Vec<_>>();
            register_order.sort_by(
                |(left_order, left, left_score), (right_order, right, right_score)| {
                    right_score
                        .cmp(left_score)
                        .then_with(|| {
                            (Some(*right) == value.preferred_register)
                                .cmp(&(Some(*left) == value.preferred_register))
                        })
                        .then_with(|| left_order.cmp(right_order))
                },
            );
            let mut selected = None;
            for (_, register, _) in register_order {
                if !self
                    .matrix
                    .interferes_validated(register, range.validated())
                    .map_err(JointAllocationError::union)?
                {
                    selected = Some(register);
                    break;
                }
            }
            if let Some(register) = selected {
                self.matrix
                    .assign_validated(value.id, register, range.validated())
                    .map_err(JointAllocationError::union)?;
                self.assignments[value.value.0 as usize] = Some(register);
                continue;
            }

            // The priority worklist has already established that this blocked
            // region is cheaper to displace than every previously colored
            // region. For ordinary movable pressure, no resident identities
            // or occupancy cuts are needed: split once at the SSA definition
            // into earliest dominating-use fragments. Materialize exact cuts
            // only when immutable fixed occupancy is the sole blocker.
            if let AllocationValueClass::Region { root, uses } = &value.class {
                let movable_pressure = registers
                    .iter()
                    .filter(|register| value.allowed_registers.contains(**register))
                    .try_fold(false, |found, &register| {
                        if found {
                            Ok(true)
                        } else {
                            self.matrix
                                .interferes_bundle_validated(register, range.validated())
                                .map_err(JointAllocationError::union)
                        }
                    })?;
                if movable_pressure {
                    let frontier = AllocationPressurePoint {
                        block: value.interval.definition.block(),
                        slot: value.interval.definition.slot(),
                    };
                    self.pending.push(allocation_queue_item(&value)?);
                    return Ok(JointAllocationOutcome::NeedsSplit(RegionSplitRequest {
                        blocked_value: value.value,
                        definition: value.interval.definition,
                        conflicts: Vec::new(),
                        candidates: vec![RegionSplitCandidate {
                            value: value.value,
                            root: *root,
                            uses: uses.clone(),
                            pressure_points: vec![frontier],
                        }],
                        preferred_frontier: Some((value.value, frontier)),
                    }));
                }
            }

            let mut conflicts = Vec::with_capacity(registers.len());
            let mut split_points = BTreeMap::<VReg, BTreeSet<AllocationPressurePoint>>::new();
            let mut has_movable_cut = false;
            let mut has_fixed_cut = false;
            let mut collector = ConflictCollector::default();
            for &register in registers
                .iter()
                .filter(|register| value.allowed_registers.contains(**register))
            {
                let mut residents = Vec::new();
                let mut cuts = Vec::new();
                self.matrix
                    .collect_interference_validated(
                        register,
                        range.validated(),
                        self.problem.value_count as usize,
                        &mut collector,
                        &mut residents,
                        &mut cuts,
                    )
                    .map_err(JointAllocationError::union)?;
                let mut resident_values = Vec::with_capacity(residents.len());
                for &resident_id in &residents {
                    let resident = self.problem.bundle(resident_id).ok_or_else(|| {
                        JointAllocationError::new(
                            "JOINT_ALLOC.CONFLICT_RANGE",
                            Some(value.interval.definition.block()),
                            Some(value.value),
                            "interval matrix references a missing resident value",
                        )
                    })?;
                    resident_values.push(resident.value);
                }
                for cut in &cuts {
                    let segment = value.interval.segments.get(cut.segment).ok_or_else(|| {
                        JointAllocationError::new(
                            "JOINT_ALLOC.OCCUPANCY_CUT_RANGE",
                            Some(value.interval.definition.block()),
                            Some(value.value),
                            "interval union returned a cut outside the blocked sparse range",
                        )
                    })?;
                    let point = AllocationPressurePoint {
                        block: segment.block,
                        slot: cut.start,
                    };
                    if matches!(value.class, AllocationValueClass::Region { .. }) {
                        split_points.entry(value.value).or_default().insert(point);
                    }
                    match cut.owner {
                        OccupancyOwner::Bundle(resident_id) => {
                            has_movable_cut = true;
                            let resident = self.problem.bundle(resident_id).ok_or_else(|| {
                                JointAllocationError::new(
                                    "JOINT_ALLOC.CONFLICT_RANGE",
                                    Some(segment.block),
                                    Some(value.value),
                                    "occupancy cut references a missing resident value",
                                )
                            })?;
                            if matches!(resident.class, AllocationValueClass::Region { .. }) {
                                split_points
                                    .entry(resident.value)
                                    .or_default()
                                    .insert(point);
                            }
                        }
                        OccupancyOwner::Fixed(_) => has_fixed_cut = true,
                    }
                }
                conflicts.push(RegisterConflicts {
                    register,
                    values: resident_values,
                    cuts,
                });
            }
            let preferred_frontier = if has_movable_cut
                && !has_fixed_cut
                && matches!(value.class, AllocationValueClass::Region { .. })
            {
                let frontier = AllocationPressurePoint {
                    block: value.interval.definition.block(),
                    slot: value.interval.definition.slot(),
                };
                split_points
                    .entry(value.value)
                    .or_default()
                    .insert(frontier);
                Some((value.value, frontier))
            } else {
                None
            };
            if split_points.is_empty() {
                let resident_summary = conflicts
                    .iter()
                    .map(|conflict| {
                        let values = conflict
                            .values
                            .iter()
                            .filter_map(|resident| self.problem.value(*resident))
                            .map(allocation_value_summary)
                            .collect::<Vec<_>>()
                            .join(", ");
                        format!("{:?}=[{values}]", conflict.register)
                    })
                    .collect::<Vec<_>>()
                    .join("; ");
                return Err(JointAllocationError::new(
                    "JOINT_ALLOC.UNSPLITTABLE_PRESSURE",
                    Some(value.interval.definition.block()),
                    Some(value.value),
                    format!(
                        "explicit transition ranges exceed the physical register set; blocked={}; residents={resident_summary}",
                        allocation_value_summary(&value)
                    ),
                ));
            }
            let candidates = split_points
                .into_iter()
                .map(|(candidate, pressure_points)| {
                    let candidate = self.problem.value(candidate).ok_or_else(|| {
                        JointAllocationError::new(
                            "JOINT_ALLOC.SPLIT_RANGE",
                            Some(value.interval.definition.block()),
                            Some(value.value),
                            "split candidate is outside the allocation value table",
                        )
                    })?;
                    let AllocationValueClass::Region { root, uses } = &candidate.class else {
                        return Err(JointAllocationError::new(
                            "JOINT_ALLOC.SPLIT_CLASS",
                            Some(candidate.interval.definition.block()),
                            Some(candidate.value),
                            "fixed transition was selected as a root split candidate",
                        ));
                    };
                    Ok(RegionSplitCandidate {
                        value: candidate.value,
                        root: *root,
                        uses: uses.clone(),
                        pressure_points: pressure_points.into_iter().collect(),
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            self.pending.push(allocation_queue_item(&value)?);
            return Ok(JointAllocationOutcome::NeedsSplit(RegionSplitRequest {
                blocked_value: value.value,
                definition: value.interval.definition,
                conflicts,
                candidates,
                preferred_frontier,
            }));
        }

        if !self.deferred.is_empty() {
            self.matrix.verify().map_err(JointAllocationError::union)?;
            return Ok(JointAllocationOutcome::DeferredRound);
        }

        self.problem
            .coalesce_affinities(&mut self.matrix, &self.ranges, &mut self.assignments)?;
        let result = JointAllocation {
            assignments: self.assignments.clone(),
        };
        self.problem.verify(cfg, registers, &result)?;
        self.matrix.verify().map_err(JointAllocationError::union)?;
        Ok(JointAllocationOutcome::Complete(result))
    }
}

fn same_allocation_geometry(left: &AllocationValue, right: &AllocationValue) -> bool {
    left.id == right.id
        && left.value == right.value
        && left.class == right.class
        && left.spill_cost == right.spill_cost
        && left.preferred_register == right.preferred_register
        && left.allowed_registers == right.allowed_registers
        && left.interval.definition.block() == right.interval.definition.block()
        && left.interval.definition.slot() == right.interval.definition.slot()
        && matches!(
            (left.interval.definition, right.interval.definition),
            (DefinitionSite::Phi { .. }, DefinitionSite::Phi { .. })
                | (
                    DefinitionSite::Instruction { .. },
                    DefinitionSite::Instruction { .. }
                )
        )
        && left.interval.segments == right.interval.segments
        && left.interval.uses.len() == right.interval.uses.len()
}

fn allocation_value_summary(value: &AllocationValue) -> String {
    format!(
        "{} class={:?} spill_cost={:?} def={:?} uses={} segments={} allowed={:?}",
        value.value,
        value.class,
        value.spill_cost,
        value.interval.definition,
        value.interval.uses.len(),
        value.interval.segments.len(),
        value.allowed_registers
    )
}

fn session_allocation_value(
    expanded: &ExpandedAllocationProblem,
    constraints: &AllocationConstraintModel,
    ownership: &RegionOwnershipIndex,
    home_plans: &[RootHomePlan],
    value: VReg,
    registers: &[PhysReg],
) -> Result<Option<AllocationValue>, JointAllocationError> {
    let Some(interval) = expanded
        .intervals
        .intervals
        .get(value.0 as usize)
        .and_then(Option::as_ref)
    else {
        return Ok(None);
    };
    if interval.value != value || interval.segments.is_empty() {
        return Err(JointAllocationError::new(
            "JOINT_ALLOC.SESSION_INTERVAL_IDENTITY",
            Some(interval.definition.block()),
            Some(value),
            "changed sparse interval has a malformed stable identity",
        ));
    }
    let (class, spill_cost, preferred_register) = if let Some(owner) = ownership.owner(value) {
        if owner.uses.is_empty() || owner.uses.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(JointAllocationError::new(
                "JOINT_ALLOC.SESSION_REGION_USES",
                Some(interval.definition.block()),
                Some(value),
                "changed register region has no strictly ordered root uses",
            ));
        }
        let root = expanded.roots.get(owner.root.0 as usize).ok_or_else(|| {
            JointAllocationError::new(
                "JOINT_ALLOC.SESSION_ROOT_RANGE",
                Some(interval.definition.block()),
                Some(value),
                "changed register region references a missing root",
            )
        })?;
        for &use_id in &owner.uses {
            let use_ = root.uses.get(use_id.0 as usize).ok_or_else(|| {
                JointAllocationError::new(
                    "JOINT_ALLOC.SESSION_USE_RANGE",
                    Some(interval.definition.block()),
                    Some(value),
                    "changed register region references a missing root use",
                )
            })?;
            let preference = match use_.source {
                ExpandedUseSource::OriginalRegister { preferred_register }
                | ExpandedUseSource::RegisterRegion {
                    preferred_register, ..
                } => preferred_register,
                ExpandedUseSource::Materialized(_) | ExpandedUseSource::Edge(_) => {
                    return Err(JointAllocationError::new(
                        "JOINT_ALLOC.SESSION_REGION_SOURCE",
                        Some(use_.site.block()),
                        Some(value),
                        "indexed register region now references a non-register source",
                    ));
                }
            };
            if use_.value != value
                || preference != owner.preferred_register
                || !interval.contains_use_coordinate(use_.site)
            {
                return Err(JointAllocationError::new(
                    "JOINT_ALLOC.SESSION_REGION_IDENTITY",
                    Some(use_.site.block()),
                    Some(value),
                    format!(
                        "changed register region and exact live interval disagree: use_value={:?}, expected_value={value:?}, use_preference={preference:?}, expected_preference={:?}, use_site={:?}, interval_uses={:?}",
                        use_.value, owner.preferred_register, use_.site, interval.uses,
                    ),
                ));
            }
        }
        let home_plan = home_plans.get(owner.root.0 as usize).ok_or_else(|| {
            JointAllocationError::new(
                "JOINT_ALLOC.SESSION_HOME_COST_ROOT",
                Some(interval.definition.block()),
                Some(value),
                "changed register region has no function-lifetime home-cost row",
            )
        })?;
        let stack_exists = expanded
            .stack_homes
            .iter()
            .any(|home| home.root == owner.root && home.kind == ExpandedStackHomeKind::Root);
        let spill_cost = home_plan
            .spill_cost(&owner.uses, stack_exists)
            .map_err(JointAllocationError::home)?;
        (
            AllocationValueClass::Region {
                root: owner.root,
                uses: owner.uses.clone(),
            },
            Some(spill_cost),
            owner.preferred_register,
        )
    } else {
        (AllocationValueClass::Fixed, None, None)
    };
    let result = AllocationValue {
        id: AllocationBundleId(value.0),
        value,
        interval: interval.clone(),
        class,
        spill_cost,
        live_length: cached_program_order_length(expanded, interval)?,
        preferred_register,
        allowed_registers: constraints.allowed(value).ok_or_else(|| {
            JointAllocationError::new(
                "JOINT_ALLOC.SESSION_CONSTRAINT_VALUE",
                Some(interval.definition.block()),
                Some(value),
                "changed machine value has no target-register constraint row",
            )
        })?,
    };
    validate_allocatable_value(&result, registers)?;
    Ok(Some(result))
}

fn cached_program_order_length(
    expanded: &ExpandedAllocationProblem,
    interval: &LiveInterval,
) -> Result<u64, JointAllocationError> {
    expanded
        .incremental_liveness
        .program_order_length(interval.value)
        .ok_or_else(|| {
            JointAllocationError::new(
                "JOINT_ALLOC.LIVE_LENGTH_CACHE",
                Some(interval.definition.block()),
                Some(interval.value),
                "active allocation interval has no persistent program-order length",
            )
        })
}

fn interval_program_order_length(
    interval: &LiveInterval,
    intervals: &super::live_interval::LiveIntervals,
    cfg: &NormalizedCfg,
) -> Result<u64, JointAllocationError> {
    let mut total = 0_u64;
    for segment in &interval.segments {
        let block = cfg
            .block_index
            .get(&segment.block)
            .copied()
            .ok_or_else(|| {
                JointAllocationError::new(
                    "JOINT_ALLOC.LIVE_LENGTH_BLOCK",
                    Some(segment.block),
                    Some(interval.value),
                    "live segment is outside the normalized CFG",
                )
            })?;
        let length = intervals.block_slots[block]
            .program_order_distance(segment.start, segment.end)
            .ok_or_else(|| {
                JointAllocationError::new(
                    "JOINT_ALLOC.LIVE_LENGTH",
                    Some(segment.block),
                    Some(interval.value),
                    "live segment endpoints are outside emitted instruction order",
                )
            })?;
        total = total.checked_add(length).ok_or_else(|| {
            JointAllocationError::new(
                "JOINT_ALLOC.LIVE_LENGTH",
                Some(segment.block),
                Some(interval.value),
                "allocation range length exceeds u64",
            )
        })?;
    }
    if total == 0 {
        return Err(JointAllocationError::new(
            "JOINT_ALLOC.LIVE_LENGTH",
            Some(interval.definition.block()),
            Some(interval.value),
            "active allocation value has an empty sparse range",
        ));
    }
    Ok(total)
}

fn validate_allocatable_value(
    value: &AllocationValue,
    registers: &[PhysReg],
) -> Result<(), JointAllocationError> {
    if matches!(value.class, AllocationValueClass::Region { .. }) != value.spill_cost.is_some() {
        return Err(JointAllocationError::new(
            "JOINT_ALLOC.SPILL_COST_CLASS",
            Some(value.interval.definition.block()),
            Some(value.value),
            "splittable region and exact spill-cost ownership disagree",
        ));
    }
    if value
        .preferred_register
        .is_some_and(|register| !registers.contains(&register))
    {
        return Err(JointAllocationError::new(
            "JOINT_ALLOC.STALE_PREFERENCE",
            Some(value.interval.definition.block()),
            Some(value.value),
            "expanded register preference is outside the target register set",
        ));
    }
    if value.allowed_registers.is_empty()
        || !registers
            .iter()
            .any(|register| value.allowed_registers.contains(*register))
    {
        return Err(JointAllocationError::new(
            "JOINT_ALLOC.EMPTY_ALLOWED_REGISTERS",
            Some(value.interval.definition.block()),
            Some(value.value),
            "machine value has no allowed register in the target set",
        ));
    }
    Ok(())
}

fn definition_order(
    values: &[AllocationValue],
    cfg: &NormalizedCfg,
) -> Result<BTreeSet<DefinitionOrderEntry>, JointAllocationError> {
    let rank = dominator_rank(cfg)?;
    values
        .iter()
        .map(|value| {
            Ok(DefinitionOrderEntry {
                key: definition_key(value, cfg, &rank)?,
                id: value.id,
            })
        })
        .collect()
}

fn dominator_rank(cfg: &NormalizedCfg) -> Result<Vec<usize>, JointAllocationError> {
    let block_count = cfg.idom.len();
    if block_count == 0 || cfg.block_index.len() != block_count {
        return Err(JointAllocationError::new(
            "JOINT_ALLOC.DOMINATOR_TREE",
            None,
            None,
            "joint allocation requires a complete non-empty dominator tree",
        ));
    }
    let mut children = vec![Vec::new(); block_count];
    for block in 1..block_count {
        let parent = cfg.idom[block].ok_or_else(|| {
            JointAllocationError::new(
                "JOINT_ALLOC.DOMINATOR_TREE",
                None,
                None,
                format!("reachable block {block} has no immediate dominator"),
            )
        })?;
        if parent >= block_count {
            return Err(JointAllocationError::new(
                "JOINT_ALLOC.DOMINATOR_TREE",
                None,
                None,
                format!("block {block} has out-of-range dominator {parent}"),
            ));
        }
        children[parent].push(block);
    }
    let mut rank = vec![usize::MAX; block_count];
    let mut next_rank = 0usize;
    let mut work = vec![0usize];
    while let Some(block) = work.pop() {
        if rank[block] != usize::MAX {
            return Err(JointAllocationError::new(
                "JOINT_ALLOC.DOMINATOR_TREE",
                None,
                None,
                "dominator tree contains a cycle or duplicate child",
            ));
        }
        rank[block] = next_rank;
        next_rank = next_rank.checked_add(1).ok_or_else(|| {
            JointAllocationError::new(
                "JOINT_ALLOC.DOMINATOR_TREE",
                None,
                None,
                "dominator preorder exceeds usize",
            )
        })?;
        work.extend(children[block].iter().rev().copied());
    }
    if next_rank != block_count {
        return Err(JointAllocationError::new(
            "JOINT_ALLOC.DOMINATOR_TREE",
            None,
            None,
            "dominator tree does not reach every CFG block",
        ));
    }
    Ok(rank)
}

fn definition_key(
    value: &AllocationValue,
    cfg: &NormalizedCfg,
    rank: &[usize],
) -> Result<(usize, SlotIndex, VReg), JointAllocationError> {
    let block = cfg
        .block_index
        .get(&value.interval.definition.block())
        .copied()
        .ok_or_else(|| {
            JointAllocationError::new(
                "JOINT_ALLOC.DEFINITION_BLOCK",
                Some(value.interval.definition.block()),
                Some(value.value),
                "allocation definition is outside the normalized CFG",
            )
        })?;
    let block_rank = rank.get(block).copied().ok_or_else(|| {
        JointAllocationError::new(
            "JOINT_ALLOC.DOMINATOR_RANK",
            Some(value.interval.definition.block()),
            Some(value.value),
            "allocation definition has no dominator preorder rank",
        )
    })?;
    Ok((block_rank, value.interval.definition.slot(), value.value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::native::mir::{
        BaseReg, MBlock, MFunction, MInst, OpSize, SpillDesc, VRegAllocator,
    };

    use super::super::allocation_expand::{expand, expand_unallocated};
    use super::super::home_graph::{self, HomeGraph};
    use super::super::interval_allocator::allocate_roots;
    use super::super::live_interval;

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

    fn model(function: &mut MFunction) -> (NormalizedCfg, HomeGraph) {
        let cfg = super::super::cfg::normalize(function).unwrap();
        let graph = home_graph::build(function, &cfg).unwrap();
        (cfg, graph)
    }

    fn fixed_problem(
        function: &MFunction,
        cfg: &NormalizedCfg,
        registers: &[PhysReg],
    ) -> JointAllocationProblem {
        let intervals = live_interval::analyze(function, cfg).unwrap();
        let value_count = function.vregs.count();
        let mut values = Vec::new();
        let mut value_rows = vec![None; value_count as usize];
        for (value_index, interval) in intervals.intervals.iter().enumerate() {
            let Some(interval) = interval else {
                continue;
            };
            let id = AllocationBundleId(interval.value.0);
            let live_length = interval_program_order_length(interval, &intervals, cfg).unwrap();
            value_rows[value_index] = Some(values.len());
            values.push(AllocationValue {
                id,
                value: interval.value,
                interval: interval.clone(),
                class: AllocationValueClass::Fixed,
                spill_cost: None,
                live_length,
                preferred_register: None,
                allowed_registers: RegisterMask::from_registers(registers),
            });
        }
        let definition_order = definition_order(&values, cfg).unwrap();
        JointAllocationProblem {
            value_count,
            values,
            value_rows,
            definition_order,
            target_registers: registers.to_vec(),
            affinities: Vec::new(),
            affinity_index: AffinityIndex::build(value_count, &[]).unwrap(),
            fixed_reservations: Vec::new(),
        }
    }

    #[test]
    fn affinity_csr_scores_only_incident_edges_and_counts_pair_edge_once() {
        let mut function = function(4, vec![MInst::Return]);
        let (cfg, _) = model(&mut function);
        let registers = [PhysReg::RAX, PhysReg::RDX];
        let mut problem = fixed_problem(&function, &cfg, &registers);
        problem.affinities = vec![
            WeightedAffinity {
                left: VReg(0),
                right: VReg(1),
                weight: 7,
            },
            WeightedAffinity {
                left: VReg(0),
                right: VReg(2),
                weight: 3,
            },
            WeightedAffinity {
                left: VReg(1),
                right: VReg(3),
                weight: 5,
            },
        ];
        problem.affinity_index =
            AffinityIndex::build(problem.value_count, &problem.affinities).unwrap();
        let assignments = [
            Some(PhysReg::RAX),
            Some(PhysReg::RAX),
            Some(PhysReg::RDX),
            Some(PhysReg::RAX),
        ];

        assert_eq!(
            problem.assigned_affinity_score(VReg(0), PhysReg::RAX, &assignments),
            7
        );
        assert_eq!(
            problem.assigned_affinity_score(VReg(0), PhysReg::RDX, &assignments),
            3
        );
        assert_eq!(
            problem.incident_affinity_score(VReg(0), VReg(1), None, &assignments),
            12
        );
        assert_eq!(
            problem.incident_affinity_score(VReg(0), VReg(1), Some(PhysReg::RDX), &assignments,),
            10
        );
    }

    #[test]
    fn live_length_priority_change_preserves_physical_allocation_geometry() {
        let mut function = function(
            1,
            vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: 1,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 0,
                    src: VReg(0),
                    size: OpSize::S64,
                },
                MInst::Return,
            ],
        );
        let (cfg, _) = model(&mut function);
        let registers = [PhysReg::RAX];
        let problem = fixed_problem(&function, &cfg, &registers);
        let original = problem.value(VReg(0)).unwrap();
        let mut reprioritized = original.clone();
        reprioritized.live_length += 3;

        assert!(same_allocation_geometry(original, &reprioritized));
    }

    #[test]
    fn deferred_region_uses_lazy_worklist_deletion_at_the_round_boundary() {
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
        let (cfg, graph) = model(&mut function);
        let expanded = expand_unallocated(&function, &cfg, &graph).unwrap();
        let problem = JointAllocationProblem::build(&expanded, &cfg, &graph, &registers).unwrap();
        let mut session = JointAllocationSession::new(problem, &cfg, &registers).unwrap();

        let JointAllocationOutcome::NeedsSplit(request) =
            session.allocate(&cfg, &registers).unwrap()
        else {
            panic!("three overlapping roots should require one deferred region");
        };
        let deferred = request.preferred_frontier.unwrap().0;
        let pending_before = session.pending.len();
        assert!(
            session
                .pending
                .iter()
                .any(|item| item.id == AllocationBundleId(deferred.0))
        );
        session.defer_split(deferred).unwrap();
        assert_eq!(session.pending.len(), pending_before);
        assert!(matches!(
            session.allocate(&cfg, &registers).unwrap(),
            JointAllocationOutcome::DeferredRound
        ));
    }

    #[test]
    fn fixed_reservation_requests_a_split_at_the_exact_barrier() {
        let mut function = function(
            1,
            vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: 7,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 0,
                    src: VReg(0),
                    size: OpSize::S64,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 8,
                    src: VReg(0),
                    size: OpSize::S64,
                },
                MInst::Return,
            ],
        );
        let (cfg, _) = model(&mut function);
        let intervals = live_interval::analyze(&function, &cfg).unwrap();
        let slots = &intervals.block_slots[0];
        let reservation = FixedRegisterReservation {
            register: PhysReg::RAX,
            segment: super::super::live_interval::LiveSegment {
                block: BlockId(0),
                start: slots.instruction_clobber(1).unwrap(),
                end: slots.instruction_def(1).unwrap(),
            },
        };
        let mut problem = fixed_problem(&function, &cfg, &[PhysReg::RAX]);
        let value = problem
            .values
            .iter_mut()
            .find(|value| value.value == VReg(0))
            .unwrap();
        value.class = AllocationValueClass::Region {
            root: LiveBundleId(0),
            uses: vec![BundleUseId(0), BundleUseId(1)],
        };
        value.spill_cost = Some(2);
        value.preferred_register = Some(PhysReg::RAX);
        problem.fixed_reservations = vec![reservation];

        let JointAllocationOutcome::NeedsSplit(request) =
            problem.allocate(&cfg, &[PhysReg::RAX]).unwrap()
        else {
            panic!("the sole register is reserved inside the live range");
        };
        assert_ne!(request.definition.slot(), reservation.segment.start);
        assert_eq!(request.conflicts.len(), 1);
        assert!(request.conflicts[0].values.is_empty());
        assert_eq!(
            request.conflicts[0].cuts,
            vec![OccupancyCut {
                segment: 0,
                start: reservation.segment.start,
                end: reservation.segment.end,
                owner: OccupancyOwner::Fixed(super::super::interval_union::FixedReservationId(0)),
            }]
        );
        assert_eq!(
            request.candidates[0].pressure_points,
            vec![AllocationPressurePoint {
                block: BlockId(0),
                slot: reservation.segment.start,
            }]
        );
    }

    #[test]
    fn every_machine_definition_is_reallocated_and_preferences_are_not_assignments() {
        let instructions = vec![
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
            MInst::Return,
        ];
        let mut function = function(3, instructions);
        let (cfg, graph) = model(&mut function);
        let registers = [PhysReg::RAX, PhysReg::RDX];
        let plan = allocate_roots(&graph, &cfg, &registers).unwrap();
        let mut expanded = expand(&function, &cfg, &graph, &plan, &registers).unwrap();
        for root in &mut expanded.roots {
            for use_ in &mut root.uses {
                if matches!(use_.value, VReg(0) | VReg(1)) {
                    let ExpandedUseSource::OriginalRegister { preferred_register } =
                        &mut use_.source
                    else {
                        panic!("both add inputs should remain complete register roots");
                    };
                    *preferred_register = Some(PhysReg::RAX);
                }
            }
        }

        let problem = JointAllocationProblem::build(&expanded, &cfg, &graph, &registers).unwrap();
        assert_eq!(
            problem.values.len(),
            expanded
                .intervals
                .intervals
                .iter()
                .filter(|interval| interval.is_some())
                .count()
        );
        assert!(problem.values.iter().any(|value| {
            value.value == VReg(2) && matches!(value.class, AllocationValueClass::Fixed)
        }));

        let JointAllocationOutcome::Complete(allocation) =
            problem.allocate(&cfg, &registers).unwrap()
        else {
            panic!("two-register strict SSA should color without another split");
        };
        assert_ne!(allocation.assignments[0], allocation.assignments[1]);
        assert_eq!(
            BTreeSet::from([
                allocation.assignments[0].unwrap(),
                allocation.assignments[1].unwrap(),
            ]),
            BTreeSet::from([PhysReg::RAX, PhysReg::RDX])
        );
        problem.verify(&cfg, &registers, &allocation).unwrap();
    }

    #[test]
    fn movable_pressure_requests_one_blocked_definition_frontier_without_cut_materialization() {
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
        let mut function = function(12, instructions);
        let (cfg, graph) = model(&mut function);
        let registers = [PhysReg::RAX, PhysReg::RDX];
        let plan = allocate_roots(&graph, &cfg, &registers).unwrap();
        let expanded = expand(&function, &cfg, &graph, &plan, &registers).unwrap();
        let problem = JointAllocationProblem::build(&expanded, &cfg, &graph, &registers).unwrap();

        let JointAllocationOutcome::NeedsSplit(request) =
            problem.allocate(&cfg, &registers).unwrap()
        else {
            panic!("the explicit state transition should expose retained-root pressure");
        };
        assert!(request.conflicts.is_empty());
        let [candidate] = request.candidates.as_slice() else {
            panic!("movable pressure should request exactly the blocked region");
        };
        assert_eq!(candidate.value, request.blocked_value);
        let frontier = AllocationPressurePoint {
            block: request.definition.block(),
            slot: request.definition.slot(),
        };
        assert_eq!(candidate.pressure_points, vec![frontier]);
        assert_eq!(
            request.preferred_frontier,
            Some((candidate.value, frontier))
        );
    }

    #[test]
    fn pressure_with_only_fixed_machine_ranges_is_a_producer_error() {
        let instructions = vec![
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
            MInst::Return,
        ];
        let mut function = function(3, instructions);
        let cfg = super::super::cfg::normalize(&mut function).unwrap();
        let registers = [PhysReg::RAX];
        let problem = fixed_problem(&function, &cfg, &registers);

        let error = problem.allocate(&cfg, &registers).unwrap_err();
        assert_eq!(error.rule, "JOINT_ALLOC.UNSPLITTABLE_PRESSURE");
    }

    #[test]
    fn physical_bundle_identity_does_not_shift_across_a_dead_vreg_hole() {
        let instructions = vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 7,
            },
            MInst::Mov {
                dst: VReg(2),
                src: VReg(0),
            },
            MInst::Return,
        ];
        let registers = [PhysReg::RAX, PhysReg::RDX];
        let mut function = function(3, instructions);
        let (cfg, _) = model(&mut function);
        let problem = fixed_problem(&function, &cfg, &registers);

        assert_eq!(problem.value(VReg(1)), None);
        assert_eq!(
            problem.value(VReg(2)).map(|value| value.id),
            Some(AllocationBundleId(2))
        );
        let JointAllocationOutcome::Complete(allocation) =
            problem.allocate(&cfg, &registers).unwrap()
        else {
            panic!("two fixed values must fit in two registers");
        };
        assert_eq!(allocation.assignments[VReg(1).0 as usize], None);
        assert!(allocation.assignments[VReg(2).0 as usize].is_some());
    }

    #[test]
    fn persistent_session_retains_unchanged_matrix_memberships() {
        let registers = [PhysReg::RAX, PhysReg::RDX];
        let mut before = function(
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
        let (before_cfg, _) = model(&mut before);
        let before_problem = fixed_problem(&before, &before_cfg, &registers);
        let mut session =
            JointAllocationSession::new(before_problem, &before_cfg, &registers).unwrap();
        let JointAllocationOutcome::Complete(before_allocation) =
            session.allocate(&before_cfg, &registers).unwrap()
        else {
            panic!("the initial fixed problem must fit");
        };

        let mut after = function(
            3,
            vec![
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
                MInst::Return,
            ],
        );
        let (after_cfg, _) = model(&mut after);
        let after_problem = fixed_problem(&after, &after_cfg, &registers);
        session.update(after_problem, &registers).unwrap();

        for value in [VReg(0), VReg(1)] {
            assert_eq!(
                session.assignments[value.0 as usize],
                before_allocation.assignments[value.0 as usize]
            );
            assert_eq!(
                session.matrix.register(AllocationBundleId(value.0)),
                before_allocation.assignments[value.0 as usize]
            );
        }
        assert_eq!(
            session
                .pending
                .iter()
                .map(|item| item.id)
                .collect::<Vec<_>>(),
            vec![AllocationBundleId(2)]
        );
        let JointAllocationOutcome::Complete(after_allocation) =
            session.allocate(&after_cfg, &registers).unwrap()
        else {
            panic!("the extended fixed problem must fit");
        };
        assert!(after_allocation.assignments[VReg(2).0 as usize].is_some());
    }

    #[test]
    fn mutually_exclusive_cfg_ranges_share_one_register_in_joint_coloring() {
        let mut values = VRegAllocator::new();
        for _ in 0..3 {
            values.alloc();
        }
        let mut function = MFunction::new(values, vec![SpillDesc::transient(); 3]);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: VReg(0),
            value: 1,
        });
        entry.push(MInst::Branch {
            cond: VReg(0),
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });
        let mut left = MBlock::new(BlockId(1));
        left.push(MInst::LoadImm {
            dst: VReg(1),
            value: 11,
        });
        left.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 0,
            src: VReg(1),
            size: OpSize::S64,
        });
        left.push(MInst::Jump { target: BlockId(3) });
        let mut right = MBlock::new(BlockId(2));
        right.push(MInst::LoadImm {
            dst: VReg(2),
            value: 13,
        });
        right.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 8,
            src: VReg(2),
            size: OpSize::S64,
        });
        right.push(MInst::Jump { target: BlockId(3) });
        let mut merge = MBlock::new(BlockId(3));
        merge.push(MInst::Return);
        function.blocks = vec![entry, left, right, merge];
        let cfg = super::super::cfg::normalize(&mut function).unwrap();
        let registers = [PhysReg::RAX];
        let problem = fixed_problem(&function, &cfg, &registers);

        let JointAllocationOutcome::Complete(allocation) =
            problem.allocate(&cfg, &registers).unwrap()
        else {
            panic!("branch-exclusive ranges must not be linearized into interference");
        };
        assert!(
            allocation
                .assignments
                .iter()
                .flatten()
                .all(|register| *register == PhysReg::RAX)
        );
    }
}
