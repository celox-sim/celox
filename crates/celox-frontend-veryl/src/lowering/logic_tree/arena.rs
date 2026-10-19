use crate::HashMap;
use num_bigint::BigUint;
use std::{
    cell::RefCell,
    hash::Hash,
    ops::{Deref, DerefMut},
    rc::Rc,
};
use veryl_analyzer::ir::VarId;

/// One lowering operation owns its scratch context alongside its nodes.
/// Only the node arena crosses the frontend boundary.
pub(crate) struct SLTNodeArena<A: Hash + Eq + Clone> {
    nodes: celox_slt::SLTNodeArena<A>,
    pub(super) loop_ranges: Rc<RefCell<HashMap<VarId, BigUint>>>,
    pub(crate) file_dependencies: crate::file_dependencies::FileDependencyCollector,
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
            loop_ranges: Rc::new(RefCell::new(source.loop_ranges.borrow().clone())),
            file_dependencies: source.file_dependencies.clone(),
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
            file_dependencies: Default::default(),
        }
    }
}
impl<A: Hash + Eq + Clone> Clone for SLTNodeArena<A> {
    fn clone(&self) -> Self {
        Self {
            nodes: self.nodes.clone(),
            loop_ranges: Rc::new(RefCell::new(self.loop_ranges.borrow().clone())),
            file_dependencies: self.file_dependencies.clone(),
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
        let arena = SLTNodeArena::<VarId>::new();
        let independent = SLTNodeArena::<VarId>::new();
        let variable = VarId::default();
        let _outer = LoopRangeScope::with_max(&arena, variable, 7);
        assert!(independent.loop_ranges.borrow().is_empty());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _inner = LoopRangeScope::with_max(&arena, variable, 3);
            assert_eq!(arena.loop_ranges.borrow()[&variable], BigUint::from(3u8));
            panic!("failed lowering");
        }));
        assert!(result.is_err());
        assert_eq!(arena.loop_ranges.borrow()[&variable], BigUint::from(7u8));
        let fork = SLTNodeArena::<VarId>::with_context_from(&arena);
        let _fork = LoopRangeScope::with_max(&fork, variable, 1);
        assert_eq!(arena.loop_ranges.borrow()[&variable], BigUint::from(7u8));
    }
}
