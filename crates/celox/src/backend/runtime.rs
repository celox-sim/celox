use malachite_bigint::BigUint;

use crate::{
    HashMap, SimulatorOptions,
    ir::{AbsoluteAddr, SignalRef},
};

use super::{JitEngine, MemoryLayout, get_byte_size};
// ... (omitting intermediate types for brevity in thought, but I will provide the full block in actual call)
use thiserror::Error;
pub type SimFunc = unsafe extern "C" fn(*mut u8) -> u64;

/// Opaque handle to a compiled event (clock / async-reset) function.
/// Holds the JIT-compiled function pointer directly — no indirection.
/// Obtained once via [`JitBackend::resolve_event`] and passed to
/// [`JitBackend::eval_apply_ff_at`] for zero-cost dispatch.
#[derive(Clone, Copy)]
pub struct EventRef {
    pub func: SimFunc,
    pub addr: AbsoluteAddr,
    pub id: usize,
}

impl std::fmt::Debug for EventRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventRef")
            .field("func", &(self.func as usize))
            .field("addr", &self.addr)
            .field("id", &self.id)
            .finish()
    }
}
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SimulatorErrorCode {
    DetectedTrueLoop,
    InternalError,
    NotAnEvent(String),
}
impl std::fmt::Display for SimulatorErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DetectedTrueLoop => write!(f, "Detected True Loop"),
            Self::InternalError => write!(f, "Internal Error"),
            Self::NotAnEvent(name) => write!(
                f,
                "Signal '{}' is not an event (only clock and async reset signals can be scheduled). Use `modify()` for synchronous signals.",
                name
            ),
        }
    }
}

pub struct JitBackend {
    engine: JitEngine,
    memory: Vec<u64>,
    comb_func: SimFunc,
    pub event_map: HashMap<AbsoluteAddr, EventRef>,
    pub eval_only_event_map: HashMap<AbsoluteAddr, EventRef>,
    pub apply_event_map: HashMap<AbsoluteAddr, EventRef>,
    pub id_to_addr: Vec<AbsoluteAddr>,
    pub id_to_event: Vec<EventRef>,
}

