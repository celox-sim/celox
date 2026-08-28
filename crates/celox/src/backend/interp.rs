//! Tier-0 interpreting execution backend.
//!
//! [`InterpBackend`] implements [`SimBackend`] by executing the laid-out SIR
//! directly on the Tier-0 interpreter ([`crate::interpreter::execute_unit`])
//! instead of generated machine code. It shares the exact memory image ABI
//! with [`super::JitBackend`] — little-endian byte packing, four-state value
//! and mask regions stored adjacently per object, the runtime-event-buffer
//! and comb-capture-enable pointers installed into the state header, and the
//! triggered-bits bitset region — so a simulation can migrate individual
//! execution units between the interpreter and compiled tiers without any
//! state translation.
//!
//! Trigger marking follows the compiled per-kind protocol: the first word
//! of every trigger-bearing object is snapshotted at group entry, and each
//! store or commit with triggers extracts its range's old and new values to
//! detect posedge/negedge edges and active reset levels. Marking only runs
//! when the backend is constructed with `emit_triggers`, like the compiled
//! codegen flag.

#![cfg(feature = "host-runtime")]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use celox_design::RegionedAbsoluteAddrBase;
use celox_sir::{ExecutionUnit, SIROffset, SIRValue, TriggerIdWithKind};
use num_bigint::BigUint;
use num_traits::{ToPrimitive, Zero};

use super::{
    EventHandle, MemoryLayout, RuntimeEventBuffer, SimBackend, SimulatorErrorCode, get_byte_size,
};
use crate::backend::memory_layout::{
    RUNTIME_EVENT_HEADER_SIZE, RUNTIME_EVENT_SLOT_ARG_COUNT_OFFSET,
    RUNTIME_EVENT_SLOT_PAYLOAD_OFFSET, RUNTIME_EVENT_SLOT_SEQ_OFFSET,
    RUNTIME_EVENT_SLOT_SITE_OFFSET, RUNTIME_EVENT_WRITING, STATE_HEADER_RUNTIME_EVENT_ADDR_OFFSET,
};
use crate::interpreter::{
    InterpError, InterpMachine, Registers, ResolvedAccess, StoreSnapshot, execute_prepared_unit,
};
use crate::ir::{SPARSE_WORKING_REGION, STABLE_REGION, WORKING_REGION};
use crate::{
    HashMap, SimulatorError, SimulatorOptions,
    ir::{AbsoluteAddr, LaidOutProgram, RegionedAbsoluteAddr, SignalArrayLayout, SignalRef},
};

/// Opaque handle to an interpreted event (clock / async-reset) group.
///
/// Mirrors [`super::EventRef`] minus the compiled function pointers: the
/// address selects the SIR execution-unit group and the id participates in
/// scheduler ordering and triggered-bit bookkeeping.
#[derive(Clone, Copy, Debug)]
pub struct InterpEventRef {
    addr: AbsoluteAddr,
    id: usize,
}

impl InterpEventRef {
    /// Rebuild a handle from its parts, e.g. when a tiered simulation
    /// translates its stable event ids back onto the interpreted tier.
    pub(crate) fn from_parts(addr: AbsoluteAddr, id: usize) -> Self {
        Self { addr, id }
    }
}

impl EventHandle for InterpEventRef {
    fn id(&self) -> usize {
        self.id
    }

    fn addr(&self) -> AbsoluteAddr {
        self.addr
    }
}

/// Map an interpreter failure onto the backend error code space.
///
/// `SIRTerminator::Error` carries a positive true-loop convergence code,
/// matching the compiled contract where a positive return value reports
/// [`SimulatorErrorCode::DetectedTrueLoopCode`].
fn error_code(error: InterpError) -> SimulatorErrorCode {
    match error {
        // The interpreter is not constrained by the compiled function ABI's
        // "zero return means success" convention, so every fatal code —
        // including zero-valued assertion site codes — carries its runtime
        // error info through unchanged.
        InterpError::Fatal(code) => SimulatorErrorCode::DetectedTrueLoopCode(code),
        _ => SimulatorErrorCode::InternalError,
    }
}

fn low_mask(bits: usize) -> BigUint {
    if bits == 0 {
        BigUint::zero()
    } else {
        (BigUint::from(1u8) << bits) - 1u8
    }
}

fn narrow_mask(bits: usize) -> u64 {
    match bits {
        0 => 0,
        64.. => u64::MAX,
        bits => (1u64 << bits) - 1,
    }
}

fn unpack_u64(bytes: &[u8], shift: usize, bits: usize) -> u64 {
    let mut raw = 0u128;
    for (index, byte) in bytes.iter().enumerate() {
        raw |= u128::from(*byte) << (index * 8);
    }
    ((raw >> shift) as u64) & narrow_mask(bits)
}

fn pack_u64(bytes: &mut [u8], shift: usize, bits: usize, value: u64) {
    let mut current = 0u128;
    for (index, byte) in bytes.iter().enumerate() {
        current |= u128::from(*byte) << (index * 8);
    }
    let field_mask = u128::from(narrow_mask(bits)) << shift;
    current = (current & !field_mask) | ((u128::from(value) << shift) & field_mask);
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = (current >> (index * 8)) as u8;
    }
}

/// Write `value` into element-strided storage: each element's low
/// `element_width` bits land at its stride slot, masked to the element width
/// so no padding bits leak inside or past the slot.
///
/// # Safety
/// Callers must bound `offset + (element_count - 1) * stride +
/// ceil(element_width / 8)` inside the allocation `base` points at.
unsafe fn scatter_strided(
    base: *mut u8,
    offset: usize,
    array: &SignalArrayLayout,
    value: &BigUint,
) {
    let element_bytes = array.element_width.div_ceil(8);
    let element_mask = low_mask(array.element_width);
    for index in 0..array.element_count {
        let chunk = ((value >> (index * array.element_width)) & &element_mask).to_bytes_le();
        unsafe {
            let dst = base.add(offset + index * array.element_stride);
            for byte in 0..element_bytes {
                *dst.add(byte) = chunk.get(byte).copied().unwrap_or(0);
            }
        }
    }
}

/// Read element-strided storage back into a packed logical value, gathering
/// every byte of each element and dropping per-element padding bits.
///
/// # Safety
/// Callers must bound `offset + (element_count - 1) * stride +
/// ceil(element_width / 8)` inside the allocation `base` points at.
unsafe fn gather_strided(base: *const u8, offset: usize, array: &SignalArrayLayout) -> BigUint {
    let element_bytes = array.element_width.div_ceil(8);
    let element_mask = low_mask(array.element_width);
    let mut result = BigUint::zero();
    for index in 0..array.element_count {
        unsafe {
            let start = base.add(offset + index * array.element_stride);
            let raw = BigUint::from_bytes_le(std::slice::from_raw_parts(start, element_bytes));
            result |= (raw & &element_mask) << (index * array.element_width);
        }
    }
    result
}

/// Interpreter view over one backend's live memory image.
///
/// Holds split borrows of the backend's storage so execution-unit slices can
/// be borrowed immutably from the SIR program at the same time.
struct Machine<'a> {
    memory: &'a mut Vec<u64>,
    layout: &'a MemoryLayout,
    four_state: bool,
    /// Per-site enable bytes for comb capture events.
    comb_capture_enabled: &'a mut [u8],
    /// First-word snapshots of trigger-bearing objects taken at group entry,
    /// used by per-kind trigger edge detection.
    trigger_snapshots: &'a [((AbsoluteAddr, u32), u64)],
    /// Whether trigger detection may mark bits; mirrors the compiled
    /// `emit_triggers` codegen flag.
    emit_triggers: bool,
}

