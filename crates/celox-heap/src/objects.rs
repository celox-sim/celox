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

/// The class identifiers the design's classes never take: a class's
/// identifier is a hash below this one (see [`class_identifier`]), and each
/// reserved class is one of those from it up.
pub const FIRST_RESERVED_CLASS: u32 = u32::MAX - 15;

/// The class of a record holding an unpacked structure as a container
/// element.
pub const RECORD_CLASS: u32 = u32::MAX;

/// The class of the heap objects the scheduler owns, semaphores and
/// mailboxes.
pub const SCHEDULER_CLASS: u32 = u32::MAX - 1;

/// The class of a box: an object no class declares, which holds an
/// automatic variable that a fork shares between the frames of its parent
/// and children. Its bytes are the variable's value, and state files save
/// a box by its value, a string by its contents (see [`Heap::box_contents`]).
pub const BOX_CLASS: u32 = u32::MAX - 2;

const _: () = {
    let reserved = [RECORD_CLASS, SCHEDULER_CLASS, BOX_CLASS];
    let mut i = 0;
    while i < reserved.len() {
        assert!(
            reserved[i] >= FIRST_RESERVED_CLASS,
            "a reserved class in range"
        );
        let mut j = i + 1;
        while j < reserved.len() {
            assert!(reserved[i] != reserved[j], "distinct reserved classes");
            j += 1;
        }
        i += 1;
    }
};

/// The identifier of a class whose name hashes to `hash`: never 0, which
/// `null` reads as, and never a reserved class's.
pub const fn class_identifier(hash: u32) -> u32 {
    if hash == 0 {
        1
    } else if hash >= FIRST_RESERVED_CLASS {
        hash - FIRST_RESERVED_CLASS + 1
    } else {
        hash
    }
}

/// The beginning of the state an object's `get_randstate` returns.
const RANDSTATE_PREFIX: &str = "celox-object-rng:";

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Object {
    class: u32,
    random_state: u64,
    random_modes: RandomModes,
    constraint_default: bool,
    constraint_modes: fxhash::FxHashMap<u32, bool>,
    saved_random_modes: Vec<RandomModes>,
    /// Remaining domain indices for each randc property, keyed by byte offset.
    random_cycles: fxhash::FxHashMap<u32, Vec<u16>>,
    bytes: Box<[u8]>,
    /// Byte offsets of the handle properties, with what they refer to.
    pub(crate) handles: Vec<(u32, HandleKind)>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Fault {
    ConstraintRandomization(String),
    NegativeArraySize,
    ContainerAllocationLimit,
    /// A property access or method call through `null` or a handle of no
    /// live object (IEEE 1800-2023 8.4).
    NullDereference,
    /// The first randomization subset does not implement virtual dispatch.
    RandomizationClassMismatch,
    /// A streaming concatenation whose sizes do not fit (IEEE 1800-2023
    /// 11.4.14), such as a stream wider than its fixed-size target.
    Stream(String),
    /// A dynamically sized array assigned to a fixed-size array of another
    /// number of elements (IEEE 1800-2023 7.6).
    ArraySize,
    /// `set_randstate` with a string that `get_randstate` did not return
    /// (IEEE 1800-2023 18.13.5).
    InvalidRandomState,
}

impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConstraintRandomization(reason) => {
                write!(f, "constraint randomization failed: {reason}")
            }
            Self::NegativeArraySize => f.write_str("negative dynamic array size"),
            Self::ContainerAllocationLimit => {
                f.write_str("dynamic container allocation limit exceeded")
            }
            Self::NullDereference => f.write_str("null object handle dereferenced"),
            Self::Stream(reason) => write!(f, "streaming concatenation: {reason}"),
            Self::ArraySize => f.write_str(
                "a dynamically sized array and the fixed-size array it is assigned to differ in \
                 size",
            ),
            Self::RandomizationClassMismatch => {
                f.write_str("class randomization through a base handle is unsupported")
            }
            Self::InvalidRandomState => {
                f.write_str("set_randstate with a state that get_randstate did not return")
            }
        }
    }
}

/// The contents of a structure record: its bytes and handle fields.
pub(crate) type RecordContents = (Box<[u8]>, Vec<(u32, HandleKind)>);

impl Heap {
    /// The handle of the record with the contents of the finished `record`,
    /// so that equal values of an unpacked structure have one handle, as
    /// equal strings and containers do: `null` for the default value, whose
    /// bytes are all zero (a `null` record reads as its members' defaults).
    /// A record is never written after it is finished.
    pub fn finish_record(&mut self, record: Handle) -> Handle {
        let Some(object) = self.objects.get(&record) else {
            return record;
        };
        if object.bytes.iter().all(|&byte| byte == 0) {
            self.objects.remove(&record);
            return NULL;
        }
        let contents = (object.bytes.clone(), object.handles.clone());
        match self.record_lookup.get(&contents) {
            Some(&canonical) if canonical != record && self.objects.contains_key(&canonical) => {
                self.objects.remove(&record);
                canonical
            }
            _ => {
                self.record_lookup.insert(contents, record);
                record
            }
        }
    }

