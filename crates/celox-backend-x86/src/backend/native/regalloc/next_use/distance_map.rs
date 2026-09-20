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
    Relative(Arc<RelativeRow>),
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
            (Self::Packed(map), Self::Relative(row)) | (Self::Relative(row), Self::Packed(map)) => {
                map.len() == row.len
                    && RelativeIter::new(row)
                        .all(|(key, value)| map.get(key).copied() == Some(value))
            }
            (Self::Relative(left), Self::Relative(right))
                if Arc::ptr_eq(&left.base, &right.base)
                    && left.instruction_delta == right.instruction_delta
                    && left.overrides == right.overrides =>
            {
                true
            }
            (Self::Frozen(_) | Self::Relative(_), Self::Frozen(_) | Self::Relative(_)) => {
                // Both representations iterate in key order. Comparing their
                // streams avoids a fresh indexed chunk lookup for every live
                // value when equivalent transfers have different bases.
                self.len() == other.len() && self.iter().eq(other.iter())
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
        let mut max_instructions = 0;
        for chunk in sorted.chunks(CHUNK_SIZE) {
            let Ok(offset) = u32::try_from(data.len()) else {
                return;
            };
            index.push((chunk[0].0, offset));
            let mut previous_key = 0;
            let mut previous_value = 0i64;
            for &(key, value) in chunk {
                max_instructions = max_instructions.max(value & PACKED_INSTRUCTION_MASK);
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
            max_instructions,
            data: data.into_boxed_slice(),
            index: index.into_boxed_slice(),
        }));
    }

    /// Freeze an entry already computed by the block transfer. Only the
    /// supplied definitions/local uses can differ from the uniformly shifted
    /// exit row, so inspect those keys instead of sorting/scanning the full row.
    pub(super) fn freeze_block_entry(
        &mut self,
        base: &Self,
        instructions: usize,
        definitions: &crate::HashSet<VReg>,
        local_uses: &[(VReg, usize)],
    ) -> bool {
        let Self::Packed(map) = &*self else {
            return false;
        };
        let Some(entry) = Self::relative_entry(
            base,
            instructions,
            definitions,
            local_uses,
            Some(map.len()),
            |key| Some(map.get(&key).copied()),
        ) else {
            return false;
        };
        *self = entry;
        true
    }

    /// Apply a small block transfer directly to an immutable row. Constructing
    /// a complete temporary hash map first would decode and reinsert every
    /// live-through value merely to discard that map after freezing it.
    pub(super) fn try_block_entry(
        base: &Self,
        instructions: usize,
        definitions: &crate::HashSet<VReg>,
        local_uses: &[(VReg, usize)],
    ) -> Option<Self> {
        Self::relative_entry(base, instructions, definitions, local_uses, None, |key| {
            let distance = if let Some(&(_, position)) =
                local_uses.iter().rev().find(|&&(value, _)| value == key)
            {
                NextUseDistance::local(position)
            } else if definitions.contains(&key) {
                return Some(None);
            } else if let Some(distance) = base.get(&key) {
                distance.checked_prepend_instructions(instructions)?
            } else {
                return Some(None);
            };
            pack(distance).map(Some)
        })
    }

    /// Join two small edits of the same live-through row without decoding the
    /// whole row. Ordinary branch arms commonly share their merge's base;
    /// outside the edited keys, minimum distance is just the smaller delta.
    /// Loop-exit adjustments are deliberately handled by the caller's general
    /// path, since they change a different component of the distance.
    pub(super) fn try_branch_join(
        left: &Self,
        right: &Self,
        left_phi_uses: &[VReg],
        right_phi_uses: &[VReg],
    ) -> Option<Self> {
        let (left_base, left_delta, left_edits) = match left {
            Self::Frozen(base) => (base, 0, &[][..]),
            Self::Relative(row) => (&row.base, row.instruction_delta, row.overrides.as_ref()),
            _ => return None,
        };
        let (right_base, right_delta, right_edits) = match right {
            Self::Frozen(base) => (base, 0, &[][..]),
            Self::Relative(row) => (&row.base, row.instruction_delta, row.overrides.as_ref()),
            _ => return None,
        };
        if !Arc::ptr_eq(left_base, right_base)
            || left_phi_uses.len().saturating_add(right_phi_uses.len()) > 32
        {
            return None;
        }
        let base = if left_delta <= right_delta {
            left
        } else {
            right
        };
        if left_edits.is_empty()
            && right_edits.is_empty()
            && left_phi_uses.is_empty()
            && right_phi_uses.is_empty()
        {
            return Some(base.clone());
        }
        let changed = left_edits
            .iter()
            .chain(right_edits)
            .map(|&(key, _)| key)
            .chain(left_phi_uses.iter().copied())
            .chain(right_phi_uses.iter().copied())
            .collect::<crate::HashSet<_>>();
        Self::relative_entry(base, 0, &changed, &[], None, |key| {
            let distance = if left_phi_uses.contains(&key) || right_phi_uses.contains(&key) {
                Some(NextUseDistance::local(0))
            } else {
                match (left.get(&key), right.get(&key)) {
                    (Some(left), Some(right)) => Some(left.min(right)),
                    (left, right) => left.or(right),
                }
            };
            match distance {
                Some(distance) => pack(distance).map(Some),
                None => Some(None),
            }
        })
    }

    fn relative_entry(
        base: &Self,
        instructions: usize,
        definitions: &crate::HashSet<VReg>,
        local_uses: &[(VReg, usize)],
        expected_len: Option<usize>,
        mut actual: impl FnMut(VReg) -> Option<Option<u32>>,
    ) -> Option<Self> {
        let (base, previous_delta, previous_overrides) = match base {
            Self::Frozen(row) => (row, 0, &[][..]),
            Self::Relative(row) => (&row.base, row.instruction_delta, row.overrides.as_ref()),
            _ => return None,
        };
        let instruction_delta = u32::try_from(instructions)
            .ok()
            .and_then(|delta| delta.checked_add(previous_delta))?;
        if expected_len.is_some_and(|len| len < CHUNK_SIZE)
            || definitions.len().saturating_add(local_uses.len()) > 32
            || instruction_delta > PACKED_INSTRUCTION_MASK
            || base.max_instructions > PACKED_INSTRUCTION_MASK - instruction_delta
        {
            return None;
        }
        let mut changed = definitions
            .iter()
            .copied()
            .chain(local_uses.iter().map(|&(key, _)| key))
            .chain(previous_overrides.iter().map(|&(key, _)| key))
            .collect::<Vec<_>>();
        changed.sort_unstable();
        changed.dedup();
        // Flatten onto the original frozen row, keeping lookup and destruction
        // depth constant even across a long chain of single-successor blocks.
        if changed.len() > 32 {
            return None;
        }
        let mut overrides = Vec::new();
        let mut len = base.len;
        for key in changed {
            if key.0 as usize >= base.values.len() {
                return None;
            }
            let old = base.get(key);
            let actual = actual(key)?;
            match (old, actual) {
                (None, Some(_)) => len += 1,
                (Some(_), None) => len -= 1,
                _ => {}
            }
            if actual != old.and_then(|value| prepend_packed(value, instruction_delta)) {
                overrides.push((key, actual));
            }
        }
        if len < CHUNK_SIZE || expected_len.is_some_and(|expected| len != expected) {
            return None;
        }
        if instruction_delta == 0 && overrides.is_empty() {
            return Some(Self::Frozen(Arc::clone(base)));
        }
        // An ordinary frozen row needs at least two encoded bytes per entry.
        if std::mem::size_of::<RelativeRow>()
            + overrides.len() * std::mem::size_of::<(VReg, Option<u32>)>()
            >= len * 2
        {
            return None;
        }
        Some(Self::Relative(Arc::new(RelativeRow {
            base: Arc::clone(base),
            instruction_delta,
            overrides: overrides.into_boxed_slice(),
            len,
        })))
    }

    fn thaw(&mut self) {
        if let Self::Relative(row) = self {
            *self = Self::Packed(
                RelativeIter::new(row)
                    .map(|(&key, value)| (key, value))
                    .collect(),
            );
        }
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
            Self::Relative(row) => row.len,
            Self::Compact(map) => map.len(),
            Self::Wide(map) => map.len(),
        }
    }

    /// Frozen rows and their local edits already validate every key against
    /// their shared identity pool. Reuse that bound when checking a function,
    /// retaining the full key check for a smaller function or mutable rows.
    pub(in crate::native::regalloc) fn first_out_of_range(&self, limit: u32) -> Option<VReg> {
        match self {
            Self::Frozen(row) if row.values.len() <= limit as usize => None,
            Self::Relative(row) if row.base.values.len() <= limit as usize => None,
            _ => self.keys().copied().find(|value| value.0 >= limit),
        }
    }

    pub(in crate::native::regalloc) fn get(&self, key: &VReg) -> Option<NextUseDistance> {
        match self {
            Self::Packed(map) => map.get(key).copied().map(unpack),
            Self::Frozen(row) => row.get(*key).map(unpack),
            Self::Relative(row) => row.get(*key).map(unpack),
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
            Self::Frozen(_) | Self::Relative(_) => unreachable!(),
            Self::Packed(map) => map.remove(key).map(unpack),
            Self::Compact(map) => map.remove(key).map(expand),
            Self::Wide(map) => map.remove(key),
        }
    }

    pub(in crate::native::regalloc) fn iter(&self) -> Iter<'_> {
        match self {
            Self::Packed(map) => Iter::Packed(map.iter()),
            Self::Frozen(row) => Iter::Frozen(FrozenIter::new(row, 0, row.len)),
            Self::Relative(row) => Iter::Relative(RelativeIter::new(row)),
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
    Relative(RelativeIter<'a>),
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
            Self::Relative(iter) => iter.next().map(|(key, value)| (key, unpack(value))),
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
    max_instructions: u32,
    data: Box<[u8]>,
    index: Box<[(VReg, u32)]>,
}

fn prepend_packed(value: u32, instructions: u32) -> Option<u32> {
    let distance = (value & PACKED_INSTRUCTION_MASK).checked_add(instructions)?;
    (distance <= PACKED_INSTRUCTION_MASK).then_some((value & !PACKED_INSTRUCTION_MASK) | distance)
}

#[derive(Debug)]
pub(in crate::native::regalloc) struct RelativeRow {
    base: Arc<FrozenRow>,
    instruction_delta: u32,
    overrides: Box<[(VReg, Option<u32>)]>,
    len: usize,
}

impl RelativeRow {
    fn get(&self, key: VReg) -> Option<u32> {
        if let Ok(index) = self.overrides.binary_search_by_key(&key, |&(key, _)| key) {
            return self.overrides[index].1;
        }
        self.base
            .get(key)
            .and_then(|value| prepend_packed(value, self.instruction_delta))
    }
}

pub(in crate::native::regalloc) struct RelativeIter<'a> {
    row: &'a RelativeRow,
    base: std::iter::Peekable<FrozenIter<'a>>,
    position: usize,
}

