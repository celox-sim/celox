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
// collection dependency. Empty stores allocate nothing. Forking shares the
// page table; the first write copies its small pointer array, then only the
// touched page and RangeStore.
const STORE_SHARDS: usize = 64;
type StorePages<A, N> = [Option<Arc<StoreShard<A, N>>>; STORE_SHARDS];

fn shard_index(key: &(impl Hash + ?Sized)) -> usize {
    let mut hasher = FxHasher::default();
    key.hash(&mut hasher);
    (hasher.finish() as usize) & (STORE_SHARDS - 1)
}

/// Statement-ordered symbolic state used while constructing SLT values.
///
/// Each clone is a cheap branch version: the fixed-size page table, its pages,
/// and every range remain shared until a definition mutates them. This gives
/// conditional evaluation SSA-like snapshot semantics while preserving the
/// map-shaped API expected by the frontend.
#[derive(Clone, Debug)]
pub struct SymbolicStore<A, N> {
    entries: Option<Arc<StorePages<A, N>>>,
    len: usize,
}

impl<A, N> Default for SymbolicStore<A, N> {
    fn default() -> Self {
        Self {
            entries: None,
            len: 0,
        }
    }
}

impl<A, N> SymbolicStore<A, N> {
    /// Create an independent statement branch from the current symbolic version.
    ///
    /// The shared page table makes this operation independent of the number of
    /// variables in the module. Definitions remain shared until either branch
    /// writes their page.
    pub fn fork(&self) -> Self {
        Self {
            entries: self.entries.as_ref().map(Arc::clone),
            len: self.len,
        }
    }
}

impl<A: Eq + Hash, N> SymbolicStore<A, N> {
    fn entry_arc(&self, key: &A) -> Option<&RangeEntry<A, N>> {
        self.entries.as_deref()?[shard_index(key)]
            .as_deref()?
            .get(key)
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
        if additional == 0 {
            return;
        }
        let per_shard = additional.div_ceil(STORE_SHARDS);
        let pages = Arc::make_mut(
            self.entries
                .get_or_insert_with(|| Arc::new(std::array::from_fn(|_| None))),
        );
        for shard in pages {
            Arc::make_mut(shard.get_or_insert_with(|| Arc::new(HashMap::default())))
                .reserve(per_shard);
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
        let pages = Arc::make_mut(
            self.entries
                .get_or_insert_with(|| Arc::new(std::array::from_fn(|_| None))),
        );
        let shard = Arc::make_mut(
            pages[shard_index(&key)].get_or_insert_with(|| Arc::new(HashMap::default())),
        );
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
        let index = shard_index(key);
        if !self.entries.as_deref()?[index]
            .as_deref()?
            .contains_key(key)
        {
            return None;
        }
        let pages = Arc::make_mut(self.entries.as_mut().expect("the page table exists"));
        let shard = pages[index].as_mut().expect("the populated shard exists");
        let removed = Arc::make_mut(shard).remove(key);
        if removed.is_some() {
            self.len -= 1;
        }
        if shard.is_empty() {
            pages[index] = None;
        }
        if self.len == 0 {
            self.entries = None;
        }
        removed
    }

    pub fn iter(&self) -> Iter<'_, A, N> {
        Iter {
            shards: self.entries.as_deref().map(|pages| pages.iter()),
            current: None,
            remaining: self.len,
        }
    }

    pub fn values(&self) -> Values<'_, A, N> {
        Values {
            shards: self.entries.as_deref().map(|pages| pages.iter()),
            current: None,
            remaining: self.len,
        }
    }

    pub fn keys(&self) -> Keys<'_, A, N> {
        Keys {
            shards: self.entries.as_deref().map(|pages| pages.iter()),
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
    /// skipped. The returned sparse iterator is therefore the only set that
    /// can require phi construction. Keys are borrowed directly from their
    /// store version, so walking a diff does not allocate or clone addresses.
    pub fn differing_keys<'a>(&'a self, other: &'a Self) -> impl Iterator<Item = &'a A> {
        let versions_match = match (&self.entries, &other.entries) {
            (None, None) => true,
            (Some(left), Some(right)) => Arc::ptr_eq(left, right),
            _ => false,
        };
        DifferingKeys {
            left_shards: self.entries.as_deref(),
            right_shards: other.entries.as_deref(),
            next_shard: if versions_match { STORE_SHARDS } else { 0 },
            left_shard: None,
            right_shard: None,
            left_entries: None,
            right_keys: None,
        }
    }
}

