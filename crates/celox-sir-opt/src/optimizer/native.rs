//! Post-merge optimization used by the native backend.

use super::passes::control_flow::{pass_effect_case_dispatch, pass_guarded_region_sinking};
use super::passes::dataflow::{
    pass_circular_priority, pass_pack_concat_phi, pass_vectorize_concat,
};
use super::*;
use std::sync::Arc;

pub fn optimize_merged_chain(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    element_widths: Arc<crate::HashMap<RegionedAbsoluteAddr, usize>>,
    packed_range_is_contiguous: impl Fn(RegionedAbsoluteAddr, usize, usize) -> bool,
    four_state: bool,
    recover_merged_effect_regions: bool,
    diagnostics: &crate::SirDiagnostics,
    is_cancelled: impl Fn() -> bool,
) -> Result<(), crate::OptimizationError> {
    let verify = |eu: &ExecutionUnit<RegionedAbsoluteAddr>, stage| {
        if cfg!(debug_assertions) || diagnostics.verify_boundaries {
            eu.verify_result()
                .map_err(|error| crate::OptimizationError::verification(stage, error))
        } else {
            Ok(())
        }
    };
    // Observed between pass groups so a cancelled pipeline unwinds at the
    // next boundary instead of running the remaining fused-optimization
    // stages. Callers without cancellation pass `|| false`, which folds to
    // nothing after inlining.
    let checkpoint = |stage: &'static str| -> Result<(), crate::OptimizationError> {
        if is_cancelled() {
            Err(crate::OptimizationError::cancelled(stage))
        } else {
            Ok(())
        }
    };
    let mut changed = false;
    if crate::ir::inline_single_predecessor_jumps(eu).map_err(|error| {
        crate::OptimizationError::verification("during native jump inlining", error)
    })? {
        changed = true;
    }
    verify(eu, "after native jump inlining")?;
    OptimizeBlocksPass {
        skip_final_schedule: false,
        element_widths: Arc::clone(&element_widths),
    }
    .run(eu, &PassOptions::default());
    verify(eu, "after native block optimization")?;
    // The native function is assembled after the ordinary per-EU pipeline.
    // Merging exposes constants and control-flow facts across the old EU
    // boundaries, and OptimizeBlocks can expose more of them while rewriting
    // the merged blocks.  Run full SCCP here before preserving packed sinks:
    // otherwise constant branches and their entire dead scalar arms survive
    // into lane planning and native lowering.
    ControlFlowSimplifyPass.run(
        eu,
        &PassOptions {
            four_state,
            ..PassOptions::default()
        },
    );
    verify(eu, "after native merged-chain CFG simplification")?;
    checkpoint("during native merged-chain optimization")?;
    if recover_merged_effect_regions && !four_state {
        pass_guarded_region_sinking::recover_merged_effect_regions(eu, four_state);
        pass_guarded_region_sinking::eliminate_dead_control_regions(eu);
        ControlFlowSimplifyPass.run(
            eu,
            &PassOptions {
                four_state,
                ..PassOptions::default()
            },
        );
        pass_vectorize_concat::remove_dead_definitions(eu);
        verify(eu, "after native merged effect-region recovery")?;
        changed = true;
    }
    checkpoint("during native merged-chain optimization")?;
    if !four_state {
        let collapsed = pass_vectorize_concat::collapse_packed_conditional_store_chains_with(
            eu,
            packed_range_is_contiguous,
        );
        if collapsed != 0 {
            changed = true;
            // The recovered packed predicate is a real sink for the complete
            // scalar lane graph. Run the ordinary recursive packer now, while
            // that graph is still available, rather than lowering the new
            // Concat back into scalar shifts in ISel.
            VectorizeConcatPass::default().run(eu, &PassOptions::default());
            GvnPass.run(eu, &PassOptions::default());
            verify(eu, "after native packed conditional-store recovery")?;
        }
    }
    if !four_state && pass_vectorize_concat::expose_packed_bit_store_sinks(eu) {
        changed = true;
        VectorizeConcatPass::default().run(eu, &PassOptions::default());
        GvnPass.run(eu, &PassOptions::default());
        verify(eu, "after native packed bit-store vectorization")?;
    }
    checkpoint("during native merged-chain optimization")?;
    let recovered_bit_maps = if four_state {
        0
    } else {
        pass_circular_priority::recover_native_fixed_bit_map_loops(eu)
    };
    if recovered_bit_maps != 0 {
        changed = true;
        GvnPass.run(eu, &PassOptions::default());
        OptimizeBlocksPass {
            skip_final_schedule: false,
            element_widths,
        }
        .run(eu, &PassOptions::default());
        VectorizeConcatPass::default().run(eu, &PassOptions::default());
        GvnPass.run(eu, &PassOptions::default());
        verify(eu, "after native fixed bit-map recovery")?;
    }
    checkpoint("during native merged-chain optimization")?;
    if !four_state
        && diagnostics.effect_case_dispatch
        && let Some(result) = pass_effect_case_dispatch::run(eu)
    {
        changed = true;
        let dead_control_start = crate::timing::now();
        pass_guarded_region_sinking::eliminate_dead_control_regions(eu);
        let dead_control_ns = dead_control_start.elapsed().as_nanos();
        let cfg_cleanup_start = crate::timing::now();
        ControlFlowSimplifyPass.run(
            eu,
            &PassOptions {
                four_state,
                ..PassOptions::default()
            },
        );
        let cfg_cleanup_ns = cfg_cleanup_start.elapsed().as_nanos();
        tracing::debug!(
            "[effect-case-dispatch] origin=b{} selector=r{} cases={} sinks={} \
             path_local_exits={} estimated_saving={} planning_ms={:.3} rewrite_ms={:.3} \
             dead_control_ms={:.3} cfg_cleanup_ms={:.3}",
            result.origin.0,
            result.selector.0,
            result.explicit_cases,
            result.sinks,
            result.path_local_exits,
            result.estimated_saving,
            result.planning_ns as f64 / 1_000_000.0,
            result.rewrite_ns as f64 / 1_000_000.0,
            dead_control_ns as f64 / 1_000_000.0,
            cfg_cleanup_ns as f64 / 1_000_000.0,
        );
        verify(eu, "after native effect-case dispatch")?;
    }
    if !four_state && pass_pack_concat_phi::pack_concat_phis(eu) != 0 {
        changed = true;
        GvnPass.run(eu, &PassOptions::default());
        verify(eu, "after native packed phi forwarding")?;
    }
    checkpoint("during native merged-chain optimization")?;
    if !four_state {
        // Fusion and the native-only packed/control rewrites above can create
        // new branch-local pure suffixes after the ordinary per-EU placement
        // pass has already run.  Recompute placement on this final CFG so a
        // value used only by one selected arm is not evaluated in the branch
        // head and kept live across the edge.
        pass_guarded_region_sinking::sink_pure_values_with_predicate_repair(eu);
        changed = true;
        verify(eu, "after native final pure-value placement")?;
    }
    if changed {
        pass_vectorize_concat::remove_dead_definitions(eu);
        verify(eu, "after native merged-chain DCE")?;
    }
    Ok(())
}
