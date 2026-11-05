//! Read-only scope snapshots; mutation detaches a shared table.
//!
//! A table of constants is a persistent map: a detached table shares every
//! entry it does not change with the snapshot it came from, so a scope that
//! adds a few names to a large environment copies only the paths to them.

use super::*;

/// A table of constant values, as scopes share and extend them.
pub(crate) type ConstEnv =
    imbl::GenericHashMap<String, i128, fxhash::FxBuildHasher, imbl::shared_ptr::DefaultSharedPtr>;

/// The entries of a [`SharedMap`].
pub(super) trait Entries<V>:
    Clone + Default + FromIterator<(String, V)> + IntoIterator<Item = (String, V)>
{
    /// Whether two tables hold the same entries.
    fn same_entries(&self, other: &Self) -> bool;
}

impl<V: Clone + PartialEq> Entries<V> for HashMap<String, V> {
    fn same_entries(&self, other: &Self) -> bool {
        self == other
    }
}

impl Entries<i128> for ConstEnv {
    fn same_entries(&self, other: &Self) -> bool {
        // Keys are unique, so equal sizes and every entry found in the
        // other table make the tables equal.
        self.ptr_eq(other)
            || (self.len() == other.len()
                && self
                    .iter()
                    .all(|(name, value)| other.get(name) == Some(value)))
    }
}

#[derive(Debug)]
pub(super) struct SharedMap<V, M = HashMap<String, V>>(Arc<M>, std::marker::PhantomData<fn() -> V>);

/// The shared table of a scope's constants.
pub(super) type SharedEnv = SharedMap<i128, ConstEnv>;

impl<V, M> Clone for SharedMap<V, M> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0), std::marker::PhantomData)
    }
}

impl<V, M: Entries<V>> PartialEq for SharedMap<V, M> {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self.0.same_entries(&other.0)
    }
}

impl<V, M: Entries<V>> Eq for SharedMap<V, M> {}

impl<V, M> SharedMap<V, M> {
    /// Identity of an immutable snapshot, valid while a borrower retains it.
    pub fn identity(&self) -> usize {
        Arc::as_ptr(&self.0) as usize
    }

    /// Whether another snapshot shares the entries, which an edit copies.
    #[cfg(test)]
    pub fn is_shared(&self) -> bool {
        Arc::strong_count(&self.0) > 1
    }

    /// A handle to this snapshot that does not keep its entries alive but
    /// keeps its identity from being reused.
    pub fn downgrade(&self) -> std::sync::Weak<M> {
        Arc::downgrade(&self.0)
    }
}

/// A table given to a context that keeps it: a shared table is shared
/// again, and a borrowed one is copied.
pub(super) trait ToShared<V, M = HashMap<String, V>> {
    fn to_shared(self) -> SharedMap<V, M>;
}

impl<V, M> ToShared<V, M> for &SharedMap<V, M> {
    fn to_shared(self) -> SharedMap<V, M> {
        self.clone()
    }
}

impl<V, M: Entries<V>> ToShared<V, M> for &M {
    fn to_shared(self) -> SharedMap<V, M> {
        self.clone().into()
    }
}

impl<V, M: Entries<V>> Default for SharedMap<V, M> {
    fn default() -> Self {
        M::default().into()
    }
}

impl<V, M: Entries<V>> From<M> for SharedMap<V, M> {
    fn from(values: M) -> Self {
        Self(Arc::new(values), std::marker::PhantomData)
    }
}

impl<V, M> Deref for SharedMap<V, M> {
    type Target = M;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<V, M: Entries<V>> DerefMut for SharedMap<V, M> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        Arc::make_mut(&mut self.0)
    }
}

impl<V, M: Entries<V>> FromIterator<(String, V)> for SharedMap<V, M> {
    fn from_iter<I: IntoIterator<Item = (String, V)>>(values: I) -> Self {
        M::from_iter(values).into()
    }
}

impl<V, M: Entries<V>> SharedMap<V, M> {
    /// The entries, copied only when another snapshot shares them.
    pub fn into_inner(self) -> M {
        Arc::unwrap_or_clone(self.0)
    }
}

impl<V, M: Entries<V>> IntoIterator for SharedMap<V, M> {
    type Item = (String, V);
    type IntoIter = M::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        Arc::unwrap_or_clone(self.0).into_iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_snapshots_share_reads_and_detach_mutations() {
        let parent: SharedEnv = ConstEnv::from_iter([("N".into(), 8), ("P".into(), 2)]).into();
        let mut child = parent.clone();
        let sibling = parent.clone();
        assert!(Arc::ptr_eq(&parent.0, &child.0));
        child.insert("N".into(), 16);
        child.remove("P");
        child.insert("Q".into(), 3);
        assert!(!Arc::ptr_eq(&parent.0, &child.0));
        assert!(Arc::ptr_eq(&parent.0, &sibling.0));
        assert_eq!(sibling["N"], 8);
        assert_eq!(sibling["P"], 2);
        assert!(!sibling.contains_key("Q"));
        let before = Arc::as_ptr(&child.0);
        child.retain(|name, _| name != "Q");
        assert_eq!(Arc::as_ptr(&child.0), before);
    }

    #[test]
    fn owned_iteration_preserves_other_snapshots() {
        let values: SharedEnv = ConstEnv::from_iter([("N".into(), 8)]).into();
        let snapshot = values.clone();
        assert_eq!(values.into_iter().collect::<HashMap<_, _>>()["N"], 8);
        assert_eq!(snapshot["N"], 8);
    }
}