impl Machine<'_> {
    fn trigger_snapshot(&self, key: (AbsoluteAddr, u32)) -> Option<u64> {
        self.trigger_snapshots
            .binary_search_by_key(&key, |&(entry, _)| entry)
            .ok()
            .map(|index| self.trigger_snapshots[index].1)
    }

    fn byte_slice(&self, start: usize, len: usize) -> &[u8] {
        // Safety: the layout guarantees every mapped object, its mask region,
        // and the trigger bitset fit inside the merged memory allocation.
        unsafe { std::slice::from_raw_parts((self.memory.as_ptr() as *const u8).add(start), len) }
    }

    /// Resolve a regioned SIR address to its byte offset in the merged image.
    ///
    /// Every region with a dedicated layout table is supported; anything
    /// else fails loudly rather than aliasing into unrelated storage.
    fn object_offset(&self, addr: &RegionedAbsoluteAddr) -> Result<usize, InterpError> {
        let absolute = addr.absolute_addr();
        let mapped = if addr.region == STABLE_REGION {
            self.layout.offsets.get(&absolute).copied()
        } else if addr.region == SPARSE_WORKING_REGION {
            self.layout
                .sparse_offsets
                .get(&absolute)
                .map(|relative| self.layout.sparse_base_offset + relative)
        } else if addr.region == WORKING_REGION {
            self.layout
                .working_offsets
                .get(&absolute)
                .map(|relative| self.layout.working_base_offset + relative)
        } else {
            None
        };
        mapped.ok_or_else(|| {
            InterpError::Machine(format!(
                "no interpreter storage mapped for {} in the addressed region",
                absolute
            ))
        })
    }

    /// Resolve a SIR access to its bit offset within the addressed object.
    ///
    /// `absolute` selects the object so element-strided layouts can convert
    /// an `Element` access's logical index into its physical word stride;
    /// packed layouts keep contiguous logical addressing.
    fn access_bit_offset(
        &self,
        absolute: &AbsoluteAddr,
        offset: &SIROffset,
        dynamics: &[Option<&SIRValue>; 2],
    ) -> Result<usize, InterpError> {
        fn dynamic(dynamics: &[Option<&SIRValue>; 2], slot: usize) -> Result<usize, InterpError> {
            dynamics[slot]
                .and_then(|value| value.payload.to_usize())
                .ok_or_else(|| {
                    InterpError::Machine(
                        "dynamic access offset is missing or unrepresentable".to_string(),
                    )
                })
        }

        match offset {
            SIROffset::Static(bit_offset) => {
                // Element-strided layouts remap static offsets from logical
                // packed positions to physical strided positions.
                if self.layout.unpacked_arrays.contains_key(absolute) {
                    let (byte, intra) = self.layout.map_static_bit_offset(absolute, *bit_offset);
                    Ok(byte * 8 + intra)
                } else {
                    Ok(*bit_offset)
                }
            }
            SIROffset::Dynamic(_) => dynamic(dynamics, 0),
            SIROffset::Element {
                element_width,
                bit_offset,
                ..
            } => {
                let index = dynamic(dynamics, 0)?;
                let extra = if dynamics[1].is_some() {
                    dynamic(dynamics, 1)?
                } else {
                    0
                };
                // Element-strided storage spaces elements by the layout's
                // byte stride (matching the native backend's addressing);
                // packed storage keeps elements logically contiguous.
                let stride_bits = match self.layout.unpacked_arrays.get(absolute) {
                    Some(array) => array.element_stride * 8,
                    None => *element_width,
                };
                Ok(index * stride_bits + bit_offset + extra)
            }
            SIROffset::PackedElements { bit_offset, .. } => {
                if self.layout.unpacked_arrays.contains_key(absolute) {
                    let (byte, intra) = self.layout.map_static_bit_offset(absolute, *bit_offset);
                    Ok(byte * 8 + intra)
                } else {
                    Ok(*bit_offset)
                }
            }
        }
    }

    /// Byte size of one addressable plane (value or mask) of the object.
    ///
    /// Element-strided objects reserve a whole plane per state bit kind;
    /// packed objects store the value plane immediately followed by the mask
    /// plane of the same total byte width.
    fn plane_byte_size(&self, absolute: &AbsoluteAddr) -> usize {
        self.layout
            .unpacked_arrays
            .get(absolute)
            .map(|array| array.plane_size)
            .unwrap_or_else(|| get_byte_size(self.width_of(absolute)))
    }

    fn width_of(&self, absolute: &AbsoluteAddr) -> usize {
        self.layout.widths.get(absolute).copied().unwrap_or(0)
    }

    fn is_4state_object(&self, absolute: &AbsoluteAddr) -> bool {
        self.four_state
            && self
                .layout
                .is_4states
                .get(absolute)
                .copied()
                .unwrap_or(false)
    }

    /// The array layout when the access spans an entire element-strided
    /// object, whose elements must transfer per stride slot instead of one
    /// contiguous span across element padding.
    fn whole_strided_array(
        &self,
        absolute: &AbsoluteAddr,
        bit_offset: usize,
        bits: usize,
    ) -> Option<SignalArrayLayout> {
        let array = self.layout.unpacked_arrays.get(absolute)?;
        (bit_offset == 0 && bits == array.element_count * array.element_width).then_some({
            SignalArrayLayout {
                element_width: array.element_width,
                element_count: array.element_count,
                element_stride: array.element_stride,
                plane_size: array.plane_size,
            }
        })
    }

    /// Read `bits` starting at `bit_offset` within the object at
    /// `byte_offset`. Bits past the object's declared width are never
    /// produced because every caller passes layout-derived widths.
    fn read_bits(&self, byte_offset: usize, bit_offset: usize, bits: usize) -> BigUint {
        if bits == 0 {
            return BigUint::zero();
        }
        let shift = bit_offset % 8;
        let byte_len = (shift + bits).div_ceil(8);
        let raw = BigUint::from_bytes_le(self.byte_slice(byte_offset + bit_offset / 8, byte_len));
        let shifted = if shift > 0 { raw >> shift } else { raw };
        shifted & low_mask(bits)
    }

    fn read_bits_u64(&self, byte_offset: usize, bit_offset: usize, bits: usize) -> u64 {
        debug_assert!(bits <= 64);
        if bits == 0 {
            return 0;
        }
        let shift = bit_offset % 8;
        let byte_len = (shift + bits).div_ceil(8);
        let start = byte_offset + bit_offset / 8;
        unpack_u64(self.byte_slice(start, byte_len), shift, bits)
    }

    /// Read-modify-write `bits` starting at `bit_offset`, preserving every
    /// other bit in the covered bytes.
    fn write_bits(&mut self, byte_offset: usize, bit_offset: usize, bits: usize, value: &BigUint) {
        if bits == 0 {
            return;
        }
        let shift = bit_offset % 8;
        let byte_len = (shift + bits).div_ceil(8);
        let start = byte_offset + bit_offset / 8;
        let domain = low_mask(byte_len * 8);
        let field_mask = low_mask(bits) << shift;
        let mut current = BigUint::from_bytes_le(self.byte_slice(start, byte_len));
        current &= &domain ^ &field_mask;
        current |= (value & low_mask(bits)) << shift;
        let bytes = current.to_bytes_le();
        // Safety: `start + byte_len` stays inside the merged allocation for
        // every layout-mapped object, as with the compiled backends.
        let destination = unsafe { (self.memory.as_mut_ptr() as *mut u8).add(start) };
        for index in 0..byte_len {
            unsafe {
                *destination.add(index) = bytes.get(index).copied().unwrap_or(0);
            }
        }
    }

    fn write_bits_u64(&mut self, byte_offset: usize, bit_offset: usize, bits: usize, value: u64) {
        debug_assert!(bits <= 64);
        if bits == 0 {
            return;
        }
        let shift = bit_offset % 8;
        let byte_len = (shift + bits).div_ceil(8);
        let start = byte_offset + bit_offset / 8;
        // Safety: the same layout-derived range is used by `write_bits`.
        let bytes = unsafe {
            std::slice::from_raw_parts_mut(
                (self.memory.as_mut_ptr() as *mut u8).add(start),
                byte_len,
            )
        };
        pack_u64(bytes, shift, bits, value);
    }

    fn mark_trigger_bit(&mut self, id: usize) {
        let offset = self.layout.triggered_bits_offset + id / 8;
        let end = self.layout.triggered_bits_offset + self.layout.triggered_bits_total_size;
        if offset >= end {
            return;
        }
        // Safety: bounds-checked against the trigger bitset region above.
        unsafe {
            *self.byte_mut(offset) |= 1 << (id % 8);
        }
    }

    // Safety: callers bound `offset` inside the merged allocation.
    fn byte_mut(&mut self, offset: usize) -> *mut u8 {
        unsafe { (self.memory.as_mut_ptr() as *mut u8).add(offset) }
    }

    /// Read one unaligned little-endian `u64` bookkeeping word.
    ///
    /// # Safety
    /// Callers must bound `offset + 8` inside the merged allocation.
    unsafe fn read_u64(&self, offset: usize) -> u64 {
        unsafe {
            let ptr = (self.memory.as_ptr() as *const u8).add(offset) as *const u64;
            ptr.read_unaligned()
        }
    }

    /// Write one unaligned little-endian `u64` bookkeeping word.
    ///
    /// # Safety
    /// Callers must bound `offset + 8` inside the merged allocation.
    unsafe fn write_u64(&mut self, offset: usize, value: u64) {
        unsafe {
            let ptr = (self.memory.as_mut_ptr() as *mut u8).add(offset) as *mut u64;
            ptr.write_unaligned(value)
        }
    }

    /// Read one byte of the merged image.
    ///
    /// # Safety
    /// Callers must bound `offset` inside the merged allocation.
    unsafe fn read_u8(&self, offset: usize) -> u8 {
        unsafe { *(self.memory.as_ptr() as *const u8).add(offset) }
    }

    /// Write one byte of the merged image.
    ///
    /// # Safety
    /// Callers must bound `offset` inside the merged allocation.
    unsafe fn write_u8(&mut self, offset: usize, value: u8) {
        unsafe { *(self.memory.as_mut_ptr() as *mut u8).add(offset) = value }
    }

    /// Little-endian 64-bit words of `value`, as used by the runtime-event
    /// record payload ABI (short values pad with zero words implicitly).
    fn value_words(value: &BigUint) -> Vec<u64> {
        value.to_u64_digits()
    }

    /// Write one runtime event record into the shared ring buffer whose
    /// address is installed in the state header, following the compiled
    /// protocol: reserve the slot with a `WRITING` marker, fill site and
    /// payload fields plainly, then publish with release semantics.
    fn emit_event_record(&mut self, site_id: u32, args: &[SIRValue]) {
        // Safety: the state header always holds a live buffer pointer that
        // outlives simulation, and the ring arithmetic stays inside the
        // buffer allocation because `seq` is masked by the capacity.
        unsafe {
            let event_ptr = self.read_u64(STATE_HEADER_RUNTIME_EVENT_ADDR_OFFSET) as *mut AtomicU64;
            let seq = (*event_ptr).load(Ordering::Acquire);
            let capacity = self.layout.runtime_event_capacity as u64;
            if capacity == 0 {
                return;
            }
            let slot_index = (seq & (capacity - 1)) as usize;
            let slot_base = event_ptr
                .cast::<u8>()
                .add(RUNTIME_EVENT_HEADER_SIZE + slot_index * self.layout.runtime_event_slot_size);
            let slot_seq = slot_base.add(RUNTIME_EVENT_SLOT_SEQ_OFFSET) as *const AtomicU64;
            // Publish the writing marker with an RMW like the compiled
            // writer: the exchange orders it ahead of the plain payload
            // stores below, so a concurrent drain can never observe a torn
            // mix of the old committed record and the new one.
            (*slot_seq).swap(RUNTIME_EVENT_WRITING, Ordering::AcqRel);
            slot_base
                .add(RUNTIME_EVENT_SLOT_SITE_OFFSET)
                .cast::<u64>()
                .write_unaligned(site_id as u64);
            slot_base
                .add(RUNTIME_EVENT_SLOT_ARG_COUNT_OFFSET)
                .cast::<u64>()
                .write_unaligned(args.len() as u64);

            if let Some(site_layout) = self.layout.runtime_event_site_layouts.get(site_id as usize)
            {
                for (index, arg) in args.iter().enumerate() {
                    let Some(arg_layout) = site_layout.args.get(index) else {
                        continue;
                    };
                    let value_digits = Self::value_words(&arg.payload);
                    let mask_digits = Self::value_words(&arg.mask);
                    for word in 0..arg_layout.word_count {
                        let payload = slot_base.add(
                            RUNTIME_EVENT_SLOT_PAYLOAD_OFFSET
                                + (arg_layout.value_word_offset + word) * 8,
                        );
                        payload
                            .cast::<u64>()
                            .write_unaligned(value_digits.get(word).copied().unwrap_or(0));
                        let mask_payload = slot_base.add(
                            RUNTIME_EVENT_SLOT_PAYLOAD_OFFSET
                                + (arg_layout.mask_word_offset + word) * 8,
                        );
                        mask_payload
                            .cast::<u64>()
                            .write_unaligned(mask_digits.get(word).copied().unwrap_or(0));
                    }
                }
            }

            (*slot_seq).store(seq, Ordering::Release);
            (*event_ptr).store(seq.wrapping_add(1), Ordering::Release);
        }
    }

    /// Copy-on-write preparation for a store into the sparse working region:
    /// every 64-bit chunk touched by `[bit_offset, bit_offset + bits)` is
    /// copied from the stable region before it is overwritten, mirroring the
    /// compiled `prepare_sparse_store` lowering.
    fn prepare_sparse_store(
        &mut self,
        addr: &RegionedAbsoluteAddr,
        bit_offset: usize,
        bits: usize,
    ) -> Result<(), InterpError> {
        let absolute = addr.absolute_addr();
        let Some(sparse) = self.layout.sparse_layouts.get(&absolute) else {
            return Err(InterpError::Machine(format!(
                "sparse store to {absolute} without a sparse layout"
            )));
        };
        let stable_base = self.layout.offsets[&absolute];
        let sparse_base = self.layout.sparse_base_offset + self.layout.sparse_offsets[&absolute];
        let plane_size = self.plane_byte_size(&absolute);
        let plane_count = if self.four_state && self.is_4state_object(&absolute) {
            2
        } else {
            1
        };

        if bits == 0 {
            return Ok(());
        }
        let start_chunk = bit_offset / 64;
        let end_chunk = (bit_offset + bits - 1) / 64;
        for chunk in start_chunk..=end_chunk {
            let dirty_word_index = chunk / 64;
            let dirty_mask = 1u64 << (chunk % 64);
            let dirty_word_addr = sparse.dirty_words_offset + dirty_word_index * 8;
            // Safety: layout bounds the dirty-word region.
            let was_dirty = unsafe { self.read_u64(dirty_word_addr) } & dirty_mask != 0;
            if !was_dirty {
                for plane in 0..plane_count {
                    let delta = plane * plane_size + chunk * 8;
                    // Safety: both chunks live inside the merged allocation.
                    let stable_chunk = unsafe { self.read_u64(stable_base + delta) };
                    unsafe { self.write_u64(sparse_base + delta, stable_chunk) };
                }
            }
            // Safety: bounded by the dirty-word region above.
            unsafe {
                let current = self.read_u64(dirty_word_addr);
                self.write_u64(dirty_word_addr, current | dirty_mask);
            }
            let summary_addr = sparse.summary_words_offset + (dirty_word_index / 64) * 8;
            // Safety: bounded by the summary region.
            unsafe {
                let current = self.read_u64(summary_addr);
                self.write_u64(summary_addr, current | (1u64 << (dirty_word_index % 64)));
            }
        }
        Ok(())
    }

    /// Flush every dirty sparse chunk of `src` back to its stable home and
    /// clear the dirty bookkeeping, matching the compiled sparse commit. The
    /// per-instruction access width is deliberately ignored: the compiled
    /// commit copies whole dirty chunks so partially written next-state data
    /// is never lost.
    fn commit_sparse_object(&mut self, src: &RegionedAbsoluteAddr) -> Result<(), InterpError> {
        let absolute = src.absolute_addr();
        let Some(sparse) = self.layout.sparse_layouts.get(&absolute) else {
            return Err(InterpError::Machine(format!(
                "sparse commit of {absolute} without a sparse layout"
            )));
        };
        let dst_base = self.layout.offsets[&absolute];
        let src_base = self.layout.sparse_base_offset + self.layout.sparse_offsets[&absolute];
        let plane_size = self.plane_byte_size(&absolute);
        let plane_count = if self.four_state && self.is_4state_object(&absolute) {
            2
        } else {
            1
        };
        let last_chunk = sparse.chunk_count.saturating_sub(1);
        let last_len = plane_size.saturating_sub(last_chunk * 8);

        for summary_index in 0..sparse.summary_word_count {
            let summary_addr = sparse.summary_words_offset + summary_index * 8;
            // Safety: summary words are part of the merged allocation.
            let mut summary_bits = unsafe { self.read_u64(summary_addr) };
            unsafe { self.write_u64(summary_addr, 0) };
            while summary_bits != 0 {
                let word_index = summary_bits.trailing_zeros() as usize + summary_index * 64;
                let dirty_addr = sparse.dirty_words_offset + word_index * 8;
                // Safety: dirty words are part of the merged allocation.
                let mut dirty_bits = unsafe { self.read_u64(dirty_addr) };
                unsafe { self.write_u64(dirty_addr, 0) };
                while dirty_bits != 0 {
                    let chunk = word_index * 64 + dirty_bits.trailing_zeros() as usize;
                    let len = if chunk == last_chunk { last_len } else { 8 };
                    for plane in 0..plane_count {
                        let delta = plane * plane_size + chunk * 8;
                        for byte in 0..len {
                            // Safety: both chunks live inside the merged
                            // allocation; `delta + byte` stays in bounds.
                            let value = unsafe { self.read_u8(src_base + delta + byte) };
                            unsafe { self.write_u8(dst_base + delta + byte, value) };
                        }
                    }
                    dirty_bits &= dirty_bits - 1;
                }
                summary_bits &= summary_bits - 1;
            }
        }
        Ok(())
    }
}

