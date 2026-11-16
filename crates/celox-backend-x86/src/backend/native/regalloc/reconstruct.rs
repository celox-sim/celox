//! Materialize a SpillPlan and reconstruct strict SSA with dominance frontiers.

use std::collections::{BTreeSet, VecDeque};

use crate::HashMap;
use crate::native::memory_effect::{self, UnknownMemory};
use crate::native::mir::{
    BaseReg, BlockId, MBlock, MFunction, MInst, OpSize, PhiNode, SpillDesc, SpillKind, VReg,
};

use super::cfg::NormalizedCfg;
use super::materialized_state_home::{MaterializedStateReload, MaterializedStateStore};
use super::reload::{
    ExpectedMaterializedReload, MemoryPhiFactoring, PointUse, ReloadRecipeAnalysis, ResolvedBase,
    ResolvedRecipe, materialize_pure_step,
};
use super::spill_plan::{
    LogicalValue, PlannedEdgeOp, PlannedOp, PointSide, ProgramPoint, SpillHome, SpillPlan,
};

pub(super) struct ReconstructionResult {
    pub frame_size: u32,
    pub recipe_reloads: Vec<ExpectedMaterializedReload>,
    pub state_stores: Vec<MaterializedStateStore>,
    pub state_reloads: Vec<MaterializedStateReload>,
    pub shared_reload_blocks: Vec<MemoryPhiFactoring>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReconstructError {
    pub rule: &'static str,
    pub block: Option<BlockId>,
    pub instruction: Option<usize>,
    pub values: Vec<VReg>,
    pub message: String,
}

impl ReconstructError {
    fn new(
        rule: &'static str,
        block: Option<BlockId>,
        instruction: Option<usize>,
        values: Vec<VReg>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            rule,
            block,
            instruction,
            values,
            message: message.into(),
        }
    }
}

fn stack_color_error(error: super::stack_color::StackColorError) -> ReconstructError {
    let mut values = error.values;
    values.extend(error.homes.into_iter().map(|home| VReg(home.0)));
    values.sort_unstable();
    values.dedup();
    ReconstructError::new(
        error.rule,
        error.block,
        error.instruction,
        values,
        error.message,
    )
}

#[derive(Clone)]
enum MaterializedOp {
    Spill {
        value: LogicalValue,
        home: SpillHome,
    },
    Reload {
        value: LogicalValue,
        home: SpillHome,
        fresh: VReg,
        recipe: Option<PreparedRecipe>,
    },
    HomeTransfer {
        source: LogicalValue,
        source_home: SpillHome,
        destination: LogicalValue,
        destination_home: SpillHome,
        fresh: VReg,
        recipe: Option<PreparedRecipe>,
    },
}

#[derive(Clone)]
struct PreparedRecipe {
    expected: ResolvedRecipe,
    instructions: Vec<MInst>,
}

