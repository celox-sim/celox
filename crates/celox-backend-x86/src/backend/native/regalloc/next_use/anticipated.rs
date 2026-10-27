//! Shared storage for the values anticipated after each block's phis.
//!
//! Like the distance rows, neighbouring blocks mostly anticipate the same
//! values. A set is either a dense shared set or a short sorted list of
//! changes over one; a change list always refers to a dense set directly, so
//! lookups stay one hash probe plus a binary search however long a chain of
//! blocks shares the base.

use std::sync::Arc;

use crate::HashSet;
use crate::native::mir::VReg;

/// The most changes a relative set records before it becomes dense.
const MAX_OVERRIDES: usize = 32;

#[derive(Debug, Clone)]
pub(in crate::native::regalloc) enum AnticipatedSet {
    Dense(Arc<HashSet<VReg>>),
    Relative(Arc<RelativeSet>),
}

#[derive(Debug)]
pub(in crate::native::regalloc) struct RelativeSet {
    base: Arc<HashSet<VReg>>,
    /// Sorted by value: whether each changed value is in the set.
    overrides: Box<[(VReg, bool)]>,
    len: usize,
}

impl Default for AnticipatedSet {
    fn default() -> Self {
        Self::Dense(Arc::default())
    }
}

impl AnticipatedSet {
    pub(super) fn dense(values: HashSet<VReg>) -> Self {
        Self::Dense(Arc::new(values))
    }

    /// The dense set the set is stored against.
    pub(super) fn base(&self) -> &Arc<HashSet<VReg>> {
        match self {
            Self::Dense(base) => base,
            Self::Relative(row) => &row.base,
        }
    }

    /// The values the set changes against [`Self::base`].
    pub(super) fn changed(&self) -> impl Iterator<Item = VReg> + '_ {
        let overrides: &[(VReg, bool)] = match self {
            Self::Dense(_) => &[],
            Self::Relative(row) => &row.overrides,
        };
        overrides.iter().map(|&(value, _)| value)
    }

    /// The set equal to `base` outside `changed`, with membership `actual`
    /// for every value of `changed`, stored against `base`; `None` when
    /// that takes more than [`MAX_OVERRIDES`] changes.
    pub(super) fn relative(
        base: &Arc<HashSet<VReg>>,
        changed: impl IntoIterator<Item = VReg>,
        mut actual: impl FnMut(VReg) -> bool,
    ) -> Option<Self> {
        let mut changed = changed.into_iter().collect::<Vec<_>>();
        changed.sort_unstable();
        changed.dedup();
        let mut overrides = Vec::new();
        let mut len = base.len();
        for value in changed {
            let present = actual(value);
            if present != base.contains(&value) {
                if overrides.len() == MAX_OVERRIDES {
                    return None;
                }
                if present {
                    len += 1;
                } else {
                    len -= 1;
                }
                overrides.push((value, present));
            }
        }
        if overrides.is_empty() {
            return Some(Self::Dense(Arc::clone(base)));
        }
        Some(Self::Relative(Arc::new(RelativeSet {
            base: Arc::clone(base),
            overrides: overrides.into_boxed_slice(),
            len,
        })))
    }

    pub(in crate::native::regalloc) fn contains(&self, value: &VReg) -> bool {
        match self {
            Self::Dense(values) => values.contains(value),
            Self::Relative(row) => {
                match row
                    .overrides
                    .binary_search_by_key(value, |&(changed, _)| changed)
                {
                    Ok(index) => row.overrides[index].1,
                    Err(_) => row.base.contains(value),
                }
            }
        }
    }

    pub(in crate::native::regalloc) fn len(&self) -> usize {
        match self {
            Self::Dense(values) => values.len(),
            Self::Relative(row) => row.len,
        }
    }

    pub(in crate::native::regalloc) fn iter(&self) -> impl Iterator<Item = &VReg> {
        let (base, overrides): (&HashSet<VReg>, &[(VReg, bool)]) = match self {
            Self::Dense(values) => (values, &[]),
            Self::Relative(row) => (&row.base, &row.overrides),
        };
        let changed = |value: &VReg| {
            overrides
                .binary_search_by_key(value, |&(changed, _)| changed)
                .is_ok()
        };
        base.iter().filter(move |value| !changed(value)).chain(
            overrides
                .iter()
                .filter(|(_, present)| *present)
                .map(|(value, _)| value),
        )
    }

    #[cfg(test)]
    pub(in crate::native::regalloc) fn insert(&mut self, value: VReg) {
        let mut values = self.iter().copied().collect::<HashSet<_>>();
        values.insert(value);
        *self = Self::dense(values);
    }
}

impl PartialEq for AnticipatedSet {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Dense(left), Self::Dense(right)) if Arc::ptr_eq(left, right) => true,
            (Self::Relative(left), Self::Relative(right))
                if Arc::ptr_eq(&left.base, &right.base) =>
            {
                left.overrides == right.overrides
            }
            _ => self.len() == other.len() && self.iter().all(|value| other.contains(value)),
        }
    }
}

impl Eq for AnticipatedSet {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_sets_match_their_dense_contents() {
        let base = Arc::new((0..100).map(VReg).collect::<HashSet<_>>());
        let set = AnticipatedSet::relative(&base, [VReg(3), VReg(150), VReg(7)], |value| {
            value == VReg(150) || value == VReg(7)
        })
        .unwrap();
        let mut expected = (0..100).map(VReg).collect::<HashSet<_>>();
        expected.remove(&VReg(3));
        expected.insert(VReg(150));
        let dense = AnticipatedSet::dense(expected.clone());
        assert_eq!(set, dense);
        assert_eq!(set.len(), expected.len());
        assert_eq!(set.iter().copied().collect::<HashSet<_>>(), expected);
        for value in (0..200).map(VReg) {
            assert_eq!(set.contains(&value), expected.contains(&value));
        }
        // Unchanged sets share the base.
        let same = AnticipatedSet::relative(&base, [VReg(1)], |_| true).unwrap();
        assert!(matches!(&same, AnticipatedSet::Dense(shared) if Arc::ptr_eq(shared, &base)));
        // Too many changes do not fit a relative set.
        assert!(AnticipatedSet::relative(&base, (0..40).map(VReg), |_| false).is_none());
    }
}