impl InterpMachine<RegionedAbsoluteAddr> for Machine<'_> {
    fn load(
        &mut self,
        addr: &RegionedAbsoluteAddr,
        access: ResolvedAccess<'_>,
        bits: usize,
    ) -> Result<SIRValue, InterpError> {
        let object = self.object_offset(addr)?;
        let absolute_addr_ref = addr.absolute_addr();
        let bit_offset =
            self.access_bit_offset(&absolute_addr_ref, access.offset, &access.dynamics)?;
        let absolute = addr.absolute_addr();
        let payload = self.read_bits(object, bit_offset, bits);
        if self.is_4state_object(&absolute) {
            let mask_offset = object + self.plane_byte_size(&absolute);
            let mask = self.read_bits(mask_offset, bit_offset, bits);
            Ok(SIRValue::new_four_state(payload, mask))
        } else {
            Ok(SIRValue::new(payload))
        }
    }

    fn load_u64(
        &mut self,
        addr: &RegionedAbsoluteAddr,
        access: ResolvedAccess<'_>,
        bits: usize,
    ) -> Result<u64, InterpError> {
        debug_assert!(bits <= 64);
        let object = self.object_offset(addr)?;
        let absolute = addr.absolute_addr();
        debug_assert!(!self.is_4state_object(&absolute));
        let bit_offset = self.access_bit_offset(&absolute, access.offset, &access.dynamics)?;
        Ok(self.read_bits_u64(object, bit_offset, bits))
    }

    fn prepare_store(
        &mut self,
        addr: &RegionedAbsoluteAddr,
        access: ResolvedAccess<'_>,
        bits: usize,
    ) -> Result<(), InterpError> {
        if addr.region == SPARSE_WORKING_REGION {
            let absolute = addr.absolute_addr();
            let bit_offset = self.access_bit_offset(&absolute, access.offset, &access.dynamics)?;
            // A whole-object transfer on an element-strided array writes
            // every physical byte of its plane; mark dirty chunks across
            // that physical extent even when padding expands it past the
            // logical width.
            let marked_bits = match self.whole_strided_array(&absolute, bit_offset, bits) {
                Some(array) => array.plane_size * 8,
                None => bits,
            };
            self.prepare_sparse_store(addr, bit_offset, marked_bits)?;
        }
        Ok(())
    }

    fn store(
        &mut self,
        addr: &RegionedAbsoluteAddr,
        access: ResolvedAccess<'_>,
        bits: usize,
        value: &SIRValue,
    ) -> Result<(), InterpError> {
        let object = self.object_offset(addr)?;
        let absolute_addr_ref = addr.absolute_addr();
        let bit_offset =
            self.access_bit_offset(&absolute_addr_ref, access.offset, &access.dynamics)?;
        let absolute = addr.absolute_addr();
        if let Some(array) = self.whole_strided_array(&absolute, bit_offset, bits) {
            let base = self.memory.as_mut_ptr() as *mut u8;
            unsafe {
                scatter_strided(base, object, &array, &value.payload);
                if self.is_4state_object(&absolute) {
                    scatter_strided(base, object + array.plane_size, &array, &value.mask);
                }
            }
            return Ok(());
        }
        self.write_bits(object, bit_offset, bits, &value.payload);
        if self.is_4state_object(&absolute) {
            let mask_offset = object + self.plane_byte_size(&absolute);
            self.write_bits(mask_offset, bit_offset, bits, &value.mask);
        }
        Ok(())
    }

    fn store_u64(
        &mut self,
        addr: &RegionedAbsoluteAddr,
        access: ResolvedAccess<'_>,
        bits: usize,
        value: u64,
    ) -> Result<(), InterpError> {
        debug_assert!(bits <= 64);
        let object = self.object_offset(addr)?;
        let absolute = addr.absolute_addr();
        debug_assert!(!self.is_4state_object(&absolute));
        let bit_offset = self.access_bit_offset(&absolute, access.offset, &access.dynamics)?;
        if let Some(array) = self.whole_strided_array(&absolute, bit_offset, bits) {
            let value = BigUint::from(value);
            unsafe {
                scatter_strided(self.memory.as_mut_ptr().cast(), object, &array, &value);
            }
        } else {
            self.write_bits_u64(object, bit_offset, bits, value);
        }
        Ok(())
    }

    fn notify_trigger_only_store(
        &mut self,
        addr: &RegionedAbsoluteAddr,
        triggers: &[TriggerIdWithKind],
    ) -> Result<(), InterpError> {
        // Zero-width aliased stores detect a change on the aliased signal's
        // first byte, mirroring the compiled dedicated path.
        if !self.emit_triggers || triggers.is_empty() {
            return Ok(());
        }
        let absolute = addr.absolute_addr();
        let Some(snapshot) = self.trigger_snapshot((absolute, addr.region)) else {
            return Ok(());
        };
        let base = self.object_offset(addr)?;
        // Safety: layout-mapped objects fit inside the merged image.
        let current = unsafe { self.read_u64(base) } as u8;
        if current != snapshot as u8 {
            for trigger in triggers {
                self.mark_trigger_bit(trigger.id);
            }
        }
        Ok(())
    }

    fn commit(
        &mut self,
        src: &RegionedAbsoluteAddr,
        dst: &RegionedAbsoluteAddr,
        access: ResolvedAccess<'_>,
        bits: usize,
    ) -> Result<(), InterpError> {
        if src.region == SPARSE_WORKING_REGION {
            return self.commit_sparse_object(src);
        }
        let src_absolute = src.absolute_addr();
        let bit_offset = self.access_bit_offset(&src_absolute, access.offset, &access.dynamics)?;
        let src_object = self.object_offset(src)?;
        let dst_object = self.object_offset(dst)?;

        if let Some(array) = self.whole_strided_array(&src_absolute, bit_offset, bits) {
            let src_base = self.memory.as_ptr() as *const u8;
            let payload = unsafe { gather_strided(src_base, src_object, &array) };
            let dst_base = self.memory.as_mut_ptr() as *mut u8;
            unsafe {
                scatter_strided(dst_base, dst_object, &array, &payload);
            }
            let dst_absolute = dst.absolute_addr();
            if self.is_4state_object(&dst_absolute) {
                let mask = if self
                    .layout
                    .is_4states
                    .get(&src_absolute)
                    .copied()
                    .unwrap_or(false)
                {
                    unsafe { gather_strided(src_base, src_object + array.plane_size, &array) }
                } else {
                    BigUint::zero()
                };
                unsafe {
                    scatter_strided(dst_base, dst_object + array.plane_size, &array, &mask);
                }
            }
            return Ok(());
        }

        let payload = self.read_bits(src_object, bit_offset, bits);
        self.write_bits(dst_object, bit_offset, bits, &payload);

        let dst_absolute = dst.absolute_addr();
        if self.is_4state_object(&dst_absolute) {
            let src_absolute = src.absolute_addr();
            let mask = if self
                .layout
                .is_4states
                .get(&src_absolute)
                .copied()
                .unwrap_or(false)
            {
                self.read_bits(
                    src_object + self.plane_byte_size(&src_absolute),
                    bit_offset,
                    bits,
                )
            } else {
                BigUint::zero()
            };
            let dst_mask_offset = dst_object + self.plane_byte_size(&dst_absolute);
            self.write_bits(dst_mask_offset, bit_offset, bits, &mask);
        }
        Ok(())
    }

    fn notify_triggers(
        &mut self,
        addr: &RegionedAbsoluteAddr,
        access: ResolvedAccess<'_>,
        bits: usize,
        triggers: &[TriggerIdWithKind],
    ) -> Result<(), InterpError> {
        if !self.emit_triggers || triggers.is_empty() {
            return Ok(());
        }
        // Per-kind edge detection identical to the compiled lowering: the
        // stored range's old value comes from the group-entry snapshot and
        // its new value from live memory, both confined to the first word.
        let absolute = addr.absolute_addr();
        let Some(snapshot) = self.trigger_snapshot((absolute, addr.region)) else {
            return Ok(());
        };
        let base = self.object_offset(addr)?;
        let bit_offset = self.access_bit_offset(&absolute, access.offset, &access.dynamics)?;
        let range_mask = if bits >= 64 {
            u64::MAX
        } else {
            (1u64 << bits) - 1
        };
        let old = if bit_offset >= 64 {
            0
        } else {
            (snapshot >> bit_offset) & range_mask
        };
        let new = self
            .read_bits(base, bit_offset, bits.min(64))
            .to_u64()
            .unwrap_or(0);
        for trigger in triggers {
            let marked = match trigger.kind {
                celox_sir::DomainKind::ClockPosedge => old == 0 && new == 1,
                celox_sir::DomainKind::ClockNegedge => old == 1 && new == 0,
                celox_sir::DomainKind::ResetAsyncHigh => new == 1,
                celox_sir::DomainKind::ResetAsyncLow => new == 0,
                celox_sir::DomainKind::Other => old != new,
            };
            if marked {
                self.mark_trigger_bit(trigger.id);
            }
        }
        Ok(())
    }

    fn capture_store_range(
        &mut self,
        addr: &RegionedAbsoluteAddr,
        access: ResolvedAccess<'_>,
        bits: usize,
    ) -> Result<StoreSnapshot, InterpError> {
        let object = self.object_offset(addr)?;
        let absolute_addr_ref = addr.absolute_addr();
        let bit_offset =
            self.access_bit_offset(&absolute_addr_ref, access.offset, &access.dynamics)?;
        let absolute = addr.absolute_addr();
        let value_words = Self::value_words(&self.read_bits(object, bit_offset, bits));
        let mask_words = if self.is_4state_object(&absolute) {
            let mask_offset = object + self.plane_byte_size(&absolute);
            Self::value_words(&self.read_bits(mask_offset, bit_offset, bits))
        } else {
            Vec::new()
        };
        Ok(StoreSnapshot {
            value_words,
            mask_words,
        })
    }

    fn enable_comb_captures(
        &mut self,
        addr: &RegionedAbsoluteAddr,
        access: ResolvedAccess<'_>,
        bits: usize,
        before: &StoreSnapshot,
        sites: &[u32],
    ) -> Result<(), InterpError> {
        if sites.is_empty() {
            return Ok(());
        }
        let object = self.object_offset(addr)?;
        let absolute_addr_ref = addr.absolute_addr();
        let bit_offset =
            self.access_bit_offset(&absolute_addr_ref, access.offset, &access.dynamics)?;
        let absolute = addr.absolute_addr();
        let changed = Self::value_words(&self.read_bits(object, bit_offset, bits))
            != before.value_words
            || (self.is_4state_object(&absolute)
                && Self::value_words(&self.read_bits(
                    object + self.plane_byte_size(&absolute),
                    bit_offset,
                    bits,
                )) != before.mask_words);
        if changed {
            for &site in sites {
                let index = site as usize;
                if index < self.comb_capture_enabled.len() {
                    self.comb_capture_enabled[index] = 1;
                }
            }
        }
        Ok(())
    }

    fn emit_runtime_event(&mut self, site_id: u32, args: &[SIRValue]) -> Result<(), InterpError> {
        self.emit_event_record(site_id, args);
        Ok(())
    }

    fn emit_comb_capture_event(
        &mut self,
        site_id: u32,
        args: &[SIRValue],
        fatal_error_code: Option<i64>,
        consume_enabled: bool,
    ) -> Result<(), InterpError> {
        let index = site_id as usize;
        if index >= self.comb_capture_enabled.len() || self.comb_capture_enabled[index] == 0 {
            return Ok(());
        }
        self.emit_event_record(site_id, args);
        if consume_enabled {
            self.comb_capture_enabled[index] = 0;
        }
        match fatal_error_code {
            Some(code) => Err(InterpError::Fatal(code)),
            None => Ok(()),
        }
    }

    fn enable_comb_capture_if_changed(
        &mut self,
        old: &SIRValue,
        new: &SIRValue,
        sites: &[u32],
    ) -> Result<(), InterpError> {
        if sites.is_empty() {
            return Ok(());
        }
        let mut changed = Self::value_words(&old.payload) != Self::value_words(&new.payload);
        if self.four_state {
            changed |= Self::value_words(&old.mask) != Self::value_words(&new.mask);
        }
        if changed {
            for &site in sites {
                let index = site as usize;
                if index < self.comb_capture_enabled.len() {
                    self.comb_capture_enabled[index] = 1;
                }
            }
        }
        Ok(())
    }
}

