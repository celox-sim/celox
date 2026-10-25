//! Class objects (IEEE 1800-2023 clause 8).
//!
//! An object is the class it was created as and the bytes of its
//! properties, laid out by the frontend in the encoding of state memory.
//! The heap knows nothing of the layout except which properties hold
//! handles, which the code that creates an object declares right after
//! creating it, so a collection can follow them.

use crate::{Handle, HandleKind, Heap};

/// The handle of `null`, the value of zero-initialized memory.
pub const NULL: Handle = 0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Object {
    class: u32,
    random_state: u64,
    random_modes: RandomModes,
    saved_random_modes: Vec<RandomModes>,
    /// Remaining domain indices for each randc property, keyed by byte offset.
    random_cycles: fxhash::FxHashMap<u32, Vec<u16>>,
    bytes: Box<[u8]>,
    /// Byte offsets of the handle properties, with what they refer to.
    pub(crate) handles: Vec<(u32, HandleKind)>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct RandomModes {
    overrides: fxhash::FxHashMap<u32, bool>,
    /// Non-rand properties temporarily made random by an argument list.
    selected: fxhash::FxHashSet<u32>,
}

impl Object {
    fn random(&mut self) -> u64 {
        self.random_state = self.random_state.wrapping_add(0x9e3779b97f4a7c15);
        let mut word = self.random_state;
        word = (word ^ (word >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        word = (word ^ (word >> 27)).wrapping_mul(0x94d049bb133111eb);
        word ^ (word >> 31)
    }

    pub(crate) fn size(&self) -> usize {
        self.bytes.len()
    }
}

/// An error that compiled code made in a heap operation, which the runtime
/// reports once control returns to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fault {
    /// A property access or method call through `null` or a handle of no
    /// live object (IEEE 1800-2023 8.4).
    NullDereference,
    /// The first randomization subset does not implement virtual dispatch.
    RandomizationClassMismatch,
}

impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NullDereference => f.write_str("null object handle dereferenced"),
            Self::RandomizationClassMismatch => {
                f.write_str("class randomization through a base handle is unsupported")
            }
        }
    }
}

impl Heap {
    /// A new object of `class` with `size` zeroed bytes.
    pub fn new_object(&mut self, class: u32, size: usize) -> Handle {
        let handle = self.next_object;
        self.next_object = handle
            .checked_add(1)
            .expect("the heap created more objects than handles can name");
        self.objects.insert(
            handle,
            Object {
                class,
                random_state: 0,
                random_modes: RandomModes::default(),
                saved_random_modes: Vec::new(),
                random_cycles: Default::default(),
                bytes: vec![0; size].into(),
                handles: Vec::new(),
            },
        );
        self.created += 1;
        handle
    }

    /// Seed an object's independent random stream (IEEE 1800-2023 18.13.3).
    pub fn seed_object(&mut self, handle: Handle, seed: u32) {
        if let Some(object) = self.object_mut(handle) {
            object.random_state = u64::from(seed);
        }
    }

    /// SplitMix64: a full-period state transition with a bijective output
    /// permutation, including seed zero. The language does not fix the RNG.
    pub fn random_object(&mut self, handle: Handle) -> u64 {
        let Some(object) = self.object_mut(handle) else {
            return 0;
        };
        object.random()
    }

    /// Draw without replacement from an immutable domain (IEEE 1800-2023
    /// 18.4.2). Lazy Fisher-Yates uses bounded storage and uniform draws.
    /// Seeding changes the RNG, but does not discard an unfinished cycle.
    pub(crate) fn random_cyclic_object(&mut self, handle: Handle, offset: u32, count: u64) -> u64 {
        assert!((1..=256).contains(&count), "invalid randc domain size");
        let Some(object) = self.object_mut(handle) else {
            return 0;
        };
        let remaining = object.random_cycles.entry(offset).or_default();
        if remaining.is_empty() {
            remaining.extend(0..count as u16);
        }
        let count = remaining.len() as u64;
        let threshold = count.wrapping_neg() % count;
        let index = loop {
            let draw = object.random();
            if draw >= threshold {
                break (draw % count) as usize;
            }
        };
        u64::from(
            object
                .random_cycles
                .get_mut(&offset)
                .unwrap()
                .swap_remove(index),
        )
    }

    /// Random properties start enabled. Modes are shared by aliases of an
    /// object and are part of its checkpoint and differential state.
    pub fn random_mode(&mut self, handle: Handle, offset: u32, random: bool) -> bool {
        self.object_mut(handle).is_some_and(|object| {
            object
                .random_modes
                .overrides
                .get(&offset)
                .copied()
                .unwrap_or(random)
        })
    }

    pub fn set_random_mode(&mut self, handle: Handle, offset: u32, enabled: bool) {
        if let Some(object) = self.object_mut(handle) {
            object.random_modes.overrides.insert(offset, enabled);
        }
    }

