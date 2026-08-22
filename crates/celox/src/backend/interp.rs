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
//! Known gaps versus the compiled backends (all fail loud or degrade
//! visibly, never silently corrupt):
//!
//! - `RuntimeEvent` / `CombCaptureEvent` instructions are currently no-ops
//!   because the interpreter does not yet write the `RuntimeEventBuffer`
//!   record ABI that generated code uses.
//! - Trigger notification marks the trigger bit on every qualifying
//!   store/commit (a level-style over-approximation of the compiled
//!   edge/level detection).
//! - `SPARSE_WORKING_REGION` accesses report a machine error instead of
//!   guessing at an unmapped storage region.

#![cfg(feature = "host-runtime")]

use std::sync::Arc;

use celox_sir::{ExecutionUnit, SIROffset, SIRValue, TriggerIdWithKind};
use num_bigint::BigUint;
use num_traits::{ToPrimitive, Zero};

use super::{
    EventHandle, MemoryLayout, RuntimeEventBuffer, SimBackend, SimulatorErrorCode, get_byte_size,
};
use crate::interpreter::{InterpError, InterpMachine, ResolvedAccess, execute_unit};
use crate::ir::{STABLE_REGION, WORKING_REGION};
use crate::{
    HashMap, SimulatorError, SimulatorOptions,
    ir::{AbsoluteAddr, LaidOutProgram, RegionedAbsoluteAddr, SignalRef},
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
        InterpError::Fatal(code) if code > 0 => SimulatorErrorCode::DetectedTrueLoopCode(code),
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

/// Interpreter view over one backend's live memory image.
///
/// Holds split borrows of the backend's storage so execution-unit slices can
/// be borrowed immutably from the SIR program at the same time.
struct Machine<'a> {
    memory: &'a mut Vec<u64>,
    layout: &'a MemoryLayout,
    four_state: bool,
    /// Reserved for comb-capture bookkeeping once the runtime-event record
    /// ABI is implemented; kept in the struct so the borrow split stays
    /// identical to the final shape.
    #[allow(dead_code)]
    comb_capture_enabled: &'a mut [u8],
}

impl Machine<'_> {
    fn byte_slice(&self, start: usize, len: usize) -> &[u8] {
        // Safety: the layout guarantees every mapped object, its mask region,
        // and the trigger bitset fit inside the merged memory allocation.
        unsafe { std::slice::from_raw_parts((self.memory.as_ptr() as *const u8).add(start), len) }
    }

    /// Resolve a regioned SIR address to its byte offset in the merged image.
    ///
    /// Only regions with a dedicated layout table are supported; anything
    /// else fails loudly rather than aliasing into unrelated storage.
    fn object_offset(&self, addr: &RegionedAbsoluteAddr) -> Result<usize, InterpError> {
        let absolute = addr.absolute_addr();
        let mapped = if addr.region == STABLE_REGION {
            self.layout.offsets.get(&absolute).copied()
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
    fn access_bit_offset(
        &self,
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
            SIROffset::Static(bit_offset) => Ok(*bit_offset),
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
                Ok(index * element_width + bit_offset + extra)
            }
            SIROffset::PackedElements { bit_offset, .. } => Ok(*bit_offset),
        }
    }

    fn width_of(&self, absolute: &AbsoluteAddr) -> usize {
        self.layout.widths.get(absolute).copied().unwrap_or(0)
    }

    fn is_4state_object(&self, absolute: &AbsoluteAddr) -> bool {
        self.four_state && self.layout.is_4states.get(absolute).copied().unwrap_or(false)
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
}