/// Collect the addresses of objects whose stores carry triggers. Their
/// first 64-bit word is snapshotted at group entry so trigger marking can
/// detect actual changes, mirroring the compiled backends.
fn collect_trigger_addrs(
    units: &[ExecutionUnit<RegionedAbsoluteAddr>],
) -> Vec<(AbsoluteAddr, u32)> {
    let mut addrs = crate::HashSet::<(AbsoluteAddr, u32)>::default();
    for unit in units {
        for block in unit.blocks.values() {
            for inst in &block.instructions {
                match inst {
                    celox_sir::SIRInstruction::Store(addr, _, _, _, triggers, _)
                        if !triggers.is_empty() =>
                    {
                        addrs.insert((addr.absolute_addr(), addr.region));
                    }
                    celox_sir::SIRInstruction::Commit(_, dst, _, _, triggers)
                        if !triggers.is_empty() =>
                    {
                        addrs.insert((dst.absolute_addr(), dst.region));
                    }
                    _ => {}
                }
            }
        }
    }
    let mut addrs: Vec<_> = addrs.into_iter().collect();
    addrs.sort_unstable();
    addrs
}

/// One scheduled SIR unit with register metadata prepared once at backend
/// construction. Keeping register storage with the unit removes per-tick
/// HashMap walks and slot clearing from the interpreter dispatch path.
struct PreparedUnit {
    unit: ExecutionUnit<RegionedAbsoluteAddr>,
    registers: Registers,
}

