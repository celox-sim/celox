//! Value containers. Published entries are immutable; updates allocate a new
//! handle so ordinary state stores detect changes and array assignment copies
//! values. Class handles inside elements retain reference semantics.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use crate::{Fault, Handle, Heap, Root};

pub const NEW: u64 = 80;
pub const LEN: u64 = 81;
pub const GET: u64 = 82;
pub const GET_MASK: u64 = 83;
pub const SET: u64 = 84;
/// Complete a newly created, unpublished SET/INSERT result with its X/Z mask.
pub const SET_MASK: u64 = 85;
pub const INSERT: u64 = 86;
pub const DELETE: u64 = 87;
pub const CLEAR: u64 = 88;
pub const EXISTS: u64 = 89;
pub const FIRST: u64 = 90;
pub const LAST: u64 = 91;
pub const NEXT: u64 = 92;
pub const PREV: u64 = 93;
pub const ITER_VALID: u64 = 94;
pub const EQUAL: u64 = 95;
pub const CASE_EQUAL: u64 = 96;
/// Reverse sequence a, preserving element values and X/Z masks.
pub const REVERSE: u64 = 97;
/// Sort sequence a ascending; b selects signed integral comparison.
pub const SORT: u64 = 98;
/// Sort sequence a descending; b selects signed integral comparison.
pub const RSORT: u64 = 99;

/// The operation parameter encodes the element width (bits 0..6), whether
/// it is two-state (7), its handle kind (8..9: none/string/object), container
/// kind (10..11: dynamic/queue/associative), queue capacity (12..43, 0 means
/// unbounded), and associative key ordering (44 signed, 45 string).
pub const fn parameter(
    width: u32,
    two_state: bool,
    handle: u64,
    kind: u64,
    capacity: u32,
    signed_key: bool,
    string_key: bool,
) -> u64 {
    width as u64
        | (two_state as u64) << 7
        | handle << 8
        | kind << 10
        | (capacity as u64) << 12
        | (signed_key as u64) << 44
        | (string_key as u64) << 45
}

