//! The runtime heap of a simulator.
//!
//! Dynamic values live here and state memory refers to them by 32-bit
//! handles, never by address, so state images stay position independent.
//! Strings are interned and immutable: equal contents have equal handles, so
//! stores, commits, change detection and `==` compare handles as integers.
//!
//! References from state memory are not counted. A collection marks the
//! entries reachable from the handles the caller passes as roots and frees
//! the rest; callers collect only where every copy of a handle that may still
//! be read is in a place they can enumerate. Handles are never reused, so a
//! stale copy that is only compared, such as the value a wait started from,
//! cannot equal a later string.

use std::sync::Arc;

use fxhash::{FxHashMap as HashMap, FxHashSet as HashSet};

pub mod ops;

/// A reference to a heap entry, as stored in state memory.
pub type Handle = u32;

/// Byte width of a handle slot in state memory.
pub const HANDLE_BYTES: usize = 4;

/// The handle of `""`, the value of zero-initialized state memory.
pub const EMPTY_STRING: Handle = 0;

/// Entries created since the last collection before another one is due,
/// unless the live set is larger.
const MIN_COLLECTION_INTERVAL: usize = 1024;

/// Dynamic strings referenced by [`Handle`].
#[derive(Clone, Debug)]
pub struct Heap {
    strings: HashMap<Handle, Arc<[u8]>>,
    lookup: HashMap<Arc<[u8]>, Handle>,
    /// The handle the next string gets.
    next: Handle,
    /// Mutable strings under construction, see [`ops`].
    buffers: Vec<Option<Vec<u8>>>,
    free_buffers: Vec<u64>,
    /// Entries below this handle are never collected.
    pinned: Handle,
    created: usize,
    live_after_collection: usize,
}

impl Default for Heap {
    fn default() -> Self {
        Self::new()
    }
}

impl Heap {
    pub fn new() -> Self {
        let empty: Arc<[u8]> = Arc::from(&[][..]);
        let mut lookup = HashMap::default();
        lookup.insert(empty.clone(), EMPTY_STRING);
        let mut strings = HashMap::default();
        strings.insert(EMPTY_STRING, empty);
        Self {
            strings,
            lookup,
            next: EMPTY_STRING + 1,
            buffers: Vec::new(),
            free_buffers: Vec::new(),
            pinned: 1,
            created: 0,
            live_after_collection: 1,
        }
    }

    /// The handle of the string `bytes`, creating it if needed. NUL bytes are
    /// removed first, as assigning a literal to a string does (IEEE 1800-2023
    /// 6.16).
    pub fn intern(&mut self, bytes: &[u8]) -> Handle {
        if bytes.contains(&0) {
            let stripped: Vec<u8> = bytes.iter().copied().filter(|&byte| byte != 0).collect();
            return self.intern_stripped(&stripped);
        }
        self.intern_stripped(bytes)
    }

    fn intern_stripped(&mut self, bytes: &[u8]) -> Handle {
        if let Some(&handle) = self.lookup.get(bytes) {
            return handle;
        }
        let value: Arc<[u8]> = Arc::from(bytes);
        let handle = self.next;
        self.next = handle
            .checked_add(1)
            .expect("the heap created more strings than handles can name");
        self.strings.insert(handle, value.clone());
        self.lookup.insert(value, handle);
        self.created += 1;
        handle
    }

    /// The contents of `handle`. A handle that names no string reads as `""`,
    /// so a value written into a handle slot by the host cannot crash a read.
    pub fn string(&self, handle: Handle) -> &[u8] {
        self.strings.get(&handle).map_or(&[], |value| value)
    }

    /// Whether `handle` names a live string.
    pub fn contains(&self, handle: Handle) -> bool {
        self.strings.contains_key(&handle)
    }

    /// Whether `other` holds the same strings under the same handles, the
    /// same strings under construction, and names new strings alike.
    pub fn same_contents(&self, other: &Heap) -> bool {
        self.strings == other.strings
            && self.next == other.next
            && self.buffers == other.buffers
            && self.free_buffers == other.free_buffers
            && self.pinned == other.pinned
    }

