//! Materialize a SpillPlan and reconstruct strict SSA with dominance frontiers.

use std::collections::{BTreeSet, HashMap, VecDeque};

use crate::backend::native::mir::{
    BaseReg, BlockId, MBlock, MFunction, MInst, OpSize, PhiNode, SpillDesc, SpillKind, VReg,
};

use super::cfg::NormalizedCfg;
use super::next_use::NextUseAnalysis;
use super::reload::{
    ExpectedMaterializedReload, PointUse, PureStep, ReloadRecipeAnalysis, ResolvedBase,
    ResolvedRecipe,
};
use super::spill_plan::{LogicalValue, PlannedOp, ProgramPoint, SpillHome, SpillPlan};

pub(super) struct ReconstructionResult {
    pub frame_size: u32,
    pub recipe_reloads: Vec<ExpectedMaterializedReload>,
    pub shared_reload_blocks: usize,
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
}

#[derive(Clone)]
struct PreparedRecipe {
    expected: ResolvedRecipe,
    instructions: Vec<MInst>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ReloadMaterialization {
    Recipe(ResolvedRecipe),
    Immediate(u64),
    Stack,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct EdgeReloadShape {
    value: LogicalValue,
    home: SpillHome,
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

pub(super) fn reconstruct(
    func: &mut MFunction,
    cfg: &NormalizedCfg,
    plan: &SpillPlan,
    _next_use: &NextUseAnalysis,
    reload_recipes: &ReloadRecipeAnalysis,
) -> Result<ReconstructionResult, ReconstructError> {
    let recipe_homes = recipe_only_homes(func, plan, reload_recipes)?;
    let stack_offsets = stack_layout(func, plan, &recipe_homes)?;
    verify_reload_homes(func, plan, &stack_offsets, &recipe_homes)?;
    let original_vregs = func.vregs.count() as usize;
    let mut logical_for_vreg = (0..original_vregs)
        .map(|index| plan.logical.of(VReg(index as u32)))
        .collect::<Vec<_>>();
    let mut insertions = HashMap::<(usize, usize), Vec<MaterializedOp>>::new();
    let mut reload_blocks = HashMap::<LogicalValue, BTreeSet<usize>>::new();
    let mut edge_reload_bundles = Vec::<EdgeReloadBundle>::new();
    let spilled_phis = plan
        .point_ops
        .iter()
        .filter_map(|(_, operation)| match operation {
            PlannedOp::SpillPhi { value, .. } => Some(*value),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    for block in 0..func.blocks.len() {
        let removed = func.blocks[block]
            .phis
            .iter()
            .filter(|phi| spilled_phis.contains(&plan.logical.of(phi.dst)))
            .cloned()
            .collect::<Vec<_>>();
        func.blocks[block]
            .phis
            .retain(|phi| !spilled_phis.contains(&plan.logical.of(phi.dst)));
        for phi in removed {
            let home = plan.homes.of_vreg(phi.dst);
            if recipe_homes.contains(&home) {
                continue;
            }
            for (predecessor, source) in phi.sources {
                let Some(&predecessor) = cfg.block_index.get(&predecessor) else {
                    return Err(ReconstructError::new(
                        "RECONSTRUCT.PHI_PREDECESSOR_EXISTS",
                        Some(func.blocks[block].id),
                        None,
                        vec![phi.dst, source],
                        "spilled phi names a predecessor outside normalized CFG",
                    ));
                };
                let source = plan.logical.of(source);
                if plan.s_exit[predecessor].contains(&source) {
                    continue;
                }
                let instruction = func.blocks[predecessor].insts.len() - 1;
                insertions
                    .entry((predecessor, instruction))
                    .or_default()
                    .push(MaterializedOp::Spill {
                        value: source,
                        home,
                    });
            }
        }
    }
    for &(point, operation) in &plan.point_ops {
        if matches!(operation, PlannedOp::SpillPhi { .. }) {
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
        let recipe = reload_recipe_at_point(reload_recipes, point, operation, &recipe_homes)?;
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
        let Some(instruction) = predecessor_block.insts.len().checked_sub(1) else {
            return Err(ReconstructError::new(
                "RECONSTRUCT.EDGE_PREDECESSOR_TERMINATED",
                Some(predecessor_block.id),
                None,
                Vec::new(),
                "edge operation predecessor block is empty",
            ));
        };
        let mut bundle = EdgeReloadBundle {
            predecessor,
            successor,
            shape: Vec::new(),
            final_definitions: Vec::new(),
            definitions: Vec::new(),
            instruction_count: 0,
        };
        let mut reloads_only = true;
        for &operation in operations {
            let recipe = reload_recipe_on_edge(
                func,
                reload_recipes,
                predecessor,
                successor,
                instruction,
                operation,
                &recipe_homes,
            )?;
            let materialized = materialize_operation(
                func,
                plan,
                predecessor,
                instruction,
                operation,
                &mut logical_for_vreg,
                &mut insertions,
                &mut reload_blocks,
                recipe,
            )?;
            let Some(materialized) = materialized else {
                reloads_only = false;
                continue;
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
        }
        if reloads_only && !bundle.shape.is_empty() {
            edge_reload_bundles.push(bundle);
        }
    }

    let affected = reload_blocks.keys().copied().collect::<BTreeSet<_>>();
    let mut definition_blocks = HashMap::<LogicalValue, BTreeSet<usize>>::new();
    let mut existing_phi_blocks = HashMap::<LogicalValue, BTreeSet<usize>>::new();
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

    let mut reconstruction_phis = HashMap::<(usize, LogicalValue), VReg>::new();
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
    let mut stacks = HashMap::<LogicalValue, Vec<VReg>>::new();
    let mut recipe_reloads = Vec::<ExpectedMaterializedReload>::new();
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
        &recipe_homes,
        &mut recipe_reloads,
    )?;
    let shared_reload_blocks =
        share_identical_edge_reload_bundles(func, &edge_reload_bundles, &mut recipe_reloads)?;
    eliminate_dead_definitions(func, &mut recipe_reloads);

    let frame_size = u32::try_from(stack_offsets.len())
        .ok()
        .and_then(|homes| homes.checked_mul(8))
        .ok_or_else(|| {
            ReconstructError::new(
                "RECONSTRUCT.FRAME_SIZE_RANGE",
                None,
                None,
                Vec::new(),
                "spill frame size exceeds u32",
            )
        })?;
    Ok(ReconstructionResult {
        frame_size,
        recipe_reloads,
        shared_reload_blocks,
    })
}

fn recipe_only_homes(
    func: &MFunction,
    plan: &SpillPlan,
    analysis: &ReloadRecipeAnalysis,
) -> Result<BTreeSet<SpillHome>, ReconstructError> {
    let mut candidates = BTreeSet::<SpillHome>::new();
    let mut rejected = BTreeSet::<SpillHome>::new();
    for &(point, operation) in &plan.point_ops {
        match operation {
            PlannedOp::Reload { value, home } => {
                candidates.insert(home);
                if available_recipe_at_point(analysis, point, value).is_none() {
                    rejected.insert(home);
                }
            }
            PlannedOp::SpillPhi { .. } => {}
            PlannedOp::Spill { .. } => {}
        }
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
        let Some(_successor_block) = func.blocks.get(successor) else {
            return Err(ReconstructError::new(
                "RECONSTRUCT.EDGE_SUCCESSOR_EXISTS",
                Some(predecessor_block.id),
                None,
                Vec::new(),
                format!("edge operation successor index {successor} is outside function"),
            ));
        };
        let Some(instruction) = predecessor_block.insts.len().checked_sub(1) else {
            return Err(ReconstructError::new(
                "RECONSTRUCT.EDGE_PREDECESSOR_TERMINATED",
                Some(predecessor_block.id),
                None,
                Vec::new(),
                "edge operation predecessor block is empty",
            ));
        };
        for &operation in operations {
            match operation {
                PlannedOp::Reload { value, home } => {
                    candidates.insert(home);
                    if available_recipe_before_terminator(
                        analysis,
                        predecessor_block.id,
                        instruction,
                        value,
                    )
                    .is_none()
                    {
                        rejected.insert(home);
                    }
                }
                PlannedOp::SpillPhi { .. } => {}
                PlannedOp::Spill { .. } => {}
            }
        }
    }
    // A phi congruence class may use SimState itself as the merged home.  It
    // is safe to omit its SpillPhi and edge stores only when every concrete
    // reload selected by the planner has an exact recipe at that point.
    candidates.retain(|home| !rejected.contains(home));
    Ok(candidates)
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

fn available_recipe_before_terminator(
    analysis: &ReloadRecipeAnalysis,
    predecessor: BlockId,
    instruction: usize,
    value: LogicalValue,
) -> Option<&ResolvedRecipe> {
    let query = PointUse {
        block: predecessor,
        instruction,
        value: VReg(value.0),
    };
    analysis.resolved_recipe_at_point(query)
}

fn reload_recipe_at_point(
    analysis: &ReloadRecipeAnalysis,
    point: ProgramPoint,
    operation: PlannedOp,
    recipe_homes: &BTreeSet<SpillHome>,
) -> Result<Option<ResolvedRecipe>, ReconstructError> {
    let PlannedOp::Reload { value, home } = operation else {
        return Ok(None);
    };
    if !recipe_homes.contains(&home) {
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
                "spill-planner state recipe disappeared before reconstruction",
            )
        })
}

fn reload_recipe_on_edge(
    func: &MFunction,
    analysis: &ReloadRecipeAnalysis,
    predecessor: usize,
    successor: usize,
    instruction: usize,
    operation: PlannedOp,
    recipe_homes: &BTreeSet<SpillHome>,
) -> Result<Option<ResolvedRecipe>, ReconstructError> {
    let PlannedOp::Reload { value, home } = operation else {
        return Ok(None);
    };
    if !recipe_homes.contains(&home) {
        return Ok(None);
    }
    let predecessor_id = func.blocks[predecessor].id;
    let successor_id = func.blocks[successor].id;
    available_recipe_before_terminator(analysis, predecessor_id, instruction, value)
        .cloned()
        .map(Some)
        .ok_or_else(|| {
            ReconstructError::new(
                "RECONSTRUCT.EDGE_RECIPE_STABLE",
                Some(predecessor_id),
                None,
                vec![VReg(value.0)],
                format!("state recipe disappeared on edge {predecessor_id} -> {successor_id}"),
            )
        })
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
    let mut definition_inputs = HashMap::<VReg, Vec<VReg>>::new();
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

fn stack_layout(
    func: &MFunction,
    plan: &SpillPlan,
    recipe_homes: &BTreeSet<SpillHome>,
) -> Result<HashMap<SpillHome, i32>, ReconstructError> {
    let homes = plan
        .point_ops
        .iter()
        .map(|(_, operation)| *operation)
        .chain(plan.edge_ops.values().flatten().copied())
        .filter_map(|operation| match operation {
            PlannedOp::Spill { home, .. } => (!is_rematerializable(func, plan, home)
                && !recipe_homes.contains(&home))
            .then_some(home),
            PlannedOp::SpillPhi { home, .. } => (!recipe_homes.contains(&home)).then_some(home),
            PlannedOp::Reload { .. } => None,
        })
        .collect::<BTreeSet<_>>();
    let mut result = HashMap::with_capacity(homes.len());
    for (index, home) in homes.into_iter().enumerate() {
        let Some(offset) = index
            .checked_mul(8)
            .and_then(|value| i32::try_from(value).ok())
        else {
            return Err(ReconstructError::new(
                "RECONSTRUCT.STACK_OFFSET_RANGE",
                None,
                None,
                Vec::new(),
                "spill frame exceeds signed 32-bit addressing range",
            ));
        };
        result.insert(home, offset);
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
            if let PlannedOp::Reload { value, home } = operation
                && rematerialized_logical_value(func, value).is_none()
                && !recipe_homes.contains(&home)
            {
                if !stack_offsets.contains_key(&home) {
                    let block = func.blocks.get(edge.0).map(|block| block.id);
                    return Err(ReconstructError::new(
                        "RECONSTRUCT.RELOAD_HOME_EXISTS",
                        block,
                        None,
                        vec![VReg(value.0)],
                        format!(
                            "edge reload has no spill home {home:?}; {}",
                            describe_missing_home(func, plan, value, home)
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
    let states = func
        .blocks
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
        .collect::<Vec<_>>();
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
                        value,
                        home,
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

fn prepare_recipe(
    func: &mut MFunction,
    logical_for_vreg: &mut Vec<LogicalValue>,
    logical: LogicalValue,
    expected: ResolvedRecipe,
) -> Result<(VReg, PreparedRecipe), ReconstructError> {
    let mut instructions = Vec::with_capacity(expected.steps.len() + 1);
    let mut current = alloc_fresh(func, logical_for_vreg, logical)?;
    instructions.push(match &expected.base {
        ResolvedBase::Constant(value) => MInst::LoadImm {
            dst: current,
            value: *value,
        },
        ResolvedBase::State(state) => MInst::Load {
            dst: current,
            base: BaseReg::SimState,
            offset: state.load.offset,
            size: state.load.size,
        },
    });
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

fn materialize_pure_step(step: PureStep, dst: VReg, source: VReg) -> MInst {
    match step {
        PureStep::Copy64 => MInst::Mov { dst, src: source },
        PureStep::Copy32 => MInst::Mov32 { dst, src: source },
        PureStep::AndImm64 { immediate } => MInst::AndImm {
            dst,
            src: source,
            imm: immediate,
        },
        PureStep::AndImm32 { immediate } => MInst::AndImm32 {
            dst,
            src: source,
            imm: immediate,
        },
        PureStep::OrImm64 { immediate } => MInst::OrImm {
            dst,
            src: source,
            imm: immediate,
        },
        PureStep::ShrImm64 { immediate } => MInst::ShrImm {
            dst,
            src: source,
            imm: immediate,
        },
        PureStep::ShlImm64 { immediate } => MInst::ShlImm {
            dst,
            src: source,
            imm: immediate,
        },
        PureStep::SarImm64 { immediate } => MInst::SarImm {
            dst,
            src: source,
            imm: immediate,
        },
        PureStep::AddImm64 { immediate } => MInst::AddImm {
            dst,
            src: source,
            imm: immediate,
        },
        PureStep::SubImm64 { immediate } => MInst::SubImm {
            dst,
            src: source,
            imm: immediate,
        },
        PureStep::BitNot64 => MInst::BitNot { dst, src: source },
        PureStep::Neg64 => MInst::Neg { dst, src: source },
    }
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
) -> Result<usize, ReconstructError> {
    let mut grouped = HashMap::<EdgeReloadGroupKey, Vec<usize>>::new();
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
        return Ok(0);
    }
    let shared_count = plans.len();

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
    }

    recipe_reloads.retain(|reload| !removed_recipe_reloads.contains(&reload.reload));
    Ok(shared_count)
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
) -> Result<(), ReconstructError> {
    let mut operations = insertions.remove(&(block, instruction)).unwrap_or_default();
    // A SpillPlan program point is parallel.  When materialized serially,
    // evictions must free their registers before operand reloads consume them.
    operations.sort_by_key(|operation| match operation {
        MaterializedOp::Spill { .. } => 0,
        MaterializedOp::Reload { .. } => 1,
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
                    recipe_reloads.push(ExpectedMaterializedReload {
                        reload: fresh,
                        expected: recipe.expected,
                    });
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
mod tests {
    use super::*;
    use crate::backend::native::mir::{BlockId, MBlock, VRegAllocator};

    #[test]
    fn reconstruction_reports_vreg_exhaustion() {
        let mut vregs = VRegAllocator::new();
        vregs.set_next_for_test(u32::MAX);
        let mut func = MFunction::new(vregs, Vec::new());
        let mut logical_for_vreg = Vec::new();

        let error = alloc_fresh(&mut func, &mut logical_for_vreg, LogicalValue(0)).unwrap_err();

        assert_eq!(error.rule, "RECONSTRUCT.VREG_EXHAUSTED");
        assert_eq!(func.vregs.count(), u32::MAX);
    }

    #[test]
    fn removes_dead_cyclic_phi_webs() {
        let mut vregs = VRegAllocator::new();
        let source = vregs.alloc();
        let live = vregs.alloc();
        let dead_left = vregs.alloc();
        let dead_right = vregs.alloc();
        let output = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 5]);
        let mut block = MBlock::new(BlockId(0));
        block.phis.push(PhiNode {
            dst: live,
            sources: vec![(BlockId(0), source)],
        });
        block.phis.push(PhiNode {
            dst: dead_left,
            sources: vec![(BlockId(0), dead_right)],
        });
        block.phis.push(PhiNode {
            dst: dead_right,
            sources: vec![(BlockId(0), dead_left)],
        });
        block.push(MInst::Mov {
            dst: output,
            src: live,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 0,
            src: output,
            size: OpSize::S64,
        });
        block.push(MInst::Return);
        func.push_block(block);

        let mut recipe_reloads = Vec::new();
        assert_eq!(
            eliminate_dead_definitions(&mut func, &mut recipe_reloads),
            BTreeSet::from([dead_left, dead_right])
        );
        assert_eq!(func.blocks[0].phis.len(), 1);
        assert_eq!(func.blocks[0].phis[0].dst, live);
    }

    #[test]
    fn removes_unused_original_and_planned_definitions() {
        let mut vregs = VRegAllocator::new();
        let original_dead = vregs.alloc();
        let dead = vregs.alloc();
        let live = vregs.alloc();
        let output = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 4]);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: original_dead,
            base: BaseReg::SimState,
            offset: 0,
            size: OpSize::S64,
        });
        block.push(MInst::Load {
            dst: dead,
            base: BaseReg::StackFrame,
            offset: 0,
            size: OpSize::S64,
        });
        block.push(MInst::Load {
            dst: live,
            base: BaseReg::StackFrame,
            offset: 8,
            size: OpSize::S64,
        });
        block.push(MInst::Mov {
            dst: output,
            src: live,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 8,
            src: output,
            size: OpSize::S64,
        });
        block.push(MInst::Return);
        func.push_block(block);

        let mut recipe_reloads = vec![
            ExpectedMaterializedReload {
                reload: dead,
                expected: ResolvedRecipe {
                    base: ResolvedBase::Constant(0),
                    steps: Vec::new(),
                },
            },
            ExpectedMaterializedReload {
                reload: live,
                expected: ResolvedRecipe {
                    base: ResolvedBase::Constant(1),
                    steps: Vec::new(),
                },
            },
        ];
        assert_eq!(
            eliminate_dead_definitions(&mut func, &mut recipe_reloads),
            BTreeSet::from([original_dead, dead])
        );
        assert_eq!(func.blocks[0].insts.len(), 4);
        assert_eq!(func.blocks[0].insts[0].def(), Some(live));
        assert_eq!(recipe_reloads.len(), 1);
        assert_eq!(recipe_reloads[0].reload, live);
    }

    #[test]
    fn fresh_representative_tracks_the_logical_value() {
        let mut vregs = VRegAllocator::new();
        let original = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient()]);
        let mut logical_for_vreg = vec![LogicalValue(original.0)];

        let fresh =
            alloc_fresh(&mut func, &mut logical_for_vreg, LogicalValue(original.0)).unwrap();

        assert_eq!(logical_for_vreg[fresh.0 as usize], LogicalValue(original.0));
    }

    #[test]
    fn shares_identical_edge_reload_tails_without_reloading_the_resident_edge() {
        let mut vregs = VRegAllocator::new();
        let original = vregs.alloc();
        let first_condition = vregs.alloc();
        let second_condition = vregs.alloc();
        let left_reload = vregs.alloc();
        let right_reload = vregs.alloc();
        let merged = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 6]);

        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::Load {
            dst: original,
            base: BaseReg::SimState,
            offset: 80,
            size: OpSize::S64,
        });
        entry.push(MInst::LoadImm {
            dst: first_condition,
            value: 1,
        });
        entry.push(MInst::Branch {
            cond: first_condition,
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });

        let mut left = MBlock::new(BlockId(1));
        left.push(MInst::Load {
            dst: left_reload,
            base: BaseReg::StackFrame,
            offset: 0,
            size: OpSize::S64,
        });
        left.push(MInst::Jump { target: BlockId(4) });

        let mut decision = MBlock::new(BlockId(2));
        decision.push(MInst::LoadImm {
            dst: second_condition,
            value: 1,
        });
        decision.push(MInst::Branch {
            cond: second_condition,
            true_bb: BlockId(3),
            false_bb: BlockId(5),
        });

        let mut right = MBlock::new(BlockId(3));
        right.push(MInst::Load {
            dst: right_reload,
            base: BaseReg::StackFrame,
            offset: 0,
            size: OpSize::S64,
        });
        right.push(MInst::Jump { target: BlockId(4) });

        let mut join = MBlock::new(BlockId(4));
        join.phis.push(PhiNode {
            dst: merged,
            sources: vec![
                (BlockId(1), left_reload),
                (BlockId(3), right_reload),
                (BlockId(5), original),
            ],
        });
        join.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 88,
            src: merged,
            size: OpSize::S64,
        });
        join.push(MInst::Return);

        let mut resident = MBlock::new(BlockId(5));
        resident.push(MInst::Jump { target: BlockId(4) });
        func.blocks = vec![entry, left, decision, right, join, resident];
        func.verify();

        let shape = EdgeReloadShape {
            value: LogicalValue(original.0),
            home: SpillHome(original.0),
            materialization: ReloadMaterialization::Stack,
        };
        let bundles = vec![
            EdgeReloadBundle {
                predecessor: 1,
                successor: 4,
                shape: vec![shape.clone()],
                final_definitions: vec![left_reload],
                definitions: vec![left_reload],
                instruction_count: 1,
            },
            EdgeReloadBundle {
                predecessor: 3,
                successor: 4,
                shape: vec![shape],
                final_definitions: vec![right_reload],
                definitions: vec![right_reload],
                instruction_count: 1,
            },
        ];

        let shared =
            share_identical_edge_reload_bundles(&mut func, &bundles, &mut Vec::new()).unwrap();

        assert_eq!(shared, 1);
        assert_eq!(func.blocks.len(), 7);
        assert_eq!(
            func.blocks[4].phis[0].sources,
            vec![(BlockId(6), left_reload), (BlockId(5), original)]
        );
        assert_eq!(
            func.blocks
                .iter()
                .flat_map(|block| &block.insts)
                .filter(|instruction| matches!(
                    instruction,
                    MInst::Load {
                        base: BaseReg::StackFrame,
                        offset: 0,
                        size: OpSize::S64,
                        ..
                    }
                ))
                .count(),
            1
        );
        assert_eq!(
            func.blocks[1].insts,
            vec![MInst::Jump { target: BlockId(6) }]
        );
        assert_eq!(
            func.blocks[3].insts,
            vec![MInst::Jump { target: BlockId(6) }]
        );
        func.verify();
        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        cfg.verify(&func).unwrap();
        func.verify();
    }

    #[test]
    fn missing_phi_representative_is_a_structured_error() {
        let mut vregs = VRegAllocator::new();
        let original = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient()]);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::Jump { target: BlockId(1) });
        let mut successor = MBlock::new(BlockId(1));
        successor.push(MInst::LoadImm {
            dst: original,
            value: 1,
        });
        successor.push(MInst::Return);
        func.blocks = vec![entry, successor];
        func.verify();
        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
        let plan = super::super::spill_plan::plan(&func, &cfg, &next_use, 32).unwrap();
        let mut logical_for_vreg = vec![LogicalValue(original.0)];
        let fresh =
            alloc_fresh(&mut func, &mut logical_for_vreg, LogicalValue(original.0)).unwrap();
        let successor = cfg.block_index[&BlockId(1)];
        func.blocks[successor].phis.push(PhiNode {
            dst: fresh,
            sources: Vec::new(),
        });
        let reconstruction_phis = HashMap::from([((successor, LogicalValue(original.0)), fresh)]);
        let mut children = vec![Vec::new(); func.blocks.len()];
        children[0].push(successor);
        let recipe_homes = BTreeSet::new();
        let mut recipe_reloads = Vec::new();

