//! Source-independent symbolic logic tree primitives.
//!
//! This crate owns semantic bit-range state independently of any HDL
//! frontend. Frontends provide their own address identity and SLT node type.

use std::collections::BTreeSet;

use celox_design::{ModuleId, VarAtomBase};
use fxhash::{FxHashMap as HashMap, FxHashSet as HashSet};

pub mod const_inline;
mod lower;
mod node;
mod node_facts;
mod node_rules;
mod path;
pub mod range_store;
pub mod scheduler;
mod symbolic_verify;

#[doc(hidden)]
pub use lower::matches_slt_or_scan_group;
pub use lower::{SLTToSIRLowerer, matches_slt_count_idiom};
pub use node::{
    NodeId, SLTForEffect, SLTForFoldGroupState, SLTForUpdate, SLTIndex, SLTIndexKind, SLTLoopBound,
    SLTNode, SLTNodeArena, SLTNodeArenaEditError, SLTStepOp,
};
pub use node_facts::{SLTNodeFacts, SLTNodeFactsError};
pub use path::{LogicPath, LogicPathId, LogicPathTarget};
pub use range_store::{RangeStore, RangeStoreError};
pub use scheduler::FfAccessSummary;
pub use symbolic_verify::verify_symbolic_roots;

/// Return the construction-time width cached when a node was interned.
pub fn get_width<A: std::hash::Hash + Eq + Clone>(node: NodeId, arena: &SLTNodeArena<A>) -> usize {
    arena
        .width(node)
        .unwrap_or_else(|| panic!("SLT node id n{} is outside the arena", node.0))
}

/// Symbolic state keyed by a frontend-independent semantic address.
///
/// `N` is the symbolic expression identity. It remains generic until the SLT
/// arena itself moves into this crate.
pub type SymbolicStore<A, N> = HashMap<A, RangeStore<Option<(N, HashSet<VarAtomBase<A>>)>>>;

/// Bit boundaries discovered while constructing symbolic state.
pub type BoundaryMap<A> = HashMap<A, BTreeSet<usize>>;

/// Source-independent combinational observation recipe retained until SIR construction.
#[derive(Clone, Debug)]
pub struct CombObserver<A> {
    pub site_id: u32,
    pub activation_group: u32,
    pub guard: Option<NodeId>,
    pub args: Vec<NodeId>,
    pub loop_runner: Option<NodeId>,
    pub sensitivity: Vec<VarAtomBase<A>>,
    pub local_inputs: Vec<(A, NodeId)>,
    pub observed_inputs: Vec<VarAtomBase<A>>,
    pub position_inputs: Vec<VarAtomBase<A>>,
    pub preceding_writes: Vec<VarAtomBase<A>>,
    pub written_before: Vec<VarAtomBase<A>>,
    pub written_input_atoms: Vec<VarAtomBase<A>>,
    pub written_inputs: Vec<A>,
    pub captured_in_loop: bool,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum GlueAddrBase<V> {
    Parent(V),
    Child(V),
}

impl<V: Copy> GlueAddrBase<V> {
    pub fn var_id(&self) -> V {
        match self {
            GlueAddrBase::Parent(value) | GlueAddrBase::Child(value) => *value,
        }
    }
}

impl<V: std::fmt::Display> std::fmt::Display for GlueAddrBase<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GlueAddrBase::Parent(value) => write!(f, "GlueAddr::Parent({value})"),
            GlueAddrBase::Child(value) => write!(f, "GlueAddr::Child({value})"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(bound(
    serialize = "V: serde::Serialize",
    deserialize = "V: serde::Deserialize<'de> + std::hash::Hash + Eq + Clone"
))]
pub struct GlueBlockBase<V: std::hash::Hash + Eq + Clone> {
    pub module_id: ModuleId,
    pub input_ports: Vec<(Vec<V>, LogicPath<GlueAddrBase<V>>)>,
    pub output_ports: Vec<(Vec<V>, LogicPath<GlueAddrBase<V>>)>,
    pub arena: SLTNodeArena<GlueAddrBase<V>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbolic_store_key_is_a_semantic_address_not_a_frontend_id() {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        struct DesignAddress(u32);

        let mut store = SymbolicStore::<DesignAddress, u32>::default();
        store.insert(DesignAddress(7), RangeStore::new(None, 8));

        assert!(store.contains_key(&DesignAddress(7)));
    }
}