    /// The number of live strings, including `""`.
    pub fn len(&self) -> usize {
        self.strings.len()
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    /// Bytes of string contents, excluding allocator overhead.
    pub fn content_size(&self) -> usize {
        self.strings.values().map(|value| value.len()).sum()
    }

    /// Whether enough entries were created since the last collection to make
    /// another worthwhile. The interval grows with the live set, so the cost
    /// of collecting stays proportional to the allocation that triggered it.
    pub fn should_collect(&self) -> bool {
        self.created >= MIN_COLLECTION_INTERVAL.max(self.live_after_collection)
    }

    /// Free every unpinned entry that no handle in `roots` refers to. Roots
    /// that name no entry are ignored.
    pub fn collect(&mut self, roots: impl IntoIterator<Item = Handle>) {
        let marked: HashSet<Handle> = roots.into_iter().collect();
        let pinned = self.pinned;
        let lookup = &mut self.lookup;
        self.strings.retain(|handle, value| {
            let live = *handle < pinned || marked.contains(handle);
            if !live {
                lookup.remove(value);
            }
            live
        });
        // No buffer outlives the kernel that built it.
        self.buffers.clear();
        self.free_buffers.clear();
        self.created = 0;
        self.live_after_collection = self.len();
    }

    fn new_buffer(&mut self, bytes: Vec<u8>) -> u64 {
        match self.free_buffers.pop() {
            Some(buffer) => {
                self.buffers[buffer as usize] = Some(bytes);
                buffer
            }
            None => {
                self.buffers.push(Some(bytes));
                (self.buffers.len() - 1) as u64
            }
        }
    }

    fn buffer(&mut self, buffer: u64) -> &mut Vec<u8> {
        self.buffers
            .get_mut(buffer as usize)
            .and_then(Option::as_mut)
            .unwrap_or_else(|| panic!("heap buffer {buffer} is not live"))
    }

    fn take_buffer(&mut self, buffer: u64) -> Vec<u8> {
        let bytes = std::mem::take(self.buffer(buffer));
        self.buffers[buffer as usize] = None;
        self.free_buffers.push(buffer);
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interning_gives_equal_contents_equal_handles() {
        let mut heap = Heap::new();
        let a = heap.intern(b"abc");
        assert_eq!(heap.intern(b"abc"), a);
        assert_ne!(heap.intern(b"abd"), a);
        assert_eq!(heap.intern(b""), EMPTY_STRING);
        assert_eq!(heap.string(a), b"abc");
    }

    #[test]
    fn nul_bytes_are_removed() {
        let mut heap = Heap::new();
        let a = heap.intern(b"a\0b\0");
        assert_eq!(heap.string(a), b"ab");
        assert_eq!(heap.intern(b"ab"), a);
        assert_eq!(heap.intern(b"\0\0"), EMPTY_STRING);
    }

    #[test]
    fn unknown_handles_read_as_empty() {
        let heap = Heap::new();
        assert_eq!(heap.string(12345), b"");
        assert!(!heap.contains(12345));
    }

    #[test]
    fn collection_frees_unreachable_strings_without_reusing_handles() {
        let mut heap = Heap::new();
        let kept = heap.intern(b"kept");
        let dropped = heap.intern(b"dropped");
        heap.collect([kept, 999]);
        assert_eq!(heap.string(kept), b"kept");
        assert!(!heap.contains(dropped));
        assert!(heap.contains(EMPTY_STRING));
        // A later string never takes a freed handle, and the freed contents
        // are gone from the intern table.
        let again = heap.intern(b"dropped");
        assert_ne!(again, dropped);
        assert_eq!(heap.string(again), b"dropped");
        assert_eq!(heap.string(dropped), b"");
    }

    #[test]
    fn collection_is_due_after_enough_allocation() {
        let mut heap = Heap::new();
        for index in 0..MIN_COLLECTION_INTERVAL - 1 {
            heap.intern(index.to_string().as_bytes());
        }
        assert!(!heap.should_collect());
        heap.intern(b"one more");
        assert!(heap.should_collect());
        heap.collect([]);
        assert!(!heap.should_collect());
        assert_eq!(heap.len(), 1);
    }
}
