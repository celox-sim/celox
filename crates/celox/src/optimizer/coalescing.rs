use crate::ir::*;
use crate::optimizer::{PassOptions, ProgramPass, SirPass};

mod block_opt;
pub(crate) mod commit_ops;
pub mod cost_model;
mod dead_working_stores;
mod pass_bit_extract_peephole;
mod pass_coalesce_stores;
mod pass_commit_sinking;
mod pass_concat_folding;
pub(crate) mod pass_dead_store_elimination;
mod pass_eliminate_dead_working_stores;
pub(crate) mod pass_eliminate_working_round_trip;
mod pass_gvn;
mod pass_hoist_common_branch_loads;
mod pass_identity_store_bypass;
mod pass_inline_commit_forwarding;
mod pass_manager;
mod pass_optimize_blocks;
mod pass_partial_forward;
mod pass_reschedule;
mod pass_split_coalesced_stores;
mod pass_split_wide_commits;
mod pass_store_load_forwarding;
pub(crate) mod pass_tail_call_split;
mod pass_vectorize_concat;
mod pass_xor_chain_folding;
mod shared;

pub use pass_tail_call_split::TailCallChunk;

use pass_bit_extract_peephole::BitExtractPeepholePass;
use pass_coalesce_stores::CoalesceStoresPass;
use pass_commit_sinking::CommitSinkingPass;
use pass_concat_folding::ConcatFoldingPass;
use pass_eliminate_dead_working_stores::EliminateDeadWorkingStoresPass;
use pass_gvn::GvnPass;
use pass_hoist_common_branch_loads::HoistCommonBranchLoadsPass;
use pass_inline_commit_forwarding::InlineCommitForwardingPass;
use pass_manager::ExecutionUnitPassManager;
use pass_optimize_blocks::OptimizeBlocksPass;
use pass_partial_forward::PartialForwardPass;
use pass_reschedule::ReschedulePass;
use pass_split_coalesced_stores::SplitCoalescedStoresPass;
use pass_split_wide_commits::SplitWideCommitsPass;
use pass_store_load_forwarding::StoreLoadForwardingPass;
use pass_vectorize_concat::VectorizeConcatPass;
use pass_xor_chain_folding::XorChainFoldingPass;

pub struct CoalescingPass;

impl ProgramPass for CoalescingPass {
    fn name(&self) -> &'static str {
        "coalescing"
    }

    fn run(&self, program: &mut Program, options: &PassOptions) {
        optimize_with_options(
            program,
            options.max_inflight_loads,
            options.four_state,
            &options.optimize_options,
        );
    }
}

