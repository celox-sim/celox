//! The occurrences of the class properties that event controls watch (IEEE
//! 1800-2023 9.4.2), counted where every write of an object goes:
//! [`Heap::store`]. Whatever writes a property, an assignment, `randomize`,
//! a container method writing a handle back or a DPI-C call, counts its
//! changes, so no write can pass a waiting process by.
//!
//! A process waiting on a property of an object registers it with
//! [`Heap::watch_edges`] and samples its counts. One write of a property may
//! take several stores, one per word and plane, so a write is counted as a
//! whole: the first store keeps the property's bytes, and the write ends
//! when a store reaches a word it already wrote (the next write of the
//! property), another property, or when the counts are read or the heap
//! collected.

use crate::{Handle, Heap};

/// A property of an object that an event control watches, and its
/// occurrences.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
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

/// A write of a watched property in progress (see the module).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct EdgeWrite {
    object: Handle,
    /// The index of the property among the object's watches.
    watch: usize,
    /// Its bytes before the write.
    before: Vec<u8>,
    /// The offsets of the stores the write made.
    stored: Vec<u32>,
}

impl Heap {
    /// Watch the property at `offset` of `object`, `value_bytes` bytes and,
    /// when `four_state`, as many of its mask: from now on its writes count
    /// (see [`Self::edge_count`]). Watching it again changes nothing.
    pub fn watch_edges(&mut self, object: Handle, offset: u32, value_bytes: u32, four_state: bool) {
        self.finish_edge_write();
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
    pub fn edge_count(&mut self, object: Handle, offset: u32, kind: usize) -> u32 {
        self.finish_edge_write();
        self.edges
            .get(&object)
            .and_then(|watches| watches.iter().find(|watch| watch.offset == offset))
            .and_then(|watch| watch.counts.get(kind))
            .copied()
            .unwrap_or(0)
    }

    /// Before a store of `count` bytes at `offset` of `object`: continue
    /// the write in progress, or end it and begin one of the watched
    /// property the store writes into.
    pub(crate) fn edge_store(&mut self, object: Handle, offset: usize, count: usize) {
        if self.edges.is_empty() {
            return;
        }
        let found = self
            .edges
            .get(&object)
            .and_then(|watches| watches.iter().position(|watch| watch.covers(offset, count)));
        if let (Some(write), Some(watch)) = (&mut self.edge_write, found)
            && write.object == object
            && write.watch == watch
            && !write.stored.contains(&(offset as u32))
        {
            write.stored.push(offset as u32);
            return;
        }
        self.finish_edge_write();
        let Some(watch) = found else {
            return;
        };
        let watch = &self.edges[&object][watch..][0];
        let start = watch.offset as usize;
        let range = start..start + watch.storage();
        let Some(before) = self.object_bytes(object).and_then(|bytes| bytes.get(range)) else {
            return;
        };
        self.edge_write = Some(EdgeWrite {
            object,
            watch: found.expect("a watch"),
            before: before.to_vec(),
            stored: vec![offset as u32],
        });
    }

    /// Count the write in progress, if any: a change of the property's
    /// value, and a rise or a fall of its least significant bit by Table 9-2
    /// of IEEE 1800-2023. A 4-state property holds the complements of its
    /// value and of its mask, so that zeroed bytes read as x.
    pub(crate) fn finish_edge_write(&mut self) {
        let Some(write) = self.edge_write.take() else {
            return;
        };
        let Some(&Watch {
            offset,
            value_bytes,
            four_state,
            ..
        }) = self
            .edges
            .get(&write.object)
            .and_then(|watches| watches.get(write.watch))
        else {
            return;
        };
        let start = offset as usize;
        let Some(after) = self
            .object_bytes(write.object)
            .and_then(|bytes| bytes.get(start..start + write.before.len()))
        else {
            return;
        };
        if after == &write.before[..] {
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
        let (old, new) = (bit(&write.before), bit(after));
        let rise = matches!(
            (old, new),
            (Some(false), Some(true) | None) | (None, Some(true))
        );
        let fall = matches!(
            (old, new),
            (Some(true), Some(false) | None) | (None, Some(false))
        );
        let watch = &mut self.edges.get_mut(&write.object).expect("a watch")[write.watch];
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
        // A 4-state bit holds complemented planes: 0 is value 0xff, mask
        // 0xff; 1 is value 0xfe; x is mask 0xfe. Writing 1 -> x stores the
        // value then the mask: one fall, not a fall and a rise through the
        // intermediate state.
        let mut heap = Heap::new();
        let object = heap.new_object(7, 2);
        heap.store(object, 0, 1, 0xfe);
        heap.store(object, 1, 1, 0xff);
        heap.watch_edges(object, 0, 1, true);
        heap.store(object, 0, 1, 0xff);
        heap.store(object, 1, 1, 0xfe);
        assert_eq!(
            [0, 1, 2].map(|kind| heap.edge_count(object, 0, kind)),
            [1, 0, 1]
        );
    }
}