impl<'a> RelativeIter<'a> {
    fn new(row: &'a RelativeRow) -> Self {
        Self {
            row,
            base: FrozenIter::new(&row.base, 0, row.base.len).peekable(),
            position: 0,
        }
    }
}

impl<'a> Iterator for RelativeIter<'a> {
    type Item = (&'a VReg, u32);
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let base = self.base.peek().copied();
            let change = self.row.overrides.get(self.position).copied();
            match (base, change) {
                (Some((key, value)), change)
                    if change.is_none_or(|(changed, _)| *key < changed) =>
                {
                    self.base.next();
                    return Some((
                        key,
                        prepend_packed(value, self.row.instruction_delta)
                            .expect("relative row validated distance"),
                    ));
                }
                (base, Some((key, value))) => {
                    if base.is_some_and(|(base_key, _)| *base_key == key) {
                        self.base.next();
                    }
                    self.position += 1;
                    if let Some(value) = value {
                        return Some((&self.row.base.values[key.0 as usize], value));
                    }
                }
                (None, None) => return None,
                _ => unreachable!(),
            }
        }
    }
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
    fn direct_block_transfers_match_materialized_maps_across_relative_rows() {
        let pool = (0..4096).map(VReg).collect::<Arc<[_]>>();
        let mut base = DistanceMap::default();
        for id in 0..1000 {
            base.insert(
                VReg(id * 3),
                NextUseDistance::Finite {
                    loop_exits: (id % 7) as usize,
                    instructions: (id % 101) as usize,
                },
            );
        }
        base.freeze(&pool);
        let definitions = [VReg(3), VReg(1500), VReg(4095)]
            .into_iter()
            .collect::<crate::HashSet<_>>();
        for round in 0..12 {
            let instructions = round + 3;
            let local_uses = [(VReg(0), 1), (VReg(1501), 2), (VReg(0), round)];
            let mut expected = DistanceMap::default();
            for (&key, distance) in &base {
                if !definitions.contains(&key) {
                    expected.insert(
                        key,
                        distance.checked_prepend_instructions(instructions).unwrap(),
                    );
                }
            }
            for &(key, position) in &local_uses {
                expected.insert(key, NextUseDistance::local(position));
            }
            let actual =
                DistanceMap::try_block_entry(&base, instructions, &definitions, &local_uses)
                    .expect("small exact transfer should share its frozen base");
            assert_eq!(actual, expected);
            for key in pool.iter() {
                assert_eq!(actual.get(key), expected.get(key));
            }
            base = actual;
        }
    }

    #[test]
    fn direct_block_transfers_fall_back_for_range_and_edit_limits() {
        let pool = (0..256).map(VReg).collect::<Arc<[_]>>();
        let mut base = DistanceMap::default();
        for id in 0..256 {
            base.insert(VReg(id), NextUseDistance::local(id as usize));
        }
        base.freeze(&pool);
        let empty = crate::HashSet::default();
        let shared = DistanceMap::try_block_entry(&base, 0, &empty, &[]).unwrap();
        let (DistanceMap::Frozen(left), DistanceMap::Frozen(right)) = (&base, &shared) else {
            panic!("unchanged transfer should reuse the frozen row")
        };
        assert!(Arc::ptr_eq(left, right));
        assert!(DistanceMap::try_block_entry(&base, usize::MAX, &empty, &[]).is_none());
        assert!(
            DistanceMap::try_block_entry(&base, 0, &(0..33).map(VReg).collect(), &[]).is_none()
        );
        assert!(DistanceMap::try_block_entry(&base, 0, &empty, &[(VReg(u32::MAX), 0)]).is_none());
        assert!(
            DistanceMap::try_block_entry(
                &base,
                0,
                &empty,
                &[(VReg(0), PACKED_INSTRUCTION_MASK as usize + 1)]
            )
            .is_none()
        );
        let relative = DistanceMap::try_block_entry(
            &base,
            0,
            &empty,
            &[(VReg(0), PACKED_INSTRUCTION_MASK as usize)],
        )
        .unwrap();
        assert!(DistanceMap::try_block_entry(&relative, 1, &empty, &[]).is_none());
    }

    #[test]
    fn unchanged_entry_shares_the_frozen_exit_directly() {
        let pool = (0..256).map(VReg).collect::<Arc<[_]>>();
        let mut base = DistanceMap::default();
        for id in 0..256 {
            base.insert(VReg(id), NextUseDistance::local(id as usize));
        }
        let mut entry = base.clone();
        base.freeze(&pool);
        assert!(entry.freeze_block_entry(&base, 0, &crate::HashSet::default(), &[]));
        let (DistanceMap::Frozen(base), DistanceMap::Frozen(entry)) = (&base, &entry) else {
            panic!("expected shared frozen rows")
        };
        assert!(Arc::ptr_eq(base, entry));
    }

    #[test]
    fn relative_rows_preserve_transfer_edits_order_and_mutation() {
        let pool = (0..4096).map(VReg).collect::<Arc<[_]>>();
        let mut base = DistanceMap::default();
        for id in 1..1001 {
            base.insert(
                VReg(id * 3),
                NextUseDistance::Finite {
                    loop_exits: (id % 7) as usize,
                    instructions: (id % 101) as usize,
                },
            );
        }
        base.freeze(&pool);
        let mut entry = base
            .iter()
            .map(|(&key, distance)| (key, distance.checked_prepend_instructions(17).unwrap()))
            .fold(DistanceMap::default(), |mut row, (key, distance)| {
                row.insert(key, distance);
                row
            });
        for id in [3, 1500, 3000] {
            entry.remove(&VReg(id));
        }
        for id in [0, 9, 1501, 4095] {
            entry.insert(VReg(id), NextUseDistance::local(2));
        }
        let expected = entry.clone();
        assert!(entry.freeze_block_entry(
            &base,
            17,
            &[VReg(3), VReg(1500), VReg(3000)].into_iter().collect(),
            &[(VReg(0), 2), (VReg(9), 2), (VReg(1501), 2), (VReg(4095), 2)]
        ));
        assert!(matches!(entry, DistanceMap::Relative(_)));
        assert_eq!(entry, expected);
        assert_eq!(expected, entry);
        let mut frozen = expected.clone();
        frozen.freeze(&pool);
        assert!(matches!(frozen, DistanceMap::Frozen(_)));
        assert_eq!(entry, frozen);
        assert_eq!(frozen, entry);
        let shifted =
            DistanceMap::try_block_entry(&frozen, 1, &crate::HashSet::default(), &[]).unwrap();
        assert_ne!(entry, shifted);
        assert_ne!(shifted, entry);
        frozen.insert(VReg(1501), NextUseDistance::local(3));
        frozen.freeze(&pool);
        assert_eq!(entry.len(), frozen.len());
        assert_ne!(entry, frozen);
        assert_ne!(frozen, entry);
        let mut oracle = expected
            .iter()
            .map(|(&key, distance)| (key, distance))
            .collect::<Vec<_>>();
        oracle.sort_by_key(|&(key, _)| key);
        assert_eq!(
            entry
                .iter()
                .map(|(&key, distance)| (key, distance))
                .collect::<Vec<_>>(),
            oracle
        );
        for key in pool.iter().chain([VReg(u32::MAX)].iter()) {
            assert_eq!(entry.get(key), expected.get(key));
        }
        let saved = entry.clone();
        let mut changed = expected.clone();
        assert_eq!(entry.remove(&VReg(1501)), changed.remove(&VReg(1501)));
        let wide = NextUseDistance::Finite {
            loop_exits: usize::MAX,
            instructions: usize::MAX,
        };
        assert_eq!(entry.insert(VReg(0), wide), changed.insert(VReg(0), wide));
        assert_eq!(entry, changed);
        assert_eq!(saved, expected);
    }

    #[test]
    fn chained_transfers_share_one_base_and_preserve_overrides() {
        let pool = (0..1024).map(VReg).collect::<Arc<[_]>>();
        let mut row = DistanceMap::default();
        for id in 0..512 {
            row.insert(VReg(id), NextUseDistance::local(id as usize));
        }
        row.freeze(&pool);
        let DistanceMap::Frozen(base) = &row else {
            panic!("expected frozen row")
        };
        let original = Arc::clone(base);
        for step in 0..256 {
            let mut next = DistanceMap::default();
            for (&key, distance) in &row {
                next.insert(key, distance.checked_prepend_instructions(3).unwrap());
            }
            let removed = VReg(step % 8);
            let used = VReg(512 + step % 8);
            next.remove(&removed);
            next.insert(used, NextUseDistance::local(1));
            let expected = next.clone();
            assert!(next.freeze_block_entry(
                &row,
                3,
                &[removed].into_iter().collect(),
                &[(used, 1)],
            ));
            assert_eq!(next, expected);
            let DistanceMap::Relative(relative) = &next else {
                panic!("expected relative row")
            };
            assert!(Arc::ptr_eq(&relative.base, &original));
            for key in pool.iter() {
                assert_eq!(next.get(key), expected.get(key));
            }
            let mut actual = next.iter().map(|(&k, v)| (k, v)).collect::<Vec<_>>();
            let mut oracle = expected.iter().map(|(&k, v)| (k, v)).collect::<Vec<_>>();
            actual.sort_unstable_by_key(|&(k, _)| k);
            oracle.sort_unstable_by_key(|&(k, _)| k);
            assert_eq!(actual, oracle);
            row = next;
        }
        // Too many accumulated edits fall back to the exact ordinary row.
        let mut next = DistanceMap::default();
        for (&key, distance) in &row {
            next.insert(key, distance);
        }
        let definitions = (16..48).map(VReg).collect::<crate::HashSet<_>>();
        for key in &definitions {
            next.remove(key);
        }
        let expected = next.clone();
        assert!(!next.freeze_block_entry(&row, 0, &definitions, &[]));
        next.freeze(&pool);
        assert_eq!(next, expected);
    }

    #[test]
    fn relative_rows_handle_packed_boundaries_and_fallbacks() {
        let pool = (0..1024).map(VReg).collect::<Arc<[_]>>();
        let mut base = DistanceMap::default();
        for id in 0..256 {
            base.insert(
                VReg(id),
                NextUseDistance::Finite {
                    loop_exits: 255,
                    instructions: PACKED_INSTRUCTION_MASK as usize,
                },
            );
        }
        base.freeze(&pool);
        let mut entry = base.clone();
        entry.remove(&VReg(0));
        entry.insert(VReg(1), NextUseDistance::local(0));
        let expected = entry.clone();
        assert!(entry.freeze_block_entry(
            &base,
            0,
            &[VReg(0)].into_iter().collect(),
            &[(VReg(1), 0)]
        ));
        assert_eq!(entry, expected);
        assert!(
            !expected
                .clone()
                .freeze_block_entry(&base, 1, &crate::HashSet::default(), &[])
        );
        assert!(!expected.clone().freeze_block_entry(
            &base,
            usize::MAX,
            &crate::HashSet::default(),
            &[]
        ));
        let mut composed = expected.clone();
        assert!(composed.freeze_block_entry(&entry, 0, &crate::HashSet::default(), &[]));
        assert_eq!(composed, expected);
        let mut many_edits = base.clone();
        for id in 0..33 {
            many_edits.remove(&VReg(id));
        }
        let unchanged = many_edits.clone();
        assert!(!many_edits.freeze_block_entry(&base, 0, &(0..33).map(VReg).collect(), &[]));
        assert_eq!(many_edits, unchanged);
        many_edits.freeze(&pool);
        assert_eq!(many_edits, unchanged);
        let mut wide = base.clone();
        wide.insert(VReg(2), NextUseDistance::Dead);
        assert!(!wide.freeze_block_entry(&base, 0, &crate::HashSet::default(), &[]));
        assert_eq!(wide.get(&VReg(2)), Some(NextUseDistance::Dead));
    }

    #[test]
    fn relative_rows_fall_back_when_removed_values_would_overflow_shift() {
        let pool = (0..256).map(VReg).collect::<Arc<[_]>>();
        let mut base = DistanceMap::default();
        let mut entry = DistanceMap::default();
        for id in 0..256 {
            base.insert(
                VReg(id),
                NextUseDistance::Finite {
                    loop_exits: 255,
                    instructions: if id < 2 {
                        PACKED_INSTRUCTION_MASK as usize
                    } else {
                        id as usize
                    },
                },
            );
            if id != 0 {
                entry.insert(
                    VReg(id),
                    if id == 1 {
                        NextUseDistance::local(0)
                    } else {
                        NextUseDistance::Finite {
                            loop_exits: 255,
                            instructions: id as usize + 1,
                        }
                    },
                );
            }
        }
        base.freeze(&pool);
        let expected = entry.clone();
        assert!(!entry.freeze_block_entry(
            &base,
            1,
            &[VReg(0)].into_iter().collect(),
            &[(VReg(1), 0)]
        ));
        entry.freeze(&pool);
        assert_eq!(entry, expected);
        assert_eq!(expected, entry);
        assert_eq!(entry.iter().count(), 255);
        for key in &*pool {
            assert_eq!(entry.get(key), expected.get(key));
        }
    }

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
#[test]
fn shared_base_branch_joins_match_materialized_minimums() {
    let pool = (0..4096).map(VReg).collect::<Arc<[_]>>();
    let mut base = DistanceMap::default();
    for value in 0..1000 {
        base.insert(
            VReg(value * 3),
            NextUseDistance::Finite {
                loop_exits: (value % 7) as usize,
                instructions: (value % 101) as usize,
            },
        );
    }
    base.freeze(&pool);
    let mut rows = vec![base.clone()];
    for seed in 0..8 {
        rows.push(
            DistanceMap::try_block_entry(
                &base,
                seed + 1,
                &[VReg(0), VReg(seed as u32 * 3 + 3)].into_iter().collect(),
                &[(VReg(15), seed), (VReg(3001 + seed as u32), 0)],
            )
            .unwrap(),
        );
    }
    for left in &rows {
        for right in &rows {
            for phi in [&[][..], &[VReg(0), VReg(15), VReg(4095), VReg(15)][..]] {
                let actual = DistanceMap::try_branch_join(left, right, phi, &[]).unwrap();
                let reverse = DistanceMap::try_branch_join(right, left, &[], phi).unwrap();
                let mut expected = std::collections::BTreeMap::new();
                for (&key, distance) in left.iter().chain(right.iter()) {
                    expected
                        .entry(key)
                        .and_modify(|value: &mut NextUseDistance| *value = (*value).min(distance))
                        .or_insert(distance);
                }
                for &key in phi {
                    expected.insert(key, NextUseDistance::local(0));
                }
                for row in [&actual, &reverse] {
                    assert_eq!(row.len(), expected.len());
                    assert_eq!(
                        row.iter()
                            .map(|(&key, value)| (key, value))
                            .collect::<std::collections::BTreeMap<_, _>>(),
                        expected
                    );
                }
            }
        }
    }
    assert!(DistanceMap::try_branch_join(&base, &DistanceMap::default(), &[], &[]).is_none());
    assert!(
        DistanceMap::try_branch_join(&base, &base, &(0..33).map(VReg).collect::<Vec<_>>(), &[])
            .is_none()
    );
    let mut independent = DistanceMap::default();
    for (&key, distance) in &base {
        independent.insert(key, distance);
    }
    independent.freeze(&pool);
    assert_eq!(base, independent);
    assert!(DistanceMap::try_branch_join(&base, &independent, &[], &[]).is_none());
}

