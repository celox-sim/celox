//! Compact storage for live next-use rows, with lossless wide fallback.
//!
//! Ordinary rows pack 8 bits of loop exits and 24 bits of instructions into
//! one u32. Rows needing more range fall back to two u32 values or usize.
//! On 64-bit targets the ordinary key/distance pair shrinks from 32 to 8 bytes.
//! Completed rows additionally delta-code sorted keys and distances, with
//! indexed chunks for lookup; mutation restores the ordinary representation.
//! Keep the usize-based distance API: a row that needs greater range promotes
//! to the original representation, without saturation or a new compiler limit.

use std::sync::Arc;

use super::NextUseDistance;
use crate::HashMap;
use crate::native::mir::VReg;

#[derive(Debug, Clone)]
pub(in crate::native::regalloc) enum DistanceMap {
    Packed(HashMap<VReg, u32>),
    Frozen(Arc<FrozenRow>),
    Compact(HashMap<VReg, [u32; 2]>),
    Wide(HashMap<VReg, NextUseDistance>),
}

const PACKED_INSTRUCTION_BITS: u32 = 24;
const PACKED_INSTRUCTION_MASK: u32 = (1 << PACKED_INSTRUCTION_BITS) - 1;

fn pack(distance: NextUseDistance) -> Option<u32> {
    let [loop_exits, instructions] = compact(distance)?;
    if loop_exits <= u8::MAX as u32 && instructions <= PACKED_INSTRUCTION_MASK {
        Some((loop_exits << PACKED_INSTRUCTION_BITS) | instructions)
    } else {
        None
    }
}

fn unpack(value: u32) -> NextUseDistance {
    expand([
        value >> PACKED_INSTRUCTION_BITS,
        value & PACKED_INSTRUCTION_MASK,
    ])
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
        Self::Packed(HashMap::default())
    }
}

impl PartialEq for DistanceMap {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Packed(left), Self::Packed(right)) => left == right,
            (Self::Frozen(left), Self::Frozen(right)) => {
                left.data == right.data && left.index == right.index
            }
            (Self::Packed(map), Self::Frozen(row)) | (Self::Frozen(row), Self::Packed(map)) => {
                // Dataflow compares a freshly computed map with its frozen
                // predecessor. Decode once in order instead of doing an
                // indexed chunk lookup for every key in the fresh map.
                map.len() == row.len
                    && FrozenIter::new(row, 0, row.len)
                        .all(|(key, value)| map.get(key).copied() == Some(value))
            }
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
    /// Freeze completed dataflow rows. Delta coding exploits nearby virtual
    /// registers and use positions without changing the distance domain.
    pub(in crate::native::regalloc) fn freeze(&mut self, values: &Arc<[VReg]>) {
        let Self::Packed(map) = self else { return };
        if map.len() < CHUNK_SIZE {
            return;
        }
        let mut sorted = map
            .iter()
            .map(|(&key, &value)| (key, value))
            .collect::<Vec<_>>();
        sorted.sort_unstable_by_key(|&(key, _)| key);
        if sorted
            .last()
            .is_some_and(|&(key, _)| key.0 as usize >= values.len())
        {
            // Preserve malformed rows for the structured MIR verifier.
            return;
        }
        let mut data = Vec::new();
        let mut index = Vec::new();
        for chunk in sorted.chunks(CHUNK_SIZE) {
            let Ok(offset) = u32::try_from(data.len()) else {
                return;
            };
            index.push((chunk[0].0, offset));
            let mut previous_key = 0;
            let mut previous_value = 0i64;
            for &(key, value) in chunk {
                write_varint(&mut data, u64::from(key.0 - previous_key));
                let delta = i64::from(value) - previous_value;
                write_varint(&mut data, ((delta << 1) ^ (delta >> 63)) as u64);
                previous_key = key.0;
                previous_value = i64::from(value);
            }
        }
        // A pathological ordering can compress poorly. Keep the ordinary map
        // in that case rather than making its representation larger.
        if data.len() + index.len() * std::mem::size_of::<(VReg, u32)>() >= map.len() * 8 {
            return;
        }
        *self = Self::Frozen(Arc::new(FrozenRow {
            values: Arc::clone(values),
            len: sorted.len(),
            data: data.into_boxed_slice(),
            index: index.into_boxed_slice(),
        }));
    }

    fn thaw(&mut self) {
        if let Self::Frozen(row) = self {
            let map = FrozenIter::new(row, 0, row.len)
                .map(|(&key, value)| (key, value))
                .collect();
            *self = Self::Packed(map);
        }
    }

    pub(in crate::native::regalloc) fn len(&self) -> usize {
        match self {
            Self::Packed(map) => map.len(),
            Self::Frozen(row) => row.len,
            Self::Compact(map) => map.len(),
            Self::Wide(map) => map.len(),
        }
    }

    pub(in crate::native::regalloc) fn get(&self, key: &VReg) -> Option<NextUseDistance> {
        match self {
            Self::Packed(map) => map.get(key).copied().map(unpack),
            Self::Frozen(row) => row.get(*key).map(unpack),
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
        self.thaw();
        if let Self::Packed(map) = self {
            if let Some(value) = pack(value) {
                return map.insert(key, value).map(unpack);
            }
            *self = Self::Compact(
                std::mem::take(map)
                    .into_iter()
                    .map(|(key, value)| {
                        (
                            key,
                            [
                                value >> PACKED_INSTRUCTION_BITS,
                                value & PACKED_INSTRUCTION_MASK,
                            ],
                        )
                    })
                    .collect(),
            );
        }
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
        self.thaw();
        match self {
            Self::Frozen(_) => unreachable!(),
            Self::Packed(map) => map.remove(key).map(unpack),
            Self::Compact(map) => map.remove(key).map(expand),
            Self::Wide(map) => map.remove(key),
        }
    }

    pub(in crate::native::regalloc) fn iter(&self) -> Iter<'_> {
        match self {
            Self::Packed(map) => Iter::Packed(map.iter()),
            Self::Frozen(row) => Iter::Frozen(FrozenIter::new(row, 0, row.len)),
            Self::Compact(map) => Iter::Compact(map.iter()),
            Self::Wide(map) => Iter::Wide(map.iter()),
        }
    }

    pub(in crate::native::regalloc) fn keys(&self) -> impl Iterator<Item = &VReg> {
        self.iter().map(|(key, _)| key)
    }
}