impl JitBackend {
    pub fn new(
        sir: &crate::ir::Program,
        options: &SimulatorOptions,
        mut trace: Option<&mut crate::debug::CompilationTrace>,
    ) -> Result<Self, crate::SimulatorError> {
        let layout = MemoryLayout::build(sir, options);
        let mut engine = JitEngine::new(layout, options).map_err(crate::SimulatorError::Codegen)?;

        let mut pre_clif_buf = String::new();
        let mut post_clif_buf = String::new();
        let mut native_buf = String::new();

        let (pre_clif_ptr, post_clif_ptr, native_ptr) = if trace.is_some() {
            (
                options
                    .trace
                    .pre_optimized_clif
                    .then_some(&mut pre_clif_buf),
                options
                    .trace
                    .post_optimized_clif
                    .then_some(&mut post_clif_buf),
                options.trace.native.then_some(&mut native_buf),
            )
        } else {
            (None, None, None)
        };

        // Batch compile eval_comb
        let res = engine.compile_units(&sir.eval_comb, pre_clif_ptr, post_clif_ptr, native_ptr);

        if let Some(t) = trace.as_deref_mut() {
            if options.trace.pre_optimized_clif {
                let mut full_clif = String::new();
                full_clif.push_str("=========================================\n");
                full_clif.push_str("  Cranelift IR (CLIF) Dump (Pre-Optimized)\n");
                full_clif.push_str("=========================================\n\n");
                full_clif.push_str(&pre_clif_buf);
                t.pre_optimized_clif = Some(full_clif);
            }
            if options.trace.post_optimized_clif {
                let mut full_clif = String::new();
                full_clif.push_str("=========================================\n");
                full_clif.push_str("  Cranelift IR (CLIF) Dump (Post-Optimized)\n");
                full_clif.push_str("=========================================\n\n");
                full_clif.push_str(&post_clif_buf);
                t.post_optimized_clif = Some(full_clif);
            }
            if options.trace.native {
                let mut full_native = String::new();
                full_native.push_str("=========================================\n");
                full_native.push_str("  Native Machine Code Dump\n");
                full_native.push_str("=========================================\n\n");
                full_native.push_str(&native_buf);
                t.native = Some(full_native);
            }
        }

        let comb_code_ptr = res.map_err(crate::SimulatorError::Codegen)?;

        let mut next_id = 0;
        let mut addr_to_id = HashMap::default();
        let mut id_to_addr = Vec::new();
        let mut ff_funcs = Vec::new();

        let mut compile_ffs = |ff_map: &HashMap<
            AbsoluteAddr,
            Vec<crate::ir::ExecutionUnit<crate::ir::RegionedAbsoluteAddr>>,
        >,
                               addr_to_id: &mut HashMap<AbsoluteAddr, usize>,
                               next_id: &mut usize,
                               id_to_addr: &mut Vec<AbsoluteAddr>|
         -> Result<
            HashMap<AbsoluteAddr, EventRef>,
            crate::SimulatorError,
        > {
            let mut event_map = HashMap::default();
            for (clock, units) in ff_map {
                let id = *addr_to_id.entry(*clock).or_insert_with(|| {
                    let id = *next_id;
                    *next_id += 1;
                    id_to_addr.push(*clock);
                    id
                });

                let mut ff_pre_clif_buf = String::new();
                let mut ff_post_clif_buf = String::new();
                let mut ff_native_buf = String::new();
                let (ff_pre_clif_ptr, ff_post_clif_ptr, ff_native_ptr) = if trace.is_some() {
                    (
                        options
                            .trace
                            .pre_optimized_clif
                            .then_some(&mut ff_pre_clif_buf),
                        options
                            .trace
                            .post_optimized_clif
                            .then_some(&mut ff_post_clif_buf),
                        options.trace.native.then_some(&mut ff_native_buf),
                    )
                } else {
                    (None, None, None)
                };

                let res =
                    engine.compile_units(units, ff_pre_clif_ptr, ff_post_clif_ptr, ff_native_ptr);

                if let Some(t) = trace.as_deref_mut() {
                    if options.trace.pre_optimized_clif {
                        t.pre_optimized_clif
                            .get_or_insert_with(String::new)
                            .push_str(&ff_pre_clif_buf);
                    }
                    if options.trace.post_optimized_clif {
                        t.post_optimized_clif
                            .get_or_insert_with(String::new)
                            .push_str(&ff_post_clif_buf);
                    }
                    if options.trace.native {
                        t.native
                            .get_or_insert_with(String::new)
                            .push_str(&ff_native_buf);
                    }
                }

                let ptr = res.map_err(crate::SimulatorError::Codegen)?;
                let func: SimFunc = unsafe { std::mem::transmute(ptr) };
                ff_funcs.push(func);
                event_map.insert(
                    *clock,
                    EventRef {
                        func,
                        addr: *clock,
                        id,
                    },
                );
            }
            Ok(event_map)
        };

        let mut event_map = compile_ffs(
            &sir.eval_apply_ffs,
            &mut addr_to_id,
            &mut next_id,
            &mut id_to_addr,
        )?;
        let mut eval_only_event_map = compile_ffs(
            &sir.eval_only_ffs,
            &mut addr_to_id,
            &mut next_id,
            &mut id_to_addr,
        )?;
        let mut apply_event_map = compile_ffs(
            &sir.apply_ffs,
            &mut addr_to_id,
            &mut next_id,
            &mut id_to_addr,
        )?;

        // Insert clock_domains aliases so every event signal resolves
        for (alias, canonical) in &sir.clock_domains {
            if let Some(&ev) = event_map.get(canonical) {
                event_map.insert(*alias, ev);
            }
            if let Some(&ev) = eval_only_event_map.get(canonical) {
                eval_only_event_map.insert(*alias, ev);
            }
            if let Some(&ev) = apply_event_map.get(canonical) {
                apply_event_map.insert(*alias, ev);
            }
        }

        let id_to_event: Vec<EventRef> = id_to_addr.iter().map(|addr| event_map[addr]).collect();

        let comb_func: SimFunc = unsafe { std::mem::transmute(comb_code_ptr) };

        debug_assert_eq!(
            engine.translator.layout.working_base_offset,
            (engine.translator.layout.total_size + 7) & !7
        );
        debug_assert_eq!(
            engine.translator.layout.merged_total_size,
            (engine.translator.layout.triggered_bits_offset
                + engine.translator.layout.triggered_bits_total_size
                + 7)
                & !7
        );

        let merged_total_size = engine.translator.layout.merged_total_size;
        let num_u64 = merged_total_size.div_ceil(8);
        let mut backend = Self {
            engine,
            memory: vec![0u64; num_u64],
            comb_func,
            event_map,
            eval_only_event_map,
            apply_event_map,
            id_to_addr,
            id_to_event,
        };
        if options.four_state {
            for (addr, &offset) in &backend.engine.translator.layout.offsets {
                let width = backend.engine.translator.layout.widths[addr];
                let is_4state = sir.module_variables[&sir.instance_module[&addr.instance_id]]
                    .values()
                    .find(|v| v.id == addr.var_id)
                    .map(|v| v.is_4state)
                    .unwrap_or(false);

                if is_4state {
                    let allocated_size = super::get_byte_size(width);
                    unsafe {
                        let mask_ptr =
                            (backend.memory.as_mut_ptr() as *mut u8).add(offset + allocated_size);
                        std::ptr::write_bytes(mask_ptr, 0xFF, allocated_size);
                    }
                }
            }
            for (addr, &rel_offset) in &backend.engine.translator.layout.working_offsets {
                let offset = backend.engine.translator.layout.working_base_offset + rel_offset;
                let width = backend.engine.translator.layout.widths[addr];
                let is_4state = sir.module_variables[&sir.instance_module[&addr.instance_id]]
                    .values()
                    .find(|v| v.id == addr.var_id)
                    .map(|v| v.is_4state)
                    .unwrap_or(false);

                if is_4state {
                    let allocated_size = super::get_byte_size(width);
                    unsafe {
                        let mask_ptr =
                            (backend.memory.as_mut_ptr() as *mut u8).add(offset + allocated_size);
                        std::ptr::write_bytes(mask_ptr, 0xFF, allocated_size);
                    }
                }
            }
        }

        Ok(backend)
    }