#[derive(Debug, Clone, Copy)]
struct PendingStateStore {
    block: usize,
    instruction: usize,
    home: crate::native::mir::PackedStateHome,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ReloadMaterialization {
    Recipe(ResolvedRecipe),
    Immediate(u64),
    Stack,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct EdgeReloadShape {
    source: LogicalValue,
    source_home: SpillHome,
    destination: LogicalValue,
    materialization: ReloadMaterialization,
}

struct MaterializedReload {
    shape: EdgeReloadShape,
    final_definition: VReg,
    definitions: Vec<VReg>,
    instruction_count: usize,
}

struct EdgeReloadBundle {
    predecessor: usize,
    successor: usize,
    shape: Vec<EdgeReloadShape>,
    final_definitions: Vec<VReg>,
    definitions: Vec<VReg>,
    instruction_count: usize,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct EdgeReloadGroupKey {
    successor: usize,
    shape: Vec<EdgeReloadShape>,
}

struct SharedReloadPlan {
    bundles: Vec<usize>,
    phi_replacements: Vec<VReg>,
}

#[cfg(test)]
pub(super) fn reconstruct(
    func: &mut MFunction,
    cfg: &NormalizedCfg,
    plan: &SpillPlan,
    reload_recipes: &ReloadRecipeAnalysis,
    timing: bool,
    verify: bool,
) -> Result<ReconstructionResult, ReconstructError> {
    reconstruct_with_spill_budget(func, cfg, plan, reload_recipes, timing, verify, None)
}

pub(super) fn reconstruct_with_spill_budget(
    func: &mut MFunction,
    cfg: &NormalizedCfg,
    plan: &SpillPlan,
    reload_recipes: &ReloadRecipeAnalysis,
    timing: bool,
    verify: bool,
    baseline_spill_budget: Option<usize>,
) -> Result<ReconstructionResult, ReconstructError> {
    let recipe_homes = &plan.recipe_homes;
    let phase = timing.then(crate::timing::now);
    let stack_coloring = super::stack_color::color_spill_plan(
        func,
        cfg,
        plan,
        timing,
        verify,
        baseline_spill_budget,
    )
    .map_err(stack_color_error)?;
    if let Some(start) = phase {
        tracing::debug!(
            "[regalloc-timing] reconstruct stack_color homes={} slots={} elapsed={:?}",
            stack_coloring.offsets.len(),
            stack_coloring.slot_count,
            start.elapsed()
        );
    }
    let stack_offsets = stack_coloring.offsets;
    let frame_size = stack_coloring.frame_size;
    verify_reload_homes(func, plan, &stack_offsets, recipe_homes)?;
    let original_vregs = func.vregs.count() as usize;
    let mut logical_for_vreg = (0..original_vregs)
        .map(|index| plan.logical.of(VReg(index as u32)))
        .collect::<Vec<_>>();
    let mut insertions = HashMap::<(usize, usize), Vec<MaterializedOp>>::default();
    let mut reload_blocks = HashMap::<LogicalValue, BTreeSet<usize>>::default();
    let mut edge_reload_bundles = Vec::<EdgeReloadBundle>::new();
    let phase = timing.then(crate::timing::now);
    for spill in super::ssa_state_home::planned_spills(func, cfg, plan).map_err(|error| {
        ReconstructError::new(
            error.rule,
            error.block,
            error.instruction,
            error.values,
            error.message,
        )
    })? {
        if spill.edge_transfer {
            continue;
        }
        insertions
            .entry((spill.block, spill.instruction))
            .or_default()
            .push(MaterializedOp::Spill {
                value: spill.value,
                home: spill.home,
            });
    }
    let spilled_phis = plan
        .point_ops
        .iter()
        .filter_map(|(_, operation)| match operation {
            PlannedOp::SpillPhi { value, .. } => Some(*value),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    for block in 0..func.blocks.len() {
        func.blocks[block]
            .phis
            .retain(|phi| !spilled_phis.contains(&plan.logical.of(phi.dst)));
    }
    for &(point, operation) in &plan.point_ops {
        if !matches!(operation, PlannedOp::Reload { .. }) {
            continue;
        }
        let Some(&block) = cfg.block_index.get(&point.block) else {
            return Err(ReconstructError::new(
                "RECONSTRUCT.POINT_BLOCK_EXISTS",
                Some(point.block),
                Some(point.instruction),
                vec![VReg(planned_value(operation).0)],
                "spill-plan point names a block outside normalized CFG",
            ));
        };
        let recipe = reload_recipe_at_point(reload_recipes, point, operation, plan)?;
        let _ = materialize_operation(
            func,
            plan,
            block,
            point.instruction,
            operation,
            &mut logical_for_vreg,
            &mut insertions,
            &mut reload_blocks,
            recipe,
        )?;
    }
    for (&(predecessor, successor), operations) in &plan.edge_ops {
        let Some(predecessor_block) = func.blocks.get(predecessor) else {
            return Err(ReconstructError::new(
                "RECONSTRUCT.EDGE_PREDECESSOR_EXISTS",
                None,
                None,
                Vec::new(),
                format!("edge operation predecessor index {predecessor} is outside function"),
            ));
        };
        let predecessor_id = predecessor_block.id;
        let insertion = super::cfg::edge_insertion_point(func, cfg, predecessor, successor)
            .ok_or_else(|| {
                ReconstructError::new(
                    "RECONSTRUCT.EDGE_POINT",
                    Some(predecessor_id),
                    None,
                    Vec::new(),
                    "edge operation has no single-edge materialization point",
                )
            })?;
        let mut bundle = EdgeReloadBundle {
            predecessor,
            successor,
            shape: Vec::new(),
            final_definitions: Vec::new(),
            definitions: Vec::new(),
            instruction_count: 0,
        };
        let mut reloads_only = true;
        let recipes = operations
            .iter()
            .copied()
            .map(|operation| {
                reload_recipe_on_edge(
                    func,
                    reload_recipes,
                    predecessor,
                    successor,
                    insertion,
                    operation,
                    plan,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut operation_index = 0usize;
        while operation_index < operations.len() {
            let operation = operations[operation_index];
            if let PlannedEdgeOp::Reload {
                source,
                source_home,
                destination,
            } = operation
                && let Some(PlannedEdgeOp::Spill {
                    source: spill_source,
                    destination: spill_destination,
                    destination_home,
                }) = operations.get(operation_index + 1).copied()
                && spill_source == destination
                && spill_destination == destination
            {
                reloads_only = false;
                let recipe = recipes[operation_index].clone();
                materialize_edge_home_transfer(
                    func,
                    insertion.block,
                    insertion.instruction,
                    source,
                    source_home,
                    destination,
                    destination_home,
                    &mut logical_for_vreg,
                    &mut insertions,
                    recipe,
                )?;
                operation_index += 2;
                continue;
            }
            let is_reload = match operation {
                PlannedEdgeOp::Spill { .. } => {
                    reloads_only = false;
                    false
                }
                PlannedEdgeOp::Reload { .. } => true,
            };
            let recipe = recipes[operation_index].clone();
            let materialized = materialize_edge_operation(
                func,
                plan,
                insertion.block,
                insertion.instruction,
                operation,
                &mut logical_for_vreg,
                &mut insertions,
                &mut reload_blocks,
                recipe,
            )?;
            if !is_reload {
                debug_assert!(materialized.is_none());
                operation_index += 1;
                continue;
            }
            let Some(materialized) = materialized else {
                return Err(ReconstructError::new(
                    "RECONSTRUCT.EDGE_RELOAD_MATERIALIZED",
                    Some(predecessor_id),
                    None,
                    vec![VReg(edge_source(operation).0)],
                    "edge reload did not produce a materialization",
                ));
            };
            bundle.shape.push(materialized.shape);
            bundle.final_definitions.push(materialized.final_definition);
            bundle.definitions.extend(materialized.definitions);
            bundle.instruction_count = bundle
                .instruction_count
                .checked_add(materialized.instruction_count)
                .ok_or_else(|| {
                    ReconstructError::new(
                        "RECONSTRUCT.EDGE_RELOAD_SIZE_RANGE",
                        Some(predecessor_id),
                        None,
                        Vec::new(),
                        "materialized edge-reload bundle exceeds addressable MIR size",
                    )
                })?;
            operation_index += 1;
        }
        // Bundle sharing rewrites a predecessor suffix. Successor-entry edge
        // operations are already shared by that one edge block and have no
        // predecessor suffix to rewrite.
        if insertion.block == predecessor && reloads_only && !bundle.shape.is_empty() {
            edge_reload_bundles.push(bundle);
        }
    }
    if let Some(start) = phase {
        tracing::debug!(
            "[regalloc-timing] reconstruct materialize_plan insertions={} reload_values={} edge_bundles={} elapsed={:?}",
            insertions.len(),
            reload_blocks.len(),
            edge_reload_bundles.len(),
            start.elapsed()
        );
    }

    let phase = timing.then(crate::timing::now);
    let affected = reload_blocks.keys().copied().collect::<BTreeSet<_>>();
    let mut definition_blocks = HashMap::<LogicalValue, BTreeSet<usize>>::default();
    let mut existing_phi_blocks = HashMap::<LogicalValue, BTreeSet<usize>>::default();
    for (block, mir_block) in func.blocks.iter().enumerate() {
        for phi in &mir_block.phis {
            let logical = reconstruct_logical(&logical_for_vreg, phi.dst, mir_block.id)?;
            if affected.contains(&logical) {
                definition_blocks.entry(logical).or_default().insert(block);
                existing_phi_blocks
                    .entry(logical)
                    .or_default()
                    .insert(block);
            }
        }
        for inst in &mir_block.insts {
            if let Some(definition) = inst.def() {
                let logical = reconstruct_logical(&logical_for_vreg, definition, mir_block.id)?;
                if affected.contains(&logical) {
                    definition_blocks.entry(logical).or_default().insert(block);
                }
            }
        }
    }
    for (logical, blocks) in reload_blocks {
        definition_blocks.entry(logical).or_default().extend(blocks);
    }

    let mut reconstruction_phis = HashMap::<(usize, LogicalValue), VReg>::default();
    for logical in affected {
        let mut has_phi = existing_phi_blocks.remove(&logical).unwrap_or_default();
        let mut queue = definition_blocks
            .get(&logical)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect::<VecDeque<_>>();
        while let Some(definition) = queue.pop_front() {
            for &frontier in &cfg.dominance_frontier[definition] {
                if !plan.w_entry[frontier].contains(&logical) {
                    continue;
                }
                if !has_phi.insert(frontier) {
                    continue;
                }
                let fresh = alloc_fresh(func, &mut logical_for_vreg, logical)?;
                reconstruction_phis.insert((frontier, logical), fresh);
                func.blocks[frontier].phis.push(PhiNode {
                    dst: fresh,
                    sources: Vec::new(),
                });
                queue.push_back(frontier);
            }
        }
    }
    if let Some(start) = phase {
        tracing::debug!(
            "[regalloc-timing] reconstruct place_phis phis={} elapsed={:?}",
            reconstruction_phis.len(),
            start.elapsed()
        );
    }

    let phase = timing.then(crate::timing::now);
    let mut children = vec![Vec::new(); func.blocks.len()];
    for (block, &idom) in cfg.idom.iter().enumerate().skip(1) {
        let Some(idom) = idom else {
            return Err(ReconstructError::new(
                "RECONSTRUCT.DOMINATOR_TREE",
                Some(func.blocks[block].id),
                None,
                Vec::new(),
                "non-entry block has no immediate dominator",
            ));
        };
        children[idom].push(block);
    }
    let mut stacks = HashMap::<LogicalValue, Vec<VReg>>::default();
    let mut recipe_reloads = Vec::<ExpectedMaterializedReload>::new();
    let mut pending_state_stores = Vec::<PendingStateStore>::new();
    let mut state_reloads = Vec::<MaterializedStateReload>::new();
    rename_block(
        0,
        func,
        cfg,
        plan,
        &children,
        &reconstruction_phis,
        &stack_offsets,
        &logical_for_vreg,
        &mut insertions,
        &mut stacks,
        recipe_homes,
        &mut recipe_reloads,
        &mut pending_state_stores,
        &mut state_reloads,
    )?;
    if let Some(start) = phase {
        tracing::debug!(
            "[regalloc-timing] reconstruct rename recipe_reloads={} state_reloads={} elapsed={:?}",
            recipe_reloads.len(),
            state_reloads.len(),
            start.elapsed()
        );
    }
    let phase = timing.then(crate::timing::now);
    let state_stores = resolve_state_store_ordinals(func, &pending_state_stores)?;
    let shared_reload_blocks = share_identical_edge_reload_bundles(
        func,
        &edge_reload_bundles,
        &mut recipe_reloads,
        &mut state_reloads,
    )?;
    let removed = eliminate_dead_definitions(func, &mut recipe_reloads);
    state_reloads.retain(|reload| !removed.contains(&reload.reload));
    if let Some(start) = phase {
        tracing::debug!(
            "[regalloc-timing] reconstruct finalize shared_blocks={} removed_defs={} elapsed={:?}",
            shared_reload_blocks.len(),
            removed.len(),
            start.elapsed()
        );
    }

    Ok(ReconstructionResult {
        frame_size,
        recipe_reloads,
        state_stores,
        state_reloads,
        shared_reload_blocks,
    })
}

fn available_recipe_at_point(
    analysis: &ReloadRecipeAnalysis,
    point: ProgramPoint,
    value: LogicalValue,
) -> Option<&ResolvedRecipe> {
    let query = PointUse {
        block: point.block,
        instruction: point.instruction,
        value: VReg(value.0),
    };
    analysis.resolved_recipe_at_point(query)
}

fn reload_recipe_at_point(
    analysis: &ReloadRecipeAnalysis,
    point: ProgramPoint,
    operation: PlannedOp,
    plan: &SpillPlan,
) -> Result<Option<ResolvedRecipe>, ReconstructError> {
    let PlannedOp::Reload { value, home } = operation else {
        return Ok(None);
    };
    let key = (point.block, point.instruction, value);
    if let Some(recipe) = plan.state_reload_recipes.get(&key) {
        return Ok(Some(recipe.clone()));
    }
    if plan.state_homes.contains_key(&home) {
        return Err(ReconstructError::new(
            "RECONSTRUCT.POINT_STATE_HOME_RECIPE",
            Some(point.block),
            Some(point.instruction),
            vec![VReg(value.0)],
            format!("selected state home {home:?} has no exact reload recipe"),
        ));
    }
    let planned_recipe = plan.recipe_reloads.contains(&key);
    if !plan.recipe_homes.contains(&home) && !planned_recipe {
        return Ok(None);
    }
    available_recipe_at_point(analysis, point, value)
        .cloned()
        .map(Some)
        .ok_or_else(|| {
            ReconstructError::new(
                "RECONSTRUCT.POINT_RECIPE_STABLE",
                Some(point.block),
                Some(point.instruction),
                vec![VReg(value.0)],
                "planned MemorySSA recipe disappeared before reconstruction",
            )
        })
}

fn reload_recipe_on_edge(
    func: &MFunction,
    analysis: &ReloadRecipeAnalysis,
    predecessor: usize,
    successor: usize,
    insertion: super::cfg::EdgeInsertionPoint,
    operation: PlannedEdgeOp,
    plan: &SpillPlan,
) -> Result<Option<ResolvedRecipe>, ReconstructError> {
    let PlannedEdgeOp::Reload {
        source,
        source_home,
        ..
    } = operation
    else {
        return Ok(None);
    };
    let predecessor_id = func.blocks[predecessor].id;
    let successor_id = func.blocks[successor].id;
    let insertion_block = func.blocks[insertion.block].id;
    let point = ProgramPoint {
        block: insertion_block,
        instruction: insertion.instruction,
        side: PointSide::Before,
    };
    let key = (point.block, point.instruction, source);
    if let Some(recipe) = plan.state_reload_recipes.get(&key) {
        return Ok(Some(recipe.clone()));
    }
    if plan.state_homes.contains_key(&source_home) {
        return Err(ReconstructError::new(
            "RECONSTRUCT.EDGE_STATE_HOME_RECIPE",
            Some(predecessor_id),
            None,
            vec![VReg(source.0)],
            format!(
                "selected state home {source_home:?} has no exact recipe on edge {predecessor_id} -> {successor_id}"
            ),
        ));
    }
    if !plan.recipe_homes.contains(&source_home) {
        return Ok(None);
    }
    available_recipe_at_point(analysis, point, source)
        .cloned()
        .map(Some)
        .ok_or_else(|| {
            ReconstructError::new(
                "RECONSTRUCT.EDGE_RECIPE_STABLE",
                Some(predecessor_id),
                None,
                vec![VReg(source.0)],
                format!("state recipe disappeared on edge {predecessor_id} -> {successor_id}"),
            )
        })
}

pub(super) fn hazardous_edge_state_homes(
    func: &MFunction,
    cfg: &NormalizedCfg,
    plan: &SpillPlan,
    analysis: &ReloadRecipeAnalysis,
) -> Result<BTreeSet<SpillHome>, ReconstructError> {
    let mut hazardous = BTreeSet::new();
    for (&(predecessor, successor), operations) in &plan.edge_ops {
        let insertion = super::cfg::edge_insertion_point(func, cfg, predecessor, successor)
            .ok_or_else(|| {
                ReconstructError::new(
                    "RECONSTRUCT.EDGE_POINT",
                    func.blocks.get(predecessor).map(|block| block.id),
                    None,
                    Vec::new(),
                    "edge operation has no single-edge materialization point",
                )
            })?;
        let recipes = operations
            .iter()
            .copied()
            .map(|operation| {
                reload_recipe_on_edge(
                    func,
                    analysis,
                    predecessor,
                    successor,
                    insertion,
                    operation,
                    plan,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        hazardous.extend(hazardous_state_home_writes(
            operations,
            &recipes,
            &plan.state_homes,
        ));
    }
    Ok(hazardous)
}

/// Mark every SSA definition reachable from an observable instruction and
/// remove the rest after spill reconstruction.
///
/// Reconstruction can replace every use of an original state-backed value by
/// point reloads. Limiting cleanup to the newly inserted reload definitions
/// leaves that original load (and any pure producer chain feeding it) in the
/// emitted program even though its value is overwritten immediately. Marking
/// through both instruction and phi definitions removes those chains and dead
/// cyclic phi webs in one linear graph walk.
fn eliminate_dead_definitions(
    func: &mut MFunction,
    recipe_reloads: &mut Vec<ExpectedMaterializedReload>,
) -> BTreeSet<VReg> {
    let mut definition_inputs = HashMap::<VReg, Vec<VReg>>::default();
    let mut work = Vec::<VReg>::new();
    for block in &func.blocks {
        for phi in &block.phis {
            definition_inputs.insert(
                phi.dst,
                phi.sources.iter().map(|(_, source)| *source).collect(),
            );
        }
        for instruction in &block.insts {
            let inputs = instruction.uses().into_iter().collect::<Vec<_>>();
            if let Some(definition) = instruction.def() {
                definition_inputs.insert(definition, inputs);
            } else {
                work.extend(inputs);
            }
        }
    }

    let mut required = BTreeSet::<VReg>::new();
    while let Some(value) = work.pop() {
        if !required.insert(value) {
            continue;
        }
        if let Some(inputs) = definition_inputs.get(&value) {
            work.extend(inputs.iter().copied());
        }
    }

    let removed = definition_inputs
        .keys()
        .copied()
        .filter(|definition| !required.contains(definition))
        .collect::<BTreeSet<_>>();
    for block in &mut func.blocks {
        block.phis.retain(|phi| required.contains(&phi.dst));
        block.insts.retain(|instruction| {
            instruction
                .def()
                .is_none_or(|definition| required.contains(&definition))
        });
    }
    // These definitions were intentionally erased, so they no longer denote
    // materialized reloads in final MIR. Retain every other expectation: a
    // definition missing for any other reason must still fail the independent
    // verifier instead of being hidden here.
    recipe_reloads.retain(|reload| !removed.contains(&reload.reload));
    removed
}

fn resolve_state_store_ordinals(
    func: &MFunction,
    pending: &[PendingStateStore],
) -> Result<Vec<MaterializedStateStore>, ReconstructError> {
    let mut by_location = HashMap::with_capacity_and_hasher(pending.len(), Default::default());
    for store in pending {
        if by_location
            .insert((store.block, store.instruction), store.home)
            .is_some()
        {
            return Err(ReconstructError::new(
                "RECONSTRUCT.STATE_STORE_UNIQUE",
                func.blocks.get(store.block).map(|block| block.id),
                Some(store.instruction),
                Vec::new(),
                "more than one state-home store occupies one MIR instruction",
            ));
        }
    }
    let mut result = Vec::with_capacity(pending.len());
    for (block, row) in func.blocks.iter().enumerate() {
        let mut write_ordinal = 0usize;
        for (instruction, inst) in row.insts.iter().enumerate() {
            let writes = memory_effect::writes(inst);
            let affects_state = writes.unknown_memory()
                == Some(UnknownMemory::Direct(BaseReg::SimState))
                || writes.ranges().any(|range| range.base == BaseReg::SimState);
            if let Some(&home) = by_location.get(&(block, instruction)) {
                if !affects_state
                    || !matches!(
                        inst,
                        MInst::Store {
                            base: BaseReg::SimState,
                            offset,
                            size,
                            ..
                        } if *offset == home.offset && *size == home.size
                    )
                {
                    return Err(ReconstructError::new(
                        "RECONSTRUCT.STATE_STORE_SHAPE",
                        Some(row.id),
                        Some(instruction),
                        Vec::new(),
                        format!("state-home store does not materialize {home:?}"),
                    ));
                }
                result.push(MaterializedStateStore {
                    block: row.id,
                    write_ordinal,
                    home,
                });
            }
            if affects_state {
                write_ordinal = write_ordinal.checked_add(1).ok_or_else(|| {
                    ReconstructError::new(
                        "RECONSTRUCT.STATE_STORE_ORDINAL_RANGE",
                        Some(row.id),
                        Some(instruction),
                        Vec::new(),
                        "per-block SimState write ordinal exceeds usize",
                    )
                })?;
            }
        }
    }
    if result.len() != pending.len() {
        return Err(ReconstructError::new(
            "RECONSTRUCT.STATE_STORE_LOCATION",
            None,
            None,
            Vec::new(),
            format!(
                "resolved {} of {} state-home store locations",
                result.len(),
                pending.len()
            ),
        ));
    }
    Ok(result)
}

fn verify_reload_homes(
    func: &MFunction,
    plan: &SpillPlan,
    stack_offsets: &HashMap<SpillHome, i32>,
    recipe_homes: &BTreeSet<SpillHome>,
) -> Result<(), ReconstructError> {
    for &(point, operation) in &plan.point_ops {
        if let PlannedOp::Reload { value, home } = operation
            && rematerialized_logical_value(func, value).is_none()
            && !recipe_homes.contains(&home)
            && !plan.state_homes.contains_key(&home)
            && !plan
                .recipe_reloads
                .contains(&(point.block, point.instruction, value))
        {
            if !stack_offsets.contains_key(&home) {
                return Err(ReconstructError::new(
                    "RECONSTRUCT.RELOAD_HOME_EXISTS",
                    Some(point.block),
                    Some(point.instruction),
                    vec![VReg(value.0)],
                    format!(
                        "reload has no spill home {home:?}; {}",
                        describe_missing_home(func, plan, value, home)
                    ),
                ));
            }
        }
    }
    for (&edge, operations) in &plan.edge_ops {
        for &operation in operations {
            if let PlannedEdgeOp::Reload {
                source,
                source_home,
                ..
            } = operation
                && rematerialized_logical_value(func, source).is_none()
                && !recipe_homes.contains(&source_home)
                && !plan.state_homes.contains_key(&source_home)
            {
                if !stack_offsets.contains_key(&source_home) {
                    let block = func.blocks.get(edge.0).map(|block| block.id);
                    return Err(ReconstructError::new(
                        "RECONSTRUCT.RELOAD_HOME_EXISTS",
                        block,
                        None,
                        vec![VReg(source.0)],
                        format!(
                            "edge reload has no source home {source_home:?}; {}",
                            describe_missing_home(func, plan, source, source_home)
                        ),
                    ));
                }
            }
        }
    }
    Ok(())
}

fn describe_missing_home(
    func: &MFunction,
    plan: &SpillPlan,
    logical: LogicalValue,
    home: SpillHome,
) -> String {
    let definitions = func
        .blocks
        .iter()
        .flat_map(|block| {
            block
                .phis
                .iter()
                .filter(move |phi| phi.dst.0 == logical.0)
                .map(move |_| format!("{}:phi", block.id))
                .chain(
                    block
                        .insts
                        .iter()
                        .enumerate()
                        .filter(move |(_, inst)| inst.def().is_some_and(|dst| dst.0 == logical.0))
                        .map(move |(instruction, _)| format!("{}:i{instruction}", block.id)),
                )
        })
        .collect::<Vec<_>>();
    let states = if plan.s_entry.is_empty() {
        vec!["scheduling states released; only reconstruction states retained".to_owned()]
    } else {
        func.blocks
            .iter()
            .enumerate()
            .filter(|(block, _)| {
                plan.w_entry[*block].contains(&logical)
                    || plan.s_entry[*block].contains(&logical)
                    || plan.w_exit[*block].contains(&logical)
                    || plan.s_exit[*block].contains(&logical)
            })
            .take(24)
            .map(|(block, mir_block)| {
                format!(
                    "{}:[W{} S{} -> W{} S{}]",
                    mir_block.id,
                    u8::from(plan.w_entry[block].contains(&logical)),
                    u8::from(plan.s_entry[block].contains(&logical)),
                    u8::from(plan.w_exit[block].contains(&logical)),
                    u8::from(plan.s_exit[block].contains(&logical))
                )
            })
            .collect::<Vec<_>>()
    };
    let operations = plan
        .point_ops
        .iter()
        .filter(|(_, operation)| match operation {
            PlannedOp::Spill { home: op_home, .. }
            | PlannedOp::Reload { home: op_home, .. }
            | PlannedOp::SpillPhi { home: op_home, .. } => *op_home == home,
        })
        .take(24)
        .map(|(point, operation)| format!("{point:?}:{operation:?}"))
        .collect::<Vec<_>>();
    format!("defs={definitions:?} states={states:?} ops={operations:?}")
}

#[allow(clippy::too_many_arguments)]
fn materialize_operation(
    func: &mut MFunction,
    plan: &SpillPlan,
    block: usize,
    instruction: usize,
    operation: PlannedOp,
    logical_for_vreg: &mut Vec<LogicalValue>,
    insertions: &mut HashMap<(usize, usize), Vec<MaterializedOp>>,
    reload_blocks: &mut HashMap<LogicalValue, BTreeSet<usize>>,
    recipe: Option<ResolvedRecipe>,
) -> Result<Option<MaterializedReload>, ReconstructError> {
    let (operation, reload) = match operation {
        PlannedOp::Spill { value, home } | PlannedOp::SpillPhi { value, home } => {
            (MaterializedOp::Spill { value, home }, None)
        }
        PlannedOp::Reload { value, home } => {
            let materialization = recipe.as_ref().map_or_else(
                || {
                    rematerialized_logical_value(func, value).map_or(
                        ReloadMaterialization::Stack,
                        ReloadMaterialization::Immediate,
                    )
                },
                |recipe| ReloadMaterialization::Recipe(recipe.clone()),
            );
            let (fresh, recipe, definitions, instruction_count) = if let Some(recipe) = recipe {
                let (fresh, prepared) = prepare_recipe(func, logical_for_vreg, value, recipe)?;
                let definitions = prepared
                    .instructions
                    .iter()
                    .filter_map(MInst::def)
                    .collect::<Vec<_>>();
                let instruction_count = prepared.instructions.len();
                (fresh, Some(prepared), definitions, instruction_count)
            } else {
                let fresh = alloc_fresh(func, logical_for_vreg, value)?;
                (fresh, None, vec![fresh], 1)
            };
            reload_blocks.entry(value).or_default().insert(block);
            (
                MaterializedOp::Reload {
                    value,
                    home,
                    fresh,
                    recipe,
                },
                Some(MaterializedReload {
                    shape: EdgeReloadShape {
                        source: value,
                        source_home: home,
                        destination: value,
                        materialization,
                    },
                    final_definition: fresh,
                    definitions,
                    instruction_count,
                }),
            )
        }
    };
    let _ = plan;
    insertions
        .entry((block, instruction))
        .or_default()
        .push(operation);
    Ok(reload)
}

#[allow(clippy::too_many_arguments)]
fn materialize_edge_operation(
    func: &mut MFunction,
    plan: &SpillPlan,
    block: usize,
    instruction: usize,
    operation: PlannedEdgeOp,
    logical_for_vreg: &mut Vec<LogicalValue>,
    insertions: &mut HashMap<(usize, usize), Vec<MaterializedOp>>,
    reload_blocks: &mut HashMap<LogicalValue, BTreeSet<usize>>,
    recipe: Option<ResolvedRecipe>,
) -> Result<Option<MaterializedReload>, ReconstructError> {
    if matches!(operation, PlannedEdgeOp::Spill { .. }) {
        // All standalone point and edge spills come from the one
        // `planned_spills` expansion.  Materializing them again here would
        // duplicate every edge store.  Reload+spill home transfers are
        // recognized and emitted atomically by the caller.
        return Ok(None);
    }
    let (materialized, reload) = match operation {
        PlannedEdgeOp::Spill { .. } => unreachable!(),
        PlannedEdgeOp::Reload {
            source,
            source_home,
            destination,
        } => {
            let materialization = recipe.as_ref().map_or_else(
                || {
                    rematerialized_logical_value(func, source).map_or(
                        ReloadMaterialization::Stack,
                        ReloadMaterialization::Immediate,
                    )
                },
                |recipe| ReloadMaterialization::Recipe(recipe.clone()),
            );
            let (fresh, prepared, definitions, instruction_count) = if let Some(recipe) = recipe {
                let (fresh, prepared) = prepare_recipe(func, logical_for_vreg, source, recipe)?;
                let definitions = prepared
                    .instructions
                    .iter()
                    .filter_map(MInst::def)
                    .collect::<Vec<_>>();
                let instruction_count = prepared.instructions.len();
                (fresh, Some(prepared), definitions, instruction_count)
            } else {
                let fresh = alloc_fresh(func, logical_for_vreg, source)?;
                (fresh, None, vec![fresh], 1)
            };
            reload_blocks.entry(source).or_default().insert(block);
            (
                MaterializedOp::Reload {
                    value: source,
                    home: source_home,
                    fresh,
                    recipe: prepared,
                },
                Some(MaterializedReload {
                    shape: EdgeReloadShape {
                        source,
                        source_home,
                        destination,
                        materialization,
                    },
                    final_definition: fresh,
                    definitions,
                    instruction_count,
                }),
            )
        }
    };
    let _ = plan;
    insertions
        .entry((block, instruction))
        .or_default()
        .push(materialized);
    Ok(reload)
}

#[allow(clippy::too_many_arguments)]
fn materialize_edge_home_transfer(
    func: &mut MFunction,
    block: usize,
    instruction: usize,
    source: LogicalValue,
    source_home: SpillHome,
    destination: LogicalValue,
    destination_home: SpillHome,
    logical_for_vreg: &mut Vec<LogicalValue>,
    insertions: &mut HashMap<(usize, usize), Vec<MaterializedOp>>,
    recipe: Option<ResolvedRecipe>,
) -> Result<(), ReconstructError> {
    let (fresh, recipe) = if let Some(recipe) = recipe {
        let (fresh, prepared) = prepare_recipe(func, logical_for_vreg, destination, recipe)?;
        (fresh, Some(prepared))
    } else {
        (alloc_fresh(func, logical_for_vreg, destination)?, None)
    };
    insertions
        .entry((block, instruction))
        .or_default()
        .push(MaterializedOp::HomeTransfer {
            source,
            source_home,
            destination,
            destination_home,
            fresh,
            recipe,
        });
    Ok(())
}

fn prepare_recipe(
    func: &mut MFunction,
    logical_for_vreg: &mut Vec<LogicalValue>,
    logical: LogicalValue,
    expected: ResolvedRecipe,
) -> Result<(VReg, PreparedRecipe), ReconstructError> {
    let mut instructions = Vec::with_capacity(expected.steps.len() + 1);
    // W/S reserves one register for each reload result, but not an additional
    // register for a recipe prefix retained across reloads. Materialize every
    // recipe as its own linear chain so each intermediate dies when the next
    // one is defined and can reuse the same physical register.
    let mut current = alloc_fresh(func, logical_for_vreg, logical)?;
    instructions.push(materialize_recipe_base(&expected.base, current));
    for &step in &expected.steps {
        let destination = alloc_fresh(func, logical_for_vreg, logical)?;
        instructions.push(materialize_pure_step(step, destination, current));
        current = destination;
    }
    Ok((
        current,
        PreparedRecipe {
            expected,
            instructions,
        },
    ))
}

fn hazardous_state_home_writes(
    operations: &[PlannedEdgeOp],
    recipes: &[Option<ResolvedRecipe>],
    state_homes: &std::collections::BTreeMap<SpillHome, crate::native::mir::PackedStateHome>,
) -> BTreeSet<SpillHome> {
    let state_loads = recipes
        .iter()
        .filter_map(|recipe| match recipe.as_ref().map(|recipe| &recipe.base) {
            Some(ResolvedBase::State(state)) => Some(state.load),
            Some(ResolvedBase::Constant(_)) | None => None,
        })
        .collect::<Vec<_>>();
    operations
        .iter()
        .filter_map(|operation| match operation {
            PlannedEdgeOp::Spill {
                destination_home, ..
            } => state_homes.get(destination_home).copied().and_then(|home| {
                state_loads
                    .iter()
                    .any(|&load| state_load_overlaps_home(load, home))
                    .then_some(*destination_home)
            }),
            PlannedEdgeOp::Reload { .. } => None,
        })
        .collect()
}

fn state_load_overlaps_home(
    load: super::reload::StateLoad,
    home: crate::native::mir::PackedStateHome,
) -> bool {
    let load_start = i64::from(load.offset);
    let Some(load_end) = load_start.checked_add(i64::from(load.size.bytes())) else {
        return true;
    };
    let home_start = i64::from(home.offset);
    let Some(home_end) = home_start.checked_add(i64::from(home.size.bytes())) else {
        return true;
    };
    load_start < home_end && home_start < load_end
}

fn materialize_recipe_base(base: &ResolvedBase, destination: VReg) -> MInst {
    match base {
        ResolvedBase::Constant(value) => MInst::LoadImm {
            dst: destination,
            value: *value,
        },
        ResolvedBase::State(state) => MInst::Load {
            dst: destination,
            base: BaseReg::SimState,
            offset: state.load.offset,
            size: state.load.size,
        },
    }
}

fn prepared_recipe_is_direct_state_home_reload(
    recipe: &PreparedRecipe,
    destination: VReg,
    home: crate::native::mir::PackedStateHome,
) -> bool {
    matches!(
        recipe.instructions.as_slice(),
        [MInst::Load {
            dst,
            base: BaseReg::SimState,
            offset,
            size,
        }] if *dst == destination && *offset == home.offset && *size == home.size
    )
}

#[allow(clippy::too_many_arguments)]
fn rename_block(
    root: usize,
    func: &mut MFunction,
    cfg: &NormalizedCfg,
    plan: &SpillPlan,
    children: &[Vec<usize>],
    reconstruction_phis: &HashMap<(usize, LogicalValue), VReg>,
    stack_offsets: &HashMap<SpillHome, i32>,
    logical_for_vreg: &[LogicalValue],
    insertions: &mut HashMap<(usize, usize), Vec<MaterializedOp>>,
    stacks: &mut HashMap<LogicalValue, Vec<VReg>>,
    recipe_homes: &BTreeSet<SpillHome>,
    recipe_reloads: &mut Vec<ExpectedMaterializedReload>,
    pending_state_stores: &mut Vec<PendingStateStore>,
    state_reloads: &mut Vec<MaterializedStateReload>,
) -> Result<(), ReconstructError> {
    enum Event {
        Enter(usize),
        Exit(Vec<LogicalValue>),
    }
    let mut work = vec![Event::Enter(root)];
    while let Some(event) = work.pop() {
        match event {
            Event::Exit(pushed) => {
                for logical in pushed.into_iter().rev() {
                    let Some(stack) = stacks.get_mut(&logical) else {
                        return Err(ReconstructError::new(
                            "RECONSTRUCT.RENAME_STACK_BALANCED",
                            None,
                            None,
                            vec![VReg(logical.0)],
                            "representative stack disappeared before dominator exit",
                        ));
                    };
                    if stack.pop().is_none() {
                        return Err(ReconstructError::new(
                            "RECONSTRUCT.RENAME_STACK_BALANCED",
                            None,
                            None,
                            vec![VReg(logical.0)],
                            "representative stack underflow at dominator exit",
                        ));
                    }
                }
            }
            Event::Enter(block) => {
                let mut pushed = Vec::<LogicalValue>::new();
                let block_id = func.blocks[block].id;
                for phi in &func.blocks[block].phis {
                    let logical = reconstruct_logical(logical_for_vreg, phi.dst, block_id)?;
                    stacks.entry(logical).or_default().push(phi.dst);
                    pushed.push(logical);
                }
                let original = std::mem::take(&mut func.blocks[block].insts);
                let mut rewritten = Vec::with_capacity(original.len());
                for (instruction, mut inst) in original.into_iter().enumerate() {
                    emit_insertions(
                        block,
                        instruction,
                        func,
                        plan,
                        stack_offsets,
                        logical_for_vreg,
                        insertions,
                        stacks,
                        &mut pushed,
                        &mut rewritten,
                        recipe_homes,
                        recipe_reloads,
                        pending_state_stores,
                        state_reloads,
                    )?;
                    let uses = inst.uses().into_iter().collect::<BTreeSet<_>>();
                    for original_use in uses {
                        let logical =
                            reconstruct_logical(logical_for_vreg, original_use, block_id)?;
                        if let Some(&representative) =
                            stacks.get(&logical).and_then(|stack| stack.last())
                        {
                            inst.rewrite_use(original_use, representative);
                        }
                    }
                    if let Some(definition) = inst.def() {
                        let logical = reconstruct_logical(logical_for_vreg, definition, block_id)?;
                        stacks.entry(logical).or_default().push(definition);
                        pushed.push(logical);
                    }
                    rewritten.push(inst);
                }
                func.blocks[block].insts = rewritten;

                let predecessor_id = func.blocks[block].id;
                for &successor in &cfg.successors[block] {
                    let successor_id = func.blocks[successor].id;
                    for phi in &mut func.blocks[successor].phis {
                        let destination_logical =
                            reconstruct_logical(logical_for_vreg, phi.dst, successor_id)?;
                        if reconstruction_phis.contains_key(&(successor, destination_logical)) {
                            let Some(&representative) = stacks
                                .get(&destination_logical)
                                .and_then(|stack| stack.last())
                            else {
                                return Err(ReconstructError::new(
                                    "RECONSTRUCT.PHI_REPRESENTATIVE_EXISTS",
                                    Some(successor_id),
                                    None,
                                    vec![phi.dst, VReg(destination_logical.0)],
                                    format!(
                                        "reconstruction phi has no representative from {predecessor_id}"
                                    ),
                                ));
                            };
                            phi.sources.push((predecessor_id, representative));
                        } else if let Some(source) = phi
                            .sources
                            .iter_mut()
                            .find(|(source_predecessor, _)| *source_predecessor == predecessor_id)
                        {
                            let source_logical =
                                reconstruct_logical(logical_for_vreg, source.1, successor_id)?;
                            if let Some(&representative) =
                                stacks.get(&source_logical).and_then(|stack| stack.last())
                            {
                                source.1 = representative;
                            }
                        }
                    }
                }
                work.push(Event::Exit(pushed));
                work.extend(children[block].iter().rev().copied().map(Event::Enter));
            }
        }
    }
    Ok(())
}

/// Tail-merge identical edge reload bundles before coloring.
///
/// Braun--Hack coupling is edge-sensitive: when several high-pressure arms
/// evict the same live-ins, reconstruction would otherwise copy the same
/// reload sequence into every arm and then merge the fresh representatives
/// with one phi per logical value.  A shared block preserves exactly which
/// paths execute the reloads while materializing the identical sequence only
/// once.  Stack and SimState recipes are grouped only when their complete
/// logical/home/MemorySSA shapes match.
fn share_identical_edge_reload_bundles(
    func: &mut MFunction,
    bundles: &[EdgeReloadBundle],
    recipe_reloads: &mut Vec<ExpectedMaterializedReload>,
    state_reloads: &mut Vec<MaterializedStateReload>,
) -> Result<Vec<MemoryPhiFactoring>, ReconstructError> {
    let mut grouped = HashMap::<EdgeReloadGroupKey, Vec<usize>>::default();
    for (bundle, edge) in bundles.iter().enumerate() {
        grouped
            .entry(EdgeReloadGroupKey {
                successor: edge.successor,
                shape: edge.shape.clone(),
            })
            .or_default()
            .push(bundle);
    }

    let mut groups = grouped
        .into_values()
        .filter(|group| group.len() >= 2)
        .collect::<Vec<_>>();
    for group in &mut groups {
        group.sort_unstable_by_key(|bundle| bundles[*bundle].predecessor);
    }
    groups.sort_unstable_by_key(|group| {
        let first = &bundles[group[0]];
        (first.successor, first.predecessor)
    });

    let mut plans = Vec::<SharedReloadPlan>::new();
    for group in groups {
        let Some(phi_replacements) = shared_reload_phi_replacements(func, bundles, &group) else {
            continue;
        };
        plans.push(SharedReloadPlan {
            bundles: group,
            phi_replacements,
        });
    }
    if plans.is_empty() {
        return Ok(Vec::new());
    }

    let maximum_id = func
        .blocks
        .iter()
        .map(|block| block.id.0)
        .max()
        .unwrap_or(0);
    let additional = u32::try_from(plans.len()).map_err(|_| {
        ReconstructError::new(
            "RECONSTRUCT.SHARED_RELOAD_BLOCK_ID",
            None,
            None,
            Vec::new(),
            "shared reload block count exceeds the BlockId range",
        )
    })?;
    maximum_id.checked_add(additional).ok_or_else(|| {
        ReconstructError::new(
            "RECONSTRUCT.SHARED_RELOAD_BLOCK_ID",
            None,
            None,
            Vec::new(),
            "BlockId overflow while sharing edge reload bundles",
        )
    })?;

    let mut removed_recipe_reloads = BTreeSet::<VReg>::new();
    let mut memory_phi_factorings = Vec::with_capacity(plans.len());
    for (shared_offset, plan) in plans.into_iter().enumerate() {
        let canonical_index = plan.bundles[0];
        let canonical = &bundles[canonical_index];
        let successor_id = func.blocks[canonical.successor].id;
        let shared_offset = u32::try_from(shared_offset + 1).map_err(|_| {
            ReconstructError::new(
                "RECONSTRUCT.SHARED_RELOAD_BLOCK_ID",
                None,
                None,
                Vec::new(),
                "shared reload block offset exceeds the BlockId range",
            )
        })?;
        let shared_id = BlockId(maximum_id + shared_offset);
        let predecessor_ids = plan
            .bundles
            .iter()
            .map(|bundle| func.blocks[bundles[*bundle].predecessor].id)
            .collect::<BTreeSet<_>>();

        let mut shared_instructions = Vec::<MInst>::new();
        for &bundle_index in &plan.bundles {
            let bundle = &bundles[bundle_index];
            let predecessor = &mut func.blocks[bundle.predecessor];
            let terminator = predecessor.insts.pop().ok_or_else(|| {
                ReconstructError::new(
                    "RECONSTRUCT.SHARED_RELOAD_SUFFIX",
                    Some(predecessor.id),
                    None,
                    bundle.definitions.clone(),
                    "shared edge-reload predecessor lost its terminator",
                )
            })?;
            let suffix_start = predecessor
                .insts
                .len()
                .checked_sub(bundle.instruction_count)
                .ok_or_else(|| {
                    ReconstructError::new(
                        "RECONSTRUCT.SHARED_RELOAD_SUFFIX",
                        Some(predecessor.id),
                        None,
                        bundle.definitions.clone(),
                        "shared edge-reload suffix no longer fits its predecessor",
                    )
                })?;
            let suffix = predecessor.insts.split_off(suffix_start);
            if bundle_index == canonical_index {
                shared_instructions = suffix;
            } else {
                removed_recipe_reloads.extend(bundle.final_definitions.iter().copied());
            }
            if terminator
                != (MInst::Jump {
                    target: successor_id,
                })
            {
                return Err(ReconstructError::new(
                    "RECONSTRUCT.SHARED_RELOAD_SUFFIX",
                    Some(predecessor.id),
                    None,
                    bundle.definitions.clone(),
                    "shared edge-reload predecessor changed after eligibility checking",
                ));
            }
            predecessor.push(MInst::Jump { target: shared_id });
        }

        let successor = &mut func.blocks[canonical.successor];
        for (phi, replacement) in successor.phis.iter_mut().zip(plan.phi_replacements) {
            let original = std::mem::take(&mut phi.sources);
            let mut rewritten = Vec::with_capacity(
                original
                    .len()
                    .saturating_sub(predecessor_ids.len())
                    .saturating_add(1),
            );
            let mut inserted = false;
            for (predecessor, source) in original {
                if predecessor_ids.contains(&predecessor) {
                    if !inserted {
                        rewritten.push((shared_id, replacement));
                        inserted = true;
                    }
                } else {
                    rewritten.push((predecessor, source));
                }
            }
            if !inserted {
                return Err(ReconstructError::new(
                    "RECONSTRUCT.SHARED_RELOAD_PHI",
                    Some(successor.id),
                    None,
                    vec![phi.dst],
                    "shared edge-reload phi lost every grouped predecessor",
                ));
            }
            phi.sources = rewritten;
        }

        let mut shared = MBlock::new(shared_id);
        shared.insts = shared_instructions;
        shared.push(MInst::Jump {
            target: successor_id,
        });
        func.blocks.push(shared);
        memory_phi_factorings.push(MemoryPhiFactoring {
            block: shared_id,
            successor: successor_id,
            predecessors: predecessor_ids
                .iter()
                .copied()
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        });
    }

    recipe_reloads.retain(|reload| !removed_recipe_reloads.contains(&reload.reload));
    state_reloads.retain(|reload| !removed_recipe_reloads.contains(&reload.reload));
    Ok(memory_phi_factorings)
}

fn shared_reload_phi_replacements(
    func: &MFunction,
    bundles: &[EdgeReloadBundle],
    group: &[usize],
) -> Option<Vec<VReg>> {
    let canonical = bundles.get(*group.first()?)?;
    let successor = func.blocks.get(canonical.successor)?;
    if group.iter().any(|bundle_index| {
        let bundle = &bundles[*bundle_index];
        if bundle.successor != canonical.successor
            || bundle.instruction_count == 0
            || bundle.shape != canonical.shape
            || bundle.final_definitions.len() != canonical.final_definitions.len()
        {
            return true;
        }
        let Some(predecessor) = func.blocks.get(bundle.predecessor) else {
            return true;
        };
        if !matches!(
            predecessor.insts.last(),
            Some(MInst::Jump { target }) if *target == successor.id
        ) {
            return true;
        }
        let Some(required) = bundle.instruction_count.checked_add(1) else {
            return true;
        };
        let Some(suffix_start) = predecessor.insts.len().checked_sub(required) else {
            return true;
        };
        predecessor.insts[suffix_start..predecessor.insts.len() - 1]
            .iter()
            .filter_map(MInst::def)
            .ne(bundle.definitions.iter().copied())
    }) {
        return None;
    }

    let mut replacements = Vec::with_capacity(successor.phis.len());
    for phi in &successor.phis {
        let mut sources = Vec::with_capacity(group.len());
        for &bundle_index in group {
            let predecessor_id = func.blocks[bundles[bundle_index].predecessor].id;
            let source = phi.sources.iter().find_map(|&(predecessor, source)| {
                (predecessor == predecessor_id).then_some(source)
            })?;
            sources.push(source);
        }
        if sources.iter().all(|source| *source == sources[0]) {
            replacements.push(sources[0]);
            continue;
        }
        let replacement = (0..canonical.final_definitions.len()).find_map(|position| {
            group
                .iter()
                .enumerate()
                .all(|(edge, bundle)| sources[edge] == bundles[*bundle].final_definitions[position])
                .then_some(canonical.final_definitions[position])
        })?;
        replacements.push(replacement);
    }
    Some(replacements)
}

#[allow(clippy::too_many_arguments)]
fn emit_insertions(
    block: usize,
    instruction: usize,
    func: &MFunction,
    plan: &SpillPlan,
    stack_offsets: &HashMap<SpillHome, i32>,
    logical_for_vreg: &[LogicalValue],
    insertions: &mut HashMap<(usize, usize), Vec<MaterializedOp>>,
    stacks: &mut HashMap<LogicalValue, Vec<VReg>>,
    pushed: &mut Vec<LogicalValue>,
    output: &mut Vec<MInst>,
    recipe_homes: &BTreeSet<SpillHome>,
    recipe_reloads: &mut Vec<ExpectedMaterializedReload>,
    pending_state_stores: &mut Vec<PendingStateStore>,
    state_reloads: &mut Vec<MaterializedStateReload>,
) -> Result<(), ReconstructError> {
    let mut operations = insertions.remove(&(block, instruction)).unwrap_or_default();
    // A SpillPlan program point is parallel.  When materialized serially,
    // evictions must free their registers before operand reloads consume them.
    operations.sort_by_key(|operation| match operation {
        MaterializedOp::Spill { .. } => 0,
        MaterializedOp::HomeTransfer { .. } => 1,
        MaterializedOp::Reload { .. } => 2,
    });
    for operation in operations {
        match operation {
            MaterializedOp::Spill {
                value: logical,
                home,
            } => {
                if is_rematerializable(func, plan, home) || recipe_homes.contains(&home) {
                    continue;
                }
                let Some(source) = stacks
                    .get(&logical)
                    .and_then(|representatives| representatives.last())
                    .copied()
                else {
                    return Err(ReconstructError::new(
                        "RECONSTRUCT.SPILL_REPRESENTATIVE_EXISTS",
                        func.blocks.get(block).map(|block| block.id),
                        Some(instruction),
                        vec![VReg(logical.0)],
                        "spill is not dominated by a logical definition",
                    ));
                };
                if let Some(physical) = plan.state_homes.get(&home) {
                    pending_state_stores.push(PendingStateStore {
                        block,
                        instruction: output.len(),
                        home: *physical,
                    });
                    output.push(MInst::Store {
                        base: BaseReg::SimState,
                        offset: physical.offset,
                        src: source,
                        size: physical.size,
                    });
                    continue;
                }
                let Some(&offset) = stack_offsets.get(&home) else {
                    return Err(ReconstructError::new(
                        "RECONSTRUCT.SPILL_HOME_EXISTS",
                        func.blocks.get(block).map(|block| block.id),
                        Some(instruction),
                        vec![VReg(logical.0)],
                        format!("spill home {home:?} has no frame offset"),
                    ));
                };
                output.push(MInst::Store {
                    base: BaseReg::StackFrame,
                    offset,
                    src: source,
                    size: OpSize::S64,
                });
            }
            MaterializedOp::Reload {
                value: logical,
                home,
                fresh,
                recipe,
            } => {
                let reload = if let Some(recipe) = recipe {
                    let Some(definition) = recipe.instructions.last().and_then(MInst::def) else {
                        return Err(ReconstructError::new(
                            "RECONSTRUCT.RECIPE_FINAL_DEFINITION",
                            func.blocks.get(block).map(|block| block.id),
                            Some(instruction),
                            vec![VReg(logical.0), fresh],
                            "prepared reload recipe has no final MIR definition",
                        ));
                    };
                    if definition != fresh {
                        return Err(ReconstructError::new(
                            "RECONSTRUCT.RECIPE_FINAL_IDENTITY",
                            func.blocks.get(block).map(|block| block.id),
                            Some(instruction),
                            vec![VReg(logical.0), fresh, definition],
                            "prepared reload recipe final definition changed before emission",
                        ));
                    }
                    if let Some(physical) = plan.state_homes.get(&home) {
                        state_reloads.push(MaterializedStateReload {
                            reload: fresh,
                            home: *physical,
                        });
                    } else {
                        recipe_reloads.push(ExpectedMaterializedReload {
                            reload: fresh,
                            expected: recipe.expected,
                            planned_use: Some(PointUse {
                                block: func.blocks[block].id,
                                instruction,
                                value: VReg(logical.0),
                            }),
                        });
                    }
                    output.extend(recipe.instructions);
                    stacks.entry(logical).or_default().push(fresh);
                    pushed.push(logical);
                    continue;
                } else if let Some(value) = rematerialized_logical_value(func, logical) {
                    MInst::LoadImm { dst: fresh, value }
                } else {
                    let Some(&offset) = stack_offsets.get(&home) else {
                        return Err(ReconstructError::new(
                            "RECONSTRUCT.RELOAD_HOME_EXISTS",
                            func.blocks.get(block).map(|block| block.id),
                            Some(instruction),
                            vec![VReg(logical.0)],
                            format!("reload home {home:?} has no frame offset"),
                        ));
                    };
                    MInst::Load {
                        dst: fresh,
                        base: BaseReg::StackFrame,
                        offset,
                        size: OpSize::S64,
                    }
                };
                output.push(reload);
                stacks.entry(logical).or_default().push(fresh);
                pushed.push(logical);
            }
            MaterializedOp::HomeTransfer {
                source,
                source_home,
                destination,
                destination_home,
                fresh,
                recipe,
            } => {
                if let Some(recipe) = recipe {
                    let Some(definition) = recipe.instructions.last().and_then(MInst::def) else {
                        return Err(ReconstructError::new(
                            "RECONSTRUCT.RECIPE_FINAL_DEFINITION",
                            func.blocks.get(block).map(|block| block.id),
                            Some(instruction),
                            vec![VReg(source.0), VReg(destination.0), fresh],
                            "prepared edge-transfer recipe has no final MIR definition",
                        ));
                    };
                    if definition != fresh {
                        return Err(ReconstructError::new(
                            "RECONSTRUCT.RECIPE_FINAL_IDENTITY",
                            func.blocks.get(block).map(|block| block.id),
                            Some(instruction),
                            vec![VReg(source.0), VReg(destination.0), fresh, definition],
                            "prepared edge-transfer recipe final definition changed",
                        ));
                    }
                    if let Some(physical) = plan.state_homes.get(&source_home)
                        && prepared_recipe_is_direct_state_home_reload(&recipe, fresh, *physical)
                    {
                        state_reloads.push(MaterializedStateReload {
                            reload: fresh,
                            home: *physical,
                        });
                    } else {
                        recipe_reloads.push(ExpectedMaterializedReload {
                            reload: fresh,
                            expected: recipe.expected,
                            planned_use: Some(PointUse {
                                block: func.blocks[block].id,
                                instruction,
                                value: VReg(source.0),
                            }),
                        });
                    }
                    output.extend(recipe.instructions);
                } else if let Some(value) = rematerialized_logical_value(func, source) {
                    output.push(MInst::LoadImm { dst: fresh, value });
                } else {
                    let Some(&offset) = stack_offsets.get(&source_home) else {
                        return Err(ReconstructError::new(
                            "RECONSTRUCT.RELOAD_HOME_EXISTS",
                            func.blocks.get(block).map(|block| block.id),
                            Some(instruction),
                            vec![VReg(source.0), VReg(destination.0)],
                            format!(
                                "edge transfer source home {source_home:?} has no frame offset"
                            ),
                        ));
                    };
                    output.push(MInst::Load {
                        dst: fresh,
                        base: BaseReg::StackFrame,
                        offset,
                        size: OpSize::S64,
                    });
                }

                if is_rematerializable(func, plan, destination_home)
                    || recipe_homes.contains(&destination_home)
                {
                    continue;
                }
                if let Some(physical) = plan.state_homes.get(&destination_home) {
                    pending_state_stores.push(PendingStateStore {
                        block,
                        instruction: output.len(),
                        home: *physical,
                    });
                    output.push(MInst::Store {
                        base: BaseReg::SimState,
                        offset: physical.offset,
                        src: fresh,
                        size: physical.size,
                    });
                } else {
                    let Some(&offset) = stack_offsets.get(&destination_home) else {
                        return Err(ReconstructError::new(
                            "RECONSTRUCT.SPILL_HOME_EXISTS",
                            func.blocks.get(block).map(|block| block.id),
                            Some(instruction),
                            vec![VReg(source.0), VReg(destination.0)],
                            format!(
                                "edge transfer destination home {destination_home:?} has no frame offset"
                            ),
                        ));
                    };
                    output.push(MInst::Store {
                        base: BaseReg::StackFrame,
                        offset,
                        src: fresh,
                        size: OpSize::S64,
                    });
                }
            }
        }
    }
    let _ = logical_for_vreg;
    Ok(())
}

fn reconstruct_logical(
    logical_for_vreg: &[LogicalValue],
    value: VReg,
    block: BlockId,
) -> Result<LogicalValue, ReconstructError> {
    logical_for_vreg
        .get(value.0 as usize)
        .copied()
        .ok_or_else(|| {
            ReconstructError::new(
                "RECONSTRUCT.LOGICAL_SIDETABLE_COVERS_VREG",
                Some(block),
                None,
                vec![value],
                "logical-value side table does not cover VReg",
            )
        })
}

fn alloc_fresh(
    func: &mut MFunction,
    logical_for_vreg: &mut Vec<LogicalValue>,
    logical: LogicalValue,
) -> Result<VReg, ReconstructError> {
    let fresh = func.vregs.try_alloc().map_err(|error| {
        ReconstructError::new(
            "RECONSTRUCT.VREG_EXHAUSTED",
            None,
            None,
            vec![VReg(logical.0)],
            error.to_string(),
        )
    })?;
    if fresh.0 as usize != func.spill_descs.len() || fresh.0 as usize != logical_for_vreg.len() {
        return Err(ReconstructError::new(
            "RECONSTRUCT.SIDETABLE_APPEND_POSITION",
            None,
            None,
            vec![fresh],
            "fresh VReg does not append consistently to reconstruction side tables",
        ));
    }
    func.spill_descs.push(SpillDesc::transient());
    logical_for_vreg.push(logical);
    Ok(fresh)
}

fn planned_value(operation: PlannedOp) -> LogicalValue {
    match operation {
        PlannedOp::Spill { value, .. }
        | PlannedOp::Reload { value, .. }
        | PlannedOp::SpillPhi { value, .. } => value,
    }
}

fn edge_source(operation: PlannedEdgeOp) -> LogicalValue {
    match operation {
        PlannedEdgeOp::Spill { source, .. } | PlannedEdgeOp::Reload { source, .. } => source,
    }
}

fn is_rematerializable(func: &MFunction, plan: &SpillPlan, home: SpillHome) -> bool {
    rematerialized_home_value(func, plan, home).is_some()
}

fn rematerialized_home_value(func: &MFunction, plan: &SpillPlan, home: SpillHome) -> Option<u64> {
    let mut value = None;
    for member in plan.homes.members(home) {
        let SpillKind::Remat {
            value: member_value,
        } = func.spill_desc(member)?.kind
        else {
            return None;
        };
        if value.is_some_and(|value| value != member_value) {
            return None;
        }
        value = Some(member_value);
    }
    value
}

fn rematerialized_logical_value(func: &MFunction, logical: LogicalValue) -> Option<u64> {
    let SpillKind::Remat { value } = func.spill_desc(VReg(logical.0))?.kind else {
        return None;
    };
    Some(value)
}

#[cfg(test)]
mod tests;