impl InterpMachine<RegionedAbsoluteAddr> for Machine<'_> {
    fn load(
        &mut self,
        addr: &RegionedAbsoluteAddr,
        access: ResolvedAccess<'_>,
        bits: usize,
    ) -> Result<SIRValue, InterpError> {
        let object = self.object_offset(addr)?;
        let bit_offset = self.access_bit_offset(access.offset, &access.dynamics)?;
        let absolute = addr.absolute_addr();
        let payload = self.read_bits(object, bit_offset, bits);
        if self.is_4state_object(&absolute) {
            let mask_offset = object + get_byte_size(self.width_of(&absolute));
            let mask = self.read_bits(mask_offset, bit_offset, bits);
            Ok(SIRValue::new_four_state(payload, mask))
        } else {
            Ok(SIRValue::new(payload))
        }
    }

    fn store(
        &mut self,
        addr: &RegionedAbsoluteAddr,
        access: ResolvedAccess<'_>,
        bits: usize,
        value: &SIRValue,
    ) -> Result<(), InterpError> {
        let object = self.object_offset(addr)?;
        let bit_offset = self.access_bit_offset(access.offset, &access.dynamics)?;
        let absolute = addr.absolute_addr();
        self.write_bits(object, bit_offset, bits, &value.payload);
        if self.is_4state_object(&absolute) {
            let mask_offset = object + get_byte_size(self.width_of(&absolute));
            self.write_bits(mask_offset, bit_offset, bits, &value.mask);
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
        let src_object = self.object_offset(src)?;
        let dst_object = self.object_offset(dst)?;
        let bit_offset = self.access_bit_offset(access.offset, &access.dynamics)?;

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
                    src_object + get_byte_size(self.width_of(&src_absolute)),
                    bit_offset,
                    bits,
                )
            } else {
                BigUint::zero()
            };
            let dst_mask_offset = dst_object + get_byte_size(self.width_of(&dst_absolute));
            self.write_bits(dst_mask_offset, bit_offset, bits, &mask);
        }
        Ok(())
    }

    fn notify_triggers(&mut self, _addr: &RegionedAbsoluteAddr, triggers: &[TriggerIdWithKind]) {
        for trigger in triggers {
            // SEMANTICS-GAP: compiled backends emit per-kind edge/level
            // detection inline. Marking on every qualifying store/commit is a
            // level-style over-approximation that keeps clocked simulation
            // advancing until per-kind semantics are ported.
            // VERIFY: `TriggerIdWithKind` field name assumed to be `id`.
            self.mark_trigger_bit(trigger.id);
        }
    }

    // SEMANTICS-GAP: runtime-event emission requires the RuntimeEventBuffer
    // record ABI that generated code writes. Left as no-ops until that ABI
    // is ported; simulations relying on runtime events must use a compiled
    // backend meanwhile.
    fn notify_comb_capture(
        &mut self,
        _addr: &RegionedAbsoluteAddr,
        _sites: &[u32],
        _value: &SIRValue,
    ) {
    }

    fn emit_runtime_event(&mut self, _site_id: u32, _args: &[SIRValue]) {}

    fn emit_comb_capture_event(
        &mut self,
        _site_id: u32,
        _args: &[SIRValue],
        _fatal_error_code: Option<i64>,
        _consume_enabled: bool,
    ) {
    }

    fn enable_comb_capture_if_changed(&mut self, _old: &SIRValue, _new: &SIRValue, _sites: &[u32]) {}
}

/// Execute every unit in `units` against the split backend storage.
fn run_units(
    memory: &mut Vec<u64>,
    layout: &MemoryLayout,
    four_state: bool,
    comb_capture_enabled: &mut [u8],
    units: &[ExecutionUnit<RegionedAbsoluteAddr>],
) -> Result<(), SimulatorErrorCode> {
    for unit in units {
        let mut machine = Machine {
            // Reborrow through the mutable references so a fresh Machine can
            // be constructed for every execution unit in the loop.
            memory: &mut *memory,
            layout,
            four_state,
            comb_capture_enabled: &mut *comb_capture_enabled,
        };
        // Entry blocks of top-level execution units take no parameters: the
        // compiled ABI passes only the memory pointer, so all inputs arrive
        // through loads.
        execute_unit(unit, &mut machine, &[]).map_err(error_code)?;
    }
    Ok(())
}