    #[inline]
    fn run_sim_func(&mut self, func: SimFunc) -> Result<(), SimulatorErrorCode> {
        let ptr = self.memory.as_mut_ptr() as *mut u8;
        self.run_sim_func_at(func, ptr)
    }

    #[inline]
    fn run_sim_func_at(&mut self, func: SimFunc, ptr: *mut u8) -> Result<(), SimulatorErrorCode> {
        let res = unsafe { (func)(ptr) };
        match res {
            0 => Ok(()),
            1 => Err(SimulatorErrorCode::DetectedTrueLoop),
            _ => unreachable!(),
        }
    }
    /// Execute combinational logic
    pub fn eval_comb(&mut self) -> Result<(), SimulatorErrorCode> {
        self.run_sim_func(self.comb_func)
    }

    /// Resolves an `AbsoluteAddr` into a performance-optimized [`SignalRef`].
    /// This handle allows for direct memory access without `HashMap` lookups.
    pub fn resolve_signal(&self, addr: &AbsoluteAddr) -> SignalRef {
        let offset = self.engine.translator.layout.offsets[addr];
        let width = self.engine.translator.layout.widths[addr];
        let is_4state = self.engine.translator.layout.is_4states[addr];
        SignalRef {
            offset,
            width,
            is_4state,
        }
    }

    /// Set value for a variable using a pre-resolved [`SignalRef`].
    pub fn set<T: Copy>(&mut self, signal: SignalRef, value: T) {
        let allocated_size = get_byte_size(signal.width);
        let provided_size = std::mem::size_of::<T>();

        assert!(provided_size <= allocated_size);

        unsafe {
            let base_ptr = (self.memory.as_mut_ptr() as *mut u8).add(signal.offset);
            std::ptr::write_bytes(base_ptr, 0, allocated_size);
            let ptr = base_ptr as *mut T;
            std::ptr::write_unaligned(ptr, value);

            if self.engine.translator.options.four_state && signal.is_4state {
                let mask_ptr = base_ptr.add(allocated_size);
                std::ptr::write_bytes(mask_ptr, 0, allocated_size);
            }
        }
    }