    /// A new named event: a handle no object or container ever gets.
    pub fn new_event(&mut self) -> Handle {
        self.events = true;
        let handle = self.next_object;
        self.next_object = handle
            .checked_add(1)
            .expect("the heap created more objects than handles can name");
        handle
    }

    /// The handle the next object gets, once a named event got one: a state
    /// file saves the handles of events as values.
    pub fn event_watermark(&self) -> Option<Handle> {
        self.events.then_some(self.next_object)
    }

    /// Give later objects handles from `next` on at least, so they differ
    /// from the events of a loaded state file.
    pub fn reserve_objects(&mut self, next: Handle) {
        self.events = true;
        self.next_object = self.next_object.max(next);
    }

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
                constraint_default: true,
                constraint_modes: Default::default(),
                saved_random_modes: Vec::new(),
                random_cycles: Default::default(),
                bytes: vec![0; size].into(),
                handles: Vec::new(),
            },
        );
        self.created += 1;
        handle
    }

    /// Persistent per-object constraint modes (IEEE 1800-2023 18.9).
    pub fn constraint_mode(&mut self, handle: Handle, index: u32) -> bool {
        self.object_mut(handle).is_some_and(|object| {
            object
                .constraint_modes
                .get(&index)
                .copied()
                .unwrap_or(object.constraint_default)
        })
    }

    pub fn set_constraint_mode(&mut self, handle: Handle, index: Option<u32>, enabled: bool) {
        if let Some(object) = self.object_mut(handle) {
            if let Some(index) = index {
                object.constraint_modes.insert(index, enabled);
            } else {
                object.constraint_modes.clear();
                object.constraint_default = enabled;
            }
        }
    }

    /// Seed an object's independent random stream (IEEE 1800-2023 18.13.3).
    pub fn seed_object(&mut self, handle: Handle, seed: u32) {
        if let Some(object) = self.object_mut(handle) {
            object.random_state = u64::from(seed);
        }
    }

    /// The state of an object's random stream as `get_randstate` returns it
    /// (IEEE 1800-2023 18.13.4): a string that `set_randstate` restores.
    pub fn object_randstate(&mut self, handle: Handle) -> Handle {
        let Some(state) = self.object_mut(handle).map(|object| object.random_state) else {
            return self.intern(b"");
        };
        self.intern(format!("{RANDSTATE_PREFIX}{state:016x}").as_bytes())
    }

    /// Restore an object's random stream from `state`, a string that
    /// `get_randstate` returned (IEEE 1800-2023 18.13.5), so that its
    /// randomizations repeat; another string is a fault.
    pub fn set_object_randstate(&mut self, handle: Handle, state: Handle) {
        let parsed = std::str::from_utf8(self.string(state))
            .ok()
            .and_then(|text| text.strip_prefix(RANDSTATE_PREFIX))
            .filter(|digits| digits.len() == 16)
            .and_then(|digits| u64::from_str_radix(digits, 16).ok());
        let Some(state) = parsed else {
            self.fault.get_or_insert(Fault::InvalidRandomState);
            return;
        };
        if let Some(object) = self.object_mut(handle) {
            object.random_state = state;
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
        let left = cycle(object, offset, count, false).len() as u64;
        let threshold = left.wrapping_neg() % left;
        let index = loop {
            let draw = object.random();
            if draw >= threshold {
                break (draw % left) as usize;
            }
        };
        u64::from(cycle(object, offset, count, false).swap_remove(index))
    }

    /// The entries the current cycle of the randc property at `offset`, of
    /// `count` entries, has not produced; `restart` starts a new cycle.
    pub(crate) fn cycle_remaining(
        &mut self,
        handle: Handle,
        offset: u32,
        count: u64,
        restart: bool,
    ) -> Vec<u64> {
        self.object_mut(handle).map_or_else(Vec::new, |object| {
            cycle(object, offset, count, restart)
                .iter()
                .map(|entry| u64::from(*entry))
                .collect()
        })
    }

    /// Record that the cycle of the randc property at `offset`, of `count`
    /// entries, produced `entry`.
    pub(crate) fn take_cycle_entry(&mut self, handle: Handle, offset: u32, count: u64, entry: u64) {
        if let Some(object) = self.object_mut(handle) {
            let remaining = cycle(object, offset, count, false);
            if let Some(index) = remaining.iter().position(|left| u64::from(*left) == entry) {
                remaining.swap_remove(index);
            }
        }
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

    /// The bytes of box `object` (see [`BOX_CLASS`]), and what its value
    /// refers to when it is a handle; `None` for any other object.
    pub fn box_contents(&self, object: Handle) -> Option<(&[u8], Option<HandleKind>)> {
        let object = self
            .objects
            .get(&object)
            .filter(|object| object.class == BOX_CLASS)?;
        let kind = match object.handles.as_slice() {
            [] => None,
            [(0, kind)] => Some(*kind),
            _ => return None,
        };
        Some((&object.bytes, kind))
    }

    /// A new box (see [`BOX_CLASS`]) holding `bytes`, a handle of `kind`
    /// if any.
    pub fn new_box(&mut self, bytes: &[u8], kind: Option<HandleKind>) -> Handle {
        let handle = self.new_object(BOX_CLASS, bytes.len());
        if let Some(object) = self.object_mut(handle) {
            object.bytes.copy_from_slice(bytes);
        }
        if let Some(kind) = kind {
            self.declare_handle(handle, 0, kind);
        }
        handle
    }

    /// The bytes of the properties of `object`, recording a fault when
    /// there is none.
    pub(crate) fn object_bytes_mut(&mut self, object: Handle) -> Option<&mut [u8]> {
        self.object_mut(object).map(|object| &mut object.bytes[..])
    }

    /// The bytes of the properties of `object`, if it is live.
    pub(crate) fn object_bytes(&self, object: Handle) -> Option<&[u8]> {
        self.objects.get(&object).map(|object| &object.bytes[..])
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
        self.write(object, offset, &value.to_le_bytes()[..count.min(8)]);
    }

    /// Write the bytes of `buffer`, which holds them most significant first,
    /// into `object` at `offset`, as one write (see
    /// [`crate::ops::OBJECT_STORE_BYTES`]); the buffer is consumed.
    pub fn store_bytes(&mut self, object: Handle, offset: usize, buffer: u64) {
        let mut bytes = self.take_buffer(buffer);
        bytes.reverse();
        self.write(object, offset, &bytes);
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

/// The cycle of the randc property at `offset` of `object`: the entries,
/// `0..count`, that it has not produced, a new cycle once it has produced
/// them all or when `restart` asks. Both the unconstrained draw and the
/// constraint solver keep a property's cycle here, as the indices of the
/// members of an enumeration and as the values of an integral property
/// (IEEE 1800-2023 18.4.2).
fn cycle(object: &mut Object, offset: u32, count: u64, restart: bool) -> &mut Vec<u16> {
    let remaining = object.random_cycles.entry(offset).or_default();
    if restart || remaining.is_empty() {
        remaining.clear();
        remaining.extend(0..count as u16);
    }
    remaining
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
    fn equal_records_share_one_handle_until_collected() {
        let mut heap = Heap::new();
        let record = |heap: &mut Heap, value: u64| {
            let record = heap.new_object(u32::MAX, 8);
            heap.declare_handle(record, 0, HandleKind::String);
            heap.store(record, 4, 4, value);
            heap.finish_record(record)
        };
        let first = record(&mut heap, 1);
        let second = record(&mut heap, 1);
        let other = record(&mut heap, 2);
        assert_eq!(first, second);
        assert_ne!(first, other);
        assert_eq!(heap.objects.len(), 2);
        assert_eq!(record(&mut heap, 0), NULL);
        assert_eq!(heap.objects.len(), 2);
        heap.collect([Root::object(other)]);
        assert_eq!(heap.record_lookup.len(), 1);
        assert_ne!(record(&mut heap, 1), first);
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
    fn constraint_modes_are_object_state_and_do_not_follow_random_mode_scopes() {
        let mut heap = Heap::new();
        let handle = heap.new_object(1, 4);
        assert!(heap.constraint_mode(handle, 0));
        let original = heap.clone();
        heap.set_constraint_mode(handle, None, false);
        heap.set_constraint_mode(handle, Some(1), true);
        assert!(!heap.same_contents(&original));
        heap.push_random_modes(handle);
        heap.set_constraint_mode(handle, Some(0), true);
        heap.pop_random_modes(handle);
        assert!(heap.constraint_mode(handle, 0));
        assert!(heap.constraint_mode(handle, 1));
        assert!(!heap.constraint_mode(handle, 2));
        let bytes = serde_json::to_vec(&heap.objects[&handle]).unwrap();
        let restored: Object = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(restored, heap.objects[&handle]);
        let copy = heap.copy_object(handle);
        heap.set_constraint_mode(copy, None, true);
        assert!(heap.constraint_mode(copy, 2));
        assert!(!heap.constraint_mode(handle, 2));
        assert!(!heap.constraint_mode(NULL, 0));
        assert_eq!(heap.take_fault(), Some(Fault::NullDereference));
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

#[cfg(test)]
mod reserved_classes {
    use super::*;

    #[test]
    fn no_class_identifier_is_reserved() {
        for hash in [0, 1, FIRST_RESERVED_CLASS - 1]
            .into_iter()
            .chain(FIRST_RESERVED_CLASS..=u32::MAX)
        {
            let id = class_identifier(hash);
            assert!(id != 0 && id < FIRST_RESERVED_CLASS, "hash {hash:#x}");
        }
    }
}
