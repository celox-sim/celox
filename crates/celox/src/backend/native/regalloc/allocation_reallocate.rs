//! Joint physical allocation model for expanded original and synthetic values.
//!
//! Home expansion invalidates every physical assignment made before synthetic
//! stores, reloads, and recipe nodes existed. This module rebuilds one sparse
//! allocation problem from the expanded IR. Existing register numbers are
//! affinities only. A failed coloring returns exact resident conflicts and the
//! root regions which may be split; it never invents a scratch register or
//! silently finalizes a value to memory.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
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
use super::interval_union::{
    AllocationBundleId, IntervalUnionError, LiveIntervalMatrix, SparseRange,
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RegionSplitCandidate {
    pub value: VReg,
    pub root: LiveBundleId,
    pub uses: Vec<BundleUseId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RegionSplitRequest {
    pub blocked_value: VReg,
    pub definition: DefinitionSite,
    pub conflicts: Vec<RegisterConflicts>,
    pub candidates: Vec<RegionSplitCandidate>,
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
    pending: VecDeque<AllocationBundleId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum JointAllocationOutcome {
    Complete(JointAllocation),
    NeedsSplit(RegionSplitRequest),
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
    preferred_register: PhysReg,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct IndexedRegion {
    root: LiveBundleId,
    uses: Vec<BundleUseId>,
    preferred_register: PhysReg,
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
    ) -> Result<Self, JointAllocationError> {
        Self::build_from_constraints(expanded, cfg, registers, constraints)
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

        Self::build_from_constraints(expanded, cfg, registers, &constraints)
    }

    fn build_from_constraints(
        expanded: &ExpandedAllocationProblem,
        cfg: &NormalizedCfg,
        registers: &[PhysReg],
        constraints: &AllocationConstraintModel,
    ) -> Result<Self, JointAllocationError> {
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
                                .resolve_stack_store_use_site(
                                    instruction,
                                    home.id,
                                    value,
                                    &expanded.intervals,
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
                        .resolve_stack_store_use_site(
                            instruction,
                            home.id,
                            value,
                            &expanded.intervals,
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
            let (class, preferred_register) = if let Some(mut region) = regions.remove(&value) {
                region.uses.sort_unstable();
                region.sites.sort_unstable();
                region.sites.dedup();
                let mut owned_sites = region.sites.clone();
                owned_sites.extend(fixed_region_uses.get(&value).into_iter().flatten().copied());
                owned_sites.sort_unstable();
                owned_sites.dedup();
                if region.uses.is_empty()
                    || region.uses.windows(2).any(|pair| pair[0] >= pair[1])
                    || interval.uses != owned_sites
                {
                    return Err(JointAllocationError::new(
                        "JOINT_ALLOC.REGION_USES",
                        Some(interval.definition.block()),
                        Some(value),
                        "register region plus its identified fixed stack-store use do not own the exact expanded interval uses",
                    ));
                }
                (
                    AllocationValueClass::Region {
                        root: region.root,
                        uses: region.uses,
                    },
                    Some(region.preferred_register),
                )
            } else {
                (AllocationValueClass::Fixed, None)
            };
            let id = AllocationBundleId(value.0);
            value_rows[value.0 as usize] = Some(values.len());
            values.push(AllocationValue {
                id,
                value,
                interval: interval.clone(),
                class,
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
        Ok(Self {
            value_count: expanded.ir.value_count(),
            values,
            value_rows,
            definition_order,
            target_registers: registers.to_vec(),
            affinities: constraints.affinities.clone(),
        })
    }

    pub(super) fn value(&self, value: VReg) -> Option<&AllocationValue> {
        let row = self.value_rows.get(value.0 as usize).copied().flatten()?;
        self.values.get(row)
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
        self.affinities
            .iter()
            .filter_map(|affinity| {
                let other = if affinity.left == value {
                    affinity.right
                } else if affinity.right == value {
                    affinity.left
                } else {
                    return None;
                };
                (assignments.get(other.0 as usize).copied().flatten() == Some(register))
                    .then_some(u64::from(affinity.weight))
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
                .collect::<Vec<_>>();
            candidates.sort_by(
                |(left_order, left_candidate), (right_order, right_candidate)| {
                    let left_score = self.incident_affinity_score(
                        left.value,
                        right.value,
                        Some(*left_candidate),
                        assignments,
                    );
                    let right_score = self.incident_affinity_score(
                        left.value,
                        right.value,
                        Some(*right_candidate),
                        assignments,
                    );
                    right_score
                        .cmp(&left_score)
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
            for (_, candidate) in candidates {
                let after = self.incident_affinity_score(
                    left.value,
                    right.value,
                    Some(candidate),
                    assignments,
                );
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
        self.affinities
            .iter()
            .filter(|affinity| {
                matches!(affinity.left, value if value == left || value == right)
                    || matches!(affinity.right, value if value == left || value == right)
            })
            .filter(|affinity| assigned(affinity.left) == assigned(affinity.right))
            .map(|affinity| u64::from(affinity.weight))
            .sum()
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

        let mut grouped = BTreeMap::<VReg, (PhysReg, Vec<BundleUseId>)>::new();
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
        let matrix =
            LiveIntervalMatrix::new(cfg, registers).map_err(JointAllocationError::union)?;
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
        let pending = problem
            .definition_order
            .iter()
            .map(|entry| entry.id)
            .collect();
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
        let rebuilt = JointAllocationProblem::build_session_with_constraints(
            expanded,
            cfg,
            registers,
            constraints.model(),
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
        changed_root: LiveBundleId,
    ) -> Result<(), JointAllocationError> {
        if registers != self.problem.target_registers {
            return Err(JointAllocationError::new(
                "JOINT_ALLOC.TARGET_REGISTER_SET",
                None,
                None,
                "incremental joint update uses a different physical register set",
            ));
        }
        let constraint_changes = self
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
            .update(expanded, cfg, graph, changed_blocks, changed_values)
            .map_err(JointAllocationError::constraints)?;
        let ownership_changes = self
            .ownership
            .as_mut()
            .ok_or_else(|| {
                JointAllocationError::new(
                    "JOINT_ALLOC.SESSION_REGION_STATE",
                    None,
                    None,
                    "persistent joint update has no region-ownership index",
                )
            })?
            .update_root(expanded, changed_root)?;
        let mut affected = changed_values.iter().copied().collect::<BTreeSet<_>>();
        affected.extend(constraint_changes);
        affected.extend(ownership_changes);

        let constraints = self.constraints.as_ref().unwrap().model();
        let ownership = self.ownership.as_ref().unwrap();
        let mut replacements = Vec::with_capacity(affected.len());
        for &value in &affected {
            replacements.push((
                value,
                session_allocation_value(expanded, constraints, ownership, value, registers)?,
            ));
        }
        let affinities = constraints.affinities.clone();
        self.apply_value_delta(
            expanded.ir.value_count(),
            replacements,
            affinities,
            cfg,
            registers,
        )
    }

    fn apply_value_delta(
        &mut self,
        value_count: u32,
        replacements: Vec<(VReg, Option<AllocationValue>)>,
        affinities: Vec<WeightedAffinity>,
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
        self.problem.affinities = affinities;

        let mut pending = self.pending.drain(..).collect::<Vec<_>>();
        pending.retain(|id| {
            self.problem.value(VReg(id.0)).is_some() && self.assignments[id.0 as usize].is_none()
        });
        pending.extend(changed.into_iter().filter_map(|value| {
            (self.problem.value(value).is_some() && self.assignments[value.0 as usize].is_none())
                .then_some(AllocationBundleId(value.0))
        }));
        pending.sort_unstable();
        pending.dedup();
        let mut ordered = pending
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
                Ok((definition_key(value, cfg, &self.definition_rank)?, id))
            })
            .collect::<Result<Vec<_>, JointAllocationError>>()?;
        ordered.sort_unstable_by_key(|(key, _)| *key);
        self.pending = ordered.into_iter().map(|(_, id)| id).collect();
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

        self.pending.clear();
        self.pending.extend(
            next.definition_order
                .iter()
                .map(|entry| entry.id)
                .filter(|id| self.assignments[id.0 as usize].is_none()),
        );
        self.problem = next;
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
        while let Some(id) = self.pending.pop_front() {
            let value = self.problem.bundle(id).cloned().ok_or_else(|| {
                JointAllocationError::new(
                    "JOINT_ALLOC.ORDER_RANGE",
                    None,
                    Some(VReg(id.0)),
                    "definition order references a missing allocation value",
                )
            })?;
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
                .collect::<Vec<_>>();
            register_order.sort_by(|(left_order, left), (right_order, right)| {
                let left_score =
                    self.problem
                        .assigned_affinity_score(value.value, *left, &self.assignments);
                let right_score =
                    self.problem
                        .assigned_affinity_score(value.value, *right, &self.assignments);
                right_score
                    .cmp(&left_score)
                    .then_with(|| {
                        (Some(*right) == value.preferred_register)
                            .cmp(&(Some(*left) == value.preferred_register))
                    })
                    .then_with(|| left_order.cmp(right_order))
            });
            let mut selected = None;
            for (_, register) in register_order {
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

            let mut conflicts = Vec::with_capacity(registers.len());
            let mut split_values = BTreeSet::<VReg>::new();
            if matches!(value.class, AllocationValueClass::Region { .. }) {
                split_values.insert(value.value);
            }
            for &register in registers
                .iter()
                .filter(|register| value.allowed_registers.contains(**register))
            {
                let residents = self
                    .matrix
                    .conflicts_validated(register, range.validated())
                    .map_err(JointAllocationError::union)?;
                let mut resident_values = Vec::with_capacity(residents.len());
                for resident in residents {
                    let resident = self.problem.bundle(resident).ok_or_else(|| {
                        JointAllocationError::new(
                            "JOINT_ALLOC.CONFLICT_RANGE",
                            Some(value.interval.definition.block()),
                            Some(value.value),
                            "interval matrix references a missing resident value",
                        )
                    })?;
                    resident_values.push(resident.value);
                    if matches!(resident.class, AllocationValueClass::Region { .. }) {
                        split_values.insert(resident.value);
                    }
                }
                conflicts.push(RegisterConflicts {
                    register,
                    values: resident_values,
                });
            }
            if split_values.is_empty() {
                return Err(JointAllocationError::new(
                    "JOINT_ALLOC.UNSPLITTABLE_PRESSURE",
                    Some(value.interval.definition.block()),
                    Some(value.value),
                    "explicit transition ranges exceed the physical register set",
                ));
            }
            let candidates = split_values
                .into_iter()
                .map(|candidate| {
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
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            self.pending.push_front(value.id);
            return Ok(JointAllocationOutcome::NeedsSplit(RegionSplitRequest {
                blocked_value: value.value,
                definition: value.interval.definition,
                conflicts,
                candidates,
            }));
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

fn session_allocation_value(
    expanded: &ExpandedAllocationProblem,
    constraints: &AllocationConstraintModel,
    ownership: &RegionOwnershipIndex,
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
    let (class, preferred_register) = if let Some(owner) = ownership.owner(value) {
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
                || !interval.uses.contains(&use_.site)
            {
                return Err(JointAllocationError::new(
                    "JOINT_ALLOC.SESSION_REGION_IDENTITY",
                    Some(use_.site.block()),
                    Some(value),
                    "changed register region and exact live interval disagree",
                ));
            }
        }
        (
            AllocationValueClass::Region {
                root: owner.root,
                uses: owner.uses.clone(),
            },
            Some(owner.preferred_register),
        )
    } else {
        (AllocationValueClass::Fixed, None)
    };
    let result = AllocationValue {
        id: AllocationBundleId(value.0),
        value,
        interval: interval.clone(),
        class,
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

fn validate_allocatable_value(
    value: &AllocationValue,
    registers: &[PhysReg],
) -> Result<(), JointAllocationError> {
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

    use super::super::allocation_expand::expand;
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
        for (value_index, interval) in intervals.intervals.into_iter().enumerate() {
            let Some(interval) = interval else {
                continue;
            };
            let id = AllocationBundleId(interval.value.0);
            value_rows[value_index] = Some(values.len());
            values.push(AllocationValue {
                id,
                value: interval.value,
                interval,
                class: AllocationValueClass::Fixed,
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
        }
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
                    *preferred_register = PhysReg::RAX;
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
        assert_eq!(allocation.assignments[0], Some(PhysReg::RAX));
        assert_eq!(allocation.assignments[1], Some(PhysReg::RDX));
        problem.verify(&cfg, &registers, &allocation).unwrap();
    }

    #[test]
    fn synthetic_pressure_returns_the_conflicting_root_regions_for_splitting() {
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
        assert!(!request.conflicts.is_empty());
        assert!(!request.candidates.is_empty());
        assert!(
            request
                .candidates
                .iter()
                .all(|candidate| matches!(candidate.value, VReg(1) | VReg(2)))
        );
        assert!(
            request
                .conflicts
                .iter()
                .flat_map(|conflict| &conflict.values)
                .any(|value| matches!(value, VReg(1) | VReg(2)))
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
            session.pending.iter().copied().collect::<Vec<_>>(),
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
