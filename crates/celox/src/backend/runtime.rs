use std::sync::Arc;

use malachite_bigint::BigUint;

use crate::{
    HashMap, SimulatorError, SimulatorOptions,
    ir::{AbsoluteAddr, SignalRef},
};

use super::{JitEngine, MemoryLayout, get_byte_size};
use thiserror::Error;
pub type SimFunc = unsafe extern "C" fn(*mut u8) -> u64;
#[allow(dead_code)]
pub type BatchFunc = unsafe extern "C" fn(*mut u8, *const u8, *mut u8, i64) -> u64;

/// Opaque handle to a compiled event (clock / async-reset) function.
/// Holds the JIT-compiled function pointer directly — no indirection.
/// Obtained once via [`JitBackend::resolve_event`] and passed to
/// [`JitBackend::eval_apply_ff_at`] for zero-cost dispatch.
#[derive(Clone, Copy)]
pub struct EventRef {
    pub func: SimFunc,
    /// Merged eval_apply_ff + eval_comb function, if available.
    pub merged_func: Option<SimFunc>,
    pub addr: AbsoluteAddr,
    pub id: usize,
}

impl std::fmt::Debug for EventRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventRef")
            .field("func", &(self.func as usize))
            .field("merged_func", &self.merged_func.map(|f| f as usize))
            .field("addr", &self.addr)
            .field("id", &self.id)
            .finish()
    }
}

impl super::EventHandle for EventRef {
    fn id(&self) -> usize {
        self.id
    }

