//! Incremental readiness, with the original declaration-priority order.

use std::{cmp::Reverse, collections::BinaryHeap};

use fxhash::{FxHashMap as HashMap, FxHashSet as HashSet};

pub(super) struct DependencyOrder {
    prerequisites: Vec<usize>,
    dependents: Vec<Vec<usize>>,
    ready: BinaryHeap<Reverse<usize>>,
    remaining: usize,
    #[cfg(test)]
    released_edges: usize,
}

impl DependencyOrder {
    /// Lower indices have priority. Callers list signals before parameters,
    /// each in declaration order. Only names in this scope form graph edges.
    pub(super) fn new<'a>(
        declarations: impl IntoIterator<Item = (&'a str, &'a HashSet<String>)>,
    ) -> Self {
        let declarations: Vec<_> = declarations.into_iter().collect();
        let names: HashMap<_, _> = declarations
            .iter()
            .enumerate()
            .map(|(index, (name, _))| (*name, index))
            .collect();
        debug_assert_eq!(names.len(), declarations.len());
        let mut prerequisites = vec![0; declarations.len()];
        let mut dependents = vec![Vec::new(); declarations.len()];
        for (index, (_, dependencies)) in declarations.iter().enumerate() {
            for name in *dependencies {
                if let Some(&dependency) = names.get(name.as_str()) {
                    prerequisites[index] += 1;
                    dependents[dependency].push(index);
                }
            }
        }
        let ready = prerequisites
            .iter()
            .enumerate()
            .filter(|(_, count)| **count == 0)
            .map(|(index, _)| Reverse(index))
            .collect();
        Self {
            prerequisites,
            dependents,
            ready,
            remaining: declarations.len(),
            #[cfg(test)]
            released_edges: 0,
        }
    }

    pub(super) fn is_complete(&self) -> bool {
        self.remaining == 0
    }

    pub(super) fn pop_ready(&mut self) -> Option<usize> {
        self.ready.pop().map(|Reverse(index)| index)
    }

    /// Release dependencies only after the declaration's metadata was bound.
    /// A failed binding must retain its original diagnostic before cycle errors.
    pub(super) fn complete(&mut self, index: usize) {
        self.remaining -= 1;
        for &dependent in &self.dependents[index] {
            #[cfg(test)]
            {
                self.released_edges += 1;
            }
            self.prerequisites[dependent] -= 1;
            if self.prerequisites[dependent] == 0 {
                self.ready.push(Reverse(dependent));
            }
        }
    }
}

#[cfg(test)]
#[path = "dependency_order/tests.rs"]
mod tests;