    /// Set value for a variable using a pre-resolved [`SignalRef`] and `BigUint`.
    pub fn set_wide(&mut self, signal: SignalRef, value: BigUint) {
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

            if self.engine.translator.options.four_state && signal.is_4state {
                let mask_ptr = dst_ptr.add(allocated_size);
                std::ptr::write_bytes(mask_ptr, 0, allocated_size);
            }
        }
    }

    /// Get value of a variable using a pre-resolved [`SignalRef`].
    pub fn get(&self, signal: SignalRef) -> BigUint {
        let byte_size = super::get_byte_size(signal.width);
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

    /// Get value of a variable as a specific integer type without creating a `BigUint`.
    /// The type `T` must be large enough to hold the signal width.
    pub fn get_as<T: Default + Copy>(&self, signal: SignalRef) -> T {
        let byte_size = super::get_byte_size(signal.width);
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

        // Mask extra bits if signal width is not a multiple of 8
        let extra_bits = byte_size * 8 - signal.width;
        if extra_bits > 0 {
            // This masking is tricky with generic T.
            // Since we mostly use this for clock edges (usually 1-bit),
            // and provided_size is likely 1, 8, or 64, we can handle common cases.
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

    /// Check if a signal has an entry in the Working region.
    #[allow(dead_code)]
    pub fn has_working_offset(&self, addr: &AbsoluteAddr) -> bool {
        self.engine
            .translator
            .layout
            .working_offsets
            .contains_key(addr)
    }

    /// Set 4-state value for a variable using a pre-resolved [`SignalRef`].
    pub fn set_four_state(&mut self, signal: SignalRef, value: BigUint, mask: BigUint) {
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

            if self.engine.translator.options.four_state && signal.is_4state {
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

    /// Get 4-state value for a variable using a pre-resolved [`SignalRef`].
    pub fn get_four_state(&self, signal: SignalRef) -> (BigUint, BigUint) {
        let byte_size = get_byte_size(signal.width);
        let v_ptr: *const u8 = unsafe { (self.memory.as_ptr() as *const u8).add(signal.offset) };
        let v_slice = unsafe { std::slice::from_raw_parts(v_ptr, byte_size) };
        let mut v_val = BigUint::from_bytes_le(v_slice);

        let mut m_val = if self.engine.translator.options.four_state && signal.is_4state {
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

    /// Resolve an `AbsoluteAddr` (clock or async-reset signal) into an
    /// [`EventRef`] handle.  This does a one-time `HashMap` lookup; the
    /// returned handle can then be passed to [`eval_apply_ff_at`] for zero-cost
    /// direct function-pointer dispatch.
    pub fn resolve_event(&self, addr: &AbsoluteAddr) -> EventRef {
        self.event_map[addr]
    }

    pub fn resolve_event_opt(&self, addr: &AbsoluteAddr) -> Option<EventRef> {
        self.event_map.get(addr).copied()
    }

    pub fn resolve_eval_only_event(&self, addr: &AbsoluteAddr) -> Option<EventRef> {
        self.eval_only_event_map.get(addr).copied()
    }

    pub fn resolve_apply_event(&self, addr: &AbsoluteAddr) -> Option<EventRef> {
        self.apply_event_map.get(addr).copied()
    }

    pub fn eval_apply_ff_at(&mut self, event: EventRef) -> Result<(), SimulatorErrorCode> {
        self.run_sim_func(event.func)
    }

    pub fn eval_only_ff_at(&mut self, event: EventRef) -> Result<(), SimulatorErrorCode> {
        self.run_sim_func(event.func)
    }

    pub fn apply_ff_at(&mut self, event: EventRef) -> Result<(), SimulatorErrorCode> {
        self.run_sim_func(event.func)
    }

    /// Returns a raw pointer to the JIT memory and its total size in bytes.
    pub fn memory_as_ptr(&self) -> (*const u8, usize) {
        let size = self.engine.translator.layout.merged_total_size;
        (self.memory.as_ptr() as *const u8, size)
    }

    /// Returns a mutable raw pointer to the JIT memory and its total size in bytes.
    pub fn memory_as_mut_ptr(&mut self) -> (*mut u8, usize) {
        let size = self.engine.translator.layout.merged_total_size;
        (self.memory.as_mut_ptr() as *mut u8, size)
    }

    /// Returns the stable region size in bytes.
    pub fn stable_region_size(&self) -> usize {
        self.engine.translator.layout.total_size
    }

    /// Returns a reference to the memory layout.
    pub fn layout(&self) -> &MemoryLayout {
        &self.engine.translator.layout
    }

    pub fn num_events(&self) -> usize {
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

    /// Clears the triggered bits bitset in JIT memory.
    pub fn clear_triggered_bits(&mut self) {
        let base_ptr = self.memory.as_mut_ptr() as *mut u8;
        let triggered_bits_ptr =
            unsafe { base_ptr.add(self.engine.translator.layout.triggered_bits_offset) };
        let total_size = self.engine.translator.layout.triggered_bits_total_size;
        unsafe {
            std::ptr::write_bytes(triggered_bits_ptr, 0, total_size);
        }
    }

    /// Manually marks a trigger bit as triggered in JIT memory.
    pub fn mark_triggered_bit(&mut self, id: usize) {
        let byte_idx = id / 8;
        let bit_idx = id % 8;
        let base_ptr = self.memory.as_mut_ptr() as *mut u8;
        let triggered_bits_ptr =
            unsafe { base_ptr.add(self.engine.translator.layout.triggered_bits_offset) };
        unsafe {
            let byte_ptr = triggered_bits_ptr.add(byte_idx);
            *byte_ptr |= 1 << bit_idx;
        }
    }

    /// Reads back the triggered bits bitset and returns it as a BitSet.
    pub fn get_triggered_bits(&self) -> bit_set::BitSet {
        let mut bits = bit_set::BitSet::with_capacity(self.num_events());
        let base_ptr = self.memory.as_ptr() as *const u8;
        let triggered_bits_ptr =
            unsafe { base_ptr.add(self.engine.translator.layout.triggered_bits_offset) };
        let total_size = self.engine.translator.layout.triggered_bits_total_size;

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
