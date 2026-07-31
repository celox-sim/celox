use veryl_analyzer::ir::VarId;

use crate::{HashSet, ir::VarAtomBase};

use super::NodeId;

// Frontend specialization of the source-independent symbolic state contract.
pub type SymbolicStore<A> = celox_slt::SymbolicStore<A, NodeId>;
pub type BoundaryMap<A> = celox_slt::BoundaryMap<A>;

#[derive(Clone)]
pub(super) struct LoopControlState {
    pub(super) store: SymbolicStore<VarId>,
    pub(super) boundaries: BoundaryMap<VarId>,
    pub(super) continue_expr: NodeId,
    pub(super) continue_sources: HashSet<VarAtomBase<VarId>>,
}

#[derive(Clone)]
pub(super) struct FunctionControlState {
    pub(super) store: SymbolicStore<VarId>,
    pub(super) boundaries: BoundaryMap<VarId>,
    pub(super) live_expr: NodeId,
    pub(super) live_sources: HashSet<VarAtomBase<VarId>>,
}

#[derive(Clone)]
pub(super) struct FunctionLoopControlState {
    pub(super) function: FunctionControlState,
    pub(super) continue_expr: NodeId,
    pub(super) continue_sources: HashSet<VarAtomBase<VarId>>,
}
