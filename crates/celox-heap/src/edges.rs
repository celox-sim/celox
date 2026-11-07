//! The occurrences of the class properties that event controls watch (IEEE
//! 1800-2023 9.4.2), counted where every write of an object goes:
//! [`Heap::write`]. Whatever writes a property, an assignment, `randomize`,
//! a container method writing a handle back or a DPI-C call, counts its
//! changes, so no write can pass a waiting process by.
//!
//! A process waiting on a property of an object registers it with
//! [`Heap::watch_edges`] and samples its counts. A write of a property is
//! one call with the whole value, both planes of a 4-state value included
//! (wider ones go through a buffer, see [`crate::ops::OBJECT_STORE_BYTES`]),
//! so each call that changes a watched property is one occurrence.

use crate::{Handle, Heap};

/// A property of an object that an event control watches, and its
/// occurrences.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Watch {
    offset: u32,
    /// The bytes of its value; a 4-state property's mask follows them.
    value_bytes: u32,
    four_state: bool,
    /// Changes, rises and falls of the value.
    counts: [u32; 3],
}

impl Watch {
    fn storage(&self) -> usize {
        self.value_bytes as usize * if self.four_state { 2 } else { 1 }
    }

    /// Whether a store of `count` bytes at `offset` writes into it.
    fn covers(&self, offset: usize, count: usize) -> bool {
        let start = self.offset as usize;
        offset < start + self.storage() && start < offset + count
    }
}

impl Heap {
    /// Watch the property at `offset` of `object`, `value_bytes` bytes and,
    /// when `four_state`, as many of its mask: from now on its writes count
    /// (see [`Self::edge_count`]). Watching it again changes nothing.
    pub fn watch_edges(&mut self, object: Handle, offset: u32, value_bytes: u32, four_state: bool) {
        if self.object_bytes(object).is_none() {
            return;
        }
        let watches = self.edges.entry(object).or_default();
        if !watches.iter().any(|watch| watch.offset == offset) {
            watches.push(Watch {
                offset,
                value_bytes,
                four_state,
                counts: [0; 3],
            });
        }
    }

    /// The occurrences counted for the watched property at `offset` of
    /// `object`: its changes (`kind` 0), rises (1) or falls (2) of its least
    /// significant bit; 0 for a property no one watches.
    pub fn edge_count(&self, object: Handle, offset: u32, kind: usize) -> u32 {
        self.edges
            .get(&object)
            .and_then(|watches| watches.iter().find(|watch| watch.offset == offset))
            .and_then(|watch| watch.counts.get(kind))
            .copied()
            .unwrap_or(0)
    }

    /// Write `bytes` into `object` at `offset`: one write of each property
    /// it covers. A watched property it changes counts a change of its
    /// value, and a rise or a fall of its least significant bit by Table 9-2
    /// of IEEE 1800-2023. A 4-state property holds the complements of its
    /// value and of its mask, so that zeroed bytes read as x.
    pub(crate) fn write(&mut self, object: Handle, offset: usize, bytes: &[u8]) {
        let range = offset..offset + bytes.len();
        let watched: Vec<(usize, Vec<u8>)> =
            match (self.edges.get(&object), self.object_bytes(object)) {
                (Some(watches), Some(contents)) => watches
                    .iter()
                    .enumerate()
                    .filter(|(_, watch)| watch.covers(offset, bytes.len()))
                    .filter_map(|(index, watch)| {
                        let start = watch.offset as usize;
                        Some((
                            index,
                            contents.get(start..start + watch.storage())?.to_vec(),
                        ))
                    })
                    .collect(),
                _ => Vec::new(),
            };
        let Some(contents) = self.object_bytes_mut(object) else {
            return;
        };
        if let Some(target) = contents.get_mut(range) {
            target.copy_from_slice(bytes);
        }
        for (index, before) in watched {
            self.count_write(object, index, &before);
        }
    }

