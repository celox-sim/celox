//! The private slots of an activation's frame, kept as the bytes of their
//! planes in state memory.
//!
//! A kernel's frame can hold the temporaries of everything inlined into it,
//! tens of thousands of slots, and an activation saves and restores all of
//! them whenever another activation of the kernel runs. Copying the bytes of
//! the slots, in runs of adjacent state memory, costs a copy of the frame;
//! a slot is decoded into a value only when something reads it.

use std::sync::Arc;

use num_bigint::BigUint;

use crate::{SignalRef, backend::SimBackend, timing::Value};

/// Where the planes of each private slot of a frame are, in state memory and
/// in a [`FrameImage`].
#[derive(Debug, PartialEq, Eq)]
pub struct ImageLayout {
    four_state: bool,
    slots: Vec<SlotImage>,
    /// Runs of adjacent state memory, as (memory offset, image offset,
    /// bytes), which the image copies.
    runs: Vec<(usize, usize, usize)>,
    /// The bytes of an image.
    len: usize,
    /// The first slot at each state offset.
    index: fxhash::FxHashMap<usize, usize>,
}

#[derive(Debug, PartialEq, Eq)]
struct SlotImage {
    signal: SignalRef,
    /// The offset of its value plane in the image; its mask plane, if it
    /// has one, follows.
    at: usize,
    /// The bytes of one plane.
    plane: usize,
    /// Whether the image holds a mask plane: the slot is four-state in a
    /// four-state layout.
    mask: bool,
}

impl ImageLayout {
    /// The layout of the private slots `slots` of a frame, in a state layout
    /// that keeps mask planes if `four_state`.
    pub fn new(slots: &[SignalRef], four_state: bool) -> Self {
        let mut layout = Self {
            four_state,
            slots: Vec::with_capacity(slots.len()),
            runs: Vec::new(),
            len: 0,
            index: fxhash::FxHashMap::default(),
        };
        for (position, &signal) in slots.iter().enumerate() {
            let mask = four_state && signal.is_4state;
            let (plane, mask_offset) = match signal.array_layout {
                None => {
                    let bytes = signal.width.div_ceil(8);
                    (bytes, signal.offset + bytes)
                }
                Some(array) => (array.element_extent(), signal.offset + array.plane_size),
            };
            let at = layout.len;
            layout.push_run(signal.offset, plane);
            if mask {
                layout.push_run(mask_offset, plane);
            }
            layout.slots.push(SlotImage {
                signal,
                at,
                plane,
                mask,
            });
            layout.index.entry(signal.offset).or_insert(position);
        }
        layout
    }

    fn push_run(&mut self, memory: usize, bytes: usize) {
        if let Some(last) = self.runs.last_mut()
            && last.0 + last.2 == memory
        {
            last.2 += bytes;
        } else {
            self.runs.push((memory, self.len, bytes));
        }
        self.len += bytes;
    }

    /// The position of private slot `signal` in the frame.
    pub fn index_of(&self, signal: SignalRef) -> Option<usize> {
        self.index
            .get(&signal.offset)
            .copied()
            .filter(|&index| self.slots[index].signal == signal)
    }

    /// The number of private slots.
    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }
}

/// The values of the private slots of an activation's frame (see
/// [`ImageLayout`]): none, before the activation first saves its frame.
#[derive(Clone, Debug, Default)]
pub struct FrameImage {
    layout: Option<Arc<ImageLayout>>,
    bytes: Vec<u8>,
}

impl FrameImage {
    /// The image of the slots of `layout` as the state memory of `backend`
    /// holds them now.
    pub fn capture<B: SimBackend>(layout: &Arc<ImageLayout>, backend: &B) -> Self {
        let mut image = Self {
            layout: Some(layout.clone()),
            bytes: vec![0; layout.len],
        };
        image.save(backend);
        image
    }

