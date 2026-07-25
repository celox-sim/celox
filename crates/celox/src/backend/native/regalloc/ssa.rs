//! Verified non-iterative SSA register-allocation pipeline.

use std::collections::BTreeSet;

use crate::backend::native::mir::{MFunction, VReg};

use super::assignment::AssignmentMap;
use super::cfg::NormalizedCfg;
use super::next_use::NextUseAnalysis;
use super::reload::{PlanningRecipes, PointUse};
use super::spill_plan::{PlannedEdgeOp, PlannedOp, SpillPlan};

pub(super) struct Allocation {
    pub assignment: AssignmentMap,
    pub spill_frame_size: u32,
}

/// Execute scheduling's downstream phases exactly once: W/S planning, SSA
/// reconstruction, late Perm construction, and implicit chordal coloring.
pub(super) fn allocate(
    func: &mut MFunction,
    cfg: &NormalizedCfg,
    next_use: &NextUseAnalysis,
    planning_recipes: &PlanningRecipes,
) -> Result<Allocation, super::RegallocError> {
    let timing = std::env::var_os("CELOX_REGALLOC_TIMING").is_some()
        || std::env::var_os("CELOX_PHASE_TIMING").is_some();
    let phase = timing.then(crate::timing::now);
    let mut plan = super::spill_plan::plan_with_recipe_costs(
        func,
        cfg,
        next_use,
        planning_recipes,
        super::NUM_REGS,
    )
    .map_err(|error| {
        super::RegallocError::new(
            "spill planning",
            error.rule,
            error.block,
            error.instruction,
            error.values,
            error.message,
        )
    })?;
    plan.verify(func, cfg, super::NUM_REGS).map_err(|error| {
        super::RegallocError::new(
            "spill-plan verification",
            error.rule,
            error.block,
            error.instruction,
            error.values,
            error.message,
        )
    })?;
    if let Some(start) = phase {
        eprintln!(
            "[regalloc-timing] ssa spill_plan elapsed={:?}",
            start.elapsed()
        );
    }

    let phase = timing.then(crate::timing::now);
    super::ssa_state_home::select(func, cfg, &mut plan).map_err(|error| {
        super::RegallocError::new(
            "packed state-home selection",
            error.rule,
            error.block,
            error.instruction,
            error.values,
            error.message,
        )
    })?;
    super::ssa_state_home::verify(func, cfg, &plan).map_err(|error| {
        super::RegallocError::new(
            "packed state-home verification",
            error.rule,
            error.block,
            error.instruction,
            error.values,
            error.message,
        )
    })?;
    if let Some(start) = phase {
        eprintln!(
            "[regalloc-timing] ssa packed_state_homes homes={} reloads={} elapsed={:?}",
            plan.state_homes.len(),
            plan.state_reload_recipes.len(),
            start.elapsed()
        );
    }

    // Edge coupling operations are chosen by the spill planner, so their
    // materialization points do not exist as MIR uses during the first recipe
    // analysis. Query their exact CFG-isolated insertion points only after the
    // complete plan is available.
    let phase = timing.then(crate::timing::now);
    let requested_points = planner_reload_queries(func, cfg, &plan)?;
    let reload_recipes = super::reload::analyze_with_queries(func, cfg, &requested_points)
        .map_err(|error| {
            super::reload_recipe_error("spill-planner reload-recipe analysis", error)
        })?;
    plan.select_recipe_homes(func, cfg, &reload_recipes)
        .map_err(|error| {
            super::RegallocError::new(
                "spill-home selection",
                error.rule,
                error.block,
                error.instruction,
                error.values,
                error.message,
            )
        })?;
    plan.verify(func, cfg, super::NUM_REGS).map_err(|error| {
        super::RegallocError::new(
            "final spill-plan verification",
            error.rule,
            error.block,
            error.instruction,
            error.values,
            error.message,
        )
    })?;
    plan.verify_recipe_homes(func, cfg, &reload_recipes)
        .map_err(|error| {
            super::RegallocError::new(
                "recipe-home verification",
                error.rule,
                error.block,
                error.instruction,
                error.values,
                error.message,
            )
        })?;
    super::ssa_state_home::verify(func, cfg, &plan).map_err(|error| {
        super::RegallocError::new(
            "final packed state-home verification",
            error.rule,
            error.block,
            error.instruction,
            error.values,
            error.message,
        )
    })?;
    if let Err(error) = super::home_verify::verify(func, cfg, &plan) {
        let (block, instruction) = match error.location {
            Some(super::home_verify::HomeLocation::Point(point)) => {
                (Some(point.block), Some(point.instruction))
            }
            Some(super::home_verify::HomeLocation::Edge { predecessor, .. }) => {
                (Some(predecessor), None)
            }
            None => (None, None),
        };
        return Err(super::RegallocError::new(
            "spill-home verification",
            error.rule,
            block,
            instruction,
            error.value.map(|value| VReg(value.0)).into_iter().collect(),
            error.message,
        ));
    }
    if let Some(start) = phase {
        eprintln!(
            "[regalloc-timing] ssa planner_reload_recipe_analyze_verify queries={} elapsed={:?}",
            requested_points.len(),
            start.elapsed()
        );
    }

    let phase = timing.then(crate::timing::now);
    let reconstruction =
        super::reconstruct::reconstruct(func, cfg, &plan, next_use, &reload_recipes).map_err(
            |error| {
                super::RegallocError::new(
                    "SSA reconstruction",
                    error.rule,
                    error.block,
                    error.instruction,
                    error.values,
                    error.message,
                )
            },
        )?;
    if let Some(start) = phase {
        eprintln!(
            "[regalloc-timing] ssa reconstruct vregs={} insts={} frame={} shared_reload_blocks={} elapsed={:?}",
            func.vregs.count(),
            func.blocks
                .iter()
                .map(|block| block.insts.len())
                .sum::<usize>(),
            reconstruction.frame_size,
            reconstruction.shared_reload_blocks.len(),
            start.elapsed()
        );
    }
    func.verify_result().map_err(|error| {
        super::RegallocError::mir("SSA reconstruction structural verification", error)
    })?;

    // Shared edge-reload tails add real CFG blocks after the original
    // allocation graph was frozen. Rebuild the normalized graph once, then
    // use it for every independent post-reconstruction proof and coloring
    // phase. No spill planning or allocation retry is performed.
    let reconstructed_cfg = if !reconstruction.shared_reload_blocks.is_empty() {
        let rebuilt = super::cfg::normalize(func)
            .map_err(|error| super::cfg_error("shared reload CFG normalization", error))?;
        rebuilt.verify(func).map_err(|error| {
            super::cfg_error("shared reload CFG normalization verification", error)
        })?;
        func.verify_result().map_err(|error| {
            super::RegallocError::mir("shared reload CFG structural verification", error)
        })?;
        Some(rebuilt)
    } else {
        None
    };
    let cfg = reconstructed_cfg.as_ref().unwrap_or(cfg);

    super::allocation_ir::verify_materialized_state_homes(
        func,
        cfg,
        &reconstruction.state_stores,
        &reconstruction.state_reloads,
    )
    .map_err(|error| {
        super::RegallocError::new(
            "materialized packed state-home verification",
            error.rule,
            error.block,
            error.instruction,
            error.values,
            error.message,
        )
    })?;

    let inserted_state_writes = reconstruction
        .state_stores
        .iter()
        .map(|store| (store.block, store.write_ordinal))
        .collect::<Vec<_>>();
    super::reload::verify_expected_materialized_reloads_after_state_spills(
        func,
        cfg,
        &reconstruction.recipe_reloads,
        &inserted_state_writes,
        &reconstruction.shared_reload_blocks,
    )
    .map_err(|error| {
        super::reload_recipe_error("spill-planner reload-recipe verification", error)
    })?;

    // Prove the spill result itself fits the machine before Perm boundaries
    // introduce fresh representatives.  This keeps pressure correctness
    // independent from constraint legalization and follows the frozen phase
    // order: reconstruct -> pressure proof -> Perm -> color.
    let phase = timing.then(crate::timing::now);
    let reconstructed_analysis = super::analysis::analyze(func);
    if let Err(error) = super::pressure::verify(func, &reconstructed_analysis, super::NUM_REGS) {
        return Err(super::RegallocError::new(
            "reconstructed pressure verification",
            "PRESSURE.EXCEEDS_CAPACITY",
            Some(error.block),
            Some(error.instruction),
            Vec::new(),
            error.to_string(),
        ));
    }
    if let Some(start) = phase {
        eprintln!(
            "[regalloc-timing] ssa reconstructed_pressure_verify elapsed={:?}",
            start.elapsed()
        );
    }

    let phase = timing.then(crate::timing::now);
    let (color_cfg, perms) =
        super::legalize::materialize_constraint_perms(func, cfg).map_err(|error| {
            super::RegallocError::new(
                "Perm materialization and verification",
                error.rule,
                error.block,
                error.instruction,
                error.values,
                error.message,
            )
        })?;
    func.verify_result()
        .map_err(|error| super::RegallocError::mir("Perm structural verification", error))?;
    if let Some(start) = phase {
        eprintln!(
            "[regalloc-timing] ssa constraint_perms boundaries={} vregs={} elapsed={:?}",
            perms.boundaries.len(),
            func.vregs.count(),
            start.elapsed()
        );
    }

    let phase = timing.then(crate::timing::now);
    let analysis = super::analysis::analyze(func);
    let coloring = super::color::color_ssa(func, &color_cfg, &analysis, &perms, super::NUM_REGS)
        .map_err(|error| {
            let message = error.to_string();
            super::RegallocError::new(
                "SSA coloring",
                error.rule,
                Some(error.block),
                error.instruction,
                error.value.into_iter().chain(error.related).collect(),
                message,
            )
        })?;
    for (&destination, &register) in &coloring.perm_matching {
        if coloring.assignment.get(destination) != Some(register) {
            return Err(super::RegallocError::new(
                "SSA coloring verification",
                "COLOR.PERM_MATCHING_APPLIED",
                None,
                None,
                vec![destination],
                format!("Perm matching color {register:?} was not applied"),
            ));
        }
    }
    if let Some(start) = phase {
        eprintln!("[regalloc-timing] ssa color elapsed={:?}", start.elapsed());
    }

    Ok(Allocation {
        assignment: coloring.assignment,
        spill_frame_size: reconstruction.frame_size,
    })
}

