//! SIR pass implementations grouped by optimization domain.

pub(in crate::optimizer) mod analysis;
pub(in crate::optimizer) mod control_flow;
pub(in crate::optimizer) mod dataflow;
pub(in crate::optimizer) mod memory;

pub use analysis::cost_model;
pub use memory::{commit_ops, pass_eliminate_working_round_trip};

pub(in crate::optimizer) use control_flow::pass_branchify_mux::BranchifyMuxPass;
pub(in crate::optimizer) use control_flow::pass_control_flow_simplify::{
    ControlFlowSimplifyPass, PostGvnCfgCleanupPass,
};
pub(in crate::optimizer) use control_flow::pass_guarded_region_sinking::GuardedRegionSinkingPass;
pub(in crate::optimizer) use control_flow::pass_optimize_blocks::OptimizeBlocksPass;
pub(in crate::optimizer) use control_flow::pass_phi_outcome_compression::PhiOutcomeCompressionPass;
pub(in crate::optimizer) use control_flow::pass_sparse_case_dispatch::SparseCaseDispatchPass;
pub(in crate::optimizer) use dataflow::pass_bit_extract_peephole::BitExtractPeepholePass;
pub(in crate::optimizer) use dataflow::pass_circular_priority::CircularPriorityPass;
pub(in crate::optimizer) use dataflow::pass_concat_folding::ConcatFoldingPass;
pub(in crate::optimizer) use dataflow::pass_dead_code_elimination::DeadCodeEliminationPass;
pub(in crate::optimizer) use dataflow::pass_gvn::GvnPass;
pub(in crate::optimizer) use dataflow::pass_hoist_common_branch_loads::HoistCommonBranchLoadsPass;
pub(in crate::optimizer) use dataflow::pass_indexed_store_recovery::IndexedStoreRecoveryPass;
pub(in crate::optimizer) use dataflow::pass_loop_idiom::LoopIdiomPass;
pub(in crate::optimizer) use dataflow::pass_masked_array_any::MaskedArrayAnyPass;
pub(in crate::optimizer) use dataflow::pass_packed_scatter_store::PackedScatterStorePass;
pub(in crate::optimizer) use dataflow::pass_vectorize_concat::VectorizeConcatPass;
pub(in crate::optimizer) use dataflow::pass_xor_chain_folding::XorChainFoldingPass;
pub(in crate::optimizer) use memory::pass_coalesce_stores::CoalesceStoresPass;
pub(in crate::optimizer) use memory::pass_commit_sinking::CommitSinkingPass;
pub(in crate::optimizer) use memory::pass_eliminate_dead_working_stores::EliminateDeadWorkingStoresPass;
pub(in crate::optimizer) use memory::pass_partial_forward::PartialForwardPass;
pub(in crate::optimizer) use memory::pass_reschedule::ReschedulePass;
pub(in crate::optimizer) use memory::pass_split_coalesced_stores::SplitCoalescedStoresPass;
pub(in crate::optimizer) use memory::pass_split_wide_commits::SplitWideCommitsPass;
pub(in crate::optimizer) use memory::pass_store_load_forwarding::StoreLoadForwardingPass;