pub(in crate::native::regalloc) enum Iter<'a> {
    Frozen(FrozenIter<'a>),
    Packed(std::collections::hash_map::Iter<'a, VReg, u32>),
    Compact(std::collections::hash_map::Iter<'a, VReg, [u32; 2]>),
    Wide(std::collections::hash_map::Iter<'a, VReg, NextUseDistance>),
}

impl<'a> Iterator for Iter<'a> {
    type Item = (&'a VReg, NextUseDistance);
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Packed(iter) => iter.next().map(|(key, value)| (key, unpack(*value))),
            Self::Frozen(iter) => iter.next().map(|(key, value)| (key, unpack(value))),
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

const CHUNK_SIZE: usize = 64;

#[derive(Debug)]
pub(in crate::native::regalloc) struct FrozenRow {
    // Shared dense identity table preserves the borrowed-key iterator API.
    values: Arc<[VReg]>,
    len: usize,
    data: Box<[u8]>,
    index: Box<[(VReg, u32)]>,
}

fn write_varint(data: &mut Vec<u8>, mut value: u64) {
    while value >= 128 {
        data.push(value as u8 | 128);
        value >>= 7;
    }
    data.push(value as u8);
}

fn read_varint(data: &[u8], offset: &mut usize) -> u64 {
    let mut value = 0;
    let mut shift = 0;
    loop {
        let byte = data[*offset];
        *offset += 1;
        value |= u64::from(byte & 127) << shift;
        if byte < 128 {
            return value;
        }
        shift += 7;
    }
}

impl FrozenRow {
    fn get(&self, key: VReg) -> Option<u32> {
        let chunk = self
            .index
            .partition_point(|&(first, _)| first <= key)
            .checked_sub(1)?;
        let start = chunk * CHUNK_SIZE;
        for (&candidate, value) in FrozenIter::new(self, chunk, (self.len - start).min(CHUNK_SIZE))
        {
            if candidate == key {
                return Some(value);
            }
            if candidate > key {
                break;
            }
        }
        None
    }
}

pub(in crate::native::regalloc) struct FrozenIter<'a> {
    row: &'a FrozenRow,
    offset: usize,
    position: usize,
    remaining: usize,
    key: u32,
    value: i64,
}

impl<'a> FrozenIter<'a> {
    fn new(row: &'a FrozenRow, chunk: usize, remaining: usize) -> Self {
        Self {
            row,
            offset: row.index[chunk].1 as usize,
            position: 0,
            remaining,
            key: 0,
            value: 0,
        }
    }
}

impl<'a> Iterator for FrozenIter<'a> {
    type Item = (&'a VReg, u32);
    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        if self.position.is_multiple_of(CHUNK_SIZE) {
            self.key = 0;
            self.value = 0;
        }
        self.key += read_varint(&self.row.data, &mut self.offset) as u32;
        let delta = read_varint(&self.row.data, &mut self.offset);
        self.value += ((delta >> 1) as i64) ^ -((delta & 1) as i64);
        self.position += 1;
        self.remaining -= 1;
        Some((&self.row.values[self.key as usize], self.value as u32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frozen_rows_preserve_lookup_iteration_and_mutation() {
        let values = (0..4096).map(VReg).collect::<Arc<[_]>>();
        let mut row = DistanceMap::default();
        for i in 0..1000 {
            row.insert(
                VReg(i * 3),
                NextUseDistance::Finite {
                    loop_exits: if i % 100 < 50 { 255 } else { 0 },
                    instructions: (1000 - i) as usize,
                },
            );
        }
        let expected = row.clone();
        row.freeze(&values);
        assert!(matches!(row, DistanceMap::Frozen(_)));
        assert_eq!(row, expected);
        assert_eq!(expected, row);
        let mut different = expected.clone();
        different.insert(VReg(3), NextUseDistance::local(0));
        assert_ne!(different, row);
        assert_ne!(row, different);
        assert_eq!(row.iter().count(), expected.len());
        for (key, value) in row.iter() {
            assert_eq!(expected.get(key), Some(value));
        }
        for key in values.iter() {
            assert_eq!(row.get(key), expected.get(key));
        }
        let mut changed = row.clone();
        assert_eq!(changed.remove(&VReg(3)), expected.get(&VReg(3)));
        assert_eq!(row, expected);
        assert_eq!(changed.get(&VReg(3)), None);
        changed.freeze(&values);
        let wide = NextUseDistance::Finite {
            loop_exits: usize::MAX,
            instructions: usize::MAX,
        };
        changed.insert(VReg(3), wide);
        assert_eq!(changed.get(&VReg(3)), Some(wide));
        assert_eq!(changed.len(), expected.len());
    }

    #[test]
    fn frozen_rows_handle_exact_chunks_and_empty_rows() {
        let values = (0..256).map(VReg).collect::<Arc<[_]>>();
        for size in [0, 1, 63, 64, 65, 127, 128, 129, 256] {
            let mut row = DistanceMap::default();
            for i in 0..size {
                row.insert(VReg(i), NextUseDistance::local(0));
            }
            let expected = row.clone();
            row.freeze(&values);
            assert_eq!(row, expected);
            assert_eq!(row.iter().count(), size as usize);
            for key in values.iter() {
                assert_eq!(row.get(key), expected.get(key));
            }
        }
    }

    #[test]
    fn packed_boundaries_fall_back_independently_without_changing_rows() {
        for overflow in [
            NextUseDistance::Finite {
                loop_exits: 256,
                instructions: 0,
            },
            NextUseDistance::Finite {
                loop_exits: 0,
                instructions: 1 << 24,
            },
            NextUseDistance::Dead,
        ] {
            let mut row = DistanceMap::default();
            let boundary = NextUseDistance::Finite {
                loop_exits: 255,
                instructions: (1 << 24) - 1,
            };
            row.insert(VReg(0), boundary);
            assert!(matches!(row, DistanceMap::Packed(_)));
            let packed = row.clone();
            row.insert(VReg(1), overflow);
            assert_eq!(row.get(&VReg(0)), Some(boundary));
            assert_eq!(row.remove(&VReg(1)), Some(overflow));
            assert_eq!(row, packed);
        }
    }

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
            if index == 0 {
                assert!(matches!(row, DistanceMap::Packed(_)));
            } else if index == 1 {
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
