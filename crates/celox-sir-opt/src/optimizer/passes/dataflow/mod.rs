use super::analysis::{cost_model, shared, sir_analysis, state_ssa};
use crate::optimizer::pipeline::pass_manager;

pub(in crate::optimizer) mod pass_bit_extract_peephole;
pub(in crate::optimizer) mod pass_circular_priority;
pub(in crate::optimizer) mod pass_concat_folding;
pub(in crate::optimizer) mod pass_dead_code_elimination;
pub(in crate::optimizer) mod pass_gvn;
pub(in crate::optimizer) mod pass_hoist_common_branch_loads;
pub(in crate::optimizer) mod pass_indexed_store_recovery;
pub(in crate::optimizer) mod pass_loop_idiom;
pub(in crate::optimizer) mod pass_masked_array_any;
pub(in crate::optimizer) mod pass_pack_concat_phi;
pub(in crate::optimizer) mod pass_packed_scatter_store;
pub(in crate::optimizer) mod pass_vectorize_concat;
pub(in crate::optimizer) mod pass_xor_chain_folding;