#[derive(Clone, Copy)]
struct Layout(u64);
impl Layout {
    fn bits(self) -> u64 {
        u64::MAX >> (64 - (self.0 & 127))
    }
    fn default(self) -> Element {
        Element {
            value: if self.0 & 128 == 0 { self.bits() } else { 0 },
            mask: if self.0 & 128 == 0 { self.bits() } else { 0 },
        }
    }
    fn kind(self) -> u64 {
        (self.0 >> 10) & 3
    }
    fn capacity(self) -> usize {
        let n = ((self.0 >> 12) & 0xffff_ffff) as usize;
        if n == 0 { usize::MAX } else { n }
    }
    fn key(self, heap: &Heap, value: u64) -> Key {
        if self.0 & (1 << 45) != 0 {
            Key::String(Arc::from(heap.string(value as Handle)))
        } else {
            Key::Integer(value ^ if self.0 & (1 << 44) != 0 { 1 << 63 } else { 0 })
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
struct Element {
    value: u64,
    mask: u64,
}
#[derive(
    Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
enum Key {
    Integer(u64),
    String(Arc<[u8]>),
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
enum Contents {
    Sequence(VecDeque<Element>),
    Associative(BTreeMap<Key, Element>),
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub(crate) struct Container {
    layout: u64,
    contents: Contents,
}
impl Container {
    fn empty(layout: Layout) -> Self {
        Self {
            layout: layout.0,
            contents: if layout.kind() == 2 {
                Contents::Associative(BTreeMap::new())
            } else {
                Contents::Sequence(VecDeque::new())
            },
        }
    }
    fn len(&self) -> usize {
        match &self.contents {
            Contents::Sequence(v) => v.len(),
            Contents::Associative(v) => v.len(),
        }
    }
    pub(crate) fn size(&self) -> usize {
        self.len() * std::mem::size_of::<Element>()
            + match &self.contents {
                Contents::Associative(v) => v
                    .keys()
                    .map(|k| match k {
                        Key::String(s) => s.len(),
                        _ => 8,
                    })
                    .sum(),
                _ => 0,
            }
    }
}

impl Heap {
    fn publish_container(&mut self, container: Container, complete: bool) -> Handle {
        if container.len() == 0 {
            return 0;
        }
        let handle = self.next_object;
        self.next_object = handle
            .checked_add(1)
            .expect("heap container handle space exhausted");
        self.containers.insert(handle, Arc::new(container));
        self.created += 1;
        if complete {
            self.finish_container(handle)
        } else {
            handle
        }
    }
    fn finish_container(&mut self, handle: Handle) -> Handle {
        let Some(container) = self.containers.get(&handle) else {
            return handle;
        };
        if let Some(&canonical) = self.container_lookup.get(container) {
            if canonical != handle {
                self.containers.remove(&handle);
            }
            canonical
        } else {
            self.container_lookup.insert(container.clone(), handle);
            handle
        }
    }
    pub(crate) fn container_roots(&self, handle: Handle) -> Vec<Root> {
        let Some(container) = self.containers.get(&handle) else {
            return Vec::new();
        };
        let kind = match (container.layout >> 8) & 3 {
            1 => crate::HandleKind::String,
            2 => crate::HandleKind::Object,
            _ => return Vec::new(),
        };
        let root = |element: &Element| Root {
            kind,
            handle: element.value as Handle,
        };
        match &container.contents {
            Contents::Sequence(v) => v.iter().map(root).collect(),
            Contents::Associative(v) => v.values().map(root).collect(),
        }
    }
    /// Execute a container helper. Indices have already undergone language
    /// conversion; invalid four-state indices use an out-of-range sentinel.
    pub(crate) fn container_call(
        &mut self,
        op: u64,
        parameter: u64,
        a: u64,
        b: u64,
        c: u64,
    ) -> u64 {
        let layout = Layout(parameter);
        let key = layout.key(self, b);
        let existing = self.containers.get(&(a as Handle));
        let get = || {
            existing
                .and_then(|entry| match &entry.contents {
                    Contents::Sequence(v) => usize::try_from(b).ok().and_then(|i| v.get(i)),
                    Contents::Associative(v) => v.get(&key),
                })
                .copied()
                .unwrap_or_else(|| layout.default())
        };
        match op {
            EQUAL | CASE_EQUAL => {
                let other = self.containers.get(&(b as Handle));
                let equal_element = |left: Element, right: Element| {
                    if op == CASE_EQUAL {
                        if left == right { 1 } else { 0 }
                    } else if (left.value ^ right.value) & !(left.mask | right.mask) != 0 {
                        0
                    } else if left.mask | right.mask != 0 {
                        2
                    } else {
                        1
                    }
                };
                let mut unknown = false;
                let mut compare = |left: Element, right: Element| {
                    let equal = equal_element(left, right);
                    unknown |= equal == 2;
                    equal != 0
                };
                let left = existing
                    .map(|entry| (**entry).clone())
                    .unwrap_or_else(|| Container::empty(layout));
                let right = other
                    .map(|entry| (**entry).clone())
                    .unwrap_or_else(|| Container::empty(layout));
                let equal = match (&left.contents, &right.contents) {
                    (Contents::Sequence(a), Contents::Sequence(b)) => {
                        a.len() == b.len() && a.iter().zip(b).all(|(&a, &b)| compare(a, b))
                    }
                    (Contents::Associative(a), Contents::Associative(b)) => {
                        a.len() == b.len()
                            && a.iter()
                                .zip(b)
                                .all(|((ak, &a), (bk, &b))| ak == bk && compare(a, b))
                    }
                    _ => false,
                };
                return if !equal {
                    0
                } else if unknown {
                    3
                } else {
                    1
                };
            }
            LEN => return existing.map_or(0, |v| v.len() as u64),
            GET => {
                return if c == 0 {
                    get().value
                } else {
                    layout.default().value
                };
            }
            GET_MASK => {
                return if c == 0 {
                    get().mask
                } else {
                    layout.default().mask
                };
            }
            EXISTS => {
                return existing.is_some_and(|entry| match &entry.contents {
                    Contents::Associative(v) => v.contains_key(&key),
                    _ => false,
                }) as u64;
            }
            CLEAR => return 0,
            FIRST | LAST | NEXT | PREV | ITER_VALID => {
                let traversal = if op == ITER_VALID { c } else { op };
                let found = existing.and_then(|entry| {
                    let Contents::Associative(v) = &entry.contents else {
                        return None;
                    };
                    match traversal {
                        FIRST => v.first_key_value(),
                        LAST => v.last_key_value(),
                        NEXT => v
                            .range((
                                std::ops::Bound::Excluded(key.clone()),
                                std::ops::Bound::Unbounded,
                            ))
                            .next(),
                        _ => v
                            .range((
                                std::ops::Bound::Unbounded,
                                std::ops::Bound::Excluded(key.clone()),
                            ))
                            .next_back(),
                    }
                    .map(|(k, _)| k.clone())
                });
                if op == ITER_VALID {
                    return found.is_some() as u64;
                }
                return match found {
                    Some(Key::Integer(k)) => {
                        k ^ if layout.0 & (1 << 44) != 0 {
                            1 << 63
                        } else {
                            0
                        }
                    }
                    Some(Key::String(k)) => self.intern(&k) as u64,
                    None => 0,
                };
            }
            SET_MASK => {
                if let Some(entry) = self.containers.get_mut(&(a as Handle)) {
                    let entry = Arc::make_mut(entry);
                    let element = match &mut entry.contents {
                        Contents::Sequence(v) => v.get_mut(b as usize),
                        Contents::Associative(v) => v.get_mut(&key),
                    };
                    if let Some(element) = element {
                        element.mask = c & layout.bits();
                    }
                }
                return self.finish_container(a as Handle) as u64;
            }
            NEW => {
                // Bound allocation before converting to usize or reserving.
                if (a as i64) < 0 {
                    self.fault.get_or_insert(Fault::NegativeArraySize);
                    return 0;
                }
                if a > 1 << 24 {
                    self.fault.get_or_insert(Fault::ContainerAllocationLimit);
                    return 0;
                }
                let mut result = Container::empty(layout);
                let Contents::Sequence(v) = &mut result.contents else {
                    return 0;
                };
                v.resize((a as usize).min(layout.capacity()), layout.default());
                if let Some(Container {
                    contents: Contents::Sequence(source),
                    ..
                }) = self.containers.get(&(b as Handle)).map(AsRef::as_ref)
                {
                    for (dst, src) in v.iter_mut().zip(source) {
                        *dst = *src;
                    }
                }
                return self.publish_container(result, true) as u64;
            }
            _ => {}
        }
        let mut result = existing
            .map(|entry| (**entry).clone())
            .unwrap_or_else(|| Container::empty(layout));
        let value = Element {
            value: c & layout.bits(),
            mask: 0,
        };
        match &mut result.contents {
            Contents::Sequence(v) => {
                let Ok(index) = usize::try_from(b) else {
                    return a;
                };
                match op {
                    REVERSE => v.make_contiguous().reverse(),
                    SORT | RSORT => {
                        let strings = (layout.0 >> 8) & 3 == 1;
                        let sign = if b != 0 && !strings {
                            1 << ((layout.0 & 127) - 1)
                        } else {
                            0
                        };
                        let compare = |left: &Element, right: &Element| {
                            let order = if strings {
                                self.string(left.value as Handle)
                                    .cmp(self.string(right.value as Handle))
                            } else {
                                (left.value ^ sign).cmp(&(right.value ^ sign))
                            };
                            if op == RSORT { order.reverse() } else { order }
                        };
                        let values = v.make_contiguous();
                        // X/Z comparisons yield X (11.4.4). Preserve those
                        // elements and sort the fully known runs between them;
                        // Rust's sort requires a total comparison within a run.
                        for known in values.split_mut(|value| value.mask != 0) {
                            known.sort_by(&compare);
                        }
                    }
                    SET if index < v.len() => v[index] = value,
                    SET if layout.kind() == 1 && index == v.len() && index < layout.capacity() => {
                        v.push_back(value)
                    }
                    INSERT if index <= v.len() && index < layout.capacity() => {
                        v.insert(index, value);
                        if v.len() > layout.capacity() {
                            v.pop_back();
                        }
                    }
                    DELETE if index < v.len() => {
                        v.remove(index);
                    }
                    _ => return a,
                }
            }
            Contents::Associative(v) => match op {
                SET => {
                    v.insert(key, value);
                }
                DELETE => {
                    if v.remove(&key).is_none() {
                        return a;
                    }
                }
                _ => return a,
            },
        }
        self.publish_container(result, !matches!(op, SET | INSERT)) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::encode;
    fn call(heap: &mut Heap, op: u64, p: u64, a: u64, b: u64, c: u64) -> u64 {
        heap.call(encode(op, p), a, b, c)
    }
    #[test]
    fn completed_updates_intern_values_and_collection_releases_lookup_entries() {
        let mut heap = Heap::new();
        let p = parameter(8, true, 0, 0, 0, false, false);
        let a = call(&mut heap, NEW, p, 2, 0, 0);
        let b = call(&mut heap, SET, p, a, 0, 7);
        let b = call(&mut heap, SET_MASK, p, b, 0, 0);
        let same = call(&mut heap, SET, p, b, 0, 7);
        assert_eq!(call(&mut heap, SET_MASK, p, same, 0, 0), b);
        assert_eq!(call(&mut heap, NEW, p, 2, b, 0), b);
        heap.collect([Root::object(b as Handle)]);
        assert_eq!(heap.container_lookup.len(), 1);
        let old = b;
        heap.collect([]);
        assert!(heap.container_lookup.is_empty());
        let new = call(&mut heap, NEW, p, 2, 0, 0);
        assert_ne!(old, new);
    }

    #[test]
    fn resize_copy_delete_and_four_state_defaults() {
        let mut heap = Heap::new();
        let p = parameter(32, false, 0, 0, 0, false, false);
        let a = call(&mut heap, NEW, p, 2, 0, 0);
        assert_eq!(call(&mut heap, GET_MASK, p, a, 0, 0), u32::MAX as u64);
        let b = call(&mut heap, SET, p, a, 0, 42);
        assert_eq!(call(&mut heap, GET, p, a, 0, 0), u32::MAX as u64);
        let c = call(&mut heap, NEW, p, 4, b, 0);
        assert_eq!(call(&mut heap, GET, p, c, 0, 0), 42);
        assert_eq!(call(&mut heap, GET_MASK, p, c, 3, 0), u32::MAX as u64);
        assert_eq!(call(&mut heap, SET, p, c, u64::MAX, 3), c);
        assert_eq!(call(&mut heap, CLEAR, p, c, 0, 0), 0);
        call(&mut heap, NEW, p, u64::MAX, 0, 0);
        assert_eq!(heap.take_fault(), Some(Fault::NegativeArraySize));
    }
    #[test]
    fn bounded_queue_insert_and_value_copies() {
        let mut heap = Heap::new();
        let p = parameter(8, true, 0, 1, 2, false, false);
        let a = call(&mut heap, INSERT, p, 0, 0, 10);
        let b = call(&mut heap, INSERT, p, a, 0, 20);
        let c = call(&mut heap, INSERT, p, b, 0, 30);
        assert_eq!(call(&mut heap, LEN, p, c, 0, 0), 2);
        assert_eq!(call(&mut heap, GET, p, c, 0, 0), 30);
        assert_eq!(call(&mut heap, GET, p, c, 1, 0), 20);
        assert_eq!(call(&mut heap, GET, p, b, 1, 0), 10);
        assert_eq!(call(&mut heap, INSERT, p, c, 2, 40), c);
    }

    #[test]
    fn ordering_interns_unchanged_values_and_retains_nested_string_roots() {
        let mut heap = Heap::new();
        let p = parameter(32, true, 1, 1, 0, false, false);
        let z = heap.intern(b"z") as u64;
        let a = heap.intern(b"a") as u64;
        let first = call(&mut heap, INSERT, p, 0, 0, z);
        let first = call(&mut heap, SET_MASK, p, first, 0, 0);
        let source = call(&mut heap, INSERT, p, first, 1, a);
        let source = call(&mut heap, SET_MASK, p, source, 1, 0);
        let sorted = call(&mut heap, SORT, p, source, 0, 0);
        assert_ne!(source, sorted);
        assert_eq!(call(&mut heap, GET, p, source, 0, 0), z);
        assert_eq!(call(&mut heap, GET, p, sorted, 0, 0), a);
        assert_eq!(call(&mut heap, SORT, p, sorted, 0, 0), sorted);
        assert_eq!(call(&mut heap, RSORT, p, sorted, 0, 0), source);
        assert_eq!(call(&mut heap, REVERSE, p, sorted, 0, 0), source);
        heap.collect([Root::object(sorted as Handle)]);
        assert!(heap.contains(z as Handle));
        assert!(heap.contains(a as Handle));
        assert!(!heap.containers.contains_key(&(source as Handle)));
        heap.collect([]);
        assert!(!heap.contains(z as Handle));
        assert!(!heap.contains(a as Handle));
    }
    #[test]
    fn associative_order_and_gc_follow_element_handles() {
        let mut heap = Heap::new();
        let p = parameter(32, true, 1, 2, 0, true, false);
        let string = heap.intern(b"live");
        let a = call(&mut heap, SET, p, 0, 3, string as u64);
        let b = call(&mut heap, SET, p, a, (-2i64) as u64, string as u64);
        assert_eq!(call(&mut heap, FIRST, p, b, 0, 0), (-2i64) as u64);
        assert_eq!(call(&mut heap, NEXT, p, b, (-2i64) as u64, 0), 3);
        let before = heap.clone();
        heap.collect([Root::object(b as Handle)]);
        assert!(!heap.same_contents(&before));
        assert!(heap.contains(string));
        assert!(!heap.containers.contains_key(&(a as Handle)));
        heap.collect([]);
        assert!(!heap.contains(string));
    }
}
