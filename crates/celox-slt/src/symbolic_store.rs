use std::hash::Hash;
use std::ops::Index;
use std::sync::Arc;

use celox_design::VarAtomBase;
use fxhash::{FxHashMap as HashMap, FxHashSet as HashSet};

use crate::RangeStore;

type SymbolicRange<A, N> = RangeStore<Option<(N, HashSet<VarAtomBase<A>>)>>;

/// Symbolic state whose range maps are shared until one branch mutates them.
///
/// Branch evaluation frequently clones the complete store while changing only
/// a handful of variables. Keeping the outer map independent preserves normal
/// map value semantics; sharing each range map avoids deep-cloning untouched
/// B-trees at every branch.
#[derive(Clone, Debug)]
pub struct SymbolicStore<A, N> {
    entries: HashMap<A, Arc<SymbolicRange<A, N>>>,
}

impl<A, N> Default for SymbolicStore<A, N> {
    fn default() -> Self {
        Self {
            entries: HashMap::default(),
        }
    }
}

impl<A: Eq + Hash, N> SymbolicStore<A, N> {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn reserve(&mut self, additional: usize) {
        self.entries.reserve(additional);
    }

    pub fn contains_key(&self, key: &A) -> bool {
        self.entries.contains_key(key)
    }

    pub fn get(&self, key: &A) -> Option<&SymbolicRange<A, N>> {
        self.entries.get(key).map(AsRef::as_ref)
    }

    pub fn insert(
        &mut self,
        key: A,
        value: SymbolicRange<A, N>,
    ) -> Option<Arc<SymbolicRange<A, N>>> {
        self.entries.insert(key, Arc::new(value))
    }

    pub fn remove(&mut self, key: &A) -> Option<Arc<SymbolicRange<A, N>>> {
        self.entries.remove(key)
    }

    pub fn iter(&self) -> Iter<'_, A, N> {
        Iter {
            inner: self.entries.iter(),
        }
    }

    pub fn values(&self) -> Values<'_, A, N> {
        Values {
            inner: self.entries.values(),
        }
    }

    pub fn keys(&self) -> impl ExactSizeIterator<Item = &A> {
        self.entries.keys()
    }

    pub fn extend<I>(&mut self, values: I)
    where
        I: IntoIterator<Item = (A, SymbolicRange<A, N>)>,
    {
        self.entries.extend(
            values
                .into_iter()
                .map(|(key, value)| (key, Arc::new(value))),
        );
    }
}

impl<A: Clone + Eq + Hash, N: Clone> SymbolicStore<A, N> {
    pub fn clone_entry_from(&mut self, key: &A, source: &Self) -> bool {
        let Some(value) = source.entries.get(key) else {
            return false;
        };
        self.entries.insert(key.clone(), Arc::clone(value));
        true
    }

    pub fn entry(&mut self, key: A) -> Entry<'_, A, N> {
        Entry {
            inner: self.entries.entry(key),
        }
    }

    pub fn get_mut(&mut self, key: &A) -> Option<&mut SymbolicRange<A, N>> {
        self.entries.get_mut(key).map(Arc::make_mut)
    }

    pub fn values_mut(&mut self) -> ValuesMut<'_, A, N> {
        ValuesMut {
            inner: self.entries.values_mut(),
        }
    }
}

pub struct Entry<'a, A, N> {
    inner: std::collections::hash_map::Entry<'a, A, Arc<SymbolicRange<A, N>>>,
}

impl<'a, A: Clone, N: Clone> Entry<'a, A, N> {
    pub fn or_insert_with(
        self,
        default: impl FnOnce() -> SymbolicRange<A, N>,
    ) -> &'a mut SymbolicRange<A, N> {
        let value = match self.inner {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => entry.insert(Arc::new(default())),
        };
        Arc::make_mut(value)
    }
}

impl<A: Eq + Hash, N> Index<&A> for SymbolicStore<A, N> {
    type Output = SymbolicRange<A, N>;

    fn index(&self, key: &A) -> &Self::Output {
        self.entries.index(key)
    }
}

pub struct Iter<'a, A, N> {
    inner: std::collections::hash_map::Iter<'a, A, Arc<SymbolicRange<A, N>>>,
}

impl<'a, A, N> Iterator for Iter<'a, A, N> {
    type Item = (&'a A, &'a SymbolicRange<A, N>);

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|(key, value)| (key, value.as_ref()))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<A, N> ExactSizeIterator for Iter<'_, A, N> {}

pub struct Values<'a, A, N> {
    inner: std::collections::hash_map::Values<'a, A, Arc<SymbolicRange<A, N>>>,
}

impl<'a, A, N> Iterator for Values<'a, A, N> {
    type Item = &'a SymbolicRange<A, N>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(AsRef::as_ref)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<A, N> ExactSizeIterator for Values<'_, A, N> {}

pub struct ValuesMut<'a, A: Clone, N: Clone> {
    inner: std::collections::hash_map::ValuesMut<'a, A, Arc<SymbolicRange<A, N>>>,
}

impl<'a, A: Clone, N: Clone> Iterator for ValuesMut<'a, A, N> {
    type Item = &'a mut SymbolicRange<A, N>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(Arc::make_mut)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<A: Clone, N: Clone> ExactSizeIterator for ValuesMut<'_, A, N> {}

pub struct IntoIter<A, N> {
    inner: std::collections::hash_map::IntoIter<A, Arc<SymbolicRange<A, N>>>,
}

impl<A: Clone, N: Clone> Iterator for IntoIter<A, N> {
    type Item = (A, SymbolicRange<A, N>);

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|(key, value)| {
            let value = Arc::try_unwrap(value).unwrap_or_else(|shared| shared.as_ref().clone());
            (key, value)
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<A: Clone, N: Clone> ExactSizeIterator for IntoIter<A, N> {}

impl<'a, A: Eq + Hash, N> IntoIterator for &'a SymbolicStore<A, N> {
    type Item = (&'a A, &'a SymbolicRange<A, N>);
    type IntoIter = Iter<'a, A, N>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<A: Clone + Eq + Hash, N: Clone> IntoIterator for SymbolicStore<A, N> {
    type Item = (A, SymbolicRange<A, N>);
    type IntoIter = IntoIter<A, N>;

    fn into_iter(self) -> Self::IntoIter {
        IntoIter {
            inner: self.entries.into_iter(),
        }
    }
}

impl<A: Clone + Eq + Hash, N: Clone> FromIterator<(A, SymbolicRange<A, N>)>
    for SymbolicStore<A, N>
{
    fn from_iter<T: IntoIterator<Item = (A, SymbolicRange<A, N>)>>(iter: T) -> Self {
        let mut store = Self::default();
        store.extend(iter);
        store
    }
}

#[cfg(test)]
mod tests {
    use celox_design::BitAccess;

    use super::*;

    #[test]
    fn cloned_store_copies_a_range_only_when_it_is_mutated() {
        let mut original = SymbolicStore::<u32, u32>::default();
        original.insert(7, RangeStore::new(None, 8));

        let mut branch = original.clone();
        branch
            .get_mut(&7)
            .unwrap()
            .update(BitAccess::new(0, 3), Some((11, HashSet::default())))
            .unwrap();

        assert_eq!(
            original[&7].get_parts(BitAccess::new(0, 7)).unwrap()[0].0,
            None
        );
        assert_eq!(
            branch[&7].get_parts(BitAccess::new(0, 7)).unwrap()[0].0,
            Some((11, HashSet::default()))
        );
    }
}
