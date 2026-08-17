use std::hash::{Hash, Hasher};
use std::ops::Index;
use std::sync::Arc;

use celox_design::VarAtomBase;
use fxhash::{FxHashMap as HashMap, FxHashSet as HashSet, FxHasher};

use crate::RangeStore;

type SymbolicRange<A, N> = RangeStore<Option<(N, HashSet<VarAtomBase<A>>)>>;
type RangeEntry<A, N> = Arc<SymbolicRange<A, N>>;
type StoreShard<A, N> = HashMap<A, RangeEntry<A, N>>;

// A fixed page table keeps branch snapshots cheap without adding a persistent
// collection dependency. Cloning a store shares every page; the first write to
// one key copies only that key's page and then the touched RangeStore.
const STORE_SHARDS: usize = 64;

fn shard_index(key: &(impl Hash + ?Sized)) -> usize {
    let mut hasher = FxHasher::default();
    key.hash(&mut hasher);
    (hasher.finish() as usize) & (STORE_SHARDS - 1)
}

/// Statement-ordered symbolic state used while constructing SLT values.
///
/// Each clone is a cheap branch version: the fixed-size page table and every
/// range remain shared until a definition mutates them. This gives conditional
/// evaluation SSA-like snapshot semantics while preserving the map-shaped API
/// expected by the frontend.
#[derive(Clone, Debug)]
pub struct SymbolicStore<A, N> {
    entries: [Arc<StoreShard<A, N>>; STORE_SHARDS],
    len: usize,
}

impl<A, N> Default for SymbolicStore<A, N> {
    fn default() -> Self {
        Self {
            entries: std::array::from_fn(|_| Arc::new(HashMap::default())),
            len: 0,
        }
    }
}

impl<A, N> SymbolicStore<A, N> {
    /// Create an independent statement branch from the current symbolic version.
    ///
    /// The fixed page table makes this operation independent of the number of
    /// variables in the module. Definitions remain shared until either branch
    /// writes their page.
    pub fn fork(&self) -> Self {
        Self {
            entries: std::array::from_fn(|index| Arc::clone(&self.entries[index])),
            len: self.len,
        }
    }
}

impl<A: Eq + Hash, N> SymbolicStore<A, N> {
    fn entry_arc(&self, key: &A) -> Option<&RangeEntry<A, N>> {
        self.entries[shard_index(key)].get(key)
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn reserve(&mut self, additional: usize)
    where
        A: Clone,
        N: Clone,
    {
        let per_shard = additional.div_ceil(STORE_SHARDS);
        for shard in &mut self.entries {
            Arc::make_mut(shard).reserve(per_shard);
        }
    }

    pub fn contains_key(&self, key: &A) -> bool {
        self.entry_arc(key).is_some()
    }

    pub fn get(&self, key: &A) -> Option<&SymbolicRange<A, N>> {
        self.entry_arc(key).map(AsRef::as_ref)
    }

    pub fn insert(&mut self, key: A, value: SymbolicRange<A, N>) -> Option<RangeEntry<A, N>>
    where
        A: Clone,
        N: Clone,
    {
        let shard = Arc::make_mut(&mut self.entries[shard_index(&key)]);
        let previous = shard.insert(key, Arc::new(value));
        if previous.is_none() {
            self.len += 1;
        }
        previous
    }

    pub fn remove(&mut self, key: &A) -> Option<RangeEntry<A, N>>
    where
        A: Clone,
        N: Clone,
    {
        let removed = Arc::make_mut(&mut self.entries[shard_index(key)]).remove(key);
        if removed.is_some() {
            self.len -= 1;
        }
        removed
    }

    pub fn iter(&self) -> Iter<'_, A, N> {
        Iter {
            shards: self.entries.iter(),
            current: None,
            remaining: self.len,
        }
    }

