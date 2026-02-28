use crate::ir::*;
use crate::optimizer::{PassOptions, ProgramPass};

mod block_opt;
mod commit_ops;
mod dead_working_stores;
mod pass_commit_sinking;
mod pass_eliminate_dead_working_stores;
mod pass_hoist_common_branch_loads;
mod pass_inline_commit_forwarding;
mod pass_manager;
mod pass_optimize_blocks;
mod pass_reschedule;
mod pass_split_wide_commits;
mod shared;

use pass_commit_sinking::CommitSinkingPass;
use pass_eliminate_dead_working_stores::EliminateDeadWorkingStoresPass;
use pass_hoist_common_branch_loads::HoistCommonBranchLoadsPass;
use pass_inline_commit_forwarding::InlineCommitForwardingPass;
use pass_manager::ExecutionUnitPassManager;
use pass_optimize_blocks::OptimizeBlocksPass;
use pass_reschedule::ReschedulePass;
use pass_split_wide_commits::SplitWideCommitsPass;

pub struct CoalescingPass;

impl ProgramPass for CoalescingPass {
    fn name(&self) -> &'static str {
        "coalescing"
    }

    fn run(&self, program: &mut Program, options: &PassOptions) {
        optimize_with_options(program, options.max_inflight_loads);
    }
}

fn optimize_with_options(program: &mut Program, max_inflight_loads: usize) {
    let options = PassOptions { max_inflight_loads };

    // 1. Unified Case (Fast Path): Full optimizations are safe.
    let mut ff_passes = ExecutionUnitPassManager::new();
    ff_passes.add_pass(HoistCommonBranchLoadsPass);
    ff_passes.add_pass(OptimizeBlocksPass);
    ff_passes.add_pass(SplitWideCommitsPass);
    ff_passes.add_pass(CommitSinkingPass);
    ff_passes.add_pass(InlineCommitForwardingPass);
    ff_passes.add_pass(EliminateDeadWorkingStoresPass);
    ff_passes.add_pass(ReschedulePass);

    for units in program.eval_apply_ffs.values_mut() {
        for eu in units {
            ff_passes.run(eu, &options);
        }
    }

    // 2. Logic-Only Cache (Split Path Phase 1):
    // MUST NOT use EliminateDeadWorkingStoresPass because the Commits are in Phase 2.
    let mut eval_only_passes = ExecutionUnitPassManager::new();
    eval_only_passes.add_pass(HoistCommonBranchLoadsPass);
    eval_only_passes.add_pass(OptimizeBlocksPass);
    eval_only_passes.add_pass(ReschedulePass);

    for units in program.eval_only_ffs.values_mut() {
        for eu in units {
            eval_only_passes.run(eu, &options);
        }
    }

    // 3. Commit-Only Cache (Split Path Phase 2):
    let mut apply_passes = ExecutionUnitPassManager::new();
    apply_passes.add_pass(HoistCommonBranchLoadsPass);
    apply_passes.add_pass(OptimizeBlocksPass); // Still useful for loading from working memory
    apply_passes.add_pass(SplitWideCommitsPass);
    apply_passes.add_pass(CommitSinkingPass);
    apply_passes.add_pass(ReschedulePass);

    for units in program.apply_ffs.values_mut() {
        for eu in units {
            apply_passes.run(eu, &options);
        }
    }

    // 4. Combinational Blocks:
    let mut comb_passes = ExecutionUnitPassManager::new();
    comb_passes.add_pass(HoistCommonBranchLoadsPass);
    comb_passes.add_pass(OptimizeBlocksPass);

    for eu in program.eval_comb.iter_mut() {
        comb_passes.run(eu, &options);
    }

    // Support combinational blocks (LogicPath)
    // module.comb_blocks contains LogicPath<VarId>.
    // We need to modify LogicPath to access instructions if we want to optimize them.
    // However, logic paths are complex trees, not linear blocks until flatted/lowered?
    // Wait, SimModule already went through lowering?
    // LogicPath structure:
    // pub struct LogicPath<A> {
    //    pub target: LogicTarget<A>,
    //    pub terms: Vec<LogicTerm<A>>,
    // }
    // LogicTerm contains ExecutionUnit or similar?
    // Let's check LogicPath definition. Actually SimModule.comb_blocks uses LogicPath<VarId>.
    // LogicPath has `AST` like structure? No, it has `terms`.
    // Let's look at `ir.rs`.
}
