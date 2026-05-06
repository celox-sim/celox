use std::collections::BTreeSet;

use veryl_analyzer::ir::VarId;

use crate::{HashMap, HashSet, ir::VarAtomBase, logic_tree::range_store::RangeStore};

use super::NodeId;

// SymbolicStore: Maps variable IDs to their current symbolic representation.
// Each variable is managed by a RangeStore, which tracks bit-ranges and their associated SLT nodes.
pub type SymbolicStore<A> = HashMap<VarId, RangeStore<Option<(NodeId, HashSet<VarAtomBase<A>>)>>>;
pub type BoundaryMap<A> = HashMap<A, BTreeSet<usize>>;

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