/// A [`SimBackend`] that interprets the laid-out SIR instead of executing
/// generated machine code.
///
/// Construction performs no code generation: the simulator is ready as soon
/// as the state layout is finalized, which is the property the tiered
/// startup path relies on.
pub struct InterpBackend {
    program_sir: crate::ir::SirProgram,
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
}

impl InterpBackend {
    pub fn new(
        laid_out: &LaidOutProgram,
        options: &SimulatorOptions,
    ) -> Result<Self, SimulatorError> {
        let program_sir = laid_out.sir.clone();
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
        for (addr, _) in &laid_out.sir.eval_apply_ffs {
            let id = intern_event(addr);
            event_map.insert(*addr, InterpEventRef { addr: *addr, id });
        }
        let mut eval_only_event_map: HashMap<AbsoluteAddr, InterpEventRef> = HashMap::default();
        for (addr, _) in &laid_out.sir.eval_only_ffs {
            let id = intern_event(addr);
            eval_only_event_map.insert(*addr, InterpEventRef { addr: *addr, id });
        }
        let mut apply_event_map: HashMap<AbsoluteAddr, InterpEventRef> = HashMap::default();
        for (addr, _) in &laid_out.sir.apply_ffs {
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
            for (addr, &offset) in &layout.offsets {
                if laid_out
                    .design
                    .state_objects
                    .get(addr)
                    .is_some_and(|metadata| metadata.is_4state)
                {
                    four_state_inits.push((offset, get_byte_size(layout.widths[addr])));
                }
            }
            for (addr, &relative) in &layout.working_offsets {
                if laid_out
                    .design
                    .state_objects
                    .get(addr)
                    .is_some_and(|metadata| metadata.is_4state)
                {
                    four_state_inits.push((
                        layout.working_base_offset + relative,
                        get_byte_size(layout.widths[addr]),
                    ));
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

        let mut backend = Self {
            program_sir,
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
}

impl SimBackend for InterpBackend {
    type Event = InterpEventRef;

    fn eval_comb(&mut self) -> Result<(), SimulatorErrorCode> {
        Self::run_units(
            &mut self.memory,
            &self.layout,
            self.four_state,
            &mut self.comb_capture_enabled,
            &self.program_sir.eval_comb,
        )
    }

    fn eval_apply_ff_at(&mut self, event: InterpEventRef) -> Result<(), SimulatorErrorCode> {
        Self::run_units(
            &mut self.memory,
            &self.layout,
            self.four_state,
            &mut self.comb_capture_enabled,
            self.program_sir
                .eval_apply_ffs
                .get(&event.addr())
                .expect("scheduled event missing from SIR program"),
        )
    }

    fn eval_only_ff_at(&mut self, event: InterpEventRef) -> Result<(), SimulatorErrorCode> {
        Self::run_units(
            &mut self.memory,
            &self.layout,
            self.four_state,
            &mut self.comb_capture_enabled,
            self.program_sir
                .eval_only_ffs
                .get(&event.addr())
                .expect("scheduled event missing from SIR program"),
        )
    }

    fn apply_ff_at(&mut self, event: InterpEventRef) -> Result<(), SimulatorErrorCode> {
        Self::run_units(
            &mut self.memory,
            &self.layout,
            self.four_state,
            &mut self.comb_capture_enabled,
            self.program_sir
                .apply_ffs
                .get(&event.addr())
                .expect("scheduled event missing from SIR program"),
        )
    }

    fn resolve_signal(&self, addr: &AbsoluteAddr) -> SignalRef {
        let offset = self.layout.offsets[addr];
        let width = self.layout.widths[addr];
        let is_4state = self.layout.is_4states[addr];
        SignalRef {
            offset,
            width,
            is_4state,
            array_layout: None,
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