fn optimize_with_options(
    program: &mut Program,
    max_inflight_loads: usize,
    four_state: bool,
    opt: &crate::optimizer::OptimizeOptions,
) {
    #[cfg(not(target_arch = "wasm32"))]
    let timing = std::env::var("CELOX_PASS_TIMING").is_ok();
    #[cfg(target_arch = "wasm32")]
    let timing = false;
    let options = PassOptions {
        max_inflight_loads,
        four_state,
        optimize_options: opt.clone(),
    };

    // Helper closure to check pass enablement.
    let on = |pass: SirPass| opt.is_enabled(pass);

    // 1. Unified Case (Fast Path): Full optimizations are safe.
    let phase_start = timing.then(crate::timing::now);
    let mut ff_passes = ExecutionUnitPassManager::new();
    // Note: EliminateWorkingRoundTripPass runs post-merge in emit_chained_eus
    // with boundary info for cross-EU independence check.
    // Per-EU elimination is NOT safe without dependency analysis.
    if on(SirPass::StoreLoadForwarding) {
        ff_passes.add_pass(StoreLoadForwardingPass);
    }
    if on(SirPass::Gvn) {
        ff_passes.add_pass(GvnPass);
    }
    if on(SirPass::ConcatFolding) {
        ff_passes.add_pass(ConcatFoldingPass);
    }
    if on(SirPass::XorChainFolding) {
        ff_passes.add_pass(XorChainFoldingPass);
    }
    if on(SirPass::HoistCommonBranchLoads) {
        ff_passes.add_pass(HoistCommonBranchLoadsPass);
    }
    if on(SirPass::BitExtractPeephole) {
        ff_passes.add_pass(BitExtractPeepholePass);
    }
    if on(SirPass::OptimizeBlocks) {
        ff_passes.add_pass(OptimizeBlocksPass {
            skip_final_schedule: on(SirPass::Reschedule),
        });
    }
    if on(SirPass::CoalesceStores) {
        ff_passes.add_pass(CoalesceStoresPass);
    }
    if on(SirPass::SplitWideCommits) {
        ff_passes.add_pass(SplitWideCommitsPass);
    }
    if on(SirPass::CommitSinking) {
        ff_passes.add_pass(CommitSinkingPass);
    }
    if on(SirPass::InlineCommitForwarding) {
        ff_passes.add_pass(InlineCommitForwardingPass);
    }
    if on(SirPass::EliminateDeadWorkingStores) {
        ff_passes.add_pass(EliminateDeadWorkingStoresPass);
    }
    if on(SirPass::Reschedule) {
        ff_passes.add_pass(ReschedulePass);
    }
    // Split wide Concat+Store back into individual stores after reschedule.
    // Coalesce_stores combines for fewer memory ops, but for large arrays
    // the Concat forces all values live simultaneously → spill hell.
    // Splitting + hoisting stores next to their defs minimizes live ranges.
    if on(SirPass::SplitCoalescedStores) {
        ff_passes.add_pass(SplitCoalescedStoresPass);
    }

    let eu_count: usize = program.eval_apply_ffs.values().map(|v| v.len()).sum();
    for units in program.eval_apply_ffs.values_mut() {
        for eu in units {
            ff_passes.run(eu, &options);
        }
    }
    if let Some(s) = phase_start {
        eprintln!("[phase] eval_apply_ffs ({eu_count} EUs): {:?}", s.elapsed());
    }

    // 2. Logic-Only Cache (Split Path Phase 1):
    // MUST NOT use EliminateDeadWorkingStoresPass because the Commits are in Phase 2.
    let phase_start = timing.then(crate::timing::now);
    let mut eval_only_passes = ExecutionUnitPassManager::new();
    if on(SirPass::StoreLoadForwarding) {
        eval_only_passes.add_pass(StoreLoadForwardingPass);
    }
    if on(SirPass::Gvn) {
        eval_only_passes.add_pass(GvnPass);
    }
    if on(SirPass::ConcatFolding) {
        eval_only_passes.add_pass(ConcatFoldingPass);
    }
    if on(SirPass::XorChainFolding) {
        eval_only_passes.add_pass(XorChainFoldingPass);
    }
    if on(SirPass::HoistCommonBranchLoads) {
        eval_only_passes.add_pass(HoistCommonBranchLoadsPass);
    }
    if on(SirPass::BitExtractPeephole) {
        eval_only_passes.add_pass(BitExtractPeepholePass);
    }
    if on(SirPass::OptimizeBlocks) {
        eval_only_passes.add_pass(OptimizeBlocksPass {
            skip_final_schedule: on(SirPass::Reschedule),
        });
    }
    if on(SirPass::CoalesceStores) {
        eval_only_passes.add_pass(CoalesceStoresPass);
    }
    if on(SirPass::Reschedule) {
        eval_only_passes.add_pass(ReschedulePass);
    }

    let eu_count: usize = program.eval_only_ffs.values().map(|v| v.len()).sum();
    for units in program.eval_only_ffs.values_mut() {
        for eu in units {
            eval_only_passes.run(eu, &options);
        }
    }
    if let Some(s) = phase_start {
        eprintln!("[phase] eval_only_ffs ({eu_count} EUs): {:?}", s.elapsed());
    }

    // 3. Commit-Only Cache (Split Path Phase 2):
    let phase_start = timing.then(crate::timing::now);
    let mut apply_passes = ExecutionUnitPassManager::new();
    if on(SirPass::StoreLoadForwarding) {
        apply_passes.add_pass(StoreLoadForwardingPass);
    }
    if on(SirPass::HoistCommonBranchLoads) {
        apply_passes.add_pass(HoistCommonBranchLoadsPass);
    }
    if on(SirPass::BitExtractPeephole) {
        apply_passes.add_pass(BitExtractPeepholePass);
    }
    if on(SirPass::OptimizeBlocks) {
        apply_passes.add_pass(OptimizeBlocksPass {
            skip_final_schedule: on(SirPass::Reschedule),
        });
    } // Still useful for loading from working memory
    if on(SirPass::CoalesceStores) {
        apply_passes.add_pass(CoalesceStoresPass);
    }
    if on(SirPass::SplitWideCommits) {
        apply_passes.add_pass(SplitWideCommitsPass);
    }
    if on(SirPass::CommitSinking) {
        apply_passes.add_pass(CommitSinkingPass);
    }
    if on(SirPass::Reschedule) {
        apply_passes.add_pass(ReschedulePass);
    }

    let eu_count: usize = program.apply_ffs.values().map(|v| v.len()).sum();
    for units in program.apply_ffs.values_mut() {
        for eu in units {
            apply_passes.run(eu, &options);
        }
    }
    if let Some(s) = phase_start {
        eprintln!("[phase] apply_ffs ({eu_count} EUs): {:?}", s.elapsed());
    }

    // 4. Combinational Blocks:
    let phase_start = timing.then(crate::timing::now);
    let mut comb_passes = ExecutionUnitPassManager::new();
    if on(SirPass::StoreLoadForwarding) {
        comb_passes.add_pass(StoreLoadForwardingPass);
        if on(SirPass::PartialForward) {
            comb_passes.add_pass(PartialForwardPass);
        }
    }
    if on(SirPass::Gvn) {
        comb_passes.add_pass(GvnPass);
    }
    if on(SirPass::ConcatFolding) {
        comb_passes.add_pass(ConcatFoldingPass);
    }
    if on(SirPass::XorChainFolding) {
        comb_passes.add_pass(XorChainFoldingPass);
    }
    if on(SirPass::HoistCommonBranchLoads) {
        comb_passes.add_pass(HoistCommonBranchLoadsPass);
    }
    if on(SirPass::BitExtractPeephole) {
        comb_passes.add_pass(BitExtractPeepholePass);
    }
    if on(SirPass::OptimizeBlocks) {
        comb_passes.add_pass(OptimizeBlocksPass {
            skip_final_schedule: false, // eval_comb has no reschedule pass
        });
    }
    if on(SirPass::CoalesceStores) {
        comb_passes.add_pass(CoalesceStoresPass);
    }
    if on(SirPass::VectorizeConcat) {
        comb_passes.add_pass(VectorizeConcatPass);
    }
    if on(SirPass::Gvn) {
        comb_passes.add_pass(GvnPass); // DCE for dead bit-extract chains after vectorization
    }

    let eu_count = program.eval_comb.len();
    for (i, eu) in program.eval_comb.iter_mut().enumerate() {
        if timing {
            let inst_count: usize = eu.blocks.values().map(|b| b.instructions.len()).sum();
            let block_count = eu.blocks.len();
            eprintln!("[phase] eval_comb eu[{i}]: blocks={block_count} insts={inst_count}");
        }
        comb_passes.run(eu, &options);
    }
    if let Some(s) = phase_start {
        eprintln!("[phase] eval_comb ({eu_count} EUs): {:?}", s.elapsed());
    }

    // Identity Store bypass: detect Store(B, identity_copy_from_A), remove it,
    // and register B→A alias for memory layout sharing.
    if on(SirPass::IdentityStoreBypass) {
        let identity_pass = pass_identity_store_bypass::IdentityStoreBypassPass::new();
        for eu in &mut program.eval_comb {
            pass_manager::ExecutionUnitPass::run(&identity_pass, eu, &options);
        }
        let identity_aliases = identity_pass.aliases.into_inner();
        if !identity_aliases.is_empty() {
            // Store alias candidates in program for memory layout validation
            program.address_aliases.extend(identity_aliases);
        }
    }

    // 5. Tail-call chain splitting for eval_comb.
    // When the estimated CLIF instruction count exceeds Cranelift's limit,
    // split into a chain of smaller functions connected by tail calls.
    //
    // Try EU-boundary / single-block splitting first (zero live-reg cost).
    // Fall back to memory-spilled multi-block splitting if needed.
    if on(SirPass::TailCallSplit) {
        if timing {
            for (i, eu) in program.eval_comb.iter().enumerate() {
                let inst_cost = cost_model::estimate_eu_cost(eu, four_state);
                let value_count = cost_model::estimate_eu_value_count(eu, four_state);
                eprintln!(
                    "[split-check] eval_comb eu[{i}]: blocks={} insts={} clif_cost={inst_cost}/{} values={value_count}/{}",
                    eu.blocks.len(),
                    eu.blocks
                        .values()
                        .map(|b| b.instructions.len())
                        .sum::<usize>(),
                    cost_model::CLIF_INST_THRESHOLD,
                    cost_model::VREG_VALUE_THRESHOLD,
                );
            }
        }
        let split_start = timing.then(crate::timing::now);
        if let Some(chunks) = pass_tail_call_split::split_if_needed(&program.eval_comb, four_state)
        {
            if timing {
                eprintln!(
                    "[split] TailCallChunks: {} chunks, took {:?}",
                    chunks.len(),
                    split_start.unwrap().elapsed()
                );
            }
            program.eval_comb_plan = Some(crate::ir::EvalCombPlan::TailCallChunks(chunks));
        } else if let Some(plan) =
            pass_tail_call_split::split_if_needed_spilled(&program.eval_comb, four_state)
        {
            if timing {
                eprintln!(
                    "[split] MemorySpilled: {} chunks, scratch={}B, took {:?}",
                    plan.chunks.len(),
                    plan.scratch_bytes,
                    split_start.unwrap().elapsed()
                );
                for (i, chunk) in plan.chunks.iter().enumerate() {
                    let blocks = chunk.eu.blocks.len();
                    let insts: usize = chunk.eu.blocks.values().map(|b| b.instructions.len()).sum();
                    eprintln!(
                        "[split]   chunk[{i}]: blocks={blocks} insts={insts} in_spills={} out_spills={} cross_edges={}",
                        chunk.incoming_spills.len(),
                        chunk.outgoing_spills.len(),
                        chunk.cross_chunk_edges.len()
                    );
                }
            }
            program.eval_comb_plan = Some(crate::ir::EvalCombPlan::MemorySpilled(plan));
        }
    }
}
