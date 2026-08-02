//! Backend-independent SIR optimizer.

use crate::ir::*;
use crate::{OptimizationContext, PassOptions, SirPass};

mod api;
mod native;
mod passes;
mod pipeline;

pub(crate) use passes::analysis::state_ssa::StateSsaError;
use passes::*;
use pipeline::pass_manager::ExecutionUnitPass;

pub use crate::{OptimizationError, OptimizationErrorKind};
pub use api::{
    eliminate_shared_comb_state_stores, eliminate_unobserved_comb_state_stores,
    optimize_rooted_comb_memory, promote_eval_apply_working_round_trips,
    promote_fused_comb_static_slots, remove_dead_sir_definitions,
    remove_final_identity_alias_stores, retain_final_identity_aliases,
};
pub use native::optimize_merged_chain;
pub use passes::{commit_ops, cost_model, pass_eliminate_working_round_trip};

pub(crate) use pipeline::run;
