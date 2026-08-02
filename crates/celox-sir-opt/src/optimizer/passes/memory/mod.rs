use super::analysis::{cost_model, dead_working_stores, shared, sir_analysis, state_ssa};
#[cfg(test)]
use super::dataflow::pass_loop_idiom;
use crate::optimizer::pipeline::pass_manager;

pub(in crate::optimizer) mod block_opt;
pub mod commit_ops;
pub(in crate::optimizer) mod fused_comb_dse;
pub(in crate::optimizer) mod pass_coalesce_stores;
pub(in crate::optimizer) mod pass_commit_sinking;
pub(in crate::optimizer) mod pass_dead_store_elimination;
pub(in crate::optimizer) mod pass_eliminate_dead_working_stores;
pub mod pass_eliminate_working_round_trip;
pub(in crate::optimizer) mod pass_global_store_load_forwarding;
pub(in crate::optimizer) mod pass_identity_store_bypass;
pub(in crate::optimizer) mod pass_inline_commit_forwarding;
pub(in crate::optimizer) mod pass_partial_forward;
pub(in crate::optimizer) mod pass_reschedule;
pub(in crate::optimizer) mod pass_split_coalesced_stores;
pub(in crate::optimizer) mod pass_split_wide_commits;
pub(in crate::optimizer) mod pass_store_load_forwarding;
