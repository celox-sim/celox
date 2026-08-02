//! Program-wide combinational passes that run after per-unit pipelines.

use super::*;
use crate::optimizer::passes::control_flow::{pass_branchify_mux, pass_guarded_region_sinking};
use crate::optimizer::passes::memory::pass_identity_store_bypass;

/// Keeping the ordering in one named stage makes those dependencies explicit.
/// Run program-wide and final-boundary combinational transforms.
///
/// These passes intentionally run after the main per-EU pipeline: several of
/// them depend on address aliases or CFG shapes produced by earlier passes.
pub(super) fn optimize_late_comb(
    program: &mut OptimizationContext,
    opt: &crate::OptimizeOptions,
    options: &PassOptions,
    unpacked_element_widths: &crate::HashMap<AbsoluteAddr, usize>,
) {
    let on = |pass: SirPass| opt.is_enabled(pass);
    let trace = opt.diagnostics.branchify_stats;
    let timing = opt.diagnostics.pass_timing;
    let mut checkpoint = crate::timing::now();
    let verify_stage = |program: &OptimizationContext, stage: &'static str| {
        if !opt.diagnostics.verify_passes {
            return;
        }
        for (unit, eu) in program.sir.eval_comb.iter().enumerate() {
            if let Err(error) = crate::verify_memory_offset_contract(program.design, eu) {
                panic!("after late comb stage {stage} in unit {unit}: {error}");
            }
            if let Err(error) =
                pass_manager::verify_unpacked_element_boundaries(eu, unpacked_element_widths)
            {
                panic!("after late comb stage {stage} in unit {unit}: {error}");
            }
        }
    };
    macro_rules! checkpoint {
        ($name:literal) => {
            if timing {
                tracing::debug!("[late-comb-timing] {}: {:?}", $name, checkpoint.elapsed());
                checkpoint = crate::timing::now();
            }
        };
    }
    let branchify_watermarks = program
        .sir
        .eval_comb
        .iter()
        .map(|eu| eu.blocks.keys().map(|block| block.0).max().unwrap_or(0))
        .collect::<Vec<_>>();

    // Identity Store bypass: share storage when B is unread; otherwise lower
    // a profitable exact copy directly from A's storage.
    if on(SirPass::IdentityStoreBypass) {
        let identity_aliases = pass_identity_store_bypass::optimize_program_identity_stores(
            program,
            options.four_state,
        );
        if !identity_aliases.is_empty() {
            program
                .layout_requirements
                .state_aliases_mut()
                .extend(identity_aliases);
        }
    }
    verify_stage(program, "identity-store bypass");
    checkpoint!("identity-store bypass");
    if trace {
        tracing::debug!("[branchify-stats] late identity");
    }

    // Identity-store bypass can make an entire expression DAG dead.
    if on(SirPass::LoopIdiom) {
        for eu in &mut program.sir.eval_comb {
            pass_manager::ExecutionUnitPass::run(&LoopIdiomPass, eu, options);
        }
    }
    verify_stage(program, "loop idiom");
    checkpoint!("loop idiom");
    if trace {
        tracing::debug!("[branchify-stats] late loop");
    }
    if on(SirPass::PackedScatterStore) {
        let packed_scatter_store = PackedScatterStorePass::for_program(program);
        for eu in &mut program.sir.eval_comb {
            pass_manager::ExecutionUnitPass::run(&packed_scatter_store, eu, options);
        }
    }
    verify_stage(program, "packed scatter");
    checkpoint!("packed scatter");
    if trace {
        tracing::debug!("[branchify-stats] late scatter");
    }
    if on(SirPass::IndexedStoreRecovery) {
        let indexed_store_recovery = IndexedStoreRecoveryPass::for_program(program);
        for eu in &mut program.sir.eval_comb {
            pass_manager::ExecutionUnitPass::run(&indexed_store_recovery, eu, options);
        }
    }
    verify_stage(program, "indexed-store recovery");
    checkpoint!("indexed-store recovery");
    if trace {
        tracing::debug!("[branchify-stats] late indexed");
    }
    if on(SirPass::GuardedRegionSinking) {
        for eu in &mut program.sir.eval_comb {
            pass_manager::ExecutionUnitPass::run(&GuardedRegionSinkingPass, eu, options);
        }
    }
    verify_stage(program, "guarded-region sinking");
    checkpoint!("guarded-region sinking");
    if trace {
        tracing::debug!("[branchify-stats] late guarded");
    }
    if on(SirPass::SparseCaseDispatch) {
        let sparse_case_pass =
            SparseCaseDispatchPass::new(program.layout_requirements.state_aliases());
        if trace {
            tracing::debug!("[branchify-stats] late sparse constructed");
        }
        for eu in &mut program.sir.eval_comb {
            pass_manager::ExecutionUnitPass::run(&sparse_case_pass, eu, options);
        }
    }
    verify_stage(program, "sparse-case dispatch");
    checkpoint!("sparse-case dispatch");
    if trace {
        tracing::debug!("[branchify-stats] late sparse");
    }

    // Recover control dependence created by the program-wide transforms, then
    // repair value placement and correlated merge state on that final CFG.
    if on(SirPass::BranchifyMux) {
        for (eu, watermark) in program.sir.eval_comb.iter_mut().zip(branchify_watermarks) {
            pass_branchify_mux::run_late_branchify_mux(eu, options, watermark);
        }
        for eu in &mut program.sir.eval_comb {
            pass_guarded_region_sinking::sink_pure_values_with_predicate_repair(eu);
            pass_manager::ExecutionUnitPass::run(&PhiOutcomeCompressionPass, eu, options);
        }
    }
    verify_stage(program, "branchify and placement repair");
    checkpoint!("branchify and placement repair");

    // Final canonicalization must precede native lowering, which fixes live
    // ranges and spill slots.
    if on(SirPass::Gvn) {
        for eu in &mut program.sir.eval_comb {
            pass_manager::ExecutionUnitPass::run(&GvnPass, eu, options);
        }
    }
    verify_stage(program, "final GVN");
    checkpoint!("final GVN");
    if on(SirPass::ControlFlowSimplify) {
        for eu in &mut program.sir.eval_comb {
            pass_manager::ExecutionUnitPass::run(&ControlFlowSimplifyPass, eu, options);
        }
    }
    verify_stage(program, "final CFG simplify");
    checkpoint!("final CFG simplify");
    let _ = checkpoint;
}
