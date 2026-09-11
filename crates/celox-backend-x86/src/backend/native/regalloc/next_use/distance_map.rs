//! Compact storage for live next-use rows, with lossless wide fallback.
//!
//! Ordinary rows contain only finite distances that fit two u32 values. On
//! 64-bit targets this shrinks each key/distance pair from 32 to 12 bytes.
//! Keep the usize-based distance API: a row that needs greater range promotes
//! to the original representation, without saturation or a new compiler limit.

use super::NextUseDistance;
use crate::HashMap;
use crate::native::mir::VReg;

#[derive(Debug, Clone)]
pub(in crate::native::regalloc) enum DistanceMap {
    Compact(HashMap<VReg, [u32; 2]>),
    Wide(HashMap<VReg, NextUseDistance>),
}

fn compact(distance: NextUseDistance) -> Option<[u32; 2]> {
    match distance {
        NextUseDistance::Finite {
            loop_exits,
            instructions,
        } => Some([loop_exits.try_into().ok()?, instructions.try_into().ok()?]),
        NextUseDistance::Dead => None,
    }
}

fn expand([loop_exits, instructions]: [u32; 2]) -> NextUseDistance {
    NextUseDistance::Finite {
        loop_exits: loop_exits as usize,
        instructions: instructions as usize,
    }
}

impl Default for DistanceMap {
    fn default() -> Self {
        Self::Compact(HashMap::default())
    }
}

impl PartialEq for DistanceMap {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Compact(left), Self::Compact(right)) => left == right,
            (Self::Wide(left), Self::Wide(right)) => left == right,
            _ => {
                self.len() == other.len()
                    && self
                        .iter()
                        .all(|(key, value)| other.get(key) == Some(value))
            }
        }
    }
}
impl Eq for DistanceMap {}

impl DistanceMap {
    pub(in crate::native::regalloc) fn len(&self) -> usize {
        match self {
            Self::Compact(map) => map.len(),
            Self::Wide(map) => map.len(),
        }
    }

    pub(in crate::native::regalloc) fn get(&self, key: &VReg) -> Option<NextUseDistance> {
        match self {
            Self::Compact(map) => map.get(key).copied().map(expand),
            Self::Wide(map) => map.get(key).copied(),
        }
    }

    pub(in crate::native::regalloc) fn contains_key(&self, key: &VReg) -> bool {
        self.get(key).is_some()
    }

    pub(in crate::native::regalloc) fn insert(
        &mut self,
        key: VReg,
        value: NextUseDistance,
    ) -> Option<NextUseDistance> {
        if let Self::Compact(map) = self {
            if let Some(value) = compact(value) {
                return map.insert(key, value).map(expand);
            }
            // Distances still have usize precision. Only rows needing a large
            // distance (or an explicit Dead in verifier tests) use wide storage.
            *self = Self::Wide(
                std::mem::take(map)
                    .into_iter()
                    .map(|(key, value)| (key, expand(value)))
                    .collect(),
            );
        }
        let Self::Wide(map) = self else {
            unreachable!()
        };
        map.insert(key, value)
    }

    pub(in crate::native::regalloc) fn insert_min(&mut self, key: VReg, value: NextUseDistance) {
        if self.get(&key).is_none_or(|old| value < old) {
            self.insert(key, value);
        }
    }

    #[cfg(test)]
    pub(in crate::native::regalloc) fn remove(&mut self, key: &VReg) -> Option<NextUseDistance> {
        match self {
            Self::Compact(map) => map.remove(key).map(expand),
            Self::Wide(map) => map.remove(key),
        }
    }

    pub(in crate::native::regalloc) fn iter(&self) -> Iter<'_> {
        match self {
            Self::Compact(map) => Iter::Compact(map.iter()),
            Self::Wide(map) => Iter::Wide(map.iter()),
        }
    }

    pub(in crate::native::regalloc) fn keys(&self) -> impl Iterator<Item = &VReg> {
        self.iter().map(|(key, _)| key)
    }
}

pub(in crate::native::regalloc) enum Iter<'a> {
    Compact(std::collections::hash_map::Iter<'a, VReg, [u32; 2]>),
    Wide(std::collections::hash_map::Iter<'a, VReg, NextUseDistance>),
}

impl<'a> Iterator for Iter<'a> {
    type Item = (&'a VReg, NextUseDistance);
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Compact(iter) => iter.next().map(|(key, value)| (key, expand(*value))),
            Self::Wide(iter) => iter.next().map(|(key, value)| (key, *value)),
        }
    }
}

impl<'a> IntoIterator for &'a DistanceMap {
    type Item = (&'a VReg, NextUseDistance);
    type IntoIter = Iter<'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_rows_preserve_boundaries_and_promote_without_truncation() {
        let mut row = DistanceMap::default();
        let mut expected = HashMap::default();
        for (index, distance) in [
            NextUseDistance::Finite {
                loop_exits: 0,
                instructions: 0,
            },
            NextUseDistance::Finite {
                loop_exits: u32::MAX as usize,
                instructions: u32::MAX as usize,
            },
            NextUseDistance::Finite {
                loop_exits: usize::MAX,
                instructions: usize::MAX,
            },
            NextUseDistance::Dead,
        ]
        .into_iter()
        .enumerate()
        {
            let key = VReg(index as u32);
            row.insert(key, distance);
            expected.insert(key, distance);
            if index < 2 {
                assert!(matches!(row, DistanceMap::Compact(_)));
            }
            assert_eq!(
                row.iter()
                    .map(|(&key, value)| (key, value))
                    .collect::<HashMap<_, _>>(),
                expected
            );
            for (key, value) in &expected {
                assert_eq!(row.get(key), Some(*value));
            }
        }
        for key in expected.keys() {
            assert_eq!(row.remove(key), expected.get(key).copied());
        }
        assert_eq!(row.len(), 0);
    }

    #[test]
    fn minimum_updates_and_equality_do_not_depend_on_storage() {
        let mut compact = DistanceMap::default();
        let mut wide = DistanceMap::Wide(HashMap::default());
        for instructions in [30, 10, 20, 0, 40] {
            let distance = NextUseDistance::Finite {
                loop_exits: 0,
                instructions,
            };
            compact.insert_min(VReg(1), distance);
            wide.insert_min(VReg(1), distance);
            assert_eq!(compact, wide);
        }
        assert_eq!(compact.get(&VReg(1)), Some(NextUseDistance::local(0)));
    }
}