    pub fn push_random_modes(&mut self, handle: Handle) {
        if let Some(object) = self.object_mut(handle) {
            object.saved_random_modes.push(object.random_modes.clone());
        }
    }

    pub fn pop_random_modes(&mut self, handle: Handle) {
        if let Some(object) = self.object_mut(handle) {
            object.random_modes = object
                .saved_random_modes
                .pop()
                .expect("paired random mode scope");
        }
    }

    pub fn select_random_mode(&mut self, handle: Handle, offset: u32, random: bool, enabled: bool) {
        if let Some(object) = self.object_mut(handle) {
            object.random_modes.overrides.insert(offset, enabled);
            if enabled && !random {
                object.random_modes.selected.insert(offset);
            } else {
                object.random_modes.selected.remove(&offset);
            }
        }
    }

    pub fn selected_random_mode(&mut self, handle: Handle, offset: u32) -> bool {
        self.object_mut(handle)
            .is_some_and(|object| object.random_modes.selected.contains(&offset))
    }

    pub(crate) fn check_object(&mut self, handle: Handle, class: u32) -> bool {
        let Some(object) = self.object_mut(handle) else {
            return false;
        };
        if class != 0 && object.class != class {
            self.fault.get_or_insert(Fault::RandomizationClassMismatch);
            return false;
        }
        true
    }

    /// A new object of the class of `object` with a copy of its properties:
    /// the handles it holds are copied, not the objects they refer to (IEEE
    /// 1800-2023 8.12). Copying `null` is a fault and gives `null`.
    pub fn copy_object(&mut self, object: Handle) -> Handle {
        let Some(copy) = self.object_mut(object).map(|object| object.clone()) else {
            return NULL;
        };
        let handle = self.new_object(copy.class, 0);
        self.objects.insert(handle, copy);
        handle
    }

    /// Declare that the property of `object` at byte `offset` holds a handle
    /// of `kind`.
    pub fn declare_handle(&mut self, object: Handle, offset: u32, kind: HandleKind) {
        if let Some(object) = self.object_mut(object) {
            object.handles.push((offset, kind));
        }
    }

    /// The class `object` was created as, or 0 for `null`.
    pub fn class_of(&self, object: Handle) -> u32 {
        self.objects.get(&object).map_or(0, |object| object.class)
    }

    /// `count` bytes of `object` at `offset`, little-endian. `count` is at
    /// most 8.
    pub fn load(&mut self, object: Handle, offset: usize, count: usize) -> u64 {
        let Some(bytes) = self.object_mut(object).map(|object| &object.bytes) else {
            return 0;
        };
        let mut word = [0; 8];
        let count = count.min(8);
        if let Some(source) = bytes.get(offset..offset + count) {
            word[..count].copy_from_slice(source);
        }
        u64::from_le_bytes(word)
    }

    /// Write the low `count` bytes of `value` into `object` at `offset`.
    pub fn store(&mut self, object: Handle, offset: usize, count: usize, value: u64) {
        let Some(object) = self.object_mut(object) else {
            return;
        };
        let count = count.min(8);
        if let Some(target) = object.bytes.get_mut(offset..offset + count) {
            target.copy_from_slice(&value.to_le_bytes()[..count]);
        }
    }

    /// The object behind `handle`, recording a fault when there is none.
    fn object_mut(&mut self, handle: Handle) -> Option<&mut Object> {
        let object = self.objects.get_mut(&handle);
        if object.is_none() {
            self.fault.get_or_insert(Fault::NullDereference);
        }
        object
    }

    /// The first fault since the last call, if any.
    pub fn take_fault(&mut self) -> Option<Fault> {
        self.fault.take()
    }

    /// Whether `handle` names a live object.
    pub fn contains_object(&self, handle: Handle) -> bool {
        self.objects.contains_key(&handle)
    }