struct DifferingKeys<'a, A, N> {
    left_shards: Option<&'a StorePages<A, N>>,
    right_shards: Option<&'a StorePages<A, N>>,
    next_shard: usize,
    left_shard: Option<&'a StoreShard<A, N>>,
    right_shard: Option<&'a StoreShard<A, N>>,
    left_entries: Option<std::collections::hash_map::Iter<'a, A, RangeEntry<A, N>>>,
    right_keys: Option<std::collections::hash_map::Keys<'a, A, RangeEntry<A, N>>>,
}

impl<'a, A: Eq + Hash, N> Iterator for DifferingKeys<'a, A, N> {
    type Item = &'a A;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            while let Some((key, left)) = self.left_entries.as_mut().and_then(Iterator::next) {
                if self
                    .right_shard
                    .and_then(|right| right.get(key))
                    .is_none_or(|right| !Arc::ptr_eq(left, right))
                {
                    return Some(key);
                }
            }

            while let Some(key) = self.right_keys.as_mut().and_then(Iterator::next) {
                if self.left_shard.is_none_or(|left| !left.contains_key(key)) {
                    return Some(key);
                }
            }

            if self.next_shard == STORE_SHARDS {
                return None;
            }
            let left = self
                .left_shards
                .and_then(|shards| shards[self.next_shard].as_ref());
            let right = self
                .right_shards
                .and_then(|shards| shards[self.next_shard].as_ref());
            self.next_shard += 1;
            if match (left, right) {
                (None, None) => true,
                (Some(left), Some(right)) => Arc::ptr_eq(left, right),
                _ => false,
            } {
                continue;
            }

            self.left_shard = left.map(|shard| shard.as_ref());
            self.right_shard = right.map(|shard| shard.as_ref());
            self.left_entries = left.map(|shard| shard.iter());
            self.right_keys = right.map(|shard| shard.keys());
        }
    }
}

impl<A: Clone + Eq + Hash, N: Clone> SymbolicStore<A, N> {
    pub fn clone_entry_from(&mut self, key: &A, source: &Self) -> bool {
        let Some(value) = source.entry_arc(key) else {
            return false;
        };
        let pages = Arc::make_mut(
            self.entries
                .get_or_insert_with(|| Arc::new(std::array::from_fn(|_| None))),
        );
        let shard = Arc::make_mut(
            pages[shard_index(key)].get_or_insert_with(|| Arc::new(HashMap::default())),
        );
        if shard.insert(key.clone(), Arc::clone(value)).is_none() {
            self.len += 1;
        }
        true
    }

