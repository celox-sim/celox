//! Concrete spill policy and allocation-IR edits.
//!
//! Split analysis owns only live-range topology.  This module owns every
//! decision that turns a logical spill remainder into stack, State-MemorySSA,
//! or pure-rematerialization operations.  Register children produced by a
//! reload are returned to ordinary greedy allocation; only exact one-use
//! transition products are terminal allocation ranges.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::backend::native::mir::{BlockId, Uses, VReg};

use super::allocation_expand::{
    self, ExpandedAllocationProblem, ExpandedEdgeLocation, ExpandedMachineEdgeUse,
    ExpandedMaterialization, ExpandedRegisterEntry, ExpandedRegisterRegion,
    ExpandedStackDefinition, ExpandedStackHome, ExpandedStackHomeKind, ExpandedUseSource,
    RegisterRegionId,
};
use super::allocation_ir::{StackHomeId, SyntheticOperation};
use super::assignment::PhysReg;
use super::home_graph::{
    BundleUseId, HomeGraph, HomeKind, LiveBundle, LiveBundleId, STACK_HOME_CREATION_COST,
    STACK_HOME_MATERIALIZATION_COST,
};
use super::interval_allocator::{HomeSelection, IntervalAllocationError, RootHomePlan};
use super::live_interval::{DefinitionSite, LiveInterval, UseSite};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SpillEntryKind {
    Materialized,
    RegisterRegion,
}

/// Topology handed from SplitEditor to the spiller.  It deliberately contains
/// no home choice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SpillEntry {
    pub entry: BundleUseId,
    pub uses: Vec<BundleUseId>,
    pub kind: SpillEntryKind,
    pub preferred_register: Option<PhysReg>,
}

/// Concrete home decisions for one logical spill remainder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SpillPlan {
    pub root: LiveBundleId,
    pub value: VReg,
    selections: BTreeMap<BundleUseId, HomeSelection>,
    pub total_cost: u64,
}

impl SpillPlan {
    pub(super) fn selection(&self, entry: BundleUseId) -> Option<&HomeSelection> {
        self.selections.get(&entry)
    }

    #[cfg(test)]
    pub(super) fn selections(&self) -> &BTreeMap<BundleUseId, HomeSelection> {
        &self.selections
    }
}

/// Allocation facts changed while one spill remainder is materialized.
#[derive(Debug, Default)]
pub(super) struct SpillEdit {
    pub liveness_blocks: BTreeSet<BlockId>,
    pub constraint_blocks: BTreeSet<BlockId>,
}

impl SpillEdit {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SpillerError {
    pub rule: &'static str,
    pub block: Option<BlockId>,
    pub value: Option<VReg>,
    pub root: Option<LiveBundleId>,
    pub message: String,
}

impl SpillerError {
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

    fn home(error: IntervalAllocationError, root: LiveBundleId) -> Self {
        Self::new(error.rule, error.block, None, Some(root), error.message)
    }

    fn expand(error: allocation_expand::AllocationExpandError) -> Self {
        Self::new(error.rule, error.block, None, error.root, error.message)
    }
}

impl fmt::Display for SpillerError {
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

impl std::error::Error for SpillerError {}

/// Function-lifetime spill cost model and concrete editor.
#[derive(Debug)]
pub(super) struct Spiller {
    home_plans: Vec<RootHomePlan>,
}

impl Spiller {
    pub(super) fn build(graph: &HomeGraph) -> Result<Self, SpillerError> {
        let mut home_plans = Vec::with_capacity(graph.bundles.len());
        for (row, root) in graph.bundles.iter().enumerate() {
            if root.id.0 as usize != row {
                return Err(SpillerError::new(
                    "SPILLER.ROOT_IDENTITY",
                    Some(root.definition.block()),
                    Some(root.origin),
                    Some(root.id),
                    "HomeGraph root differs from its function-lifetime spill row",
                ));
            }
            home_plans.push(
                RootHomePlan::build(graph, root)
                    .map_err(|error| SpillerError::home(error, root.id))?,
            );
        }
        Ok(Self { home_plans })
    }

    pub(super) fn home_plans(&self) -> &[RootHomePlan] {
        &self.home_plans
    }

    pub(super) fn home_plan(&self, root: LiveBundleId) -> Result<&RootHomePlan, SpillerError> {
        self.home_plans.get(root.0 as usize).ok_or_else(|| {
            SpillerError::new(
                "SPILLER.HOME_ROOT",
                None,
                None,
                Some(root),
                "spill root has no function-lifetime home-cost row",
            )
        })
    }

