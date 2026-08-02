//! Named construction of the execution-unit pipelines.
//!
//! Pass ordering is part of the optimizer contract: several passes expose the
//! patterns consumed by the next one. Keeping construction here separates that
//! policy from iteration over the program's execution-unit collections.

use super::*;

#[derive(Clone, Copy)]
enum AfterGvn {
    PostCleanup,
    Simplify,
}

pub(super) struct PipelineBuilder<'a> {
    opt: &'a crate::OptimizeOptions,
    unpacked_element_widths: Arc<crate::HashMap<AbsoluteAddr, usize>>,
    element_widths: Arc<crate::HashMap<RegionedAbsoluteAddr, usize>>,
    max_native_memory_width: usize,
}

impl<'a> PipelineBuilder<'a> {
    pub(super) fn new(
        opt: &'a crate::OptimizeOptions,
        unpacked_element_widths: Arc<crate::HashMap<AbsoluteAddr, usize>>,
        element_widths: Arc<crate::HashMap<RegionedAbsoluteAddr, usize>>,
    ) -> Self {
        Self {
            opt,
            unpacked_element_widths,
            element_widths,
            max_native_memory_width: opt.max_native_memory_width(),
        }
    }

    fn manager(&self) -> ExecutionUnitPassManager {
        ExecutionUnitPassManager::new()
            .with_unpacked_element_widths(Arc::clone(&self.unpacked_element_widths))
    }

    fn on(&self, pass: SirPass) -> bool {
        self.opt.is_enabled(pass)
    }

    fn add_initial_simplification(
        &self,
        passes: &mut ExecutionUnitPassManager,
        program: &OptimizationContext<'_>,
        partial_forwarding: bool,
        indexed_store_recovery: bool,
        after_gvn: AfterGvn,
    ) {
        if self.on(SirPass::StoreLoadForwarding) {
            passes.add_pass(StoreLoadForwardingPass);
            if partial_forwarding && self.on(SirPass::PartialForward) {
                passes.add_pass(PartialForwardPass);
            }
        }
        if self.on(SirPass::ControlFlowSimplify) {
            passes.add_pass(ControlFlowSimplifyPass);
        }
        if self.on(SirPass::Gvn) {
            passes.add_pass(GvnPass);
            if self.on(SirPass::ControlFlowSimplify) {
                match after_gvn {
                    AfterGvn::PostCleanup => passes.add_pass(PostGvnCfgCleanupPass),
                    AfterGvn::Simplify => passes.add_pass(ControlFlowSimplifyPass),
                }
            }
        }
        if indexed_store_recovery && self.on(SirPass::IndexedStoreRecovery) {
            passes.add_pass(IndexedStoreRecoveryPass::for_program(program));
        }
        if self.on(SirPass::ConcatFolding) {
            passes.add_pass(ConcatFoldingPass::new(
                Arc::clone(&self.unpacked_element_widths),
                self.max_native_memory_width,
            ));
        }
        if self.on(SirPass::XorChainFolding) {
            passes.add_pass(XorChainFoldingPass);
        }
        if self.on(SirPass::HoistCommonBranchLoads) {
            passes.add_pass(HoistCommonBranchLoadsPass);
        }
    }

    fn add_memory_lowering(
        &self,
        passes: &mut ExecutionUnitPassManager,
        skip_final_schedule: bool,
    ) {
        if self.on(SirPass::BitExtractPeephole) {
            passes.add_pass(BitExtractPeepholePass);
        }
        if self.on(SirPass::OptimizeBlocks) {
            passes.add_pass(OptimizeBlocksPass {
                skip_final_schedule,
                element_widths: Arc::clone(&self.element_widths),
            });
        }
        if self.on(SirPass::CoalesceStores) {
            passes.add_pass(CoalesceStoresPass {
                element_widths: Arc::clone(&self.element_widths),
                max_store_width: self.max_native_memory_width,
            });
        }
    }

    /// Plain fused eval/apply path. Per-EU working round-trip elimination is
    /// intentionally absent because it requires cross-EU dependency data.
    pub(super) fn fused_ff(&self, program: &OptimizationContext<'_>) -> ExecutionUnitPassManager {
        let mut passes = self.manager();
        self.add_initial_simplification(&mut passes, program, false, true, AfterGvn::PostCleanup);
        self.add_memory_lowering(&mut passes, self.on(SirPass::Reschedule));
        if self.on(SirPass::SplitWideCommits) {
            passes.add_pass(SplitWideCommitsPass);
        }
        passes
    }

    /// Fused combinational + FF path, which retains the producer graph needed
    /// by the control and packed-data recovery passes.
    pub(super) fn fused_comb_ff(
        &self,
        program: &OptimizationContext<'_>,
    ) -> ExecutionUnitPassManager {
        let mut passes = self.manager();
        self.add_initial_simplification(&mut passes, program, true, true, AfterGvn::PostCleanup);
        if self.on(SirPass::GuardedRegionSinking) {
            passes.add_pass(GuardedRegionSinkingPass);
        }
        if self.on(SirPass::BranchifyMux) {
            passes.add_pass(BranchifyMuxPass);
            if self.on(SirPass::GuardedRegionSinking) {
                passes.add_pass(GuardedRegionSinkingPass);
            }
        }
        if self.on(SirPass::IndexedStoreRecovery) {
            passes.add_pass(IndexedStoreRecoveryPass::for_program(program));
        }
        if self.on(SirPass::BitExtractPeephole) {
            passes.add_pass(BitExtractPeepholePass);
        }
        if self.on(SirPass::LoopIdiom) {
            passes.add_pass(LoopIdiomPass);
        }
        if self.on(SirPass::OptimizeBlocks) {
            passes.add_pass(OptimizeBlocksPass {
                skip_final_schedule: self.on(SirPass::Reschedule),
                element_widths: Arc::clone(&self.element_widths),
            });
        }
        if self.on(SirPass::CoalesceStores) {
            passes.add_pass(CoalesceStoresPass {
                element_widths: Arc::clone(&self.element_widths),
                max_store_width: self.max_native_memory_width,
            });
        }
        self.add_packed_recovery(&mut passes, program);
        passes
    }