    pub fn values(&self) -> Values<'_, A, N> {
        Values {
            shards: self.entries.iter(),
            current: None,
            remaining: self.len,
        }
    }

    pub fn keys(&self) -> Keys<'_, A, N> {
        Keys {
            shards: self.entries.iter(),
            current: None,
            remaining: self.len,
        }
    }

    pub fn extend<I>(&mut self, values: I)
    where
        A: Clone,
        N: Clone,
        I: IntoIterator<Item = (A, SymbolicRange<A, N>)>,
    {
        for (key, value) in values {
            self.insert(key, value);
        }
    }

    /// Whether two branch versions still refer to the same definition for a key.
    ///
    /// A shared entry can bypass a deep RangeStore comparison during phi
    /// construction, which is the common case for untouched variables.
    pub fn shares_entry_with(&self, other: &Self, key: &A) -> bool {
        match (self.entry_arc(key), other.entry_arc(key)) {
            (Some(left), Some(right)) => Arc::ptr_eq(left, right),
            (None, None) => true,
            _ => false,
        }
    }

    /// Keys whose symbolic definitions differ between two branch versions.
    ///
    /// Entire shared pages are skipped without visiting their keys. Within a
    /// copied page, entries that still share the same range version are also
    /// skipped. The returned sparse set is therefore the only set that can
    /// require phi construction.
    pub fn differing_keys(&self, other: &Self) -> Vec<A>
    where
        A: Clone,
    {
        let mut differing = Vec::new();
        for (left_shard, right_shard) in self.entries.iter().zip(&other.entries) {
            if Arc::ptr_eq(left_shard, right_shard) {
                continue;
            }
            for (key, left) in left_shard.iter() {
                if right_shard
                    .get(key)
                    .is_none_or(|right| !Arc::ptr_eq(left, right))
                {
                    differing.push(key.clone());
                }
            }
            for key in right_shard.keys() {
                if !left_shard.contains_key(key) {
                    differing.push(key.clone());
                }
            }
        }
        differing
    }
}

impl<A: Clone + Eq + Hash, N: Clone> SymbolicStore<A, N> {
    pub fn clone_entry_from(&mut self, key: &A, source: &Self) -> bool {
        let Some(value) = source.entry_arc(key) else {
            return false;
        };
        let shard = Arc::make_mut(&mut self.entries[shard_index(key)]);
        if shard.insert(key.clone(), Arc::clone(value)).is_none() {
            self.len += 1;
        }
        true
    }

    pub fn entry(&mut self, key: A) -> Entry<'_, A, N> {
        let shard_index = shard_index(&key);
        Entry {
            inner: Arc::make_mut(&mut self.entries[shard_index]).entry(key),
            len: &mut self.len,
        }
    }

    pub fn get_mut(&mut self, key: &A) -> Option<&mut SymbolicRange<A, N>> {
        Arc::make_mut(&mut self.entries[shard_index(key)])
            .get_mut(key)
            .map(Arc::make_mut)
    }

    pub fn values_mut(&mut self) -> ValuesMut<'_, A, N> {
        ValuesMut {
            shards: self.entries.iter_mut(),
            current: None,
            remaining: self.len,
        }
    }
}

pub struct Entry<'a, A, N> {
    inner: std::collections::hash_map::Entry<'a, A, RangeEntry<A, N>>,
    len: &'a mut usize,
}

impl<'a, A: Clone, N: Clone> Entry<'a, A, N> {
    pub fn or_insert_with(
        self,
        default: impl FnOnce() -> SymbolicRange<A, N>,
    ) -> &'a mut SymbolicRange<A, N> {
        let value = match self.inner {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                *self.len += 1;
                entry.insert(Arc::new(default()))
            }
        };
        Arc::make_mut(value)
    }
}

impl<A: Eq + Hash, N> Index<&A> for SymbolicStore<A, N> {
    type Output = SymbolicRange<A, N>;

    fn index(&self, key: &A) -> &Self::Output {
        self.get(key).expect("symbolic store key is absent")
    }
}

pub struct Iter<'a, A, N> {
    shards: std::slice::Iter<'a, Arc<StoreShard<A, N>>>,
    current: Option<std::collections::hash_map::Iter<'a, A, RangeEntry<A, N>>>,
    remaining: usize,
}

impl<'a, A, N> Iterator for Iter<'a, A, N> {
    type Item = (&'a A, &'a SymbolicRange<A, N>);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some((key, value)) = self.current.as_mut().and_then(Iterator::next) {
                self.remaining -= 1;
                return Some((key, value.as_ref()));
            }
            self.current = Some(self.shards.next()?.iter());
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl<A, N> ExactSizeIterator for Iter<'_, A, N> {}

pub struct Values<'a, A, N> {
    shards: std::slice::Iter<'a, Arc<StoreShard<A, N>>>,
    current: Option<std::collections::hash_map::Values<'a, A, RangeEntry<A, N>>>,
    remaining: usize,
}

