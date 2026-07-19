//! Expand allocation-owned homes into an off-to-the-side allocation problem.
//!
//! The interval solver's register assignments are not final once a home is
//! selected: stack stores/reloads and recipe nodes define additional machine
//! values which can interfere with those assignments. This phase materializes
//! every selected transition in [`AllocationIr`], rewrites exact original uses,
//! updates exact liveness for affected values, and returns physical registers
//! only as preferences for the next allocation round.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::backend::native::features::VariableShiftEncoding;
use crate::backend::native::mir::{BlockId, MFunction, PackedStateHome, Uses, VReg};

use super::allocation_ir::{
    AllocationIr, AllocationIrError, StackHomeId, SyntheticInstructionId, SyntheticOperation,
};
use super::assignment::PhysReg;
use super::cfg::NormalizedCfg;
use super::home_graph::{
    BundleUseId, HomeGraph, HomeGraphError, HomeKind, LiveBundle, LiveBundleId, RecipeId,
    RecipeNode,
};
use super::interval_allocator::{
    AllocatedBundle, AllocationPlan, BundleAssignment, HomeSelection, IntervalAllocationError,
};
use super::interval_union::AllocationBundleId;
use super::live_interval::{
    IncrementalLiveness, IncrementalLivenessUpdate, LiveIntervalError, LiveIntervals, UseSite,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct RegisterRegionId(pub u32);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ExpandedMaterialization {
    Stack {
        home: StackHomeId,
        instruction: SyntheticInstructionId,
    },
    Recipe {
        kind: HomeKind,
        recipe: RecipeId,
        instructions: Vec<SyntheticInstructionId>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ExpandedUseSource {
    OriginalRegister {
        preferred_register: Option<PhysReg>,
    },
    RegisterRegion {
        region: RegisterRegionId,
        preferred_register: Option<PhysReg>,
    },
    Materialized(ExpandedMaterialization),
    /// A semantic phi source resolved directly by out-of-SSA translation.
    /// It remains in the MIR phi row but does not form a register live range
    /// at the predecessor exit.
    Edge(ExpandedEdgeLocation),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ExpandedEdgeLocation {
    Stack { home: StackHomeId },
    Immediate { value: u64, recipe: RecipeId },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ExpandedUse {
    pub id: BundleUseId,
    /// Immutable identity in the input MIR, used only by the eventual atomic
    /// rewrite.
    pub original_site: UseSite,
    /// Current position after every synthetic instruction has been inserted;
    /// this is the position owned by `intervals` and the next allocation.
    pub site: UseSite,
    pub value: VReg,
    pub source: ExpandedUseSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ExpandedRoot {
    pub id: LiveBundleId,
    pub origin: VReg,
    pub uses: Vec<ExpandedUse>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ExpandedRegisterRegion {
    pub id: RegisterRegionId,
    pub root: LiveBundleId,
    pub value: VReg,
    pub preferred_register: Option<PhysReg>,
    /// Exact immutable root use before which a home-created region is
    /// materialized. Split-copy and merge-phi representatives have no
    /// semantic-use entry: their real machine definition is the boundary.
    pub entry_use: Option<BundleUseId>,
    pub entry: ExpandedRegisterEntry,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ExpandedRegisterEntry {
    /// The immutable source-MIR definition remains a live representative only
    /// through split-copy/phi transition uses.
    Original,
    Materialized(ExpandedMaterialization),
    SplitCopy {
        instruction: SyntheticInstructionId,
        source: VReg,
    },
    SplitPhi {
        block: BlockId,
        phi: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ExpandedStackHome {
    pub id: StackHomeId,
    pub root: LiveBundleId,
    pub definition: ExpandedStackDefinition,
    pub kind: ExpandedStackHomeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ExpandedStateHome {
    pub home: PackedStateHome,
    pub root: LiveBundleId,
    pub definition: SyntheticInstructionId,
    pub value: VReg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExpandedStackDefinition {
    Store {
        instruction: SyntheticInstructionId,
        value: VReg,
    },
    Phi {
        block: BlockId,
        phi: usize,
        destination: VReg,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExpandedStackHomeKind {
    /// Persistent root home established immediately after the root definition.
    Root,
    /// Edge-local recipe result consumed directly by one phi row from memory.
    EdgeRecipe { use_id: BundleUseId },
    /// Conventional spill slot for one allocation-IR machine representative.
    /// This is distinct from the logical root home used by HDL-specific
    /// MemorySSA/rematerialization planning.
    Machine { value: VReg },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ExpandedAllocationProblem {
    pub ir: AllocationIr,
    pub intervals: LiveIntervals,
    pub incremental_liveness: IncrementalLiveness,
    pub shift_encoding: VariableShiftEncoding,
    pub roots: Vec<ExpandedRoot>,
    pub register_regions: Vec<ExpandedRegisterRegion>,
    pub region_rows: BTreeMap<RegisterRegionId, usize>,
    pub next_register_region: u32,
    pub stack_homes: Vec<ExpandedStackHome>,
    pub state_homes: Vec<ExpandedStateHome>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AllocationExpandError {
    pub rule: &'static str,
    pub block: Option<BlockId>,
    pub root: Option<LiveBundleId>,
    pub use_id: Option<BundleUseId>,
    pub message: String,
}

impl AllocationExpandError {
    fn new(
        rule: &'static str,
        block: Option<BlockId>,
        root: Option<LiveBundleId>,
        use_id: Option<BundleUseId>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            rule,
            block,
            root,
            use_id,
            message: message.into(),
        }
    }

    fn graph(error: HomeGraphError) -> Self {
        Self::new(error.rule, error.block, None, None, error.message)
    }

    fn plan(error: IntervalAllocationError) -> Self {
        Self::new(error.rule, error.block, None, None, error.message)
    }

    fn ir(error: AllocationIrError) -> Self {
        Self::new(error.rule, error.block, None, None, error.message)
    }

    fn live(error: LiveIntervalError) -> Self {
        Self::new(error.rule, error.block, None, None, error.message)
    }
}

impl fmt::Display for AllocationExpandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.rule)?;
        if let Some(block) = self.block {
            write!(formatter, " at {block}")?;
        }
        if let Some(root) = self.root {
            write!(formatter, " root={root:?}")?;
        }
        if let Some(use_id) = self.use_id {
            write!(formatter, " use={use_id:?}")?;
        }
        write!(formatter, ": {}", self.message)
    }
}

impl std::error::Error for AllocationExpandError {}

pub(super) struct LoweredMaterialization {
    pub value: VReg,
    pub source: ExpandedMaterialization,
}

pub(super) enum LoweredUseMaterialization {
    Register(LoweredMaterialization),
    Edge(ExpandedEdgeLocation),
}

pub(super) fn expand(
    func: &MFunction,
    cfg: &NormalizedCfg,
    graph: &HomeGraph,
    plan: &AllocationPlan,
    registers: &[PhysReg],
) -> Result<ExpandedAllocationProblem, AllocationExpandError> {
    graph
        .verify(func, cfg)
        .map_err(AllocationExpandError::graph)?;
    plan.verify(graph, cfg, registers)
        .map_err(AllocationExpandError::plan)?;
    let mut ir = AllocationIr::from_mir(func).map_err(AllocationExpandError::ir)?;
    let mut roots = Vec::with_capacity(graph.bundles.len());
    let mut register_regions = Vec::new();
    let mut stack_homes = Vec::new();
    let mut state_homes = Vec::new();

    for (root_index, root) in graph.bundles.iter().enumerate() {
        if root.id.0 as usize != root_index {
            return Err(AllocationExpandError::new(
                "ALLOCATION_EXPAND.ROOT_IDENTITY",
                Some(root.definition.block()),
                Some(root.id),
                None,
                "HomeGraph root differs from its dense expansion row",
            ));
        }
        let root_bundle = plan.bundles.get(root_index).ok_or_else(|| {
            AllocationExpandError::new(
                "ALLOCATION_EXPAND.ROOT_COVERAGE",
                Some(root.definition.block()),
                Some(root.id),
                None,
                "allocation plan has no root bundle for HomeGraph value",
            )
        })?;
        let leaves = collect_final_leaves(plan, root_bundle.id, root.id)?;
        let needs_stack = leaves.iter().any(|&leaf| {
            plan.bundles
                .get(leaf.0 as usize)
                .is_some_and(bundle_uses_stack)
        });
        let needs_deferred_state = leaves.iter().any(|&leaf| {
            plan.bundles
                .get(leaf.0 as usize)
                .is_some_and(bundle_uses_deferred_state)
        });
        if needs_deferred_state {
            let home = graph.deferred_homes[root_index].ok_or_else(|| {
                AllocationExpandError::new(
                    "ALLOCATION_EXPAND.DEFERRED_STATE_HOME",
                    Some(root.definition.block()),
                    Some(root.id),
                    None,
                    "allocation selected a deferred home absent from its machine root",
                )
            })?;
            ensure_state_home(&mut ir, &mut state_homes, root, home)?;
        }
        let keeps_original_register = leaves.iter().any(|&leaf| {
            plan.bundles.get(leaf.0 as usize).is_some_and(|bundle| {
                bundle.parent.is_none()
                    && !bundle.uses.is_empty()
                    && matches!(bundle.assignment, BundleAssignment::Register(_))
            })
        });
        let stack_home = if needs_stack {
            let id = StackHomeId(u32::try_from(stack_homes.len()).map_err(|_| {
                AllocationExpandError::new(
                    "ALLOCATION_EXPAND.STACK_HOME_ID_RANGE",
                    Some(root.definition.block()),
                    Some(root.id),
                    None,
                    "expanded stack-home count exceeds u32",
                )
            })?);
            let definition = match root.definition {
                super::live_interval::DefinitionSite::Phi { block, phi, .. }
                    if !keeps_original_register =>
                {
                    ir.assign_phi_definition_home(root.definition, root.origin, id)
                        .map_err(AllocationExpandError::ir)?;
                    ExpandedStackDefinition::Phi {
                        block,
                        phi,
                        destination: root.origin,
                    }
                }
                _ => {
                    let instruction = ir
                        .insert_after_definition(
                            root.definition,
                            SyntheticOperation::StackStore { home: id },
                            Uses::one(root.origin),
                            false,
                        )
                        .map_err(AllocationExpandError::ir)?
                        .instruction;
                    ExpandedStackDefinition::Store {
                        instruction,
                        value: root.origin,
                    }
                }
            };
            stack_homes.push(ExpandedStackHome {
                id,
                root: root.id,
                definition,
                kind: ExpandedStackHomeKind::Root,
            });
            Some(id)
        } else {
            None
        };

        let mut expanded_uses = vec![None::<ExpandedUse>; root.uses.len()];
        for leaf_id in leaves {
            let leaf = plan.bundles.get(leaf_id.0 as usize).ok_or_else(|| {
                AllocationExpandError::new(
                    "ALLOCATION_EXPAND.BUNDLE_RANGE",
                    Some(root.definition.block()),
                    Some(root.id),
                    None,
                    "final allocation leaf is outside the plan",
                )
            })?;
            match &leaf.assignment {
                BundleAssignment::Register(register) if leaf.parent.is_none() => {
                    for &use_id in &leaf.uses {
                        let use_ = root_use(root, use_id)?;
                        assign_expanded_use(
                            root,
                            &mut expanded_uses,
                            ExpandedUse {
                                id: use_id,
                                original_site: use_.site,
                                site: use_.site,
                                value: root.origin,
                                source: ExpandedUseSource::OriginalRegister {
                                    preferred_register: Some(*register),
                                },
                            },
                        )?;
                    }
                }
                BundleAssignment::Register(register) => {
                    let [transition] = leaf.transitions.as_slice() else {
                        return Err(AllocationExpandError::new(
                            "ALLOCATION_EXPAND.REGION_TRANSITION",
                            Some(root.definition.block()),
                            Some(root.id),
                            None,
                            "split register region does not have one entry transition",
                        ));
                    };
                    let entry_use = root
                        .uses
                        .iter()
                        .find(|use_| use_.site == transition.at)
                        .ok_or_else(|| {
                            AllocationExpandError::new(
                                "ALLOCATION_EXPAND.REGION_TRANSITION",
                                Some(transition.at.block()),
                                Some(root.id),
                                None,
                                "region transition is not an exact root use",
                            )
                        })?;
                    let lowered = lower_register_materialization(
                        &mut ir,
                        graph,
                        root,
                        entry_use.id,
                        entry_use.site,
                        &transition.home,
                        stack_home,
                    )?;
                    let region_value = lowered.value;
                    let region_id =
                        RegisterRegionId(u32::try_from(register_regions.len()).map_err(|_| {
                            AllocationExpandError::new(
                                "ALLOCATION_EXPAND.REGION_ID_RANGE",
                                Some(root.definition.block()),
                                Some(root.id),
                                None,
                                "expanded register-region count exceeds u32",
                            )
                        })?);
                    register_regions.push(ExpandedRegisterRegion {
                        id: region_id,
                        root: root.id,
                        value: region_value,
                        preferred_register: Some(*register),
                        entry_use: Some(entry_use.id),
                        entry: ExpandedRegisterEntry::Materialized(lowered.source),
                    });
                    for &use_id in &leaf.uses {
                        let use_ = root_use(root, use_id)?;
                        ir.rewrite_use(use_.site, root.origin, region_value)
                            .map_err(AllocationExpandError::ir)?;
                        assign_expanded_use(
                            root,
                            &mut expanded_uses,
                            ExpandedUse {
                                id: use_id,
                                original_site: use_.site,
                                site: use_.site,
                                value: region_value,
                                source: ExpandedUseSource::RegisterRegion {
                                    region: region_id,
                                    preferred_register: Some(*register),
                                },
                            },
                        )?;
                    }
                }
                BundleAssignment::Home(selection) => {
                    for &use_id in &leaf.uses {
                        let use_ = root_use(root, use_id)?;
                        let lowered = lower_use_materialization(
                            &mut ir,
                            graph,
                            root,
                            root.origin,
                            use_id,
                            use_.site,
                            selection,
                            stack_home,
                            &mut stack_homes,
                        )?;
                        let (value, source) = match lowered {
                            LoweredUseMaterialization::Register(lowered) => {
                                ir.rewrite_use(use_.site, root.origin, lowered.value)
                                    .map_err(AllocationExpandError::ir)?;
                                (
                                    lowered.value,
                                    ExpandedUseSource::Materialized(lowered.source),
                                )
                            }
                            LoweredUseMaterialization::Edge(location) => {
                                (root.origin, ExpandedUseSource::Edge(location))
                            }
                        };
                        assign_expanded_use(
                            root,
                            &mut expanded_uses,
                            ExpandedUse {
                                id: use_id,
                                original_site: use_.site,
                                site: use_.site,
                                value,
                                source,
                            },
                        )?;
                    }
                }
                BundleAssignment::Dead if leaf.uses.is_empty() => {}
                BundleAssignment::Unassigned
                | BundleAssignment::Split { .. }
                | BundleAssignment::Dead => {
                    return Err(AllocationExpandError::new(
                        "ALLOCATION_EXPAND.NON_FINAL_LEAF",
                        Some(root.definition.block()),
                        Some(root.id),
                        None,
                        "allocation leaf has no final register, home, or dead assignment",
                    ));
                }
            }
        }
        let uses = expanded_uses
            .into_iter()
            .enumerate()
            .map(|(index, use_)| {
                use_.ok_or_else(|| {
                    let use_id = BundleUseId(index as u32);
                    AllocationExpandError::new(
                        "ALLOCATION_EXPAND.USE_COVERAGE",
                        root.uses.get(index).map(|use_| use_.site.block()),
                        Some(root.id),
                        Some(use_id),
                        "allocation leaves did not assign this exact root use",
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        roots.push(ExpandedRoot {
            id: root.id,
            origin: root.origin,
            uses,
        });
    }

    finish_expansion(
        func,
        cfg,
        ir,
        roots,
        register_regions,
        stack_homes,
        state_homes,
    )
}

/// Seed joint allocation with the original SSA ranges and no preselected
/// stack, recipe, or register homes. Physical interference is then the first
/// mechanism allowed to split or materialize a range.
pub(super) fn expand_unallocated(
    func: &MFunction,
    cfg: &NormalizedCfg,
    graph: &HomeGraph,
) -> Result<ExpandedAllocationProblem, AllocationExpandError> {
    graph
        .verify(func, cfg)
        .map_err(AllocationExpandError::graph)?;
    let ir = AllocationIr::from_mir(func).map_err(AllocationExpandError::ir)?;
    let mut roots = Vec::with_capacity(graph.bundles.len());
    for (root_index, root) in graph.bundles.iter().enumerate() {
        if root.id.0 as usize != root_index {
            return Err(AllocationExpandError::new(
                "ALLOCATION_EXPAND.ROOT_IDENTITY",
                Some(root.definition.block()),
                Some(root.id),
                None,
                "HomeGraph root differs from its dense unallocated row",
            ));
        }
        let uses = root
            .uses
            .iter()
            .map(|use_| ExpandedUse {
                id: use_.id,
                original_site: use_.site,
                site: use_.site,
                value: root.origin,
                source: ExpandedUseSource::OriginalRegister {
                    preferred_register: None,
                },
            })
            .collect();
        roots.push(ExpandedRoot {
            id: root.id,
            origin: root.origin,
            uses,
        });
    }
    finish_expansion(func, cfg, ir, roots, Vec::new(), Vec::new(), Vec::new())
}

fn finish_expansion(
    func: &MFunction,
    cfg: &NormalizedCfg,
    mut ir: AllocationIr,
    mut roots: Vec<ExpandedRoot>,
    register_regions: Vec<ExpandedRegisterRegion>,
    stack_homes: Vec<ExpandedStackHome>,
    state_homes: Vec<ExpandedStateHome>,
) -> Result<ExpandedAllocationProblem, AllocationExpandError> {
    let intervals = analyze_and_resolve(&ir, &mut roots, cfg)?;
    // Initial expansion is indexed from its final IR snapshot. Its mutation
    // journal is therefore already represented by `intervals` and must not be
    // replayed into the incremental session.
    let _ = ir.take_liveness_delta();
    let incremental_liveness =
        IncrementalLiveness::build(&ir, cfg, &intervals).map_err(AllocationExpandError::live)?;
    let region_rows = register_regions
        .iter()
        .enumerate()
        .map(|(row, region)| (region.id, row))
        .collect::<BTreeMap<_, _>>();
    if region_rows.len() != register_regions.len() {
        return Err(AllocationExpandError::new(
            "ALLOCATION_EXPAND.REGION_IDENTITY",
            None,
            None,
            None,
            "initial register regions do not have unique identities",
        ));
    }
    let next_register_region = u32::try_from(register_regions.len()).map_err(|_| {
        AllocationExpandError::new(
            "ALLOCATION_EXPAND.REGION_ID_RANGE",
            None,
            None,
            None,
            "initial register-region count exceeds u32",
        )
    })?;
    Ok(ExpandedAllocationProblem {
        ir,
        intervals,
        incremental_liveness,
        shift_encoding: func.target_features.variable_shift_encoding(),
        roots,
        register_regions,
        region_rows,
        next_register_region,
        stack_homes,
        state_homes,
    })
}

pub(super) fn refresh(
    problem: &mut ExpandedAllocationProblem,
    cfg: &NormalizedCfg,
    changed_blocks: &BTreeSet<BlockId>,
) -> Result<IncrementalLivenessUpdate, AllocationExpandError> {
    problem
        .ir
        .publish_instruction_transaction()
        .map_err(AllocationExpandError::ir)?;
    let delta = problem.ir.take_liveness_delta();
    if super::exhaustive_verification_enabled()
        && delta
            .changed_blocks
            .iter()
            .any(|block| !changed_blocks.contains(block))
    {
        return Err(AllocationExpandError::new(
            "ALLOCATION_EXPAND.LIVENESS_JOURNAL",
            delta
                .changed_blocks
                .iter()
                .find(|block| !changed_blocks.contains(block))
                .copied(),
            None,
            None,
            "exact liveness journal contains a block omitted by the split transaction",
        ));
    }
    let update = problem
        .incremental_liveness
        .update_fact_delta(&problem.ir, cfg, &mut problem.intervals, delta)
        .map_err(AllocationExpandError::live)?;
    // Expanded root uses retain immutable block/stable-slot coordinates.
    // Synthetic insertion changes only dense lowering positions, which are
    // resolved from `original_site` when the allocation IR is rewritten or
    // finally lowered; eagerly relabeling every root use makes each split
    // round proportional to the complete changed-block use population.
    Ok(update)
}

fn analyze_and_resolve(
    ir: &AllocationIr,
    roots: &mut [ExpandedRoot],
    cfg: &NormalizedCfg,
) -> Result<LiveIntervals, AllocationExpandError> {
    ir.verify_stack_homes(cfg)
        .map_err(AllocationExpandError::ir)?;
    let intervals = ir.analyze(cfg).map_err(AllocationExpandError::ir)?;
    let original_blocks = roots
        .iter()
        .flat_map(|root| root.uses.iter().map(|use_| use_.original_site.block()))
        .collect::<BTreeSet<_>>();
    let original_use_sites = ir
        .index_original_use_sites(original_blocks)
        .map_err(AllocationExpandError::ir)?;
    for root in &mut *roots {
        for use_ in &mut root.uses {
            use_.site = ir
                .resolve_original_use_site_indexed(
                    use_.original_site,
                    &intervals,
                    &original_use_sites,
                )
                .map_err(AllocationExpandError::ir)?;
        }
    }
    verify_expanded_uses(roots, &intervals)?;
    Ok(intervals)
}

fn collect_final_leaves(
    plan: &AllocationPlan,
    bundle: AllocationBundleId,
    root: LiveBundleId,
) -> Result<Vec<AllocationBundleId>, AllocationExpandError> {
    let mut output = Vec::new();
    let mut pending = vec![bundle];
    let mut visited = BTreeSet::new();
    while let Some(bundle) = pending.pop() {
        if !visited.insert(bundle) {
            return Err(AllocationExpandError::new(
                "ALLOCATION_EXPAND.BUNDLE_TREE",
                None,
                Some(root),
                None,
                "allocation child tree contains a cycle or duplicate child",
            ));
        }
        let candidate = plan.bundles.get(bundle.0 as usize).ok_or_else(|| {
            AllocationExpandError::new(
                "ALLOCATION_EXPAND.BUNDLE_RANGE",
                None,
                Some(root),
                None,
                "allocation tree references a bundle outside the plan",
            )
        })?;
        if candidate.root != root {
            return Err(AllocationExpandError::new(
                "ALLOCATION_EXPAND.BUNDLE_ROOT",
                Some(candidate.definition.block()),
                Some(root),
                None,
                "allocation child belongs to a different HomeGraph root",
            ));
        }
        if let BundleAssignment::Split { children, .. } = &candidate.assignment {
            pending.extend(children.iter().rev().copied());
        } else {
            output.push(bundle);
        }
    }
    Ok(output)
}

fn bundle_uses_stack(bundle: &AllocatedBundle) -> bool {
    match &bundle.assignment {
        BundleAssignment::Home(selection) => selection.kind == HomeKind::Stack,
        BundleAssignment::Register(_) => bundle
            .transitions
            .iter()
            .any(|transition| transition.home.kind == HomeKind::Stack),
        BundleAssignment::Unassigned | BundleAssignment::Split { .. } | BundleAssignment::Dead => {
            false
        }
    }
}

fn bundle_uses_deferred_state(bundle: &AllocatedBundle) -> bool {
    match &bundle.assignment {
        BundleAssignment::Home(selection) => {
            matches!(selection.kind, HomeKind::DeferredState(_))
        }
        BundleAssignment::Register(_) => bundle
            .transitions
            .iter()
            .any(|transition| matches!(transition.home.kind, HomeKind::DeferredState(_))),
        BundleAssignment::Unassigned | BundleAssignment::Split { .. } | BundleAssignment::Dead => {
            false
        }
    }
}

pub(super) fn ensure_state_home(
    ir: &mut AllocationIr,
    homes: &mut Vec<ExpandedStateHome>,
    root: &LiveBundle,
    home: PackedStateHome,
) -> Result<SyntheticInstructionId, AllocationExpandError> {
    if let Some(existing) = homes
        .iter()
        .find(|existing| existing.home.id == home.id && existing.root == root.id)
    {
        if existing.home != home || existing.value != root.origin {
            return Err(AllocationExpandError::new(
                "ALLOCATION_EXPAND.DEFERRED_STATE_IDENTITY",
                Some(root.definition.block()),
                Some(root.id),
                None,
                "deferred state-home identity is shared by different machine values",
            ));
        }
        return Ok(existing.definition);
    }
    if homes
        .iter()
        .any(|existing| existing.home.id == home.id && existing.home != home)
    {
        return Err(AllocationExpandError::new(
            "ALLOCATION_EXPAND.DEFERRED_STATE_IDENTITY",
            Some(root.definition.block()),
            Some(root.id),
            None,
            "one deferred state-home version names two physical words",
        ));
    }
    let definition = ir
        .insert_after_definition(
            root.definition,
            SyntheticOperation::StateStore { home },
            Uses::one(root.origin),
            false,
        )
        .map_err(AllocationExpandError::ir)?
        .instruction;
    homes.push(ExpandedStateHome {
        home,
        root: root.id,
        definition,
        value: root.origin,
    });
    Ok(definition)
}

fn root_use(
    root: &LiveBundle,
    use_id: BundleUseId,
) -> Result<&super::home_graph::BundleUse, AllocationExpandError> {
    root.uses.get(use_id.0 as usize).ok_or_else(|| {
        AllocationExpandError::new(
            "ALLOCATION_EXPAND.USE_RANGE",
            Some(root.definition.block()),
            Some(root.id),
            Some(use_id),
            "allocation bundle use is outside its HomeGraph root",
        )
    })
}

fn assign_expanded_use(
    root: &LiveBundle,
    output: &mut [Option<ExpandedUse>],
    use_: ExpandedUse,
) -> Result<(), AllocationExpandError> {
    let slot = output.get_mut(use_.id.0 as usize).ok_or_else(|| {
        AllocationExpandError::new(
            "ALLOCATION_EXPAND.USE_RANGE",
            Some(use_.site.block()),
            Some(root.id),
            Some(use_.id),
            "expanded use is outside the root use table",
        )
    })?;
    if slot.is_some() {
        return Err(AllocationExpandError::new(
            "ALLOCATION_EXPAND.USE_OWNERSHIP",
            Some(use_.site.block()),
            Some(root.id),
            Some(use_.id),
            "more than one allocation leaf owns the same exact use",
        ));
    }
    *slot = Some(use_);
    Ok(())
}

pub(super) fn lower_use_materialization(
    ir: &mut AllocationIr,
    graph: &HomeGraph,
    root: &LiveBundle,
    current_value: VReg,
    use_id: BundleUseId,
    site: UseSite,
    selection: &HomeSelection,
    stack_home: Option<StackHomeId>,
    stack_homes: &mut Vec<ExpandedStackHome>,
) -> Result<LoweredUseMaterialization, AllocationExpandError> {
    if matches!(site, UseSite::PhiEdge { .. }) {
        return lower_phi_edge_materialization(
            ir,
            graph,
            root,
            current_value,
            use_id,
            site,
            selection,
            stack_home,
            stack_homes,
        )
        .map(LoweredUseMaterialization::Edge);
    }
    lower_register_materialization(ir, graph, root, use_id, site, selection, stack_home)
        .map(LoweredUseMaterialization::Register)
}

#[allow(clippy::too_many_arguments)]
fn lower_phi_edge_materialization(
    ir: &mut AllocationIr,
    graph: &HomeGraph,
    root: &LiveBundle,
    current_value: VReg,
    use_id: BundleUseId,
    site: UseSite,
    selection: &HomeSelection,
    stack_home: Option<StackHomeId>,
    stack_homes: &mut Vec<ExpandedStackHome>,
) -> Result<ExpandedEdgeLocation, AllocationExpandError> {
    let UseSite::PhiEdge { .. } = site else {
        return Err(AllocationExpandError::new(
            "ALLOCATION_EXPAND.EDGE_LOCATION_SITE",
            Some(site.block()),
            Some(root.id),
            Some(use_id),
            "phi-edge materialization requires an exact phi-edge use",
        ));
    };
    let location = match selection.kind {
        HomeKind::Stack => {
            if !selection.materializations.is_empty() {
                return Err(AllocationExpandError::new(
                    "ALLOCATION_EXPAND.STACK_RECIPE",
                    Some(site.block()),
                    Some(root.id),
                    Some(use_id),
                    "stack phi-edge location unexpectedly carries a recipe DAG",
                ));
            }
            ExpandedEdgeLocation::Stack {
                home: stack_home.ok_or_else(|| {
                    AllocationExpandError::new(
                        "ALLOCATION_EXPAND.STACK_HOME",
                        Some(site.block()),
                        Some(root.id),
                        Some(use_id),
                        "stack phi-edge location has no explicit root stack home",
                    )
                })?,
            }
        }
        HomeKind::Rematerialize(_) | HomeKind::State(_) => {
            let materialization = exact_recipe(selection, root, use_id, site)?;
            match graph.recipe_nodes.get(materialization.recipe.0 as usize) {
                Some(RecipeNode::Constant(value)) => ExpandedEdgeLocation::Immediate {
                    value: *value,
                    recipe: materialization.recipe,
                },
                Some(_) => {
                    let (value, _instructions) =
                        lower_recipe(ir, graph, root, site, materialization.recipe)?;
                    let id = StackHomeId(u32::try_from(stack_homes.len()).map_err(|_| {
                        AllocationExpandError::new(
                            "ALLOCATION_EXPAND.STACK_HOME_ID_RANGE",
                            Some(site.block()),
                            Some(root.id),
                            Some(use_id),
                            "expanded edge stack-home count exceeds u32",
                        )
                    })?);
                    let store = ir
                        .insert_before_use(
                            site,
                            SyntheticOperation::StackStore { home: id },
                            Uses::one(value),
                            false,
                        )
                        .map_err(AllocationExpandError::ir)?
                        .instruction;
                    stack_homes.push(ExpandedStackHome {
                        id,
                        root: root.id,
                        definition: ExpandedStackDefinition::Store {
                            instruction: store,
                            value,
                        },
                        kind: ExpandedStackHomeKind::EdgeRecipe { use_id },
                    });
                    ExpandedEdgeLocation::Stack { home: id }
                }
                None => {
                    return Err(AllocationExpandError::new(
                        "ALLOCATION_EXPAND.RECIPE_RANGE",
                        Some(site.block()),
                        Some(root.id),
                        Some(use_id),
                        "phi-edge materialization references a missing recipe node",
                    ));
                }
            }
        }
        HomeKind::DeferredState(_) => {
            return Err(AllocationExpandError::new(
                "ALLOCATION_EXPAND.DEFERRED_STATE_EDGE",
                Some(site.block()),
                Some(root.id),
                Some(use_id),
                "deferred state homes are not offered on phi edges",
            ));
        }
        HomeKind::Register => {
            return Err(AllocationExpandError::new(
                "ALLOCATION_EXPAND.HOME_CLASS",
                Some(site.block()),
                Some(root.id),
                Some(use_id),
                "register residency cannot be lowered as a phi-edge home",
            ));
        }
    };
    ir.assign_phi_edge_home(site, current_value, root.origin)
        .map_err(AllocationExpandError::ir)?;
    Ok(location)
}

fn exact_recipe<'a>(
    selection: &'a HomeSelection,
    root: &LiveBundle,
    use_id: BundleUseId,
    site: UseSite,
) -> Result<&'a super::home_graph::UseMaterialization, AllocationExpandError> {
    let mut matching = selection
        .materializations
        .iter()
        .filter(|materialization| materialization.use_id == use_id);
    let materialization = matching.next().ok_or_else(|| {
        AllocationExpandError::new(
            "ALLOCATION_EXPAND.RECIPE_USE",
            Some(site.block()),
            Some(root.id),
            Some(use_id),
            "selected non-stack home has no exact recipe for this use",
        )
    })?;
    if matching.next().is_some() {
        return Err(AllocationExpandError::new(
            "ALLOCATION_EXPAND.RECIPE_USE",
            Some(site.block()),
            Some(root.id),
            Some(use_id),
            "selected non-stack home has duplicate recipes for this use",
        ));
    }
    Ok(materialization)
}

fn lower_register_materialization(
    ir: &mut AllocationIr,
    graph: &HomeGraph,
    root: &LiveBundle,
    use_id: BundleUseId,
    site: UseSite,
    selection: &HomeSelection,
    stack_home: Option<StackHomeId>,
) -> Result<LoweredMaterialization, AllocationExpandError> {
    match selection.kind {
        HomeKind::Stack => {
            let home = stack_home.ok_or_else(|| {
                AllocationExpandError::new(
                    "ALLOCATION_EXPAND.STACK_HOME",
                    Some(site.block()),
                    Some(root.id),
                    Some(use_id),
                    "stack materialization has no explicit root stack home",
                )
            })?;
            if !selection.materializations.is_empty() {
                return Err(AllocationExpandError::new(
                    "ALLOCATION_EXPAND.STACK_RECIPE",
                    Some(site.block()),
                    Some(root.id),
                    Some(use_id),
                    "stack materialization unexpectedly carries a recipe DAG",
                ));
            }
            let inserted = ir
                .insert_before_use(
                    site,
                    SyntheticOperation::StackReload { home },
                    Uses::none(),
                    true,
                )
                .map_err(AllocationExpandError::ir)?;
            let value = inserted.definition.ok_or_else(|| {
                AllocationExpandError::new(
                    "ALLOCATION_EXPAND.SYNTHETIC_DEFINITION",
                    Some(site.block()),
                    Some(root.id),
                    Some(use_id),
                    "stack reload did not define its allocation value",
                )
            })?;
            Ok(LoweredMaterialization {
                value,
                source: ExpandedMaterialization::Stack {
                    home,
                    instruction: inserted.instruction,
                },
            })
        }
        HomeKind::Rematerialize(_) | HomeKind::State(_) | HomeKind::DeferredState(_) => {
            let materialization = exact_recipe(selection, root, use_id, site)?;
            let (value, instructions) =
                lower_recipe(ir, graph, root, site, materialization.recipe)?;
            Ok(LoweredMaterialization {
                value,
                source: ExpandedMaterialization::Recipe {
                    kind: selection.kind,
                    recipe: materialization.recipe,
                    instructions,
                },
            })
        }
        HomeKind::Register => Err(AllocationExpandError::new(
            "ALLOCATION_EXPAND.HOME_CLASS",
            Some(site.block()),
            Some(root.id),
            Some(use_id),
            "register residency cannot be lowered as a home materialization",
        )),
    }
}

fn lower_recipe(
    ir: &mut AllocationIr,
    graph: &HomeGraph,
    root: &LiveBundle,
    site: UseSite,
    recipe: RecipeId,
) -> Result<(VReg, Vec<SyntheticInstructionId>), AllocationExpandError> {
    if let Some(RecipeNode::DeferredState(home)) = graph.recipe_nodes.get(recipe.0 as usize) {
        let inserted = ir
            .insert_before_use(
                site,
                SyntheticOperation::StateReload { home: *home },
                Uses::none(),
                true,
            )
            .map_err(AllocationExpandError::ir)?;
        let value = inserted.definition.ok_or_else(|| {
            AllocationExpandError::new(
                "ALLOCATION_EXPAND.SYNTHETIC_DEFINITION",
                Some(site.block()),
                Some(root.id),
                None,
                "deferred state reload did not define its allocation value",
            )
        })?;
        return Ok((value, vec![inserted.instruction]));
    }
    let mut reachable = BTreeSet::<RecipeId>::new();
    let mut work = vec![recipe];
    while let Some(node) = work.pop() {
        if !reachable.insert(node) {
            continue;
        }
        let operation = graph.recipe_nodes.get(node.0 as usize).ok_or_else(|| {
            AllocationExpandError::new(
                "ALLOCATION_EXPAND.RECIPE_RANGE",
                Some(site.block()),
                Some(root.id),
                None,
                format!("recipe references missing node {node:?}"),
            )
        })?;
        match operation {
            RecipeNode::Constant(_) | RecipeNode::State(_) | RecipeNode::DeferredState(_) => {}
            RecipeNode::Unary { input, .. } => work.push(*input),
            RecipeNode::Or64 { left, right } => {
                work.push(*left);
                work.push(*right);
            }
        }
    }

    let mut values = BTreeMap::<RecipeId, VReg>::new();
    let mut instructions = Vec::with_capacity(reachable.len());
    for node in reachable {
        let operation = &graph.recipe_nodes[node.0 as usize];
        let uses = match operation {
            RecipeNode::Constant(_) | RecipeNode::State(_) | RecipeNode::DeferredState(_) => {
                Uses::none()
            }
            RecipeNode::Unary { input, .. } => Uses::one(*values.get(input).ok_or_else(|| {
                AllocationExpandError::new(
                    "ALLOCATION_EXPAND.RECIPE_TOPOLOGY",
                    Some(site.block()),
                    Some(root.id),
                    None,
                    format!("recipe input {input:?} is not topologically earlier than {node:?}"),
                )
            })?),
            RecipeNode::Or64 { left, right } => Uses::two(
                *values.get(left).ok_or_else(|| {
                    AllocationExpandError::new(
                        "ALLOCATION_EXPAND.RECIPE_TOPOLOGY",
                        Some(site.block()),
                        Some(root.id),
                        None,
                        format!("recipe input {left:?} is not available for {node:?}"),
                    )
                })?,
                *values.get(right).ok_or_else(|| {
                    AllocationExpandError::new(
                        "ALLOCATION_EXPAND.RECIPE_TOPOLOGY",
                        Some(site.block()),
                        Some(root.id),
                        None,
                        format!("recipe input {right:?} is not available for {node:?}"),
                    )
                })?,
            ),
        };
        let inserted = ir
            .insert_before_use(
                site,
                SyntheticOperation::RecipeNode {
                    root: root.id,
                    node,
                },
                uses,
                true,
            )
            .map_err(AllocationExpandError::ir)?;
        instructions.push(inserted.instruction);
        let definition = inserted.definition.ok_or_else(|| {
            AllocationExpandError::new(
                "ALLOCATION_EXPAND.SYNTHETIC_DEFINITION",
                Some(site.block()),
                Some(root.id),
                None,
                "recipe node did not define its allocation value",
            )
        })?;
        values.insert(node, definition);
    }
    let value = values.get(&recipe).copied().ok_or_else(|| {
        AllocationExpandError::new(
            "ALLOCATION_EXPAND.RECIPE_ROOT",
            Some(site.block()),
            Some(root.id),
            None,
            "recipe expansion did not materialize its root value",
        )
    })?;
    Ok((value, instructions))
}

fn verify_expanded_uses(
    roots: &[ExpandedRoot],
    intervals: &LiveIntervals,
) -> Result<(), AllocationExpandError> {
    for root in roots {
        for (index, use_) in root.uses.iter().enumerate() {
            verify_expanded_use(root.id, root.origin, index, use_, intervals)?;
        }
    }
    Ok(())
}

fn verify_expanded_use(
    root: LiveBundleId,
    root_origin: VReg,
    index: usize,
    use_: &ExpandedUse,
    intervals: &LiveIntervals,
) -> Result<(), AllocationExpandError> {
    if use_.id.0 as usize != index {
        return Err(AllocationExpandError::new(
            "ALLOCATION_EXPAND.USE_ORDER",
            Some(use_.site.block()),
            Some(root),
            Some(use_.id),
            "expanded root uses are not in dense HomeGraph order",
        ));
    }
    if matches!(use_.source, ExpandedUseSource::Edge(_)) {
        if !matches!(use_.site, UseSite::PhiEdge { .. }) || use_.value != root_origin {
            return Err(AllocationExpandError::new(
                "ALLOCATION_EXPAND.EDGE_USE_IDENTITY",
                Some(use_.site.block()),
                Some(root),
                Some(use_.id),
                "non-register edge location does not retain its source root phi identity",
            ));
        }
        if intervals
            .intervals
            .get(use_.value.0 as usize)
            .and_then(Option::as_ref)
            .is_some_and(|interval| interval.contains_use_coordinate(use_.site))
        {
            return Err(AllocationExpandError::new(
                "ALLOCATION_EXPAND.EDGE_REGISTER_PRESSURE",
                Some(use_.site.block()),
                Some(root),
                Some(use_.id),
                "non-register phi location still contributes a register live-range use",
            ));
        }
        return Ok(());
    }
    let interval = intervals
        .intervals
        .get(use_.value.0 as usize)
        .and_then(Option::as_ref)
        .ok_or_else(|| {
            AllocationExpandError::new(
                "ALLOCATION_EXPAND.VALUE_INTERVAL",
                Some(use_.site.block()),
                Some(root),
                Some(use_.id),
                "expanded use value has no exact live interval",
            )
        })?;
    if !interval.contains_use_coordinate(use_.site) {
        return Err(AllocationExpandError::new(
            "ALLOCATION_EXPAND.USE_REWRITE",
            Some(use_.site.block()),
            Some(root),
            Some(use_.id),
            "expanded value does not own the rewritten exact MIR use",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::native::mir::{BaseReg, MBlock, MInst, OpSize, SpillDesc, VRegAllocator};

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
        let graph = super::super::home_graph::build(function, &cfg).unwrap();
        (cfg, graph)
    }

    fn root(problem: &ExpandedAllocationProblem, value: VReg) -> &ExpandedRoot {
        problem
            .roots
            .iter()
            .find(|root| root.origin == value)
            .unwrap()
    }

    #[test]
    fn unallocated_seed_preserves_original_ssa_without_preselected_homes() {
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
            MInst::Mov {
                dst: VReg(3),
                src: VReg(2),
            },
            MInst::Return,
        ];
        let mut function = function(4, instructions);
        let (cfg, graph) = model(&mut function);
        let before = format!("{function:?}");

        let problem = expand_unallocated(&function, &cfg, &graph).unwrap();

        assert_eq!(problem.ir.value_count(), function.vregs.count());
        assert!(problem.stack_homes.is_empty());
        assert!(problem.register_regions.is_empty());
        let stack_facts = problem.ir.stack_facts().unwrap();
        assert!(stack_facts.operations.is_empty());
        assert!(stack_facts.phi_definitions.is_empty());
        for root in &problem.roots {
            for use_ in &root.uses {
                assert_eq!(use_.value, root.origin);
                assert_eq!(
                    use_.site,
                    problem
                        .ir
                        .resolve_original_use_site(use_.original_site, &problem.intervals)
                        .unwrap()
                );
                assert!(matches!(
                    use_.source,
                    ExpandedUseSource::OriginalRegister {
                        preferred_register: None
                    }
                ));
            }
        }
        assert_eq!(format!("{function:?}"), before);
    }

    #[test]
    fn stack_home_expands_to_store_reload_and_reallocated_short_value() {
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
            MInst::LoadImm {
                dst: VReg(3),
                value: 13,
            },
            MInst::LoadImm {
                dst: VReg(4),
                value: 17,
            },
            MInst::Add {
                dst: VReg(5),
                lhs: VReg(3),
                rhs: VReg(4),
            },
            MInst::Mov {
                dst: VReg(6),
                src: VReg(5),
            },
            MInst::Mov {
                dst: VReg(7),
                src: VReg(5),
            },
            MInst::Mov {
                dst: VReg(8),
                src: VReg(5),
            },
            MInst::Mov {
                dst: VReg(9),
                src: VReg(2),
            },
            MInst::Return,
        ];
        let mut function = function(10, instructions);
        let (cfg, graph) = model(&mut function);
        let registers = [PhysReg::RAX];
        let plan =
            super::super::interval_allocator::allocate_roots(&graph, &cfg, &registers).unwrap();
        let before = format!("{function:?}");

        let problem = expand(&function, &cfg, &graph, &plan, &registers).unwrap();
        let expanded = root(&problem, VReg(2));
        let [use_] = expanded.uses.as_slice() else {
            panic!("add result should have one exact use");
        };
        let ExpandedUseSource::Materialized(ExpandedMaterialization::Stack { home, .. }) =
            &use_.source
        else {
            panic!("non-rematerializable add result should use a stack reload");
        };
        assert!(
            problem
                .stack_homes
                .iter()
                .any(|stack| { stack.id == *home && stack.root == expanded.id })
        );
        assert_ne!(use_.value, expanded.origin);
        assert_eq!(
            problem.intervals.intervals[use_.value.0 as usize]
                .as_ref()
                .unwrap()
                .uses
                .as_slice(),
            [use_.site]
        );
        assert_eq!(
            problem.intervals.intervals[expanded.origin.0 as usize]
                .as_ref()
                .unwrap()
                .uses
                .len(),
            1,
            "the original add result must remain live only to its explicit stack store"
        );
        assert_eq!(format!("{function:?}"), before);
    }

    #[test]
    fn point_specific_state_and_stack_homes_expand_independently() {
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
            MInst::Add {
                dst: VReg(3),
                lhs: VReg(1),
                rhs: VReg(2),
            },
            MInst::Mov {
                dst: VReg(4),
                src: VReg(0),
            },
            MInst::Mov {
                dst: VReg(5),
                src: VReg(3),
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
                src: VReg(3),
            },
            MInst::Mov {
                dst: VReg(8),
                src: VReg(3),
            },
            MInst::Mov {
                dst: VReg(9),
                src: VReg(0),
            },
            MInst::Return,
        ];
        let mut function = function(10, instructions);
        let (cfg, graph) = model(&mut function);
        let registers = [PhysReg::RAX];
        let plan =
            super::super::interval_allocator::allocate_roots(&graph, &cfg, &registers).unwrap();

        let problem = expand(&function, &cfg, &graph, &plan, &registers).unwrap();
        let expanded = root(&problem, VReg(0));
        assert_eq!(expanded.uses.len(), 2);
        assert!(matches!(
            expanded.uses[0].source,
            ExpandedUseSource::Materialized(ExpandedMaterialization::Recipe {
                kind: HomeKind::State(_),
                ..
            })
        ));
        assert!(matches!(
            expanded.uses[1].source,
            ExpandedUseSource::Materialized(ExpandedMaterialization::Stack { .. })
        ));
        assert_ne!(expanded.uses[0].value, expanded.uses[1].value);
        assert!(
            problem
                .stack_homes
                .iter()
                .any(|home| home.root == expanded.id)
        );
    }

    #[test]
    fn split_register_region_becomes_one_synthetic_ssa_value_and_preference() {
        let instructions = vec![
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
        let mut function = function(7, instructions);
        let (cfg, graph) = model(&mut function);
        let registers = [PhysReg::RAX];
        let plan =
            super::super::interval_allocator::allocate_roots(&graph, &cfg, &registers).unwrap();

        let problem = expand(&function, &cfg, &graph, &plan, &registers).unwrap();
        let expanded = root(&problem, VReg(0));
        let region = problem
            .register_regions
            .iter()
            .find(|region| region.root == expanded.id)
            .unwrap();
        let region_uses = expanded
            .uses
            .iter()
            .filter(|use_| {
                matches!(
                    use_.source,
                    ExpandedUseSource::RegisterRegion { region: id, .. } if id == region.id
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(region.preferred_register, Some(PhysReg::RAX));
        assert_eq!(region_uses.len(), 2);
        assert!(region_uses.iter().all(|use_| use_.value == region.value));
        assert_eq!(
            problem.intervals.intervals[region.value.0 as usize]
                .as_ref()
                .unwrap()
                .uses
                .len(),
            2
        );
        assert!(matches!(
            region.entry,
            ExpandedRegisterEntry::Materialized(ExpandedMaterialization::Recipe {
                kind: HomeKind::Rematerialize(_),
                ..
            })
        ));
    }
}
