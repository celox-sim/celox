//! Shared module symbols with small, independently mutable scope overlays.

use std::sync::Arc;

use fxhash::FxHashMap as HashMap;

/// Cloning a scope shares both tables. Mutation copies only local changes when
/// another scope still shares the overlay, never the complete module table.
/// `None` in the overlay hides a symbol in the shared table.
#[derive(Debug, Clone)]
pub(super) struct ScopedMap<V> {
    base: Arc<HashMap<String, V>>,
    overlay: Arc<HashMap<String, Option<V>>>,
}

impl<V> Default for ScopedMap<V> {
    fn default() -> Self {
        Self::from(HashMap::default())
    }
}

impl<V> From<HashMap<String, V>> for ScopedMap<V> {
    fn from(base: HashMap<String, V>) -> Self {
        Self {
            base: Arc::new(base),
            overlay: Arc::default(),
        }
    }
}

impl<V> ScopedMap<V> {
    pub fn get(&self, name: &str) -> Option<&V> {
        match self.overlay.get(name) {
            Some(value) => value.as_ref(),
            None => self.base.get(name),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &V)> {
        self.base
            .iter()
            .filter(|(name, _)| !self.overlay.contains_key(*name))
            .chain(
                self.overlay
                    .iter()
                    .filter_map(|(name, value)| value.as_ref().map(|value| (name, value))),
            )
    }
}

impl<V: Clone> ScopedMap<V> {
    pub fn insert(&mut self, name: String, value: V) -> Option<V> {
        // While constructing the module, populate the base without an overlay.
        if self.overlay.is_empty()
            && let Some(base) = Arc::get_mut(&mut self.base)
        {
            return base.insert(name, value);
        }
        let old = self.get(&name).cloned();
        Arc::make_mut(&mut self.overlay).insert(name, Some(value));
        old
    }

    pub fn remove(&mut self, name: &str) -> Option<V> {
        if self.overlay.is_empty()
            && let Some(base) = Arc::get_mut(&mut self.base)
        {
            return base.remove(name);
        }
        let old = self.get(name).cloned();
        if self.base.contains_key(name) {
            Arc::make_mut(&mut self.overlay).insert(name.to_string(), None);
        } else {
            Arc::make_mut(&mut self.overlay).remove(name);
        }
        old
    }
}

impl<V: Clone> Extend<(String, V)> for ScopedMap<V> {
    fn extend<T: IntoIterator<Item = (String, V)>>(&mut self, iter: T) {
        for (name, value) in iter {
            self.insert(name, value);
        }
    }
}

impl<V: PartialEq> PartialEq for ScopedMap<V> {
    fn eq(&self, other: &Self) -> bool {
        self.iter()
            .all(|(name, value)| other.get(name) == Some(value))
            && self.iter().count() == other.iter().count()
    }
}

impl<V: Eq> Eq for ScopedMap<V> {}

/// Signedness inference accepts both module tables and scoped overlays.
pub(super) trait Signedness {
    fn signedness(&self, name: &str) -> Option<bool>;
}

impl Signedness for HashMap<String, bool> {
    fn signedness(&self, name: &str) -> Option<bool> {
        self.get(name).copied()
    }
}

impl Signedness for ScopedMap<bool> {
    fn signedness(&self, name: &str) -> Option<bool> {
        self.get(name).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_shadow_remove_and_restore_without_changing_siblings() {
        let base = ScopedMap::from(HashMap::from_iter([("x".into(), 1), ("y".into(), 2)]));
        let mut scope = base.clone();
        assert!(Arc::ptr_eq(&scope.base, &base.base));
        assert_eq!(scope.insert("x".into(), 3), Some(1));
        assert_eq!(scope.remove("y"), Some(2));
        scope.insert("z".into(), 4);
        let mut nested = scope.clone();
        assert!(Arc::ptr_eq(&nested.overlay, &scope.overlay));
        assert_eq!(nested.remove("x"), Some(3));
        assert_eq!(nested.remove("z"), Some(4));
        assert_eq!(scope.get("x"), Some(&3));
        assert_eq!(scope.get("z"), Some(&4));
        assert_eq!(base.get("x"), Some(&1));
        assert_eq!(base.get("y"), Some(&2));
        scope.insert("x".into(), 1);
        scope.insert("y".into(), 2);
        scope.remove("z");
        assert_eq!(scope, base);
        assert_eq!(scope.iter().count(), 2);
    }
}