impl PreparedUnit {
    fn new(unit: ExecutionUnit<RegionedAbsoluteAddr>, four_state: bool) -> Self {
        let registers = Registers::new(&unit.register_map, four_state);
        Self { unit, registers }
    }
}

fn prepare_units(
    units: &[ExecutionUnit<RegionedAbsoluteAddr>],
    four_state: bool,
) -> Vec<PreparedUnit> {
    units
        .iter()
        .cloned()
        .map(|unit| PreparedUnit::new(unit, four_state))
        .collect()
}

/// Execute every unit in `units` against the split backend storage.
#[allow(clippy::too_many_arguments)]
fn run_units(
    memory: &mut Vec<u64>,
    layout: &MemoryLayout,
    four_state: bool,
    comb_capture_enabled: &mut [u8],
    units: &mut [PreparedUnit],
    trigger_addrs: &[(AbsoluteAddr, u32)],
    trigger_snapshots: &mut Vec<((AbsoluteAddr, u32), u64)>,
    emit_triggers: bool,
) -> Result<(), SimulatorErrorCode> {
    // Snapshot the first word of every trigger-bearing object at group
    // entry; trigger detection compares the stored range against it. The
    // trigger-bearing addresses are precomputed per group so a tick only
    // pays for the snapshot reads themselves.
    trigger_snapshots.clear();
    if emit_triggers {
        trigger_snapshots.reserve(trigger_addrs.len());
        for &(absolute, region) in trigger_addrs {
            let addr = RegionedAbsoluteAddrBase::from_absolute_addr(region, absolute);
            let machine = Machine {
                memory: &mut *memory,
                layout,
                four_state,
                comb_capture_enabled: &mut *comb_capture_enabled,
                trigger_snapshots,
                emit_triggers,
            };
            if let Ok(base) = machine.object_offset(&addr) {
                // Snapshot the raw first word; trigger detection extracts the
                // stored range from it, so packed neighbors never leak in.
                // Safety: layout-mapped objects fit inside the merged image.
                trigger_snapshots.push(((absolute, region), unsafe { read_word(memory, base) }));
            }
        }
    }

    for prepared in units {
        let mut machine = Machine {
            // Reborrow through the mutable references so a fresh Machine can
            // be constructed for every execution unit in the loop.
            memory: &mut *memory,
            layout,
            four_state,
            comb_capture_enabled: &mut *comb_capture_enabled,
            trigger_snapshots,
            emit_triggers,
        };
        // Entry blocks of top-level execution units take no parameters: the
        // compiled ABI passes only the memory pointer, so all inputs arrive
        // through loads.
        execute_prepared_unit(
            &prepared.unit,
            &mut machine,
            &[],
            four_state,
            &mut prepared.registers,
        )
        .map_err(error_code)?;
    }
    Ok(())
}

/// Read one little-endian word from the merged image.
///
/// # Safety
/// Callers must bound `offset + 8` inside the allocation.
unsafe fn read_word(memory: &[u64], offset: usize) -> u64 {
    unsafe {
        (memory.as_ptr() as *const u8)
            .add(offset)
            .cast::<u64>()
            .read_unaligned()
    }
}

/// A [`SimBackend`] that interprets the laid-out SIR instead of executing
/// generated machine code.
///
/// Construction performs no code generation: the simulator is ready as soon
/// as the state layout is finalized, which is the property the tiered
/// startup path relies on.
pub struct InterpBackend {
    eval_comb_units: Vec<PreparedUnit>,
    eval_apply_units: HashMap<AbsoluteAddr, Vec<PreparedUnit>>,
    eval_only_units: HashMap<AbsoluteAddr, Vec<PreparedUnit>>,
    apply_units: HashMap<AbsoluteAddr, Vec<PreparedUnit>>,
    layout: MemoryLayout,
    four_state: bool,
    memory: Vec<u64>,
    runtime_event_buffer: Arc<RuntimeEventBuffer>,
    comb_capture_enabled: Vec<u8>,
    event_map: HashMap<AbsoluteAddr, InterpEventRef>,
    eval_only_event_map: HashMap<AbsoluteAddr, InterpEventRef>,
    apply_event_map: HashMap<AbsoluteAddr, InterpEventRef>,
    id_to_addr: Vec<AbsoluteAddr>,
    id_to_event: Vec<InterpEventRef>,
    four_state_inits: Vec<(usize, usize)>,
    /// Units for the fused comb+FF tick of each event, mirroring the
    /// compiled `comb_apply_func`: fused schedules when present, otherwise
    /// comb units followed by the clock's FF units.
    comb_apply_units: HashMap<AbsoluteAddr, Vec<PreparedUnit>>,
    /// Trigger-bearing addresses per schedule group, precomputed so a group
    /// invocation only performs snapshot reads.
    comb_trigger_addrs: Vec<(AbsoluteAddr, u32)>,
    event_trigger_addrs: HashMap<AbsoluteAddr, Vec<(AbsoluteAddr, u32)>>,
    /// Reused group-entry trigger snapshots; sorted by address and region.
    trigger_snapshots: Vec<((AbsoluteAddr, u32), u64)>,
    /// Whether trigger detection marks bits; matches the compiled
    /// `emit_triggers` codegen flag.
    emit_triggers: bool,
}

