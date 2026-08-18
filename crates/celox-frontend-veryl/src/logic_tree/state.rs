use veryl_analyzer::ir::VarId;

use crate::HashSet;
use celox_design::VarAtomBase;

use super::NodeId;

// Frontend specialization of the source-independent symbolic state contract.
pub type SymbolicStore<A> = celox_slt::SymbolicStore<A, NodeId>;
pub type BoundaryMap<A> = celox_slt::BoundaryMap<A>;

pub(super) struct LoopControlState {
    pub(super) store: SymbolicStore<VarId>,
    pub(super) boundaries: BoundaryMap<VarId>,
    pub(super) continue_expr: NodeId,
    pub(super) continue_sources: HashSet<VarAtomBase<VarId>>,
}

impl Clone for LoopControlState {
    fn clone(&self) -> Self {
        Self {
            store: self.store.fork(),
            boundaries: self.boundaries.clone(),
            continue_expr: self.continue_expr,
            continue_sources: self.continue_sources.clone(),
        }
    }
}

pub(super) struct FunctionControlState {
    pub(super) store: SymbolicStore<VarId>,
    pub(super) boundaries: BoundaryMap<VarId>,
    pub(super) live_expr: NodeId,
    pub(super) live_sources: HashSet<VarAtomBase<VarId>>,
}

impl Clone for FunctionControlState {
    fn clone(&self) -> Self {
        Self {
            store: self.store.fork(),
            boundaries: self.boundaries.clone(),
            live_expr: self.live_expr,
            live_sources: self.live_sources.clone(),
        }
    }
}

pub(super) struct FunctionLoopControlState {
    pub(super) function: FunctionControlState,
    pub(super) continue_expr: NodeId,
    pub(super) continue_sources: HashSet<VarAtomBase<VarId>>,
}

impl Clone for FunctionLoopControlState {
    fn clone(&self) -> Self {
        Self {
            function: self.function.clone(),
            continue_expr: self.continue_expr,
            continue_sources: self.continue_sources.clone(),
        }
    }
}