impl<'a, A, N> Iterator for Values<'a, A, N> {
    type Item = &'a SymbolicRange<A, N>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(value) = self.current.as_mut().and_then(Iterator::next) {
                self.remaining -= 1;
                return Some(value.as_ref());
            }
            self.current = Some(self.shards.next()?.values());
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl<A, N> ExactSizeIterator for Values<'_, A, N> {}

pub struct Keys<'a, A, N> {
    shards: std::slice::Iter<'a, Arc<StoreShard<A, N>>>,
    current: Option<std::collections::hash_map::Keys<'a, A, RangeEntry<A, N>>>,
    remaining: usize,
}

impl<'a, A, N> Iterator for Keys<'a, A, N> {
    type Item = &'a A;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(key) = self.current.as_mut().and_then(Iterator::next) {
                self.remaining -= 1;
                return Some(key);
            }
            self.current = Some(self.shards.next()?.keys());
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl<A, N> ExactSizeIterator for Keys<'_, A, N> {}

pub struct ValuesMut<'a, A: Clone, N: Clone> {
    shards: std::slice::IterMut<'a, Arc<StoreShard<A, N>>>,
    current: Option<std::collections::hash_map::ValuesMut<'a, A, RangeEntry<A, N>>>,
    remaining: usize,
}

impl<'a, A: Clone, N: Clone> Iterator for ValuesMut<'a, A, N> {
    type Item = &'a mut SymbolicRange<A, N>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(value) = self.current.as_mut().and_then(Iterator::next) {
                self.remaining -= 1;
                return Some(Arc::make_mut(value));
            }
            self.current = Some(Arc::make_mut(self.shards.next()?).values_mut());
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl<A: Clone, N: Clone> ExactSizeIterator for ValuesMut<'_, A, N> {}

pub struct IntoIter<A, N> {
    inner: std::vec::IntoIter<(A, SymbolicRange<A, N>)>,
}

impl<A, N> Iterator for IntoIter<A, N> {
    type Item = (A, SymbolicRange<A, N>);

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<A, N> ExactSizeIterator for IntoIter<A, N> {}

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
        let mut values = Vec::with_capacity(self.len);
        for shard in self.entries {
            let shard = Arc::try_unwrap(shard).unwrap_or_else(|shared| shared.as_ref().clone());
            values.extend(shard.into_iter().map(|(key, value)| {
                let value = Arc::try_unwrap(value).unwrap_or_else(|shared| shared.as_ref().clone());
                (key, value)
            }));
        }
        IntoIter {
            inner: values.into_iter(),
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

        let mut branch = original.fork();
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
        assert!(!original.shares_entry_with(&branch, &7));
    }

    #[test]
    fn branch_write_copies_only_one_sparse_page() {
        let mut original = SymbolicStore::<u32, u32>::default();
        for key in 0..4096 {
            original.insert(key, RangeStore::new(None, 8));
        }

        let mut branch = original.fork();
        branch
            .get_mut(&2048)
            .unwrap()
            .update(BitAccess::new(0, 0), Some((1, HashSet::default())))
            .unwrap();

        let shared_pages = original
            .entries
            .iter()
            .zip(&branch.entries)
            .filter(|(left, right)| Arc::ptr_eq(left, right))
            .count();
        assert_eq!(shared_pages, STORE_SHARDS - 1);
        assert!(original.shares_entry_with(&branch, &7));
        assert!(!original.shares_entry_with(&branch, &2048));
        assert_eq!(original.differing_keys(&branch), vec![2048]);
    }

    #[test]
    fn sharded_iterators_and_entry_api_preserve_map_semantics() {
        let mut store = SymbolicStore::<u32, u32>::default();
        for key in 0..256 {
            store.entry(key).or_insert_with(|| RangeStore::new(None, 1));
        }
        assert_eq!(store.len(), 256);
        assert_eq!(store.keys().count(), 256);
        assert_eq!(store.values().count(), 256);
        assert_eq!(store.iter().count(), 256);

        for value in store.values_mut() {
            value
                .update(BitAccess::new(0, 0), Some((9, HashSet::default())))
                .unwrap();
        }
        assert!(
            store
                .values()
                .all(|value| value.get_parts(BitAccess::new(0, 0)).unwrap()[0].0
                    == Some((9, HashSet::default())))
        );

        assert!(store.remove(&128).is_some());
        assert_eq!(store.len(), 255);
        assert_eq!(store.into_iter().count(), 255);
    }
}
