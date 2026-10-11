//! Read-only scope snapshots; mutation detaches a shared table.

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SharedMap<V>(Arc<HashMap<String, V>>);

impl<V> SharedMap<V> {
    /// Identity of an immutable snapshot, valid while a borrower retains it.
    pub fn identity(&self) -> usize {
        Arc::as_ptr(&self.0) as usize
    }
}

impl<V> Default for SharedMap<V> {
    fn default() -> Self {
        HashMap::default().into()
    }
}

impl<V> From<HashMap<String, V>> for SharedMap<V> {
    fn from(values: HashMap<String, V>) -> Self {
        Self(Arc::new(values))
    }
}

impl<V> Deref for SharedMap<V> {
    type Target = HashMap<String, V>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<V: Clone> DerefMut for SharedMap<V> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        Arc::make_mut(&mut self.0)
    }
}

impl<V: Clone> IntoIterator for SharedMap<V> {
    type Item = (String, V);
    type IntoIter = <HashMap<String, V> as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        Arc::unwrap_or_clone(self.0).into_iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_snapshots_share_reads_and_detach_mutations() {
        let parent: SharedMap<i128> = HashMap::from_iter([("N".into(), 8), ("P".into(), 2)]).into();
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
        let values: SharedMap<i128> = HashMap::from_iter([("N".into(), 8)]).into();
        let snapshot = values.clone();
        assert_eq!(values.into_iter().collect::<HashMap<_, _>>()["N"], 8);
        assert_eq!(snapshot["N"], 8);
    }
}