    /// The handles stored in the handle properties of `object`.
    pub(crate) fn handles_of(&self, object: Handle) -> Vec<(HandleKind, Handle)> {
        let Some(object) = self.objects.get(&object) else {
            return Vec::new();
        };
        object
            .handles
            .iter()
            .filter_map(|&(offset, kind)| {
                let bytes = object
                    .bytes
                    .get(offset as usize..offset as usize + crate::HANDLE_BYTES)?;
                Some((kind, Handle::from_le_bytes(bytes.try_into().ok()?)))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Root;

    #[test]
    fn properties_read_back_what_was_written() {
        let mut heap = Heap::new();
        let object = heap.new_object(7, 16);
        assert_eq!(heap.class_of(object), 7);
        heap.store(object, 4, 4, 0x1234_5678_9abc);
        assert_eq!(heap.load(object, 4, 4), 0x5678_9abc);
        assert_eq!(heap.load(object, 0, 8), 0x5678_9abc_0000_0000);
        assert_eq!(heap.take_fault(), None);
    }

    #[test]
    fn null_dereferences_are_faults_that_read_zero() {
        let mut heap = Heap::new();
        assert_eq!(heap.load(NULL, 0, 4), 0);
        heap.store(NULL, 0, 4, 1);
        assert_eq!(heap.take_fault(), Some(Fault::NullDereference));
        assert_eq!(heap.take_fault(), None);
        assert_eq!(heap.class_of(NULL), 0);
    }

    #[test]
    fn copies_are_shallow() {
        let mut heap = Heap::new();
        let inner = heap.new_object(2, 4);
        let original = heap.new_object(1, 8);
        heap.declare_handle(original, 0, HandleKind::Object);
        heap.store(original, 0, 4, inner.into());
        heap.store(original, 4, 4, 9);
        let copy = heap.copy_object(original);
        assert_ne!(copy, original);
        assert_eq!(heap.class_of(copy), 1);
        assert_eq!(heap.load(copy, 0, 4), u64::from(inner));
        heap.store(copy, 4, 4, 5);
        assert_eq!(heap.load(original, 4, 4), 9);
        // The copy keeps what its handle properties refer to alive.
        heap.collect([Root::object(copy)]);
        assert!(heap.contains_object(inner));
        assert!(!heap.contains_object(original));
        assert_eq!(heap.copy_object(NULL), NULL);
        assert_eq!(heap.take_fault(), Some(Fault::NullDereference));
    }

    #[test]
    fn collection_follows_handle_properties() {
        let mut heap = Heap::new();
        let outer = heap.new_object(1, 8);
        let inner = heap.new_object(2, 4);
        let text = heap.intern(b"kept through an object");
        let lost = heap.new_object(3, 4);
        heap.declare_handle(outer, 0, HandleKind::Object);
        heap.declare_handle(outer, 4, HandleKind::String);
        heap.store(outer, 0, 4, inner.into());
        heap.store(outer, 4, 4, text.into());
        heap.collect([Root::object(outer)]);
        assert!(heap.contains_object(outer));
        assert!(heap.contains_object(inner));
        assert!(!heap.contains_object(lost));
        assert_eq!(heap.string(text), b"kept through an object");
        heap.collect([]);
        assert!(!heap.contains_object(outer));
        assert!(!heap.contains(text));
    }
}

#[cfg(test)]
mod random_tests {
    use super::*;

    #[test]
    fn cyclic_domains_survive_reseeding_and_checkpoint_restore() {
        for count in [1, 3, 256] {
            let mut heap = Heap::new();
            let object = heap.new_object(1, 2);
            let other = heap.new_object(1, 2);
            heap.seed_object(object, 12);
            heap.seed_object(other, 12);
            let first = heap.random_cyclic_object(object, 0, count);
            assert_eq!(first, heap.random_cyclic_object(other, 0, count));
            let mut saved = heap.clone();
            let mut seen = crate::HashSet::from_iter([first]);
            for _ in 1..count {
                // Seeding does not make consumed values eligible again.
                heap.seed_object(object, 19);
                saved.seed_object(object, 19);
                let value = heap.random_cyclic_object(object, 0, count);
                assert_eq!(value, saved.random_cyclic_object(object, 0, count));
                assert!(seen.insert(value));
            }
            for offset in [0, 1, 0] {
                let seen: crate::HashSet<_> = (0..count)
                    .map(|_| heap.random_cyclic_object(object, offset, count))
                    .collect();
                assert_eq!(seen.len(), count as usize);
                assert!(seen.iter().all(|&value| value < count));
            }
            assert_eq!(heap.take_fault(), None);
        }
        let mut heap = Heap::new();
        assert_eq!(heap.random_cyclic_object(NULL, 0, 3), 0);
        assert_eq!(heap.take_fault(), Some(Fault::NullDereference));
    }

    #[test]
    fn random_streams_are_independent_and_checkpointed() {
        let mut heap = Heap::new();
        let a = heap.new_object(1, 1);
        let b = heap.new_object(1, 1);
        heap.seed_object(a, 0);
        heap.seed_object(b, 0);
        let first = heap.random_object(a);
        for _ in 0..10 {
            heap.random_object(a);
        }
        assert_eq!(first, heap.random_object(b));
        heap.seed_object(a, 0);
        assert_eq!(first, heap.random_object(a));
        let mut snapshot = heap.clone();
        for _ in 0..20 {
            assert_eq!(heap.random_object(a), snapshot.random_object(a));
        }
        assert_eq!(heap.take_fault(), None);
        heap.random_object(NULL);
        assert_eq!(heap.take_fault(), Some(Fault::NullDereference));
    }
}