    /// The image of the slots of `layout` holding `values`, in order.
    pub fn from_values<'a>(
        layout: &Arc<ImageLayout>,
        values: impl IntoIterator<Item = &'a Value>,
    ) -> Self {
        let mut image = Self {
            layout: Some(layout.clone()),
            bytes: vec![0; layout.len],
        };
        for (index, value) in values.into_iter().enumerate() {
            image.set(index, value);
        }
        image
    }

    /// Copy the slots from the state memory of `backend`.
    pub fn save<B: SimBackend>(&mut self, backend: &B) {
        let Some(layout) = &self.layout else {
            return;
        };
        let (ptr, size) = backend.memory_as_ptr();
        // Safety: the backend exposes `size` readable bytes at `ptr` for as
        // long as it is borrowed.
        let memory = unsafe { std::slice::from_raw_parts(ptr, size) };
        for &(offset, at, len) in &layout.runs {
            self.bytes[at..at + len].copy_from_slice(&memory[offset..offset + len]);
        }
    }

    /// Write the slots back into the state memory of `backend`, as setting
    /// each of them does.
    pub fn restore<B: SimBackend>(&self, backend: &mut B) {
        let Some(layout) = &self.layout else {
            return;
        };
        for &(offset, at, len) in &layout.runs {
            backend.write_memory(offset, &self.bytes[at..at + len]);
            backend.mark_vcd_range(offset, len);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.layout
            .as_ref()
            .is_none_or(|layout| layout.slots.is_empty())
    }

    /// The number of slots.
    pub fn len(&self) -> usize {
        self.layout.as_ref().map_or(0, |layout| layout.slots.len())
    }

    /// Forget the slots, as an activation that retires does.
    pub fn clear(&mut self) {
        self.layout = None;
        self.bytes = Vec::new();
    }

    /// The layout of the image.
    pub fn layout(&self) -> Option<&Arc<ImageLayout>> {
        self.layout.as_ref()
    }

    /// The value of slot `index`, as reading it from state memory gives it.
    pub fn get(&self, index: usize) -> Option<Value> {
        let layout = self.layout.as_ref()?;
        let slot = layout.slots.get(index)?;
        let plane = |at: usize| decode(slot.signal, &self.bytes[at..at + slot.plane]);
        let mask = if slot.mask {
            plane(slot.at + slot.plane)
        } else {
            BigUint::default()
        };
        Some((plane(slot.at), mask))
    }

    /// The value of private slot `signal`.
    pub fn value_of(&self, signal: SignalRef) -> Option<Value> {
        self.get(self.layout.as_ref()?.index_of(signal)?)
    }

    /// The values of the slots, in order.
    pub fn values(&self) -> impl Iterator<Item = Value> + '_ {
        (0..self.len()).map(|index| self.get(index).expect("a slot of the image"))
    }

    /// Set slot `index` to `value`, as writing it into state memory does.
    pub fn set(&mut self, index: usize, value: &Value) {
        let Some(layout) = &self.layout else {
            return;
        };
        let Some(slot) = layout.slots.get(index) else {
            return;
        };
        let (at, len, signal) = (slot.at, slot.plane, slot.signal);
        encode(signal, &mut self.bytes[at..at + len], &value.0);
        if slot.mask {
            encode(signal, &mut self.bytes[at + len..at + 2 * len], &value.1);
        }
    }

    /// Set private slot `signal` to `value`; whether the frame has it.
    pub fn set_value_of(&mut self, signal: SignalRef, value: &Value) -> bool {
        let Some(index) = self
            .layout
            .as_ref()
            .and_then(|layout| layout.index_of(signal))
        else {
            return false;
        };
        self.set(index, value);
        true
    }
}

/// The value of one plane of `signal`, whose bytes are `plane`.
fn decode(signal: SignalRef, plane: &[u8]) -> BigUint {
    match signal.array_layout {
        None => {
            let mut value = BigUint::from_bytes_le(plane);
            if !signal.width.is_multiple_of(8) {
                value &= (BigUint::from(1u8) << signal.width) - BigUint::from(1u8);
            }
            value
        }
        Some(array) => array.gather(plane),
    }
}

/// Write `value` into one plane of `signal`, whose bytes are `plane`.
fn encode(signal: SignalRef, plane: &mut [u8], value: &BigUint) {
    plane.fill(0);
    match signal.array_layout {
        None => {
            let bytes = value.to_bytes_le();
            let len = bytes.len().min(plane.len());
            plane[..len].copy_from_slice(&bytes[..len]);
            if !signal.width.is_multiple_of(8)
                && let Some(top) = plane.last_mut()
            {
                *top &= (1u8 << (signal.width % 8)) - 1;
            }
        }
        Some(array) => array.scatter(plane, value),
    }
}

/// Two images are equal when they hold the same values.
impl PartialEq for FrameImage {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.values().eq(other.values())
    }
}

impl Eq for FrameImage {}

/// How a checkpoint keeps an image: its slots and their bytes.
#[derive(serde::Serialize, serde::Deserialize)]
struct FrameImageRecord {
    slots: Vec<SignalRef>,
    four_state: bool,
    bytes: Vec<u8>,
}

impl serde::Serialize for FrameImage {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let record = FrameImageRecord {
            slots: self
                .layout
                .iter()
                .flat_map(|layout| layout.slots.iter().map(|slot| slot.signal))
                .collect(),
            four_state: self.layout.as_ref().is_some_and(|layout| layout.four_state),
            bytes: self.bytes.clone(),
        };
        record.serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for FrameImage {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let record = FrameImageRecord::deserialize(deserializer)?;
        if record.slots.is_empty() && record.bytes.is_empty() {
            return Ok(Self::default());
        }
        let layout = ImageLayout::new(&record.slots, record.four_state);
        if layout.len != record.bytes.len() {
            return Err(serde::de::Error::custom("frame image size"));
        }
        Ok(Self {
            layout: Some(Arc::new(layout)),
            bytes: record.bytes,
        })
    }
}
