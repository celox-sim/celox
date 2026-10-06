//! Optimization of lane-partitioned kernels.
//!
//! Every lane unit becomes part of a separately compiled function which may
//! run concurrently with units of other lanes; their order is derived later
//! from final memory effects. Only passes whose reasoning stays inside one
//! unit are applied. Combinational units use the sequential comb pipeline and
//! the per-unit late stages; program-wide identity aliasing is decided once
//! from the sequential kernel. FF units use the split-path evaluate and apply
//! pipelines, which never assume that a unit observes its own publication.

use super::*;
use crate::optimizer::passes::control_flow::{pass_branchify_mux, pass_guarded_region_sinking};
use celox_sir::LaneUnit;

fn run_lane_units(
    units: &mut Vec<LaneUnit<RegionedAbsoluteAddr>>,
    passes: &ExecutionUnitPassManager,
    options: &PassOptions,
) {
    if units.is_empty() {
        return;
    }
    let lanes = units.iter().map(|unit| unit.lane).collect::<Vec<_>>();
    let mut plain = std::mem::take(units)
        .into_iter()
        .map(|unit| unit.unit)
        .collect::<Vec<_>>();
    passes.run_parallel(&mut plain, options);
    *units = lanes
        .into_iter()
        .zip(plain)
        .map(|(lane, unit)| LaneUnit::new(lane, unit))
        .collect();
}

/// Run the per-unit pipelines over every partitioned kernel.
pub(super) fn optimize_parallel_units(
    program: &mut OptimizationContext<'_>,
    comb_passes: &ExecutionUnitPassManager,
    eval_only_passes: &ExecutionUnitPassManager,
    apply_passes: &ExecutionUnitPassManager,
    options: &PassOptions,
) {
    let Some(parallel) = program.sir.parallel.as_mut() else {
        return;
    };
    run_lane_units(&mut parallel.eval_comb, comb_passes, options);
    for kernel in parallel.eval_apply_ffs.values_mut() {
        run_lane_units(&mut kernel.evaluations, eval_only_passes, options);
        run_lane_units(&mut kernel.applications, apply_passes, options);
    }
}

/// Apply the per-unit stages of the late combinational pipeline, in the same
/// order as the sequential kernel, to every partitioned comb unit.
pub(super) fn optimize_late_parallel_comb(
    program: &mut OptimizationContext<'_>,
    opt: &crate::OptimizeOptions,
    options: &PassOptions,
) {
    if program
        .sir
        .parallel
        .as_ref()
        .is_none_or(|parallel| parallel.eval_comb.is_empty())
    {
        return;
    }
    let on = |pass: SirPass| opt.is_enabled(pass);
    let packed_scatter_store =
        on(SirPass::PackedScatterStore).then(|| PackedScatterStorePass::for_program(program));
    let indexed_store_recovery =
        on(SirPass::IndexedStoreRecovery).then(|| IndexedStoreRecoveryPass::for_program(program));
    let sparse_case_dispatch = on(SirPass::SparseCaseDispatch)
        .then(|| SparseCaseDispatchPass::new(program.layout_requirements.state_aliases()));
    let parallel = program
        .sir
        .parallel
        .as_mut()
        .expect("checked parallel program above");
    for LaneUnit { unit, .. } in &mut parallel.eval_comb {
        let watermark = unit.blocks.keys().map(|block| block.0).max().unwrap_or(0);
        if on(SirPass::LoopIdiom) {
            pass_manager::ExecutionUnitPass::run(&LoopIdiomPass, unit, options);
        }
        if let Some(pass) = &packed_scatter_store {
            pass_manager::ExecutionUnitPass::run(pass, unit, options);
        }
        if let Some(pass) = &indexed_store_recovery {
            pass_manager::ExecutionUnitPass::run(pass, unit, options);
        }
        if on(SirPass::GuardedRegionSinking) {
            pass_manager::ExecutionUnitPass::run(&GuardedRegionSinkingPass, unit, options);
        }
        if let Some(pass) = &sparse_case_dispatch {
            pass_manager::ExecutionUnitPass::run(pass, unit, options);
        }
        if on(SirPass::BranchifyMux) {
            pass_branchify_mux::run_late_branchify_mux(unit, options, watermark);
            pass_guarded_region_sinking::sink_pure_values_with_predicate_repair(unit);
            pass_manager::ExecutionUnitPass::run(&PhiOutcomeCompressionPass, unit, options);
        }
        if on(SirPass::Gvn) {
            pass_manager::ExecutionUnitPass::run(&GvnPass, unit, options);
        }
        if on(SirPass::ControlFlowSimplify) {
            pass_manager::ExecutionUnitPass::run(&ControlFlowSimplifyPass, unit, options);
        }
    }
}