    fn addr(&self) -> AbsoluteAddr {
        self.addr
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

/// Immutable compilation result that can be shared across simulator instances.
///
/// Contains JIT-compiled function pointers, event maps, and memory layout.
/// The `JitEngine` (which owns the JITModule code pages) is kept alive here.
///
/// # Safety
///
/// After `JITModule::finalize_definitions()`, the compiled code memory is
/// immutable. The function pointers (`SimFunc`) are plain pointers to these
/// code pages and remain valid across threads for the lifetime of this struct.
pub struct SharedJitCode {
    _engine: JitEngine,
    pub(crate) comb_func: SimFunc,
    pub(crate) event_map: HashMap<AbsoluteAddr, EventRef>,
    pub(crate) eval_only_event_map: HashMap<AbsoluteAddr, EventRef>,
    pub(crate) apply_event_map: HashMap<AbsoluteAddr, EventRef>,
    pub(crate) id_to_addr: Vec<AbsoluteAddr>,
    pub(crate) id_to_event: Vec<EventRef>,
    pub(crate) layout: MemoryLayout,
    pub(crate) options: SimulatorOptions,
    /// Pre-computed 4-state init regions: (offset, allocated_size) for stable
    /// and working regions.
    four_state_inits: Vec<(usize, usize)>,
}

// SAFETY: After JITModule finalization, compiled code is immutable.
// Function pointers are valid across threads for SharedJitCode's lifetime.
// We never call mutating methods on JitEngine after construction.
unsafe impl Send for SharedJitCode {}
unsafe impl Sync for SharedJitCode {}

impl SharedJitCode {
    /// Returns a reference to the memory layout.
    pub fn layout(&self) -> &MemoryLayout {
        &self.layout
    }
}

pub struct JitBackend {
    shared: Arc<SharedJitCode>,
    memory: Vec<u64>,
    /// Cached from `shared.comb_func` to avoid Arc dereference on the hot path.
    comb_func: SimFunc,
}

impl JitBackend {
    pub fn new(
        sir: &crate::ir::Program,
        options: &SimulatorOptions,
        trace: Option<&mut crate::debug::CompilationTrace>,
    ) -> Result<Self, crate::SimulatorError> {
        let shared = Arc::new(Self::compile(sir, options, trace)?);
        Ok(Self::from_shared(shared))
    }

    /// Build the shared JIT code from a Program.
    fn compile(
        sir: &crate::ir::Program,
        options: &SimulatorOptions,
        mut trace: Option<&mut crate::debug::CompilationTrace>,
    ) -> Result<SharedJitCode, crate::SimulatorError> {
        let layout = MemoryLayout::build(sir, options);
        let mut engine = JitEngine::new(layout, options).map_err(SimulatorError::from)?;

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

        // Batch compile eval_comb — use chunked compilation if the optimizer
        // determined that the combined CLIF would exceed Cranelift's instruction limit.
        let res = match &sir.eval_comb_plan {
            Some(crate::ir::EvalCombPlan::MemorySpilled(plan)) => {
                engine.compile_spilled_chunks(plan, pre_clif_ptr, post_clif_ptr, native_ptr)
            }
            Some(crate::ir::EvalCombPlan::TailCallChunks(chunks)) => {
                engine.compile_chunks(chunks, pre_clif_ptr, post_clif_ptr, native_ptr)
            }
            None => engine.compile_units(&sir.eval_comb, pre_clif_ptr, post_clif_ptr, native_ptr),
        };

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

        let comb_code_ptr = res.map_err(SimulatorError::from)?;

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

                let ptr = res.map_err(SimulatorError::from)?;
                let func: SimFunc = unsafe { std::mem::transmute(ptr) };
                ff_funcs.push(func);
                event_map.insert(
                    *clock,
                    EventRef {
                        func,
                        merged_func: None,
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

        // Release borrows captured by compile_ffs so engine is available again.
        // (Using `let _` instead of `drop()` to avoid clippy::drop_non_drop.)
        let _ = compile_ffs;

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

        // Compile merged eval_apply_ff + eval_comb functions per domain and
        // patch the merged_func into existing event_map entries.
        // Only when eval_comb uses the simple (non-chunked) compilation path.
        let mut eval_apply_and_comb_map: HashMap<AbsoluteAddr, SimFunc> = HashMap::default();
        if sir.eval_comb_plan.is_none() {
            for (clock, units) in &sir.eval_apply_ffs {
                if units.is_empty() {
                    continue;
                }
                let mut merged_units: Vec<_> = units.clone();
                merged_units.extend(sir.eval_comb.iter().cloned());

                let ptr = engine
                    .compile_units(&merged_units, None, None, None)
                    .map_err(SimulatorError::from)?;
                let func: SimFunc = unsafe { std::mem::transmute(ptr) };
                eval_apply_and_comb_map.insert(*clock, func);
            }
            // Patch merged_func into event_map entries
            for (addr, ev) in &mut event_map {
                if let Some(&merged) = eval_apply_and_comb_map.get(addr) {
                    ev.merged_func = Some(merged);
                }
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

        // Pre-compute 4-state initialization regions
        let mut four_state_inits = Vec::new();
        if options.four_state {
            for (addr, &offset) in &engine.translator.layout.offsets {
                let width = engine.translator.layout.widths[addr];
                let is_4state = sir.module_variables[&sir.instance_module[&addr.instance_id]]
                    .get(&addr.var_id)
                    .map(|v| v.is_4state)
                    .unwrap_or(false);

                if is_4state {
                    let allocated_size = super::get_byte_size(width);
                    four_state_inits.push((offset, allocated_size));
                }
            }
            for (addr, &rel_offset) in &engine.translator.layout.working_offsets {
                let offset = engine.translator.layout.working_base_offset + rel_offset;
                let width = engine.translator.layout.widths[addr];
                let is_4state = sir.module_variables[&sir.instance_module[&addr.instance_id]]
                    .get(&addr.var_id)
                    .map(|v| v.is_4state)
                    .unwrap_or(false);

                if is_4state {
                    let allocated_size = super::get_byte_size(width);
                    four_state_inits.push((offset, allocated_size));
                }
            }
        }

        let layout = engine.translator.layout.clone();
        let options = options.clone();

        Ok(SharedJitCode {
            _engine: engine,
            comb_func,
            event_map,
            eval_only_event_map,
            apply_event_map,
            id_to_addr,
            id_to_event,
            layout,
            options,
            four_state_inits,
        })
    }

    /// Create a new backend instance from shared compiled code.
    ///
    /// Allocates a fresh simulation memory buffer and initializes 4-state
    /// regions. The compiled function pointers are shared across instances.
    pub fn from_shared(shared: Arc<SharedJitCode>) -> Self {
        let num_u64 = shared.layout.merged_total_size.div_ceil(8);
        let mut memory = vec![0u64; num_u64];

        // Initialize 4-state regions to X (v=1, m=1)
        for &(offset, allocated_size) in &shared.four_state_inits {
            unsafe {
                let base_ptr = (memory.as_mut_ptr() as *mut u8).add(offset);
                std::ptr::write_bytes(base_ptr, 0xFF, allocated_size);
                let mask_ptr = base_ptr.add(allocated_size);
                std::ptr::write_bytes(mask_ptr, 0xFF, allocated_size);
            }
        }

        let comb_func = shared.comb_func;
        Self {
            shared,
            memory,
            comb_func,
        }
    }

    /// Returns the shared compiled code, allowing it to be reused for
    /// creating additional simulator instances without recompilation.
    pub fn shared_code(&self) -> Arc<SharedJitCode> {
        Arc::clone(&self.shared)
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
        let offset = self.shared.layout.offsets[addr];
        let width = self.shared.layout.widths[addr];
        let is_4state = self.shared.layout.is_4states[addr];
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

            if self.shared.options.four_state && signal.is_4state {
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

            if self.shared.options.four_state && signal.is_4state {
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
        self.shared.layout.working_offsets.contains_key(addr)
    }

    /// Set 4-state value for a variable using a pre-resolved [`SignalRef`].
    ///
    /// Uses IEEE 1800 encoding:
    /// - `(v=0, m=0)` → 0
    /// - `(v=1, m=0)` → 1
    /// - `(v=1, m=1)` → X (unknown)
    /// - `(v=0, m=1)` → Z (high-impedance)
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

            if self.shared.options.four_state && signal.is_4state {
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
    ///
    /// Returns `(value, mask)` using IEEE 1800 encoding:
    /// - `(v=0, m=0)` → 0
    /// - `(v=1, m=0)` → 1
    /// - `(v=1, m=1)` → X (unknown)
    /// - `(v=0, m=1)` → Z (high-impedance)
    pub fn get_four_state(&self, signal: SignalRef) -> (BigUint, BigUint) {
        let byte_size = get_byte_size(signal.width);
        let v_ptr: *const u8 = unsafe { (self.memory.as_ptr() as *const u8).add(signal.offset) };
        let v_slice = unsafe { std::slice::from_raw_parts(v_ptr, byte_size) };
        let mut v_val = BigUint::from_bytes_le(v_slice);

        let mut m_val = if self.shared.options.four_state && signal.is_4state {
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
        self.shared.event_map[addr]
    }

    pub fn resolve_event_opt(&self, addr: &AbsoluteAddr) -> Option<EventRef> {
        self.shared.event_map.get(addr).copied()
    }

    pub fn resolve_eval_only_event(&self, addr: &AbsoluteAddr) -> Option<EventRef> {
        self.shared.eval_only_event_map.get(addr).copied()
    }

    pub fn resolve_apply_event(&self, addr: &AbsoluteAddr) -> Option<EventRef> {
        self.shared.apply_event_map.get(addr).copied()
    }

    pub fn eval_apply_ff_at(&mut self, event: EventRef) -> Result<(), SimulatorErrorCode> {
        self.run_sim_func(event.func)
    }

    pub fn eval_apply_ff_and_comb_at(
        &mut self,
        merged_func: SimFunc,
    ) -> Result<(), SimulatorErrorCode> {
        self.run_sim_func(merged_func)
    }

    pub fn eval_only_ff_at(&mut self, event: EventRef) -> Result<(), SimulatorErrorCode> {
        self.run_sim_func(event.func)
    }

    pub fn apply_ff_at(&mut self, event: EventRef) -> Result<(), SimulatorErrorCode> {
        self.run_sim_func(event.func)
    }

    /// Returns a raw pointer to the JIT memory and its total size in bytes.
    pub fn memory_as_ptr(&self) -> (*const u8, usize) {
        let size = self.shared.layout.merged_total_size;
        (self.memory.as_ptr() as *const u8, size)
    }

    /// Returns a mutable raw pointer to the JIT memory and its total size in bytes.
    pub fn memory_as_mut_ptr(&mut self) -> (*mut u8, usize) {
        let size = self.shared.layout.merged_total_size;
        (self.memory.as_mut_ptr() as *mut u8, size)
    }

    /// Returns the stable region size in bytes.
    pub fn stable_region_size(&self) -> usize {
        self.shared.layout.total_size
    }

    /// Returns a reference to the memory layout.
    pub fn layout(&self) -> &MemoryLayout {
        &self.shared.layout
    }

    /// Returns the `id_to_addr` mapping (event ID → AbsoluteAddr).
    pub fn id_to_addr_slice(&self) -> &[AbsoluteAddr] {
        &self.shared.id_to_addr
    }

    /// Returns the `id_to_event` mapping (event ID → EventRef).
    pub fn id_to_event_slice(&self) -> &[EventRef] {
        &self.shared.id_to_event
    }

    pub fn num_events(&self) -> usize {
        let mut max_id = 0;
        for ev in self.shared.event_map.values() {
            max_id = max_id.max(ev.id);
        }
        for ev in self.shared.eval_only_event_map.values() {
            max_id = max_id.max(ev.id);
        }
        for ev in self.shared.apply_event_map.values() {
            max_id = max_id.max(ev.id);
        }
        if self.shared.event_map.is_empty()
            && self.shared.eval_only_event_map.is_empty()
            && self.shared.apply_event_map.is_empty()
        {
            0
        } else {
            max_id + 1
        }
    }

    /// Clears the triggered bits bitset in JIT memory.
    pub fn clear_triggered_bits(&mut self) {
        let base_ptr = self.memory.as_mut_ptr() as *mut u8;
        let triggered_bits_ptr = unsafe { base_ptr.add(self.shared.layout.triggered_bits_offset) };
        let total_size = self.shared.layout.triggered_bits_total_size;
        unsafe {
            std::ptr::write_bytes(triggered_bits_ptr, 0, total_size);
        }
    }

    /// Manually marks a trigger bit as triggered in JIT memory.
    pub fn mark_triggered_bit(&mut self, id: usize) {
        let byte_idx = id / 8;
        let bit_idx = id % 8;
        let base_ptr = self.memory.as_mut_ptr() as *mut u8;
        let triggered_bits_ptr = unsafe { base_ptr.add(self.shared.layout.triggered_bits_offset) };
        unsafe {
            let byte_ptr = triggered_bits_ptr.add(byte_idx);
            *byte_ptr |= 1 << bit_idx;
        }
    }

    /// Reads back the triggered bits bitset and returns it as a BitSet.
    pub fn get_triggered_bits(&self) -> bit_set::BitSet {
        let mut bits = bit_set::BitSet::with_capacity(self.num_events());
        let base_ptr = self.memory.as_ptr() as *const u8;
        let triggered_bits_ptr = unsafe { base_ptr.add(self.shared.layout.triggered_bits_offset) };
        let total_size = self.shared.layout.triggered_bits_total_size;

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

impl super::SimBackend for JitBackend {
    type Event = EventRef;

    fn eval_comb(&mut self) -> Result<(), SimulatorErrorCode> {
        self.eval_comb()
    }

    fn eval_apply_ff_and_comb(&mut self, event: EventRef) -> Result<(), SimulatorErrorCode> {
        if let Some(merged) = event.merged_func {
            self.eval_apply_ff_and_comb_at(merged)
        } else {
            self.eval_apply_ff_at(event)?;
            self.eval_comb()
        }
    }

    fn eval_apply_ff_at(&mut self, event: EventRef) -> Result<(), SimulatorErrorCode> {
        self.eval_apply_ff_at(event)
    }

    fn eval_only_ff_at(&mut self, event: EventRef) -> Result<(), SimulatorErrorCode> {
        self.eval_only_ff_at(event)
    }

    fn apply_ff_at(&mut self, event: EventRef) -> Result<(), SimulatorErrorCode> {
        self.apply_ff_at(event)
    }

    fn resolve_signal(&self, addr: &AbsoluteAddr) -> SignalRef {
        self.resolve_signal(addr)
    }

    fn resolve_event(&self, addr: &AbsoluteAddr) -> EventRef {
        self.resolve_event(addr)
    }

    fn resolve_event_opt(&self, addr: &AbsoluteAddr) -> Option<EventRef> {
        self.resolve_event_opt(addr)
    }

    fn resolve_eval_only_event(&self, addr: &AbsoluteAddr) -> Option<EventRef> {
        self.resolve_eval_only_event(addr)
    }

    fn resolve_apply_event(&self, addr: &AbsoluteAddr) -> Option<EventRef> {
        self.resolve_apply_event(addr)
    }

    fn set<T: Copy>(&mut self, signal: SignalRef, val: T) {
        self.set(signal, val)
    }

    fn set_wide(&mut self, signal: SignalRef, val: BigUint) {
        self.set_wide(signal, val)
    }

    fn set_four_state(&mut self, signal: SignalRef, val: BigUint, mask: BigUint) {
        self.set_four_state(signal, val, mask)
    }

    fn get(&self, signal: SignalRef) -> BigUint {
        self.get(signal)
    }

    fn get_as<T: Default + Copy>(&self, signal: SignalRef) -> T {
        self.get_as(signal)
    }

    fn get_four_state(&self, signal: SignalRef) -> (BigUint, BigUint) {
        self.get_four_state(signal)
    }

    fn memory_as_ptr(&self) -> (*const u8, usize) {
        self.memory_as_ptr()
    }

    fn memory_as_mut_ptr(&mut self) -> (*mut u8, usize) {
        self.memory_as_mut_ptr()
    }

    fn stable_region_size(&self) -> usize {
        self.stable_region_size()
    }

    fn layout(&self) -> &super::MemoryLayout {
        self.layout()
    }

    fn id_to_addr_slice(&self) -> &[AbsoluteAddr] {
        self.id_to_addr_slice()
    }

    fn id_to_event_slice(&self) -> &[EventRef] {
        self.id_to_event_slice()
    }

    fn num_events(&self) -> usize {
        self.num_events()
    }

    fn clear_triggered_bits(&mut self) {
        self.clear_triggered_bits()
    }

    fn mark_triggered_bit(&mut self, id: usize) {
        self.mark_triggered_bit(id)
    }

    fn get_triggered_bits(&self) -> bit_set::BitSet {
        self.get_triggered_bits()
    }
}