    pub(super) fn stack_home_exists(
        &self,
        expanded: &ExpandedAllocationProblem,
        root: LiveBundleId,
    ) -> Result<bool, SpillerError> {
        Ok(stack_home(expanded, root)?.is_some())
    }

    pub(super) fn deferred_state_home_exists(
        &self,
        expanded: &ExpandedAllocationProblem,
        root: LiveBundleId,
    ) -> bool {
        expanded.state_homes.iter().any(|home| home.root == root)
    }

    pub(super) fn plan(
        &self,
        expanded: &ExpandedAllocationProblem,
        root: LiveBundleId,
        value: VReg,
        entries: &[SpillEntry],
    ) -> Result<SpillPlan, SpillerError> {
        let mut ordered = entries.iter().map(|entry| entry.entry).collect::<Vec<_>>();
        ordered.sort_unstable();
        if ordered.is_empty() || ordered.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(SpillerError::new(
                "SPILLER.ENTRY_ORDER",
                None,
                Some(value),
                Some(root),
                "spill entries are empty or contain duplicate use identities",
            ));
        }
        let home_plan = self.home_plan(root)?;
        let stack_exists = self.stack_home_exists(expanded, root)?;
        let deferred_state_exists = self.deferred_state_home_exists(expanded, root);
        let partition = home_plan
            .partition_with_existing_homes(&ordered, stack_exists, deferred_state_exists)
            .map_err(|error| SpillerError::home(error, root))?;
        let mut selections = BTreeMap::new();
        let mut stack_creation_pending = !stack_exists;
        let mut deferred_state_creation_pending = !deferred_state_exists;
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
                    HomeKind::Rematerialize(_)
                    | HomeKind::State(_)
                    | HomeKind::DeferredState(_) => {
                        let materialization = piece
                            .selection
                            .materializations
                            .iter()
                            .find(|materialization| materialization.use_id == use_id)
                            .copied()
                            .ok_or_else(|| {
                                SpillerError::new(
                                    "SPILLER.HOME_COVERAGE",
                                    None,
                                    Some(value),
                                    Some(root),
                                    format!(
                                        "non-stack home has no exact recipe for entry {use_id:?}"
                                    ),
                                )
                            })?;
                        let creation_cost =
                            if matches!(piece.selection.kind, HomeKind::DeferredState(_))
                                && deferred_state_creation_pending
                            {
                                deferred_state_creation_pending = false;
                                1
                            } else {
                                0
                            };
                        HomeSelection {
                            kind: piece.selection.kind,
                            materializations: vec![materialization],
                            creation_cost,
                            materialization_cost: materialization.cost,
                        }
                    }
                    HomeKind::Register => {
                        return Err(SpillerError::new(
                            "SPILLER.HOME_CLASS",
                            None,
                            Some(value),
                            Some(root),
                            "home partition returned allocator-owned register residency",
                        ));
                    }
                };
                materialized_total = materialized_total
                    .checked_add(selection.total_cost())
                    .ok_or_else(|| {
                        SpillerError::new(
                            "SPILLER.HOME_COST_OVERFLOW",
                            None,
                            Some(value),
                            Some(root),
                            "entry home cost exceeds u64",
                        )
                    })?;
                if selections.insert(use_id, selection).is_some() {
                    return Err(SpillerError::new(
                        "SPILLER.HOME_COVERAGE",
                        None,
                        Some(value),
                        Some(root),
                        "home partition selected the same entry more than once",
                    ));
                }
            }
        }
        if selections.len() != ordered.len() || materialized_total != partition.total_cost {
            return Err(SpillerError::new(
                "SPILLER.HOME_COST_IDENTITY",
                None,
                Some(value),
                Some(root),
                format!(
                    "materialized entries cost {materialized_total}, indexed partition cost {}",
                    partition.total_cost
                ),
            ));
        }
        Ok(SpillPlan {
            root,
            value,
            selections,
            total_cost: partition.total_cost,
        })
    }

    pub(super) fn verify(
        &self,
        expanded: &ExpandedAllocationProblem,
        entries: &[SpillEntry],
        plan: &SpillPlan,
    ) -> Result<(), SpillerError> {
        let expected = self.plan(expanded, plan.root, plan.value, entries)?;
        if expected != *plan
            || plan
                .selections
                .values()
                .any(|selection| selection.kind == HomeKind::Register)
        {
            return Err(SpillerError::new(
                "SPILLER.PLAN_IDENTITY",
                None,
                Some(plan.value),
                Some(plan.root),
                "concrete spill plan differs from the exact HomeGraph partition",
            ));
        }
        Ok(())
    }

    /// Materialize one already-verified spill remainder.  No register is
    /// assigned here: persistent reload children only receive an affinity.
    pub(super) fn materialize(
        &self,
        expanded: &mut ExpandedAllocationProblem,
        graph: &HomeGraph,
        value: VReg,
        root: LiveBundleId,
        entries: &[SpillEntry],
        plan: &SpillPlan,
        replaces_complete_origin: bool,
    ) -> Result<SpillEdit, SpillerError> {
        if plan.root != root || plan.value != value {
            return Err(SpillerError::new(
                "SPILLER.PLAN_IDENTITY",
                None,
                Some(value),
                Some(root),
                "spill edit and concrete home plan have different identities",
            ));
        }
        self.verify(expanded, entries, plan)?;
        let graph_root = graph_root(graph, root)?;
        let needs_stack = plan
            .selections
            .values()
            .any(|selection| selection.kind == HomeKind::Stack);
        let existing_stack_home = stack_home(expanded, root)?;
        let mut edit = SpillEdit::default();
        let needs_deferred_state = plan
            .selections
            .values()
            .any(|selection| matches!(selection.kind, HomeKind::DeferredState(_)));
        if needs_deferred_state {
            let home = graph
                .deferred_homes
                .get(root.0 as usize)
                .copied()
                .flatten()
                .ok_or_else(|| {
                    SpillerError::new(
                        "SPILLER.DEFERRED_STATE_HOME",
                        Some(graph_root.definition.block()),
                        Some(value),
                        Some(root),
                        "spill plan selected a deferred home absent from its machine root",
                    )
                })?;
            allocation_expand::ensure_state_home(
                &mut expanded.ir,
                &mut expanded.state_homes,
                graph_root,
                home,
            )
            .map_err(SpillerError::expand)?;
            edit.record_block(graph_root.definition.block());
        }
        let stack_home = if needs_stack {
            if existing_stack_home.is_none() {
                edit.record_block(graph_root.definition.block());
            }
            Some(ensure_stack_home(
                expanded,
                graph_root,
                replaces_complete_origin,
            )?)
        } else {
            existing_stack_home
        };

        for entry in entries {
            let entry_use = expanded_use(expanded, root, entry.entry)?.clone();
            edit.record_use(entry_use.original_site);
            let selection = plan.selection(entry.entry).ok_or_else(|| {
                SpillerError::new(
                    "SPILLER.HOME_COVERAGE",
                    Some(entry_use.site.block()),
                    Some(value),
                    Some(root),
                    "spill plan omitted one topology entry",
                )
            })?;
            let lowered = allocation_expand::lower_use_materialization(
                &mut expanded.ir,
                graph,
                graph_root,
                value,
                entry.entry,
                entry_use.original_site,
                selection,
                stack_home,
                &mut expanded.stack_homes,
            )
            .map_err(SpillerError::expand)?;
            match entry.kind {
                SpillEntryKind::Materialized => {
                    if entry.uses.as_slice() != [entry.entry] {
                        return Err(SpillerError::new(
                            "SPILLER.SINGLETON_SHAPE",
                            Some(entry_use.site.block()),
                            Some(value),
                            Some(root),
                            "materialized spill entry owns more than its exact entry use",
                        ));
                    }
                    match lowered {
                        allocation_expand::LoweredUseMaterialization::Register(lowered) => {
                            rewrite_expanded_use(
                                expanded,
                                root,
                                entry.entry,
                                value,
                                lowered.value,
                                ExpandedUseSource::Materialized(lowered.source),
                                &mut edit,
                            )?;
                        }
                        allocation_expand::LoweredUseMaterialization::Edge(location) => {
                            let target = expanded
                                .roots
                                .get_mut(root.0 as usize)
                                .and_then(|root| root.uses.get_mut(entry.entry.0 as usize))
                                .ok_or_else(|| {
                                    SpillerError::new(
                                        "SPILLER.USE_RANGE",
                                        Some(entry_use.site.block()),
                                        Some(value),
                                        Some(root),
                                        "phi-edge home references a missing expanded use",
                                    )
                                })?;
                            if target.value != value {
                                return Err(SpillerError::new(
                                    "SPILLER.USE_OWNERSHIP",
                                    Some(target.site.block()),
                                    Some(target.value),
                                    Some(root),
                                    "phi-edge home no longer belongs to the spilled region",
                                ));
                            }
                            target.value = graph_root.origin;
                            target.source = ExpandedUseSource::Edge(location);
                        }
                    }
                }
                SpillEntryKind::RegisterRegion => {
                    let allocation_expand::LoweredUseMaterialization::Register(lowered) = lowered
                    else {
                        return Err(SpillerError::new(
                            "SPILLER.REGION_EDGE_ENTRY",
                            Some(entry_use.site.block()),
                            Some(value),
                            Some(root),
                            "multi-use reload region cannot start from a non-register phi-edge location",
                        ));
                    };
                    let region = fresh_region_id(expanded)?;
                    let region_row = expanded.register_regions.len();
                    if expanded.region_rows.insert(region, region_row).is_some() {
                        return Err(SpillerError::new(
                            "SPILLER.REGION_IDENTITY",
                            Some(entry_use.site.block()),
                            Some(lowered.value),
                            Some(root),
                            "new register region duplicates an existing stable identity",
                        ));
                    }
                    if expanded
                        .region_by_value
                        .insert(lowered.value, region)
                        .is_some()
                    {
                        return Err(SpillerError::new(
                            "SPILLER.REGION_VALUE_IDENTITY",
                            Some(entry_use.site.block()),
                            Some(lowered.value),
                            Some(root),
                            "new register region duplicates an active machine value",
                        ));
                    }
                    expanded.register_regions.push(ExpandedRegisterRegion {
                        id: region,
                        root,
                        value: lowered.value,
                        preferred_register: entry.preferred_register,
                        entry_use: Some(entry.entry),
                        entry: ExpandedRegisterEntry::Materialized(lowered.source),
                    });
                    for &use_id in &entry.uses {
                        rewrite_expanded_use(
                            expanded,
                            root,
                            use_id,
                            value,
                            lowered.value,
                            ExpandedUseSource::RegisterRegion {
                                region,
                                preferred_register: entry.preferred_register,
                            },
                            &mut edit,
                        )?;
                    }
                }
            }
        }
        Ok(edit)
    }

    /// Spill one exact allocation-IR machine representative conventionally.
    ///
    /// Unlike logical-root home selection, this operation owns every machine
    /// use in the live interval, including older split-copy and merge-phi
    /// transitions. The definition establishes one private stack home and a
    /// fresh one-use reload is inserted before each exact instruction use.
    /// Semantic phi sources instead retain the spill slot as an edge location;
    /// eagerly reloading every source before one edge would make all parallel
    /// phi operands simultaneously register-live. Reload products leave
    /// representative metadata and therefore enter the greedy queue at `Done`
    /// rather than recursively selecting another home.
    pub(super) fn materialize_machine_interval(
        &self,
        expanded: &mut ExpandedAllocationProblem,
        root: LiveBundleId,
        interval: &LiveInterval,
    ) -> Result<SpillEdit, SpillerError> {
        let value = interval.value;
        let current = expanded
            .intervals
            .intervals
            .get(value.0 as usize)
            .and_then(Option::as_ref)
            .ok_or_else(|| {
                SpillerError::new(
                    "SPILLER.MACHINE_INTERVAL",
                    Some(interval.definition.block()),
                    Some(value),
                    Some(root),
                    "generic spill source has no current exact live interval",
                )
            })?;
        if current != interval || interval.uses.is_empty() {
            return Err(SpillerError::new(
                "SPILLER.MACHINE_INTERVAL",
                Some(interval.definition.block()),
                Some(value),
                Some(root),
                "generic spill request is stale or has no machine uses",
            ));
        }
        let root_index = root.0 as usize;
        if !matches!(expanded.roots.get(root_index), Some(row) if row.id == root) {
            return Err(SpillerError::new(
                "SPILLER.ROOT_IDENTITY",
                Some(interval.definition.block()),
                Some(value),
                Some(root),
                "generic spill root differs from its dense expanded root row",
            ));
        }
        if expanded
            .stack_homes
            .iter()
            .any(|home| home.kind == ExpandedStackHomeKind::Machine { value })
            || expanded
                .stack_homes
                .iter()
                .enumerate()
                .any(|(row, home)| home.id.0 as usize != row)
        {
            return Err(SpillerError::new(
                "SPILLER.MACHINE_HOME_IDENTITY",
                Some(interval.definition.block()),
                Some(value),
                Some(root),
                "machine representative already has a home or stack-home IDs are not dense",
            ));
        }
        let id = StackHomeId(u32::try_from(expanded.stack_homes.len()).map_err(|_| {
            SpillerError::new(
                "SPILLER.STACK_HOME_ID_RANGE",
                Some(interval.definition.block()),
                Some(value),
                Some(root),
                "expanded stack-home count exceeds u32",
            )
        })?);
        let mut edit = SpillEdit::default();
        edit.record_block(interval.definition.block());
        let definition = match interval.definition {
            DefinitionSite::Phi { block, phi, .. } => {
                expanded
                    .ir
                    .assign_phi_definition_home(interval.definition, value, id)
                    .map_err(|error| {
                        SpillerError::new(
                            error.rule,
                            error.block,
                            error.values.first().copied(),
                            Some(root),
                            error.message,
                        )
                    })?;
                ExpandedStackDefinition::Phi {
                    block,
                    phi,
                    destination: value,
                }
            }
            DefinitionSite::Instruction { .. } => {
                let instruction = expanded
                    .ir
                    .insert_after_definition(
                        interval.definition,
                        SyntheticOperation::StackStore { home: id },
                        Uses::one(value),
                        false,
                    )
                    .map_err(|error| {
                        SpillerError::new(
                            error.rule,
                            error.block,
                            error.values.first().copied(),
                            Some(root),
                            error.message,
                        )
                    })?
                    .instruction;
                ExpandedStackDefinition::Store { instruction, value }
            }
        };
        expanded.stack_homes.push(ExpandedStackHome {
            id,
            root,
            definition,
            kind: ExpandedStackHomeKind::Machine { value },
        });

        let uses = interval.uses.iter().copied().collect::<Vec<_>>();
        for site in uses {
            let semantic_edge_use = if matches!(site, UseSite::PhiEdge { .. }) {
                let mut matching = expanded.roots[root_index]
                    .uses
                    .iter()
                    .enumerate()
                    .filter_map(|(index, use_)| {
                        (use_.value == value && use_.site == site).then_some(index)
                    });
                let first = matching.next();
                if matching.next().is_some() {
                    return Err(SpillerError::new(
                        "SPILLER.MACHINE_EDGE_IDENTITY",
                        Some(site.block()),
                        Some(value),
                        Some(root),
                        "one machine phi-edge use is owned by more than one semantic root use",
                    ));
                }
                first
            } else {
                None
            };
            if let Some(use_index) = semantic_edge_use {
                let semantic = expanded.roots[root_index].origin;
                expanded
                    .ir
                    .assign_phi_edge_home(site, value, semantic)
                    .map_err(|error| {
                        SpillerError::new(
                            error.rule,
                            error.block,
                            error.values.first().copied(),
                            Some(root),
                            error.message,
                        )
                    })?;
                let target = &mut expanded.roots[root_index].uses[use_index];
                target.value = semantic;
                target.source = ExpandedUseSource::Edge(ExpandedEdgeLocation::Stack { home: id });
                edit.record_use(site);
                continue;
            }
            if matches!(site, UseSite::PhiEdge { .. }) {
                expanded
                    .ir
                    .assign_machine_phi_edge_home(site, value)
                    .map_err(|error| {
                        SpillerError::new(
                            error.rule,
                            error.block,
                            error.values.first().copied(),
                            Some(root),
                            error.message,
                        )
                    })?;
                expanded.machine_edge_uses.push(ExpandedMachineEdgeUse {
                    root,
                    value,
                    site,
                    home: id,
                });
                edit.record_use(site);
                continue;
            }
            let reload = expanded
                .ir
                .insert_before_use(
                    site,
                    SyntheticOperation::StackReload { home: id },
                    Uses::none(),
                    true,
                )
                .map_err(|error| {
                    SpillerError::new(
                        error.rule,
                        error.block,
                        error.values.first().copied(),
                        Some(root),
                        error.message,
                    )
                })?;
            let replacement = reload.definition.ok_or_else(|| {
                SpillerError::new(
                    "SPILLER.MACHINE_RELOAD_DEFINITION",
                    Some(site.block()),
                    Some(value),
                    Some(root),
                    "generic machine reload did not define a value",
                )
            })?;
            expanded
                .ir
                .rewrite_use(site, value, replacement)
                .map_err(|error| {
                    SpillerError::new(
                        error.rule,
                        error.block,
                        error.values.first().copied(),
                        Some(root),
                        error.message,
                    )
                })?;
            edit.record_use(site);
            let root_row = &mut expanded.roots[root_index];
            for use_ in root_row
                .uses
                .iter_mut()
                .filter(|use_| use_.value == value && use_.site == site)
            {
                use_.value = replacement;
                use_.source = ExpandedUseSource::Materialized(ExpandedMaterialization::Stack {
                    home: id,
                    instruction: reload.instruction,
                });
            }
        }
        remove_register_representative(expanded, root, value)?;
        Ok(edit)
    }
}