        let error = rename_block(
            0,
            &mut func,
            &cfg,
            &plan,
            &children,
            &reconstruction_phis,
            &HashMap::new(),
            &logical_for_vreg,
            &mut HashMap::new(),
            &mut HashMap::new(),
            &recipe_homes,
            &mut recipe_reloads,
        )
        .unwrap_err();

        assert_eq!(error.rule, "RECONSTRUCT.PHI_REPRESENTATIVE_EXISTS");
        assert_eq!(error.block, Some(BlockId(1)));
    }

    fn planner_recipe_fixture(overwrite_home: bool) -> MFunction {
        let mut vregs = VRegAllocator::new();
        let stored = vregs.alloc();
        let pressure = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 2]);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: stored,
            base: BaseReg::StackFrame,
            offset: 0,
            size: OpSize::S64,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 80,
            src: stored,
            size: OpSize::S64,
        });
        block.push(MInst::Load {
            dst: pressure,
            base: BaseReg::StackFrame,
            offset: 8,
            size: OpSize::S64,
        });
        if overwrite_home {
            block.push(MInst::Store {
                base: BaseReg::SimState,
                offset: 80,
                src: pressure,
                size: OpSize::S64,
            });
        }
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 96,
            src: stored,
            size: OpSize::S64,
        });
        block.push(MInst::Return);
        func.push_block(block);
        func
    }

    fn global_state_recipe_fixture(with_pure_step: bool) -> MFunction {
        let mut vregs = VRegAllocator::new();
        let state = vregs.alloc();
        let derived = with_pure_step.then(|| vregs.alloc());
        let pressure = vregs.alloc();
        let second_pressure = with_pure_step.then(|| vregs.alloc());
        let mut func = MFunction::new(
            vregs,
            vec![SpillDesc::transient(); if with_pure_step { 4 } else { 2 }],
        );
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Load {
            dst: state,
            base: BaseReg::SimState,
            offset: 80,
            size: OpSize::S64,
        });
        if let Some(derived) = derived {
            block.push(MInst::ShrImm {
                dst: derived,
                src: state,
                imm: 3,
            });
        }
        block.push(MInst::Load {
            dst: pressure,
            base: BaseReg::StackFrame,
            offset: 0,
            size: OpSize::S64,
        });
        if let Some(second_pressure) = second_pressure {
            block.push(MInst::Load {
                dst: second_pressure,
                base: BaseReg::StackFrame,
                offset: 8,
                size: OpSize::S64,
            });
            block.push(MInst::Store {
                base: BaseReg::SimState,
                offset: 96,
                src: pressure,
                size: OpSize::S64,
            });
            block.push(MInst::Store {
                base: BaseReg::SimState,
                offset: 104,
                src: second_pressure,
                size: OpSize::S64,
            });
        }
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 112,
            src: derived.unwrap_or(state),
            size: OpSize::S64,
        });
        block.push(MInst::Return);
        func.push_block(block);
        func
    }

    fn reconstruct_with_registers(
        mut func: MFunction,
        registers: usize,
    ) -> (MFunction, ReconstructionResult) {
        func.verify();
        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let planning_recipes = super::super::reload::analyze_for_planning(&func).unwrap();
        let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
        next_use.verify(&func, &cfg).unwrap();
        let recipe_costs = planning_recipes.global_materialization_costs().unwrap();
        let plan = super::super::spill_plan::plan_with_recipe_costs(
            &func,
            &cfg,
            &next_use,
            &recipe_costs,
            registers,
        )
        .unwrap();
        plan.verify(&func, &cfg, registers).unwrap();
        super::super::home_verify::verify(&func, &cfg, &plan).unwrap();
        let requested_points = super::super::ssa::planner_reload_queries(&func, &plan).unwrap();
        let recipes =
            super::super::reload::analyze_with_queries(&func, &cfg, &requested_points).unwrap();
        let result = reconstruct(&mut func, &cfg, &plan, &next_use, &recipes).unwrap();
        let rebuilt_cfg = (result.shared_reload_blocks != 0)
            .then(|| super::super::cfg::normalize(&mut func).unwrap());
        let cfg = rebuilt_cfg.as_ref().unwrap_or(&cfg);
        super::super::reload::verify_expected_materialized_reloads(
            &func,
            cfg,
            &result.recipe_reloads,
        )
        .unwrap();
        (func, result)
    }

    fn reconstruct_with_one_register(func: MFunction) -> (MFunction, ReconstructionResult) {
        reconstruct_with_registers(func, 1)
    }

    #[test]
    fn planner_reload_uses_valid_state_home_without_a_stack_slot() {
        let (func, result) = reconstruct_with_one_register(planner_recipe_fixture(false));

        assert_eq!(result.frame_size, 0);
        assert_eq!(result.recipe_reloads.len(), 1);
        assert!(func.blocks[0].insts.iter().any(|inst| matches!(
            inst,
            MInst::Load {
                base: BaseReg::SimState,
                offset: 80,
                size: OpSize::S64,
                ..
            }
        )));
        assert!(func.blocks[0].insts.iter().all(|inst| !matches!(
            inst,
            MInst::Store {
                base: BaseReg::StackFrame,
                ..
            }
        )));
    }

    #[test]
    fn planner_uses_memory_phi_home_without_spilling_register_phi() {
        let mut vregs = VRegAllocator::new();
        let condition = vregs.alloc();
        let left_value = vregs.alloc();
        let right_value = vregs.alloc();
        let merged = vregs.alloc();
        let pressure = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 5]);

        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: condition,
            value: 1,
        });
        entry.push(MInst::Branch {
            cond: condition,
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });

        let mut left = MBlock::new(BlockId(1));
        left.push(MInst::Load {
            dst: left_value,
            base: BaseReg::StackFrame,
            offset: 0,
            size: OpSize::S64,
        });
        left.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 40,
            src: left_value,
            size: OpSize::S64,
        });
        left.push(MInst::Jump { target: BlockId(3) });

        let mut right = MBlock::new(BlockId(2));
        right.push(MInst::Load {
            dst: right_value,
            base: BaseReg::StackFrame,
            offset: 8,
            size: OpSize::S64,
        });
        right.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 40,
            src: right_value,
            size: OpSize::S64,
        });
        right.push(MInst::Jump { target: BlockId(3) });

        let mut join = MBlock::new(BlockId(3));
        join.phis.push(PhiNode {
            dst: merged,
            sources: vec![(BlockId(1), left_value), (BlockId(2), right_value)],
        });
        join.push(MInst::Load {
            dst: pressure,
            base: BaseReg::StackFrame,
            offset: 16,
            size: OpSize::S64,
        });
        join.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 80,
            src: pressure,
            size: OpSize::S64,
        });
        join.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 88,
            src: merged,
            size: OpSize::S64,
        });
        join.push(MInst::Return);
        func.blocks = vec![entry, left, right, join];

        let (func, result) = reconstruct_with_one_register(func);

        assert_eq!(result.frame_size, 0);
        assert!(!result.recipe_reloads.is_empty());
        assert!(
            func.blocks
                .iter()
                .flat_map(|block| &block.insts)
                .all(|inst| {
                    !matches!(
                        inst,
                        MInst::Store {
                            base: BaseReg::StackFrame,
                            ..
                        }
                    )
                })
        );
        assert!(
            func.blocks
                .iter()
                .flat_map(|block| &block.insts)
                .any(|inst| {
                    matches!(
                        inst,
                        MInst::Load {
                            base: BaseReg::SimState,
                            offset: 40,
                            size: OpSize::S64,
                            ..
                        }
                    )
                })
        );
    }

    #[test]
    fn planner_reload_falls_back_to_stack_after_state_home_is_overwritten() {
        let (func, result) = reconstruct_with_one_register(planner_recipe_fixture(true));

        assert_eq!(result.frame_size, 8);
        assert!(result.recipe_reloads.is_empty());
        assert!(func.blocks[0].insts.iter().any(|inst| matches!(
            inst,
            MInst::Store {
                base: BaseReg::StackFrame,
                size: OpSize::S64,
                ..
            }
        )));
    }

    #[test]
    fn planner_materializes_a_global_state_recipe_only_at_the_reload() {
        let (func, result) = reconstruct_with_one_register(global_state_recipe_fixture(false));

        assert_eq!(result.frame_size, 0);
        assert_eq!(result.recipe_reloads.len(), 1);
        assert_eq!(
            result.recipe_reloads[0].expected.steps,
            Vec::<PureStep>::new()
        );
        assert_eq!(
            func.blocks[0]
                .insts
                .iter()
                .filter(|inst| matches!(
                    inst,
                    MInst::Load {
                        base: BaseReg::SimState,
                        offset: 80,
                        size: OpSize::S64,
                        ..
                    }
                ))
                .count(),
            1
        );
    }

    #[test]
    fn planner_materializes_a_pure_recipe_with_exact_machine_width() {
        let (func, result) = reconstruct_with_registers(global_state_recipe_fixture(true), 2);

        assert_eq!(result.frame_size, 0);
        assert_eq!(result.recipe_reloads.len(), 1);
        assert_eq!(
            result.recipe_reloads[0].expected.steps,
            vec![PureStep::ShrImm64 { immediate: 3 }]
        );
        assert_eq!(
            func.blocks[0]
                .insts
                .iter()
                .filter(|inst| matches!(inst, MInst::ShrImm { imm: 3, .. }))
                .count(),
            1
        );
    }

    #[test]
    fn planner_edge_reload_uses_the_predecessor_state_home() {
        let mut vregs = VRegAllocator::new();
        let stored = vregs.alloc();
        let condition = vregs.alloc();
        let pressure_left = vregs.alloc();
        let pressure_right = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 4]);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::Load {
            dst: stored,
            base: BaseReg::StackFrame,
            offset: 0,
            size: OpSize::S64,
        });
        entry.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 80,
            src: stored,
            size: OpSize::S64,
        });
        entry.push(MInst::LoadImm {
            dst: condition,
            value: 1,
        });
        entry.push(MInst::Branch {
            cond: condition,
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });
        let mut left = MBlock::new(BlockId(1));
        left.push(MInst::Load {
            dst: pressure_left,
            base: BaseReg::StackFrame,
            offset: 8,
            size: OpSize::S64,
        });
        left.push(MInst::Load {
            dst: pressure_right,
            base: BaseReg::StackFrame,
            offset: 16,
            size: OpSize::S64,
        });
        left.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 96,
            src: pressure_left,
            size: OpSize::S64,
        });
        left.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 104,
            src: pressure_right,
            size: OpSize::S64,
        });
        left.push(MInst::Jump { target: BlockId(3) });
        let mut right = MBlock::new(BlockId(2));
        right.push(MInst::Jump { target: BlockId(3) });
        let mut join = MBlock::new(BlockId(3));
        join.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 112,
            src: stored,
            size: OpSize::S64,
        });
        join.push(MInst::Return);
        func.blocks = vec![entry, left, right, join];
        func.verify();

        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
        let plan = super::super::spill_plan::plan(&func, &cfg, &next_use, 2).unwrap();
        plan.verify(&func, &cfg, 2).unwrap();
        assert!(
            plan.edge_ops.values().flatten().any(|operation| matches!(
                operation,
                PlannedOp::Reload { value, .. } if *value == LogicalValue(stored.0)
            )),
            "{plan:#?}"
        );
        let requested = super::super::ssa::planner_reload_queries(&func, &plan).unwrap();
        assert!(!requested.is_empty());
        let recipes = super::super::reload::analyze_with_queries(&func, &cfg, &requested).unwrap();
        let result = reconstruct(&mut func, &cfg, &plan, &next_use, &recipes).unwrap();
        super::super::reload::verify_expected_materialized_reloads(
            &func,
            &cfg,
            &result.recipe_reloads,
        )
        .unwrap();

        assert_eq!(result.frame_size, 0);
        assert!(!result.recipe_reloads.is_empty());
        assert!(
            func.blocks
                .iter()
                .flat_map(|block| &block.insts)
                .all(|inst| {
                    !matches!(
                        inst,
                        MInst::Store {
                            base: BaseReg::StackFrame,
                            ..
                        }
                    )
                })
        );
    }

    #[test]
    fn planner_shares_matching_high_pressure_arm_reloads() {
        let mut vregs = VRegAllocator::new();
        let stored = vregs.alloc();
        let first_condition = vregs.alloc();
        let second_condition = vregs.alloc();
        let left_first = vregs.alloc();
        let left_second = vregs.alloc();
        let right_first = vregs.alloc();
        let right_second = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 7]);

        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::Load {
            dst: stored,
            base: BaseReg::SimState,
            offset: 80,
            size: OpSize::S64,
        });
        entry.push(MInst::LoadImm {
            dst: first_condition,
            value: 1,
        });
        entry.push(MInst::Branch {
            cond: first_condition,
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });

        let mut left = MBlock::new(BlockId(1));
        left.push(MInst::Load {
            dst: left_first,
            base: BaseReg::StackFrame,
            offset: 0,
            size: OpSize::S64,
        });
        left.push(MInst::Load {
            dst: left_second,
            base: BaseReg::StackFrame,
            offset: 8,
            size: OpSize::S64,
        });
        left.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 96,
            src: left_first,
            size: OpSize::S64,
        });
        left.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 104,
            src: left_second,
            size: OpSize::S64,
        });
        left.push(MInst::Jump { target: BlockId(4) });

        let mut decision = MBlock::new(BlockId(2));
        decision.push(MInst::LoadImm {
            dst: second_condition,
            value: 1,
        });
        decision.push(MInst::Branch {
            cond: second_condition,
            true_bb: BlockId(3),
            false_bb: BlockId(5),
        });

        let mut right = MBlock::new(BlockId(3));
        right.push(MInst::Load {
            dst: right_first,
            base: BaseReg::StackFrame,
            offset: 16,
            size: OpSize::S64,
        });
        right.push(MInst::Load {
            dst: right_second,
            base: BaseReg::StackFrame,
            offset: 24,
            size: OpSize::S64,
        });
        right.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 112,
            src: right_first,
            size: OpSize::S64,
        });
        right.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 120,
            src: right_second,
            size: OpSize::S64,
        });
        right.push(MInst::Jump { target: BlockId(4) });

        let mut join = MBlock::new(BlockId(4));
        join.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 128,
            src: stored,
            size: OpSize::S64,
        });
        join.push(MInst::Return);

        let mut resident = MBlock::new(BlockId(5));
        resident.push(MInst::Jump { target: BlockId(4) });
        func.blocks = vec![entry, left, decision, right, join, resident];

        let (func, result) = reconstruct_with_registers(func, 2);

        assert_eq!(result.shared_reload_blocks, 1);
        assert_eq!(result.frame_size, 0);
        assert_eq!(
            func.blocks
                .iter()
                .flat_map(|block| &block.insts)
                .filter(|instruction| matches!(
                    instruction,
                    MInst::Load {
                        base: BaseReg::SimState,
                        offset: 80,
                        size: OpSize::S64,
                        ..
                    }
                ))
                .count(),
            2,
            "one original resident-path load and one shared arm reload must remain"
        );
        assert!(func.blocks.iter().flat_map(|block| &block.phis).any(|phi| {
            phi.sources.len() == 2 && phi.sources.iter().any(|(_, source)| *source == stored)
        }));
    }
}