    fn add_packed_recovery(
        &self,
        passes: &mut ExecutionUnitPassManager,
        program: &OptimizationContext<'_>,
    ) {
        if self.on(SirPass::VectorizeConcat) {
            passes.add_pass(VectorizeConcatPass::new(Arc::clone(
                &self.unpacked_element_widths,
            )));
        }
        if self.on(SirPass::LoopIdiom) {
            passes.add_pass(LoopIdiomPass);
        }
        if self.on(SirPass::MaskedArrayAny) {
            passes.add_pass(MaskedArrayAnyPass::for_program(program));
        }
        if self.on(SirPass::CircularPriority) {
            passes.add_pass(CircularPriorityPass::for_program(program));
        }
    }

    pub(super) fn fused_comb_ff_late(
        &self,
        program: &OptimizationContext<'_>,
    ) -> ExecutionUnitPassManager {
        let mut passes = self.manager();
        if self.on(SirPass::GuardedRegionSinking) {
            passes.add_pass(GuardedRegionSinkingPass);
        }
        if self.on(SirPass::SparseCaseDispatch) {
            passes.add_pass(SparseCaseDispatchPass::new(
                program.layout_requirements.state_aliases(),
            ));
        }
        if self.on(SirPass::Gvn) {
            passes.add_pass(DeadCodeEliminationPass);
        }
        if self.on(SirPass::SplitWideCommits) {
            passes.add_pass(SplitWideCommitsPass);
        }
        passes
    }

    /// Shared post-pipeline for both fused FF collections. All contained passes
    /// are immutable and can safely be reused across both collections.
    pub(super) fn fused_ff_post(&self) -> ExecutionUnitPassManager {
        let mut passes = self.manager();
        if self.on(SirPass::EliminateDeadWorkingStores) {
            passes.add_pass(EliminateDeadWorkingStoresPass);
        }
        if self.on(SirPass::Reschedule) {
            passes.add_pass(ReschedulePass);
        }
        if self.on(SirPass::SplitCoalescedStores) {
            passes.add_pass(SplitCoalescedStoresPass {
                max_store_width: self.max_native_memory_width,
            });
        }
        passes
    }

    pub(super) fn eval_only(&self, program: &OptimizationContext<'_>) -> ExecutionUnitPassManager {
        let mut passes = self.manager();
        self.add_initial_simplification(&mut passes, program, false, true, AfterGvn::PostCleanup);
        self.add_memory_lowering(&mut passes, self.on(SirPass::Reschedule));
        if self.on(SirPass::Reschedule) {
            passes.add_pass(ReschedulePass);
        }
        passes
    }

    pub(super) fn apply_only(&self) -> ExecutionUnitPassManager {
        let mut passes = self.manager();
        if self.on(SirPass::StoreLoadForwarding) {
            passes.add_pass(StoreLoadForwardingPass);
        }
        if self.on(SirPass::ControlFlowSimplify) {
            passes.add_pass(ControlFlowSimplifyPass);
        }
        if self.on(SirPass::HoistCommonBranchLoads) {
            passes.add_pass(HoistCommonBranchLoadsPass);
        }
        self.add_memory_lowering(&mut passes, self.on(SirPass::Reschedule));
        if self.on(SirPass::SplitWideCommits) {
            passes.add_pass(SplitWideCommitsPass);
        }
        if self.on(SirPass::CommitSinking) {
            passes.add_pass(CommitSinkingPass);
        }
        if self.on(SirPass::Reschedule) {
            passes.add_pass(ReschedulePass);
        }
        passes
    }

    pub(super) fn combinational(
        &self,
        program: &OptimizationContext<'_>,
    ) -> ExecutionUnitPassManager {
        let mut passes = self.manager();
        self.add_initial_simplification(&mut passes, program, true, false, AfterGvn::Simplify);
        if self.on(SirPass::GuardedRegionSinking) {
            // Recover coupled outputs before branchification separates their
            // shared producer DAG behind block parameters.
            passes.add_pass(GuardedRegionSinkingPass);
        }
        if self.on(SirPass::BranchifyMux) {
            passes.add_pass(BranchifyMuxPass);
            if self.on(SirPass::GuardedRegionSinking) {
                passes.add_pass(GuardedRegionSinkingPass);
            }
        }
        if self.on(SirPass::BitExtractPeephole) {
            passes.add_pass(BitExtractPeepholePass);
        }
        if self.on(SirPass::LoopIdiom) {
            passes.add_pass(LoopIdiomPass);
        }
        if self.on(SirPass::OptimizeBlocks) {
            passes.add_pass(OptimizeBlocksPass {
                // eval_comb has no reschedule pass.
                skip_final_schedule: false,
                element_widths: Arc::clone(&self.element_widths),
            });
        }
        if self.on(SirPass::CoalesceStores) {
            passes.add_pass(CoalesceStoresPass {
                element_widths: Arc::clone(&self.element_widths),
                max_store_width: self.max_native_memory_width,
            });
        }
        self.add_packed_recovery(&mut passes, program);
        if self.on(SirPass::Gvn) {
            passes.add_pass(GvnPass);
            if self.on(SirPass::ControlFlowSimplify) {
                passes.add_pass(PostGvnCfgCleanupPass);
            }
            passes.add_pass(DeadCodeEliminationPass);
        }
        passes
    }
}