pub(super) fn planner_reload_queries(
    func: &MFunction,
    cfg: &NormalizedCfg,
    plan: &SpillPlan,
) -> Result<BTreeSet<PointUse>, super::RegallocError> {
    let mut requested = BTreeSet::new();
    for &(point, operation) in &plan.point_ops {
        let PlannedOp::Reload { value, .. } = operation else {
            continue;
        };
        requested.insert(PointUse {
            block: point.block,
            instruction: point.instruction,
            value: VReg(value.0),
        });
    }
    for (&(predecessor, successor), operations) in &plan.edge_ops {
        if !operations
            .iter()
            .any(|operation| matches!(operation, PlannedEdgeOp::Reload { .. }))
        {
            continue;
        }
        let Some(predecessor_block) = func.blocks.get(predecessor) else {
            return Err(super::RegallocError::new(
                "spill-planner reload-recipe analysis",
                "RELOAD_RECIPE.EDGE_PREDECESSOR_EXISTS",
                None,
                None,
                Vec::new(),
                format!("edge operation predecessor index {predecessor} is outside function"),
            ));
        };
        let insertion = super::cfg::edge_insertion_point(func, cfg, predecessor, successor)
            .ok_or_else(|| {
                super::RegallocError::new(
                    "spill-planner reload-recipe analysis",
                    "RELOAD_RECIPE.EDGE_POINT",
                    Some(predecessor_block.id),
                    None,
                    Vec::new(),
                    "edge reload has no single-edge materialization point",
                )
            })?;
        let block = &func.blocks[insertion.block];
        if insertion.instruction >= block.insts.len() {
            return Err(super::RegallocError::new(
                "spill-planner reload-recipe analysis",
                "RELOAD_RECIPE.EDGE_POINT",
                Some(block.id),
                Some(insertion.instruction),
                Vec::new(),
                "edge reload insertion point is outside its MIR block",
            ));
        }
        for operation in operations {
            let PlannedEdgeOp::Reload { source, .. } = operation else {
                continue;
            };
            requested.insert(PointUse {
                block: block.id,
                instruction: insertion.instruction,
                value: VReg(source.0),
            });
        }
    }
    Ok(requested)
}
