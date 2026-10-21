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

#[derive(Clone, Debug)]
pub(crate) struct Object {
    class: u32,
    bytes: Box<[u8]>,
    /// Byte offsets of the handle properties, with what they refer to.
    pub(crate) handles: Vec<(u32, HandleKind)>,
}

impl Object {
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
}

impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NullDereference => f.write_str("null object handle dereferenced"),
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
                bytes: vec![0; size].into(),
                handles: Vec::new(),
            },
        );
        self.created += 1;
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
