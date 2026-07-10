//! Verified non-iterative SSA register-allocation pipeline.

use crate::backend::native::mir::{MFunction, VReg};

use super::assignment::AssignmentMap;
use super::cfg::NormalizedCfg;
use super::next_use::NextUseAnalysis;

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
) -> Result<Allocation, super::RegallocError> {
    let timing = std::env::var_os("CELOX_REGALLOC_TIMING").is_some()
        || std::env::var_os("CELOX_PHASE_TIMING").is_some();
    let phase = timing.then(crate::timing::now);
    let plan = super::spill_plan::plan(func, cfg, next_use, super::NUM_REGS);
    plan.verify(func, cfg, super::NUM_REGS);
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
            "[regalloc-timing] ssa spill_plan elapsed={:?}",
            start.elapsed()
        );
    }

    let phase = timing.then(crate::timing::now);
    let reconstruction = super::reconstruct::reconstruct(func, cfg, &plan, next_use);
    if let Some(start) = phase {
        eprintln!(
            "[regalloc-timing] ssa reconstruct vregs={} insts={} frame={} elapsed={:?}",
            func.vregs.count(),
            func.blocks
                .iter()
                .map(|block| block.insts.len())
                .sum::<usize>(),
            reconstruction.frame_size,
            start.elapsed()
        );
    }
    func.verify();

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
    let (color_cfg, perms) = super::legalize::materialize_constraint_perms(func, cfg);
    func.verify();
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
        debug_assert_eq!(coloring.assignment.get(destination), Some(register));
    }
    if let Some(start) = phase {
        eprintln!("[regalloc-timing] ssa color elapsed={:?}", start.elapsed());
    }

    Ok(Allocation {
        assignment: coloring.assignment,
        spill_frame_size: reconstruction.frame_size,
    })
}