impl InterpBackend {
    pub fn new(
        laid_out: &LaidOutProgram,
        options: &SimulatorOptions,
    ) -> Result<Self, SimulatorError> {
        let layout = laid_out.layout().clone();
        let four_state = options.four_state;

        // Share one id space across the three schedule groups, mirroring the
        // compiled backend so scheduler ordering and trigger ids agree.
        let mut next_id = 0usize;
        let mut addr_to_id: HashMap<AbsoluteAddr, usize> = HashMap::default();
        let mut id_to_addr: Vec<AbsoluteAddr> = Vec::new();
        let mut intern_event = |addr: &AbsoluteAddr| -> usize {
            *addr_to_id.entry(*addr).or_insert_with(|| {
                let id = next_id;
                next_id += 1;
                id_to_addr.push(*addr);
                id
            })
        };

        let mut event_map: HashMap<AbsoluteAddr, InterpEventRef> = HashMap::default();
        for addr in laid_out.sir.eval_apply_ffs.keys() {
            let id = intern_event(addr);
            event_map.insert(*addr, InterpEventRef { addr: *addr, id });
        }
        let mut eval_only_event_map: HashMap<AbsoluteAddr, InterpEventRef> = HashMap::default();
        for addr in laid_out.sir.eval_only_ffs.keys() {
            let id = intern_event(addr);
            eval_only_event_map.insert(*addr, InterpEventRef { addr: *addr, id });
        }
        let mut apply_event_map: HashMap<AbsoluteAddr, InterpEventRef> = HashMap::default();
        for addr in laid_out.sir.apply_ffs.keys() {
            let id = intern_event(addr);
            apply_event_map.insert(*addr, InterpEventRef { addr: *addr, id });
        }

        // Insert event aliases so every event signal resolves, matching the
        // compiled backend's alias expansion.
        for (alias, canonical) in &laid_out.design.events.aliases {
            if let Some(event) = event_map.get(canonical) {
                event_map.insert(*alias, *event);
            }
            if let Some(event) = eval_only_event_map.get(canonical) {
                eval_only_event_map.insert(*alias, *event);
            }
            if let Some(event) = apply_event_map.get(canonical) {
                apply_event_map.insert(*alias, *event);
            }
        }

        let id_to_event = id_to_addr
            .iter()
            .map(|addr| {
                event_map
                    .get(addr)
                    .or_else(|| eval_only_event_map.get(addr))
                    .or_else(|| apply_event_map.get(addr))
                    .copied()
                    .expect("every scheduled event address resolves to an event")
            })
            .collect();

        // Pre-compute 4-state initialization regions (value and mask both
        // start as all-X), mirroring SharedJitCode.
        let mut four_state_inits = Vec::new();
        if four_state {
            // Element-strided objects reserve a whole plane per state bit
            // kind; both planes must be filled to their full plane size.
            let plane_size = |addr: &AbsoluteAddr| -> usize {
                match layout.unpacked_arrays.get(addr) {
                    Some(array) => array.plane_size,
                    None => get_byte_size(layout.widths[addr]),
                }
            };
            for (addr, &offset) in &layout.offsets {
                if laid_out
                    .design
                    .state_objects
                    .get(addr)
                    .is_some_and(|metadata| metadata.is_4state)
                {
                    four_state_inits.push((offset, plane_size(addr)));
                }
            }
            for (addr, &relative) in &layout.working_offsets {
                if laid_out
                    .design
                    .state_objects
                    .get(addr)
                    .is_some_and(|metadata| metadata.is_4state)
                {
                    four_state_inits
                        .push((layout.working_base_offset + relative, plane_size(addr)));
                }
            }
        }

        let num_u64 = layout.merged_total_size.div_ceil(8);
        let mut memory = vec![0u64; num_u64];
        let runtime_event_buffer =
            Arc::new(RuntimeEventBuffer::new(layout.runtime_event_buffer_size));
        let comb_capture_enabled = vec![0u8; layout.runtime_event_site_layouts.len().max(1)];

        for &(offset, allocated_size) in &four_state_inits {
            unsafe {
                let base_ptr = (memory.as_mut_ptr() as *mut u8).add(offset);
                std::ptr::write_bytes(base_ptr, 0xFF, allocated_size);
                let mask_ptr = base_ptr.add(allocated_size);
                std::ptr::write_bytes(mask_ptr, 0xFF, allocated_size);
            }
        }

        // Deferred testbench ticks require the scheduler's combined comb/FF
        // program; executing independently scheduled comb and FF units can
        // observe a different pre-edge snapshot around reset and NBA regions,
        // exactly as with the compiled backends.
        let mut comb_apply_units: HashMap<AbsoluteAddr, Vec<PreparedUnit>> = HashMap::default();
        for (clock, ff_units) in &laid_out.sir.eval_apply_ffs {
            let units = if let Some(fused) = laid_out.sir.eval_comb_apply_ffs.get(clock) {
                fused.clone()
            } else {
                let mut combined = laid_out.sir.eval_comb.clone();
                combined.extend(ff_units.iter().cloned());
                combined
            };
            comb_apply_units.insert(*clock, prepare_units(&units, four_state));
        }
        let eval_comb_units = prepare_units(&laid_out.sir.eval_comb, four_state);
        let eval_apply_units = laid_out
            .sir
            .eval_apply_ffs
            .iter()
            .map(|(addr, units)| (*addr, prepare_units(units, four_state)))
            .collect();
        let eval_only_units = laid_out
            .sir
            .eval_only_ffs
            .iter()
            .map(|(addr, units)| (*addr, prepare_units(units, four_state)))
            .collect();
        let apply_units = laid_out
            .sir
            .apply_ffs
            .iter()
            .map(|(addr, units)| (*addr, prepare_units(units, four_state)))
            .collect();
        // Only eval/apply clocks receive combined ticks; eval-only and
        // apply-only groups fall back to the default two-phase evaluation.
        let comb_trigger_addrs = collect_trigger_addrs(&laid_out.sir.eval_comb);
        let mut event_trigger_addrs: HashMap<AbsoluteAddr, Vec<(AbsoluteAddr, u32)>> =
            HashMap::default();
        for (clock, ff_units) in &laid_out.sir.eval_apply_ffs {
            let mut addrs = comb_trigger_addrs.clone();
            addrs.extend(collect_trigger_addrs(ff_units));
            addrs.sort_unstable();
            addrs.dedup();
            event_trigger_addrs.insert(*clock, addrs);
        }
        // Split scheduling can register the same canonical clock under both
        // maps; merge the address sets so neither phase loses snapshots.
        for (clock, units) in laid_out
            .sir
            .eval_only_ffs
            .iter()
            .chain(&laid_out.sir.apply_ffs)
        {
            let entry = event_trigger_addrs.entry(*clock).or_default();
            let mut addrs = collect_trigger_addrs(units);
            addrs.extend(entry.iter().copied());
            addrs.sort_unstable();
            addrs.dedup();
            *entry = addrs;
        }

        let mut backend = Self {
            eval_comb_units,
            eval_apply_units,
            eval_only_units,
            apply_units,
            layout,
            four_state,
            memory,
            runtime_event_buffer,
            comb_capture_enabled,
            event_map,
            eval_only_event_map,
            apply_event_map,
            id_to_addr,
            id_to_event,
            four_state_inits,
            comb_apply_units,
            comb_trigger_addrs,
            event_trigger_addrs,
            trigger_snapshots: Vec::new(),
            emit_triggers: options.emit_triggers,
        };
        backend.install_event_buffers();
        Ok(backend)
    }

    fn install_event_buffers(&mut self) {
        use crate::backend::memory_layout::{
            STATE_HEADER_COMB_CAPTURE_ENABLED_ADDR_OFFSET, STATE_HEADER_RUNTIME_EVENT_ADDR_OFFSET,
        };

        let addr = self.runtime_event_buffer.as_mut_ptr() as u64;
        let ptr = unsafe {
            (self.memory.as_mut_ptr() as *mut u8).add(STATE_HEADER_RUNTIME_EVENT_ADDR_OFFSET)
                as *mut u64
        };
        unsafe {
            std::ptr::write_unaligned(ptr, addr);
        }
        let addr = self.comb_capture_enabled.as_ptr() as u64;
        let ptr = unsafe {
            (self.memory.as_mut_ptr() as *mut u8).add(STATE_HEADER_COMB_CAPTURE_ENABLED_ADDR_OFFSET)
                as *mut u64
        };
        unsafe {
            std::ptr::write_unaligned(ptr, addr);
        }
    }

    /// Returns the pre-computed 4-state initialization regions
    /// `(offset, allocated_size)` for diagnostics and tests.
    pub fn four_state_regions(&self) -> &[(usize, usize)] {
        &self.four_state_inits
    }

    /// Hand the live simulation state to a successor backend during tier
    /// promotion. The returned memory image is byte-compatible with the
    /// compiled backends (same packed layout), and the event buffer `Arc`
    /// keeps its allocation so state-header pointers stay valid.
    pub(crate) fn tier_transfer(&mut self) -> (Vec<u64>, Arc<RuntimeEventBuffer>, Vec<u8>) {
        (
            std::mem::take(&mut self.memory),
            Arc::clone(&self.runtime_event_buffer),
            std::mem::take(&mut self.comb_capture_enabled),
        )
    }