    /// Count the write of the watch `index` of `object` that replaced
    /// `before` (see [`Self::write`]).
    fn count_write(&mut self, object: Handle, index: usize, before: &[u8]) {
        let Some(&Watch {
            offset,
            value_bytes,
            four_state,
            ..
        }) = self
            .edges
            .get(&object)
            .and_then(|watches| watches.get(index))
        else {
            return;
        };
        let start = offset as usize;
        let Some(after) = self
            .object_bytes(object)
            .and_then(|bytes| bytes.get(start..start + before.len()))
        else {
            return;
        };
        if after == before {
            return;
        }
        let value_bytes = value_bytes as usize;
        // The least significant bit: 0, 1, or neither (x or z).
        let bit = |bytes: &[u8]| -> Option<bool> {
            if four_state {
                if bytes.get(value_bytes).is_some_and(|mask| mask & 1 == 0) {
                    return None;
                }
                return Some(bytes[0] & 1 == 0);
            }
            Some(bytes[0] & 1 != 0)
        };
        let (old, new) = (bit(before), bit(after));
        let rise = matches!(
            (old, new),
            (Some(false), Some(true) | None) | (None, Some(true))
        );
        let fall = matches!(
            (old, new),
            (Some(true), Some(false) | None) | (None, Some(false))
        );
        let watch = &mut self.edges.get_mut(&object).expect("a watch")[index];
        let counts = &mut watch.counts;
        counts[0] = counts[0].wrapping_add(1);
        counts[1] = counts[1].wrapping_add(u32::from(rise));
        counts[2] = counts[2].wrapping_add(u32::from(fall));
    }
}

#[cfg(test)]
mod tests {
    use crate::Heap;

    #[test]
    fn counts_follow_their_objects() {
        let mut heap = Heap::new();
        let object = heap.new_object(7, 2);
        heap.watch_edges(object, 0, 1, false);
        // A 2-state bit: 0 -> 1 -> 0 is two changes, a rise and a fall,
        // also as consecutive stores.
        heap.store(object, 0, 1, 1);
        heap.store(object, 0, 1, 0);
        assert_eq!(
            [0, 1, 2].map(|kind| heap.edge_count(object, 0, kind)),
            [2, 1, 1]
        );
        // A store of the same value is no occurrence.
        heap.store(object, 0, 1, 0);
        assert_eq!(heap.edge_count(object, 0, 0), 2);
        // A store of another property is none either.
        heap.store(object, 1, 1, 5);
        assert_eq!(heap.edge_count(object, 0, 0), 2);
        // The counts go with the object.
        heap.collect([]);
        assert!(!heap.contains_object(object));
        assert_eq!(heap.edge_count(object, 0, 0), 0);
    }

    #[test]
    fn a_write_of_both_planes_is_one_occurrence() {
        // A 4-state bit holds complemented planes: value byte then mask
        // byte. 1 is value 0xfe, mask 0xff; x is mask 0xfe. One store of
        // both planes, 1 -> x, is one fall.
        let mut heap = Heap::new();
        let object = heap.new_object(7, 2);
        heap.store(object, 0, 2, 0xfffe);
        heap.watch_edges(object, 0, 1, true);
        heap.store(object, 0, 2, 0xfeff);
        assert_eq!(
            [0, 1, 2].map(|kind| heap.edge_count(object, 0, kind)),
            [1, 0, 1]
        );
    }

    #[test]
    fn a_wide_write_is_one_occurrence() {
        // Sixteen bytes through a buffer, most significant first: one change
        // and one rise of bit 0, however many words it spans.
        let mut heap = Heap::new();
        let object = heap.new_object(7, 16);
        heap.watch_edges(object, 0, 16, false);
        let mut bytes = vec![0u8; 16];
        bytes[0] = 1;
        bytes[15] = 0x80;
        bytes.reverse();
        let buffer = heap.new_buffer(bytes);
        heap.store_bytes(object, 0, buffer);
        assert_eq!(heap.load(object, 0, 1), 1);
        assert_eq!(heap.load(object, 15, 1), 0x80);
        assert_eq!(
            [0, 1, 2].map(|kind| heap.edge_count(object, 0, kind)),
            [1, 1, 0]
        );
    }

    #[test]
    fn heaps_with_other_occurrences_differ() {
        // Two heaps whose objects hold the same bytes differ when a watched
        // property's occurrences differ: they decide the waits.
        let build = |pulse: bool| {
            let mut heap = Heap::new();
            let object = heap.new_object(7, 1);
            heap.watch_edges(object, 0, 1, false);
            if pulse {
                heap.store(object, 0, 1, 1);
            }
            heap.store(object, 0, 1, 0);
            heap
        };
        assert!(build(false).same_contents(&build(false)));
        assert!(!build(true).same_contents(&build(false)));
    }
}
