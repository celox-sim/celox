use crate::ir::*;
use crate::optimizer::{PassOptions, ProgramPass};

mod block_opt;
mod commit_ops;
pub mod cost_model;
mod dead_working_stores;
mod pass_bit_extract_peephole;
mod pass_commit_sinking;
pub(crate) mod pass_dead_store_elimination;
mod pass_eliminate_dead_working_stores;
mod pass_hoist_common_branch_loads;
mod pass_inline_commit_forwarding;
mod pass_manager;
mod pass_optimize_blocks;
mod pass_reschedule;
mod pass_split_wide_commits;
mod pass_store_load_forwarding;
pub(crate) mod pass_tail_call_split;
mod shared;

pub use pass_tail_call_split::TailCallChunk;

use pass_bit_extract_peephole::BitExtractPeepholePass;
use pass_commit_sinking::CommitSinkingPass;
use pass_eliminate_dead_working_stores::EliminateDeadWorkingStoresPass;
use pass_hoist_common_branch_loads::HoistCommonBranchLoadsPass;
use pass_inline_commit_forwarding::InlineCommitForwardingPass;
use pass_manager::ExecutionUnitPassManager;
use pass_optimize_blocks::OptimizeBlocksPass;
use pass_reschedule::ReschedulePass;
use pass_split_wide_commits::SplitWideCommitsPass;
use pass_store_load_forwarding::StoreLoadForwardingPass;

pub struct CoalescingPass;

impl ProgramPass for CoalescingPass {
    fn name(&self) -> &'static str {
        "coalescing"
    }

    fn run(&self, program: &mut Program, options: &PassOptions) {
        optimize_with_options(program, options.max_inflight_loads, options.four_state);
    }
}

fn optimize_with_options(program: &mut Program, max_inflight_loads: usize, four_state: bool) {
    let timing = std::env::var("CELOX_PASS_TIMING").is_ok();
    let options = PassOptions {
        max_inflight_loads,
        four_state,
    };

    // 1. Unified Case (Fast Path): Full optimizations are safe.
    let phase_start = timing.then(std::time::Instant::now);
    let mut ff_passes = ExecutionUnitPassManager::new();
    ff_passes.add_pass(StoreLoadForwardingPass);
    ff_passes.add_pass(HoistCommonBranchLoadsPass);
    ff_passes.add_pass(BitExtractPeepholePass);
    ff_passes.add_pass(OptimizeBlocksPass);
    ff_passes.add_pass(SplitWideCommitsPass);
    ff_passes.add_pass(CommitSinkingPass);
    ff_passes.add_pass(InlineCommitForwardingPass);
    ff_passes.add_pass(EliminateDeadWorkingStoresPass);
    ff_passes.add_pass(ReschedulePass);

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
    let phase_start = timing.then(std::time::Instant::now);
    let mut eval_only_passes = ExecutionUnitPassManager::new();
    eval_only_passes.add_pass(StoreLoadForwardingPass);
    eval_only_passes.add_pass(HoistCommonBranchLoadsPass);
    eval_only_passes.add_pass(BitExtractPeepholePass);
    eval_only_passes.add_pass(OptimizeBlocksPass);
    eval_only_passes.add_pass(ReschedulePass);

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
    let phase_start = timing.then(std::time::Instant::now);
    let mut apply_passes = ExecutionUnitPassManager::new();
    apply_passes.add_pass(StoreLoadForwardingPass);
    apply_passes.add_pass(HoistCommonBranchLoadsPass);
    apply_passes.add_pass(BitExtractPeepholePass);
    apply_passes.add_pass(OptimizeBlocksPass); // Still useful for loading from working memory
    apply_passes.add_pass(SplitWideCommitsPass);
    apply_passes.add_pass(CommitSinkingPass);
    apply_passes.add_pass(ReschedulePass);

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
    let phase_start = timing.then(std::time::Instant::now);
    let mut comb_passes = ExecutionUnitPassManager::new();
    comb_passes.add_pass(StoreLoadForwardingPass);
    comb_passes.add_pass(HoistCommonBranchLoadsPass);
    comb_passes.add_pass(BitExtractPeepholePass);
    comb_passes.add_pass(OptimizeBlocksPass);

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

    // 5. Tail-call chain splitting for eval_comb.
    // When the estimated CLIF instruction count exceeds Cranelift's limit,
    // split into a chain of smaller functions connected by tail calls.
    //
    // Try EU-boundary / single-block splitting first (zero live-reg cost).
    // Fall back to memory-spilled multi-block splitting if needed.
    if timing {
        for (i, eu) in program.eval_comb.iter().enumerate() {
            let cost = cost_model::estimate_eu_cost(eu, four_state);
            eprintln!(
                "[split-check] eval_comb eu[{i}]: blocks={} insts={} clif_cost={cost} threshold={}",
                eu.blocks.len(),
                eu.blocks
                    .values()
                    .map(|b| b.instructions.len())
                    .sum::<usize>(),
                cost_model::CLIF_INST_THRESHOLD
            );
        }
    }
    let split_start = timing.then(std::time::Instant::now);
    if let Some(chunks) = pass_tail_call_split::split_if_needed(&program.eval_comb, four_state) {
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