#[test]
fn cached_key_bounds_preserve_invalid_value_checks() {
    let pool = (0..512).map(VReg).collect::<Arc<[_]>>();
    let mut packed = DistanceMap::default();
    for value in 0..256 {
        packed.insert(VReg(value * 2), NextUseDistance::local(1));
    }
    let mut frozen = packed.clone();
    frozen.freeze(&pool);
    assert!(matches!(frozen, DistanceMap::Frozen(_)));
    let relative = DistanceMap::try_block_entry(
        &frozen,
        1,
        &[VReg(510)].into_iter().collect(),
        &[(VReg(511), 0)],
    )
    .unwrap();
    let removed =
        DistanceMap::try_block_entry(&frozen, 1, &[VReg(510)].into_iter().collect(), &[]).unwrap();
    let mut malformed = packed.clone();
    malformed.insert(VReg(512), NextUseDistance::local(0));
    malformed.freeze(&pool);
    assert!(matches!(malformed, DistanceMap::Packed(_)));
    let compact = DistanceMap::Compact(packed.iter().map(|(&key, _)| (key, [0, 1])).collect());
    let wide = DistanceMap::Wide(packed.iter().map(|(&key, value)| (key, value)).collect());
    for row in [
        packed,
        frozen,
        relative,
        removed,
        malformed,
        compact,
        wide,
        DistanceMap::default(),
    ] {
        for limit in [0, 1, 256, 509, 510, 511, 512, 513, u32::MAX] {
            assert_eq!(
                row.first_out_of_range(limit),
                row.keys().copied().find(|value| value.0 >= limit)
            );
        }
    }
}
