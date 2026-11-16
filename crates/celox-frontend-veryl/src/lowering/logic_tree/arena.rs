use crate::HashMap;
use num_bigint::BigUint;
use std::{
    hash::Hash,
    ops::{Deref, DerefMut},
};
use veryl_analyzer::ir::VarId;

/// One lowering operation owns its scratch context alongside its nodes.
/// Only the node arena crosses the frontend boundary.
pub(crate) struct SLTNodeArena<A: Hash + Eq + Clone> {
    nodes: celox_slt::SLTNodeArena<A>,
    pub(super) loop_ranges: HashMap<VarId, BigUint>,
    /// The memory files the `always_comb` blocks lowered so far load, with
    /// the values they wrote to the loaded words before: initial state of
    /// their module (see `apply_readmem_writes`).
    pub(crate) memory_loads: Vec<celox_design::InitialStateValue<VarId>>,
    /// The call of the last of `memory_loads`, for diagnostics.
    pub(crate) memory_load_call: Option<veryl_parser::token_range::TokenRange>,
}
impl<A: Hash + Eq + Clone> SLTNodeArena<A> {
    pub(crate) fn alloc(
        &mut self,
        node: celox_slt::SLTNode<A>,
    ) -> Result<celox_slt::NodeId, celox_slt::SLTNodeFactsError> {
        self.nodes.alloc(node)
    }
    pub(crate) fn new() -> Self {
        Self::default()
    }
    pub(crate) fn with_context_from<B: Hash + Eq + Clone>(source: &SLTNodeArena<B>) -> Self {
        Self {
            nodes: Default::default(),
            loop_ranges: source.loop_ranges.clone(),
            memory_loads: Vec::new(),
            memory_load_call: None,
        }
    }
    pub(crate) fn into_inner(self) -> celox_slt::SLTNodeArena<A> {
        self.nodes
    }
}
impl<A: Hash + Eq + Clone> Default for SLTNodeArena<A> {
    fn default() -> Self {
        Self {
            nodes: Default::default(),
            loop_ranges: Default::default(),
            memory_loads: Vec::new(),
            memory_load_call: None,
        }
    }
}
impl<A: Hash + Eq + Clone> Clone for SLTNodeArena<A> {
    fn clone(&self) -> Self {
        Self {
            nodes: self.nodes.clone(),
            loop_ranges: self.loop_ranges.clone(),
            memory_loads: self.memory_loads.clone(),
            memory_load_call: self.memory_load_call,
        }
    }
}
impl<A: Hash + Eq + Clone> Deref for SLTNodeArena<A> {
    type Target = celox_slt::SLTNodeArena<A>;
    fn deref(&self) -> &Self::Target {
        &self.nodes
    }
}
impl<A: Hash + Eq + Clone> DerefMut for SLTNodeArena<A> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.nodes
    }
}

#[cfg(test)]
mod tests {
    use super::super::LoopRangeScope;
    use super::*;

    #[test]
    fn loop_bounds_are_arena_owned_and_restore_after_unwind() {
        let mut arena = SLTNodeArena::<VarId>::new();
        let independent = SLTNodeArena::<VarId>::new();
        let variable = VarId::default();
        let mut outer = LoopRangeScope::with_max(&mut arena, variable, 7);
        let arena = &mut *outer;
        assert!(independent.loop_ranges.is_empty());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let inner = LoopRangeScope::with_max(arena, variable, 3);
            assert_eq!(inner.loop_ranges[&variable], BigUint::from(3u8));
            panic!("failed lowering");
        }));
        assert!(result.is_err());
        assert_eq!(arena.loop_ranges[&variable], BigUint::from(7u8));
        let mut fork = SLTNodeArena::<VarId>::with_context_from(arena);
        let _fork = LoopRangeScope::with_max(&mut fork, variable, 1);
        assert_eq!(arena.loop_ranges[&variable], BigUint::from(7u8));
    }
}