    pub fn entry(&mut self, key: A) -> Entry<'_, A, N> {
        let shard_index = shard_index(&key);
        let pages = Arc::make_mut(
            self.entries
                .get_or_insert_with(|| Arc::new(std::array::from_fn(|_| None))),
        );
        Entry {
            inner: Arc::make_mut(
                pages[shard_index].get_or_insert_with(|| Arc::new(HashMap::default())),
            )
            .entry(key),
            len: &mut self.len,
        }
    }

    pub fn get_mut(&mut self, key: &A) -> Option<&mut SymbolicRange<A, N>> {
        let index = shard_index(key);
        if !self.entries.as_deref()?[index]
            .as_deref()?
            .contains_key(key)
        {
            return None;
        }
        let pages = Arc::make_mut(self.entries.as_mut().expect("the page table exists"));
        let shard = pages[index].as_mut().expect("the populated shard exists");
        Arc::make_mut(shard).get_mut(key).map(Arc::make_mut)
    }

    pub fn values_mut(&mut self) -> ValuesMut<'_, A, N> {
        ValuesMut {
            shards: self
                .entries
                .as_mut()
                .map(Arc::make_mut)
                .map(|pages| pages.iter_mut()),
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
    shards: Option<std::slice::Iter<'a, Option<Arc<StoreShard<A, N>>>>>,
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
            let Some(shard) = self.shards.as_mut()?.next()?.as_deref() else {
                continue;
            };
            self.current = Some(shard.iter());
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl<A, N> ExactSizeIterator for Iter<'_, A, N> {}

pub struct Values<'a, A, N> {
    shards: Option<std::slice::Iter<'a, Option<Arc<StoreShard<A, N>>>>>,
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
            let Some(shard) = self.shards.as_mut()?.next()?.as_deref() else {
                continue;
            };
            self.current = Some(shard.values());
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl<A, N> ExactSizeIterator for Values<'_, A, N> {}

pub struct Keys<'a, A, N> {
    shards: Option<std::slice::Iter<'a, Option<Arc<StoreShard<A, N>>>>>,
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
            let Some(shard) = self.shards.as_mut()?.next()?.as_deref() else {
                continue;
            };
            self.current = Some(shard.keys());
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl<A, N> ExactSizeIterator for Keys<'_, A, N> {}

pub struct ValuesMut<'a, A: Clone, N: Clone> {
    shards: Option<std::slice::IterMut<'a, Option<Arc<StoreShard<A, N>>>>>,
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
            let Some(shard) = self.shards.as_mut()?.next()?.as_mut() else {
                continue;
            };
            self.current = Some(Arc::make_mut(shard).values_mut());
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
        if let Some(pages) = self.entries {
            let pages = Arc::try_unwrap(pages).unwrap_or_else(|shared| shared.as_ref().clone());
            for shard in pages {
                let Some(shard) = shard else {
                    continue;
                };
                let shard = Arc::try_unwrap(shard).unwrap_or_else(|shared| shared.as_ref().clone());
                values.extend(shard.into_iter().map(|(key, value)| {
                    let value =
                        Arc::try_unwrap(value).unwrap_or_else(|shared| shared.as_ref().clone());
                    (key, value)
                }));
            }
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
    fn empty_store_allocates_no_shard_pages() {
        let mut store = SymbolicStore::<u32, u32>::default();
        assert!(store.entries.is_none());
        assert!(store.get_mut(&7).is_none());
        assert!(store.entries.is_none());

        let branch = store.fork();
        assert!(branch.entries.is_none());
    }

    #[test]
    fn removing_a_last_entry_releases_its_shard_page() {
        let mut store = SymbolicStore::<u32, u32>::default();
        store.insert(7, RangeStore::new(None, 8));
        let index = shard_index(&7);
        assert!(store.entries.as_deref().unwrap()[index].is_some());

        assert!(store.remove(&7).is_some());
        assert!(store.entries.is_none());
        assert!(store.is_empty());
    }

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
        assert!(Arc::ptr_eq(
            original.entries.as_ref().unwrap(),
            branch.entries.as_ref().unwrap()
        ));
        branch
            .get_mut(&2048)
            .unwrap()
            .update(BitAccess::new(0, 0), Some((1, HashSet::default())))
            .unwrap();
        assert!(!Arc::ptr_eq(
            original.entries.as_ref().unwrap(),
            branch.entries.as_ref().unwrap()
        ));

        let original_pages = original.entries.as_deref().unwrap();
        let branch_pages = branch.entries.as_deref().unwrap();
        let shared_pages = original_pages
            .iter()
            .zip(branch_pages)
            .filter(|(left, right)| {
                left.as_ref()
                    .zip(right.as_ref())
                    .is_some_and(|(left, right)| Arc::ptr_eq(left, right))
            })
            .count();
        assert_eq!(shared_pages, STORE_SHARDS - 1);
        assert!(original.shares_entry_with(&branch, &7));
        assert!(!original.shares_entry_with(&branch, &2048));
        assert_eq!(
            original
                .differing_keys(&branch)
                .copied()
                .collect::<Vec<_>>(),
            vec![2048]
        );
    }

    #[test]
    fn branch_versions_report_only_their_divergent_definitions() {
        let mut entry = SymbolicStore::<u32, u32>::default();
        for key in 0..1024 {
            entry.insert(key, RangeStore::new(None, 8));
        }

        let mut then_version = entry.fork();
        then_version
            .get_mut(&17)
            .unwrap()
            .update(BitAccess::new(0, 3), Some((1, HashSet::default())))
            .unwrap();
        let mut else_version = entry.fork();
        else_version
            .get_mut(&900)
            .unwrap()
            .update(BitAccess::new(4, 7), Some((2, HashSet::default())))
            .unwrap();

        let mut differing = then_version
            .differing_keys(&else_version)
            .copied()
            .collect::<Vec<_>>();
        differing.sort_unstable();
        assert_eq!(differing, vec![17, 900]);
        assert_eq!(
            entry
                .differing_keys(&then_version)
                .copied()
                .collect::<Vec<_>>(),
            vec![17]
        );
        assert_eq!(
            entry
                .differing_keys(&else_version)
                .copied()
                .collect::<Vec<_>>(),
            vec![900]
        );
    }

    #[test]
    fn branch_diff_iterator_reports_insertions_and_removals_once() {
        let mut left = SymbolicStore::<u32, u32>::default();
        left.insert(1, RangeStore::new(None, 8));
        left.insert(2, RangeStore::new(None, 8));

        let mut right = left.fork();
        right.remove(&1);
        right.insert(3, RangeStore::new(None, 8));

        let mut differing = left.differing_keys(&right).copied().collect::<Vec<_>>();
        differing.sort_unstable();
        assert_eq!(differing, vec![1, 3]);

        let mut reverse = right.differing_keys(&left).copied().collect::<Vec<_>>();
        reverse.sort_unstable();
        assert_eq!(reverse, vec![1, 3]);
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