fn remove_register_representative(
    expanded: &mut ExpandedAllocationProblem,
    root: LiveBundleId,
    value: VReg,
) -> Result<(), SpillerError> {
    let Some(region) = expanded.region_by_value.remove(&value) else {
        return Ok(());
    };
    let indexed = expanded.region_rows.remove(&region).ok_or_else(|| {
        SpillerError::new(
            "SPILLER.REGION_IDENTITY",
            None,
            Some(value),
            Some(root),
            "machine-spill source is absent from the representative index",
        )
    })?;
    let row = indexed;
    let removed = expanded.register_regions.swap_remove(row);
    if removed.id != region || removed.root != root || removed.value != value {
        return Err(SpillerError::new(
            "SPILLER.REGION_IDENTITY",
            None,
            Some(value),
            Some(root),
            "removed machine-spill representative has incompatible ownership",
        ));
    }
    if let Some(moved) = expanded.register_regions.get(row) {
        expanded.region_rows.insert(moved.id, row);
    }
    Ok(())
}

fn stack_home(
    expanded: &ExpandedAllocationProblem,
    root: LiveBundleId,
) -> Result<Option<StackHomeId>, SpillerError> {
    let homes = expanded
        .stack_homes
        .iter()
        .filter(|home| {
            home.root == root && home.kind == allocation_expand::ExpandedStackHomeKind::Root
        })
        .collect::<Vec<_>>();
    match homes.as_slice() {
        [] => Ok(None),
        [home] => Ok(Some(home.id)),
        _ => Err(SpillerError::new(
            "SPILLER.STACK_HOME_IDENTITY",
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
    replaces_complete_origin: bool,
) -> Result<StackHomeId, SpillerError> {
    if let Some(home) = stack_home(expanded, root.id)? {
        return Ok(home);
    }
    if expanded
        .stack_homes
        .iter()
        .enumerate()
        .any(|(index, home)| home.id.0 as usize != index)
    {
        return Err(SpillerError::new(
            "SPILLER.STACK_HOME_IDENTITY",
            Some(root.definition.block()),
            Some(root.origin),
            Some(root.id),
            "expanded stack homes are not densely identified",
        ));
    }
    let id = StackHomeId(u32::try_from(expanded.stack_homes.len()).map_err(|_| {
        SpillerError::new(
            "SPILLER.STACK_HOME_ID_RANGE",
            Some(root.definition.block()),
            Some(root.origin),
            Some(root.id),
            "expanded stack-home count exceeds u32",
        )
    })?);
    let definition = match root.definition {
        DefinitionSite::Phi { block, phi, .. } if replaces_complete_origin => {
            expanded
                .ir
                .assign_phi_definition_home(root.definition, root.origin, id)
                .map_err(|error| {
                    SpillerError::new(
                        error.rule,
                        error.block,
                        error.values.first().copied(),
                        Some(root.id),
                        error.message,
                    )
                })?;
            allocation_expand::ExpandedStackDefinition::Phi {
                block,
                phi,
                destination: root.origin,
            }
        }
        _ => {
            let instruction = expanded
                .ir
                .insert_after_definition(
                    root.definition,
                    SyntheticOperation::StackStore { home: id },
                    Uses::one(root.origin),
                    false,
                )
                .map_err(|error| {
                    SpillerError::new(
                        error.rule,
                        error.block,
                        error.values.first().copied(),
                        Some(root.id),
                        error.message,
                    )
                })?
                .instruction;
            allocation_expand::ExpandedStackDefinition::Store {
                instruction,
                value: root.origin,
            }
        }
    };
    expanded.stack_homes.push(ExpandedStackHome {
        id,
        root: root.id,
        definition,
        kind: allocation_expand::ExpandedStackHomeKind::Root,
    });
    Ok(id)
}

fn fresh_region_id(
    expanded: &mut ExpandedAllocationProblem,
) -> Result<RegisterRegionId, SpillerError> {
    let id = RegisterRegionId(expanded.next_register_region);
    expanded.next_register_region =
        expanded
            .next_register_region
            .checked_add(1)
            .ok_or_else(|| {
                SpillerError::new(
                    "SPILLER.REGION_ID_RANGE",
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
    edit: &mut SpillEdit,
) -> Result<(), SpillerError> {
    let root_index = root.0 as usize;
    let use_index = use_id.0 as usize;
    let use_ = expanded
        .roots
        .get(root_index)
        .and_then(|root| root.uses.get(use_index))
        .ok_or_else(|| {
            SpillerError::new(
                "SPILLER.USE_RANGE",
                None,
                Some(original),
                Some(root),
                format!("rewritten use {use_id:?} is outside its expanded root"),
            )
        })?
        .clone();
    if use_.value != original {
        return Err(SpillerError::new(
            "SPILLER.USE_OWNERSHIP",
            Some(use_.site.block()),
            Some(original),
            Some(root),
            "rewritten use no longer belongs to the selected spill region",
        ));
    }
    edit.record_use(use_.original_site);
    expanded
        .ir
        .rewrite_use(use_.original_site, original, replacement)
        .map_err(|error| {
            SpillerError::new(
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

fn graph_root(graph: &HomeGraph, root: LiveBundleId) -> Result<&LiveBundle, SpillerError> {
    let row = graph.bundles.get(root.0 as usize).ok_or_else(|| {
        SpillerError::new(
            "SPILLER.ROOT_RANGE",
            None,
            None,
            Some(root),
            "spill root is outside the immutable HomeGraph",
        )
    })?;
    if row.id != root {
        return Err(SpillerError::new(
            "SPILLER.ROOT_IDENTITY",
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
) -> Result<&allocation_expand::ExpandedUse, SpillerError> {
    let row = expanded.roots.get(root.0 as usize).ok_or_else(|| {
        SpillerError::new(
            "SPILLER.ROOT_RANGE",
            None,
            None,
            Some(root),
            "spill root is outside the expanded allocation problem",
        )
    })?;
    if row.id != root {
        return Err(SpillerError::new(
            "SPILLER.ROOT_IDENTITY",
            None,
            Some(row.origin),
            Some(root),
            "expanded root differs from its dense identity",
        ));
    }
    row.uses.get(use_id.0 as usize).ok_or_else(|| {
        SpillerError::new(
            "SPILLER.USE_RANGE",
            None,
            Some(row.origin),
            Some(root),
            format!("spill use {use_id:?} is outside its root"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::native::mir::{
        BaseReg, MBlock, MFunction, MInst, OpSize, PackedStateHome, PhiNode, SpillDesc,
        StateHomeId, VRegAllocator,
    };

    #[test]
    fn deferred_machine_word_home_is_created_once_and_reloaded_per_selected_use() {
        let home = PackedStateHome {
            id: StateHomeId(7),
            offset: 32,
            size: OpSize::S64,
            live_on_entry: false,
        };
        let mut values = VRegAllocator::new();
        for _ in 0..5 {
            values.alloc();
        }
        let mut descriptors = vec![SpillDesc::transient(); 5];
        descriptors[0] = SpillDesc::transient().with_deferred_state_home(home);
        let mut function = MFunction::new(values, descriptors);
        let mut block = MBlock::new(BlockId(0));
        block.insts = vec![
            MInst::LoadImm {
                dst: VReg(1),
                value: 3,
            },
            MInst::LoadImm {
                dst: VReg(2),
                value: 5,
            },
            MInst::Add {
                dst: VReg(0),
                lhs: VReg(1),
                rhs: VReg(2),
            },
            MInst::Mov {
                dst: VReg(3),
                src: VReg(0),
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 64,
                src: VReg(0),
                size: OpSize::S64,
            },
            MInst::Mov {
                dst: VReg(4),
                src: VReg(3),
            },
            MInst::Return,
        ];
        function.blocks.push(block);

        let cfg = super::super::cfg::normalize(&mut function).unwrap();
        let graph = super::super::home_graph::build(&function, &cfg).unwrap();
        let root = graph
            .bundles
            .iter()
            .find(|bundle| bundle.origin == VReg(0))
            .unwrap()
            .id;
        let mut expanded = allocation_expand::expand_unallocated(&function, &cfg, &graph).unwrap();
        let spiller = Spiller::build(&graph).unwrap();
        let entries = graph.bundles[root.0 as usize]
            .uses
            .iter()
            .map(|use_| SpillEntry {
                entry: use_.id,
                uses: vec![use_.id],
                kind: SpillEntryKind::Materialized,
                preferred_register: None,
            })
            .collect::<Vec<_>>();
        let plan = spiller.plan(&expanded, root, VReg(0), &entries).unwrap();
        assert_eq!(plan.total_cost, 3);
        assert!(
            plan.selections()
                .values()
                .all(|selection| matches!(selection.kind, HomeKind::DeferredState(StateHomeId(7))))
        );
        assert_eq!(
            plan.selections()
                .values()
                .filter(|selection| selection.creation_cost == 1)
                .count(),
            1
        );

        spiller
            .materialize(&mut expanded, &graph, VReg(0), root, &entries, &plan, true)
            .unwrap();
        expanded.ir.verify_state_homes(&cfg).unwrap();
        assert_eq!(expanded.state_homes.len(), 1);
        let lowered = expanded.ir.materialize(&function, &graph, &[]).unwrap();
        let state_stores = lowered
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .filter(|inst| {
                matches!(
                    inst,
                    MInst::Store {
                        base: BaseReg::SimState,
                        offset: 32,
                        size: OpSize::S64,
                        ..
                    }
                )
            })
            .count();
        let state_reloads = lowered
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .filter(|inst| {
                matches!(
                    inst,
                    MInst::Load {
                        base: BaseReg::SimState,
                        offset: 32,
                        size: OpSize::S64,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(state_stores, 1);
        assert_eq!(state_reloads, 2);
    }

    #[test]
    fn machine_spill_keeps_semantic_phi_source_in_its_stack_home() {
        let mut values = VRegAllocator::new();
        let condition = values.alloc();
        let left_value = values.alloc();
        let right_value = values.alloc();
        let merged = values.alloc();
        let descriptors = vec![SpillDesc::transient(); 4];
        let mut function = MFunction::new(values, descriptors);

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
            false_bb: BlockId(2),
        });
        let mut left = MBlock::new(BlockId(1));
        left.push(MInst::LoadImm {
            dst: left_value,
            value: 7,
        });
        left.push(MInst::Jump { target: BlockId(3) });
        let mut right = MBlock::new(BlockId(2));
        right.push(MInst::LoadImm {
            dst: right_value,
            value: 11,
        });
        right.push(MInst::Jump { target: BlockId(3) });
        let mut merge = MBlock::new(BlockId(3));
        merge.phis.push(PhiNode {
            dst: merged,
            sources: vec![(BlockId(1), left_value), (BlockId(2), right_value)],
        });
        merge.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 8,
            src: merged,
            size: OpSize::S64,
        });
        merge.push(MInst::Return);
        function.blocks = vec![entry, left, right, merge];

        let cfg = super::super::cfg::normalize(&mut function).unwrap();
        let graph = super::super::home_graph::build(&function, &cfg).unwrap();
        let root = graph
            .bundles
            .iter()
            .find(|bundle| bundle.origin == left_value)
            .unwrap()
            .id;
        let mut expanded = allocation_expand::expand_unallocated(&function, &cfg, &graph).unwrap();
        let interval = expanded.intervals.intervals[left_value.0 as usize]
            .as_ref()
            .unwrap()
            .clone();
        let spiller = Spiller::build(&graph).unwrap();

        expanded.ir.begin_instruction_transaction().unwrap();
        let edit = spiller
            .materialize_machine_interval(&mut expanded, root, &interval)
            .unwrap();
        allocation_expand::refresh(&mut expanded, &cfg, &edit.liveness_blocks).unwrap();

        let root_use = expanded.roots[root.0 as usize]
            .uses
            .iter()
            .find(|use_| {
                matches!(
                    use_.site,
                    UseSite::PhiEdge {
                        predecessor: BlockId(1),
                        ..
                    }
                )
            })
            .unwrap();
        assert_eq!(root_use.value, left_value);
        assert!(matches!(
            root_use.source,
            ExpandedUseSource::Edge(ExpandedEdgeLocation::Stack {
                home: StackHomeId(0)
            })
        ));
        assert_eq!(
            expanded.intervals.intervals.len(),
            function.spill_descs.len(),
            "direct phi-edge spilling must not create a reload VReg"
        );
        expanded.ir.verify_stack_homes(&cfg).unwrap();

        let allocation = super::super::allocation_split::allocate_with_splitting(
            &mut expanded,
            &graph,
            &cfg,
            super::super::assignment::ALLOCATABLE_REGS,
        )
        .unwrap();
        let lowered = super::super::allocation_lower::lower(
            &function,
            &cfg,
            &graph,
            &expanded,
            &allocation,
            super::super::assignment::ALLOCATABLE_REGS,
        )
        .unwrap();
        assert!(
            lowered
                .assignment
                .phi_edge_locations
                .values()
                .any(|location| matches!(
                    location,
                    super::super::assignment::EdgeLocation::Stack(_)
                ))
        );
    }
}