    /// Current image length in `u64` words.
    pub(crate) fn image_word_len(&self) -> usize {
        self.memory.len()
    }

    /// Current image allocation capacity in `u64` words.
    pub(crate) fn image_word_capacity(&self) -> usize {
        self.memory.capacity()
    }

    /// Reserve capacity so the image can grow to `total_words` words without
    /// reallocating. Tiered builds use this up front so a later promotion
    /// grows within the existing allocation and never moves the live image.
    pub(crate) fn reserve_image_capacity(&mut self, total_words: usize) {
        let spare = total_words.saturating_sub(self.memory.capacity());
        if spare > 0 {
            self.memory.reserve(spare);
        }
    }
}

impl SimBackend for InterpBackend {
    type Event = InterpEventRef;

    fn eval_comb(&mut self) -> Result<(), SimulatorErrorCode> {
        run_units(
            &mut self.memory,
            &self.layout,
            self.four_state,
            &mut self.comb_capture_enabled,
            &mut self.eval_comb_units,
            &self.comb_trigger_addrs,
            &mut self.trigger_snapshots,
            self.emit_triggers,
        )
    }

    fn eval_apply_ff_at(&mut self, event: InterpEventRef) -> Result<(), SimulatorErrorCode> {
        run_units(
            &mut self.memory,
            &self.layout,
            self.four_state,
            &mut self.comb_capture_enabled,
            self.eval_apply_units
                .get_mut(&event.addr())
                .expect("scheduled event missing from SIR program"),
            self.event_trigger_addrs
                .get(&event.addr())
                .map_or(&[] as &[(AbsoluteAddr, u32)], Vec::as_slice),
            &mut self.trigger_snapshots,
            self.emit_triggers,
        )
    }

    fn eval_comb_apply_ff_at(&mut self, event: InterpEventRef) -> Result<(), SimulatorErrorCode> {
        let Some(units) = self.comb_apply_units.get_mut(&event.addr()) else {
            // Events outside the eval/apply map keep the default ordering.
            self.eval_comb()?;
            return self.eval_apply_ff_at(event);
        };
        run_units(
            &mut self.memory,
            &self.layout,
            self.four_state,
            &mut self.comb_capture_enabled,
            units,
            &self.event_trigger_addrs[&event.addr()],
            &mut self.trigger_snapshots,
            self.emit_triggers,
        )
    }

    fn eval_only_ff_at(&mut self, event: InterpEventRef) -> Result<(), SimulatorErrorCode> {
        run_units(
            &mut self.memory,
            &self.layout,
            self.four_state,
            &mut self.comb_capture_enabled,
            self.eval_only_units
                .get_mut(&event.addr())
                .expect("scheduled event missing from SIR program"),
            self.event_trigger_addrs
                .get(&event.addr())
                .map_or(&[] as &[(AbsoluteAddr, u32)], Vec::as_slice),
            &mut self.trigger_snapshots,
            self.emit_triggers,
        )
    }

    fn apply_ff_at(&mut self, event: InterpEventRef) -> Result<(), SimulatorErrorCode> {
        run_units(
            &mut self.memory,
            &self.layout,
            self.four_state,
            &mut self.comb_capture_enabled,
            self.apply_units
                .get_mut(&event.addr())
                .expect("scheduled event missing from SIR program"),
            self.event_trigger_addrs
                .get(&event.addr())
                .map_or(&[] as &[(AbsoluteAddr, u32)], Vec::as_slice),
            &mut self.trigger_snapshots,
            self.emit_triggers,
        )
    }

    fn resolve_signal(&self, addr: &AbsoluteAddr) -> SignalRef {
        let offset = self.layout.offsets[addr];
        let width = self.layout.widths[addr];
        let is_4state = self.layout.is_4states[addr];
        let array_layout =
            self.layout
                .unpacked_arrays
                .get(addr)
                .map(|array| celox_runtime::SignalArrayLayout {
                    element_width: array.element_width,
                    element_count: array.element_count,
                    element_stride: array.element_stride,
                    plane_size: array.plane_size,
                });
        SignalRef {
            offset,
            width,
            is_4state,
            array_layout,
        }
    }

    fn resolve_event(&self, addr: &AbsoluteAddr) -> InterpEventRef {
        *self
            .event_map
            .get(addr)
            .expect("event not registered in the interpreted program")
    }

    fn resolve_event_opt(&self, addr: &AbsoluteAddr) -> Option<InterpEventRef> {
        self.event_map.get(addr).copied()
    }

    fn resolve_eval_only_event(&self, addr: &AbsoluteAddr) -> Option<InterpEventRef> {
        self.eval_only_event_map.get(addr).copied()
    }

    fn resolve_apply_event(&self, addr: &AbsoluteAddr) -> Option<InterpEventRef> {
        self.apply_event_map.get(addr).copied()
    }

    fn set<T: Copy>(&mut self, signal: SignalRef, value: T) {
        let allocated_size = get_byte_size(signal.width);
        let provided_size = std::mem::size_of::<T>();
        let clear_mask = self.four_state && signal.is_4state;

        assert!(provided_size <= allocated_size);

        if signal.array_layout.is_some() {
            let mut bytes = vec![0u8; provided_size];
            unsafe {
                std::ptr::copy_nonoverlapping(
                    &value as *const T as *const u8,
                    bytes.as_mut_ptr(),
                    provided_size,
                );
            }
            let wide = BigUint::from_bytes_le(&bytes);
            self.set_wide(signal, wide);
            return;
        }

        unsafe {
            let base_ptr = (self.memory.as_mut_ptr() as *mut u8).add(signal.offset);
            if !clear_mask && allocated_size == 1 {
                let raw = *(&value as *const T as *const u8);
                let byte = if signal.width < 8 {
                    raw & ((1u8 << signal.width) - 1)
                } else {
                    raw
                };
                *base_ptr = byte;
                return;
            }

            std::ptr::write_bytes(base_ptr, 0, allocated_size);
            let ptr = base_ptr as *mut T;
            std::ptr::write_unaligned(ptr, value);

            if clear_mask {
                let mask_ptr = base_ptr.add(allocated_size);
                std::ptr::write_bytes(mask_ptr, 0, allocated_size);
            }
        }
    }

