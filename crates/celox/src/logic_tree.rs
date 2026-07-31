mod comb;
pub use celox_slt::SLTToSIRLowerer;
pub use celox_slt::const_inline;
pub(crate) use celox_slt::matches_slt_count_idiom;
#[cfg(test)]
pub(crate) use celox_slt::matches_slt_or_scan_group;
pub use celox_slt::range_store;
pub use comb::SLTLoopBound;
pub use comb::SLTNode;
pub(crate) use comb::SLTNodeArenaEditError;
pub(crate) use comb::coerce_node_width;
pub(crate) use comb::parse_comb_with_loop_recovery;
pub use comb::{LogicPath, LogicPathTarget};
pub use comb::{
    NodeId, SLTForFoldGroupState, SLTNodeArena, SLTNodeFacts, SLTNodeFactsError, SymbolicStore,
    eval_assignment_expression, eval_expression, get_width,
};
#[cfg(test)]
pub(crate) use comb::{SLTForUpdate, SLTStepOp};
