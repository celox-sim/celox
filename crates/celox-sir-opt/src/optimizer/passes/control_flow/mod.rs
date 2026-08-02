use super::analysis::{
    control_region_feasibility, cost_model, placement_analysis, shared, sir_analysis,
};
#[cfg(test)]
use super::dataflow::pass_gvn;
use super::dataflow::pass_vectorize_concat;
use super::memory::block_opt;
use crate::optimizer::pipeline::pass_manager;

pub(in crate::optimizer) mod pass_branchify_mux;
pub(in crate::optimizer) mod pass_control_flow_simplify;
pub(in crate::optimizer) mod pass_effect_case_dispatch;
pub(in crate::optimizer) mod pass_guarded_region_sinking;
pub(in crate::optimizer) mod pass_optimize_blocks;
pub(in crate::optimizer) mod pass_phi_outcome_compression;
pub(in crate::optimizer) mod pass_sparse_case_dispatch;