    fn set_wide(&mut self, signal: SignalRef, value: BigUint) {
        if let Some(ref arr) = signal.array_layout {
            let base = self.memory.as_mut_ptr() as *mut u8;
            unsafe {
                scatter_strided(base, signal.offset, arr, &value);
                if self.four_state && signal.is_4state {
                    scatter_strided(base, signal.offset + arr.plane_size, arr, &BigUint::zero());
                }
            }
            return;
        }
        let allocated_size = get_byte_size(signal.width);
        let mut bytes = value.to_bytes_le();

        if bytes.len() > allocated_size {
            bytes.truncate(allocated_size);
        } else {
            bytes.resize(allocated_size, 0u8);
        }

        unsafe {
            let dst_ptr: *mut u8 = self.memory.as_mut_ptr().cast();
            let dst_ptr = dst_ptr.add(signal.offset);
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst_ptr, allocated_size);

            if self.four_state && signal.is_4state {
                let mask_ptr = dst_ptr.add(allocated_size);
                std::ptr::write_bytes(mask_ptr, 0, allocated_size);
            }
        }
    }

    fn set_four_state(&mut self, signal: SignalRef, value: BigUint, mask: BigUint) {
        if let Some(ref arr) = signal.array_layout {
            let base = self.memory.as_mut_ptr() as *mut u8;
            unsafe {
                scatter_strided(base, signal.offset, arr, &value);
                // Two-state objects have no mask plane in strided storage;
                // only touch the mask plane when four-state storage is
                // enabled for this signal.
                if self.four_state && signal.is_4state {
                    scatter_strided(base, signal.offset + arr.plane_size, arr, &mask);
                }
            }
            return;
        }
        let allocated_size = get_byte_size(signal.width);

        let mut v_bytes = value.to_bytes_le();
        if v_bytes.len() > allocated_size {
            v_bytes.truncate(allocated_size);
        } else {
            v_bytes.resize(allocated_size, 0u8);
        }

        unsafe {
            let dst_ptr: *mut u8 = self.memory.as_mut_ptr().cast();
            std::ptr::copy_nonoverlapping(
                v_bytes.as_ptr(),
                dst_ptr.add(signal.offset),
                allocated_size,
            );

            if self.four_state && signal.is_4state {
                let mut m_bytes = mask.to_bytes_le();
                if m_bytes.len() > allocated_size {
                    m_bytes.truncate(allocated_size);
                } else {
                    m_bytes.resize(allocated_size, 0u8);
                }

                std::ptr::copy_nonoverlapping(
                    m_bytes.as_ptr(),
                    dst_ptr.add(signal.offset + allocated_size),
                    allocated_size,
                );
            }
        }
    }

    fn get(&self, signal: SignalRef) -> BigUint {
        if let Some(ref arr) = signal.array_layout {
            let base = self.memory.as_ptr() as *const u8;
            return unsafe { gather_strided(base, signal.offset, arr) };
        }
        let byte_size = get_byte_size(signal.width);
        let ptr: *const u8 = unsafe { (self.memory.as_ptr() as *const u8).add(signal.offset) };
        let byte_slice = unsafe { std::slice::from_raw_parts(ptr, byte_size) };
        let mut val = BigUint::from_bytes_le(byte_slice);

        let extra_bits = byte_size * 8 - signal.width;
        if extra_bits > 0 {
            let mask = (BigUint::from(1u32) << signal.width) - 1u32;
            val &= mask;
        }
        val
    }

    fn get_as<T: Default + Copy>(&self, signal: SignalRef) -> T {
        if signal.array_layout.is_some() {
            // Gather through the wide path, then truncate into the requested
            // type; `T::default()` supplies zero bytes beyond the value.
            let byte_size = get_byte_size(signal.width);
            assert!(
                byte_size <= std::mem::size_of::<T>(),
                "Provided type is too small for signal width"
            );
            let bytes = self.get(signal).to_bytes_le();
            let mut val = T::default();
            unsafe {
                std::ptr::copy_nonoverlapping(
                    bytes.as_ptr(),
                    &mut val as *mut T as *mut u8,
                    byte_size.min(bytes.len()),
                );
            }
            return val;
        }
        let byte_size = get_byte_size(signal.width);
        let ptr: *const u8 = unsafe { (self.memory.as_ptr() as *const u8).add(signal.offset) };
        let byte_slice = unsafe { std::slice::from_raw_parts(ptr, byte_size) };

        let provided_size = std::mem::size_of::<T>();
        assert!(
            byte_size <= provided_size,
            "Provided type is too small for signal width"
        );

        let mut val = T::default();
        unsafe {
            let val_ptr = &mut val as *mut T as *mut u8;
            std::ptr::copy_nonoverlapping(byte_slice.as_ptr(), val_ptr, byte_size);
        }

        let extra_bits = byte_size * 8 - signal.width;
        if extra_bits > 0 {
            if provided_size == 1 {
                let mask = (1u8 << (8 - extra_bits)) - 1;
                let v = unsafe { std::mem::transmute_copy::<T, u8>(&val) };
                val = unsafe { std::mem::transmute_copy::<u8, T>(&(v & mask)) };
            } else if provided_size == 8 {
                let mask = (1u64 << signal.width) - 1;
                let v = unsafe { std::mem::transmute_copy::<T, u64>(&val) };
                val = unsafe { std::mem::transmute_copy::<u64, T>(&(v & mask)) };
            }
        }
        val
    }

    fn get_four_state(&self, signal: SignalRef) -> (BigUint, BigUint) {
        if let Some(ref arr) = signal.array_layout {
            let base = self.memory.as_ptr() as *const u8;
            let value = unsafe { gather_strided(base, signal.offset, arr) };
            let mask = if self.four_state && signal.is_4state {
                unsafe { gather_strided(base, signal.offset + arr.plane_size, arr) }
            } else {
                BigUint::zero()
            };
            return (value, mask);
        }
        let byte_size = get_byte_size(signal.width);
        let v_ptr: *const u8 = unsafe { (self.memory.as_ptr() as *const u8).add(signal.offset) };
        let v_slice = unsafe { std::slice::from_raw_parts(v_ptr, byte_size) };
        let mut v_val = BigUint::from_bytes_le(v_slice);

        let mut m_val = if self.four_state && signal.is_4state {
            let m_ptr: *const u8 = unsafe { v_ptr.add(byte_size) };
            let m_slice = unsafe { std::slice::from_raw_parts(m_ptr, byte_size) };
            BigUint::from_bytes_le(m_slice)
        } else {
            BigUint::from(0u32)
        };

        let extra_bits = byte_size * 8 - signal.width;
        if extra_bits > 0 {
            let bitmask = (BigUint::from(1u32) << signal.width) - 1u32;
            v_val &= &bitmask;
            m_val &= &bitmask;
        }

        (v_val, m_val)
    }

    fn memory_as_ptr(&self) -> (*const u8, usize) {
        (
            self.memory.as_ptr() as *const u8,
            self.layout.merged_total_size,
        )
    }

    fn memory_as_mut_ptr(&mut self) -> (*mut u8, usize) {
        (
            self.memory.as_mut_ptr() as *mut u8,
            self.layout.merged_total_size,
        )
    }

    fn runtime_event_buffer_as_ptr(&self) -> (*const u8, usize) {
        (
            self.runtime_event_buffer.as_ptr(),
            self.runtime_event_buffer.byte_size(),
        )
    }

    fn runtime_event_buffer(&self) -> Option<Arc<RuntimeEventBuffer>> {
        Some(Arc::clone(&self.runtime_event_buffer))
    }

    fn set_comb_capture_event_enabled(&mut self, active_sites: &[bool]) {
        self.comb_capture_enabled.fill(0);
        for (idx, active) in active_sites.iter().copied().enumerate() {
            if active && idx < self.comb_capture_enabled.len() {
                self.comb_capture_enabled[idx] = 1;
            }
        }
    }

    fn stable_region_size(&self) -> usize {
        self.layout.total_size
    }

    fn layout(&self) -> &MemoryLayout {
        &self.layout
    }

    fn id_to_addr_slice(&self) -> &[AbsoluteAddr] {
        &self.id_to_addr
    }

    fn id_to_event_slice(&self) -> &[InterpEventRef] {
        &self.id_to_event
    }

    fn num_events(&self) -> usize {
        let mut max_id = 0;
        for ev in self.event_map.values() {
            max_id = max_id.max(ev.id);
        }
        for ev in self.eval_only_event_map.values() {
            max_id = max_id.max(ev.id);
        }
        for ev in self.apply_event_map.values() {
            max_id = max_id.max(ev.id);
        }
        if self.event_map.is_empty()
            && self.eval_only_event_map.is_empty()
            && self.apply_event_map.is_empty()
        {
            0
        } else {
            max_id + 1
        }
    }

    fn clear_triggered_bits(&mut self) {
        let base_ptr = self.memory.as_mut_ptr() as *mut u8;
        let triggered_bits_ptr = unsafe { base_ptr.add(self.layout.triggered_bits_offset) };
        let total_size = self.layout.triggered_bits_total_size;
        unsafe {
            std::ptr::write_bytes(triggered_bits_ptr, 0, total_size);
        }
    }

    fn mark_triggered_bit(&mut self, id: usize) {
        let byte_idx = id / 8;
        let bit_idx = id % 8;
        let base_ptr = self.memory.as_mut_ptr() as *mut u8;
        let triggered_bits_ptr = unsafe { base_ptr.add(self.layout.triggered_bits_offset) };
        unsafe {
            let byte_ptr = triggered_bits_ptr.add(byte_idx);
            *byte_ptr |= 1 << bit_idx;
        }
    }

    fn get_triggered_bits(&self) -> bit_set::BitSet {
        let mut bits = bit_set::BitSet::with_capacity(self.num_events());
        let base_ptr = self.memory.as_ptr() as *const u8;
        let triggered_bits_ptr = unsafe { base_ptr.add(self.layout.triggered_bits_offset) };
        let total_size = self.layout.triggered_bits_total_size;

        for i in 0..total_size {
            let byte = unsafe { *triggered_bits_ptr.add(i) };
            if byte != 0 {
                for j in 0..8 {
                    if (byte & (1 << j)) != 0 {
                        bits.insert(i * 8 + j);
                    }
                }
            }
        }
        bits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn narrow_packed_access_matches_biguint_reference() {
        const WIDTHS: &[usize] = &[1, 7, 8, 31, 32, 63, 64];
        const VALUES: &[u64] = &[0, 1, 0x55, 0xdead_beef, u64::MAX];

        for bit_offset in 0..16 {
            for &bits in WIDTHS {
                let shift = bit_offset % 8;
                let byte_len = (shift + bits).div_ceil(8);
                let start = bit_offset / 8;
                for &value in VALUES {
                    let original: Vec<u8> = (0..16)
                        .map(|index| (index as u8).wrapping_mul(37).wrapping_add(11))
                        .collect();
                    let mut actual = original.clone();
                    pack_u64(&mut actual[start..start + byte_len], shift, bits, value);

                    let mut expected = BigUint::from_bytes_le(&original);
                    let field_mask = low_mask(bits) << bit_offset;
                    expected &= low_mask(original.len() * 8) ^ &field_mask;
                    expected |= (BigUint::from(value) & low_mask(bits)) << bit_offset;
                    let mut expected_bytes = expected.to_bytes_le();
                    expected_bytes.resize(original.len(), 0);
                    assert_eq!(actual, expected_bytes);

                    let loaded = unpack_u64(&actual[start..start + byte_len], shift, bits);
                    assert_eq!(loaded, value & narrow_mask(bits));
                }
            }
        }
    }
}
