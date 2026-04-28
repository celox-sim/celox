//! Wasmtime-based WASM backend for simulation.
//!
//! Compiles SIRT → WASM bytecode via [`wasm_codegen`], then instantiates and
//! executes the generated modules with Wasmtime. Provides the same interface
//! as [`JitBackend`] so that [`Simulator`] can use either backend.

use num_bigint::BigUint;
use wasmtime::{Engine, Linker, Memory, Module, Store, TypedFunc};

use crate::{
    HashMap, SimulatorOptions,
    backend::{MemoryLayout, SimulatorErrorCode, get_byte_size},
    ir::{AbsoluteAddr, Program, SignalRef},
};

use super::wasm_codegen;

/// Opaque handle to a compiled WASM event (clock/async-reset) function.
#[derive(Clone, Copy)]
pub struct WasmEventRef {
    /// Index into the `event_instances` vec in `SharedWasmCode`.
    instance_idx: usize,
    pub addr: AbsoluteAddr,
    pub id: usize,
}

impl std::fmt::Debug for WasmEventRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmEventRef")
            .field("instance_idx", &self.instance_idx)
            .field("addr", &self.addr)
            .field("id", &self.id)
            .finish()
    }
}

impl super::traits::EventHandle for WasmEventRef {
    fn id(&self) -> usize {
        self.id
    }
    fn addr(&self) -> AbsoluteAddr {
        self.addr
    }
}

impl super::traits::SimBackend for WasmBackend {
    type Event = WasmEventRef;

    fn eval_comb(&mut self) -> Result<(), super::SimulatorErrorCode> {
        WasmBackend::eval_comb(self)
    }
    fn eval_apply_ff_at(&mut self, event: WasmEventRef) -> Result<(), super::SimulatorErrorCode> {
        WasmBackend::eval_apply_ff_at(self, &event)
    }
    fn eval_only_ff_at(&mut self, event: WasmEventRef) -> Result<(), super::SimulatorErrorCode> {
        WasmBackend::eval_only_ff_at(self, &event)
    }
    fn apply_ff_at(&mut self, event: WasmEventRef) -> Result<(), super::SimulatorErrorCode> {
        WasmBackend::apply_ff_at(self, &event)
    }
    fn resolve_signal(&self, addr: &AbsoluteAddr) -> SignalRef {
        WasmBackend::resolve_signal(self, addr)
    }
    fn resolve_event(&self, addr: &AbsoluteAddr) -> WasmEventRef {
        WasmBackend::resolve_event(self, addr)
    }
    fn resolve_event_opt(&self, addr: &AbsoluteAddr) -> Option<WasmEventRef> {
        WasmBackend::resolve_event_opt(self, addr)
    }
    fn resolve_eval_only_event(&self, addr: &AbsoluteAddr) -> Option<WasmEventRef> {
        WasmBackend::resolve_eval_only_event(self, addr)
    }
    fn resolve_apply_event(&self, addr: &AbsoluteAddr) -> Option<WasmEventRef> {
        WasmBackend::resolve_apply_event(self, addr)
    }
    fn set<T: Copy>(&mut self, signal: SignalRef, value: T) {
        WasmBackend::set(self, signal, value)
    }
    fn set_wide(&mut self, signal: SignalRef, value: BigUint) {
        WasmBackend::set_wide(self, signal, value)
    }
    fn set_four_state(&mut self, signal: SignalRef, value: BigUint, mask: BigUint) {
        WasmBackend::set_four_state(self, signal, value, mask)
    }
    fn get(&self, signal: SignalRef) -> BigUint {
        WasmBackend::get(self, signal)
    }
    fn get_as<T: Default + Copy>(&self, signal: SignalRef) -> T {
        WasmBackend::get_as(self, signal)
    }
    fn get_four_state(&self, signal: SignalRef) -> (BigUint, BigUint) {
        WasmBackend::get_four_state(self, signal)
    }
    fn memory_as_ptr(&self) -> (*const u8, usize) {
        WasmBackend::memory_as_ptr(self)
    }
    fn memory_as_mut_ptr(&mut self) -> (*mut u8, usize) {
        WasmBackend::memory_as_mut_ptr(self)
    }
    fn stable_region_size(&self) -> usize {
        WasmBackend::stable_region_size(self)
    }
    fn layout(&self) -> &super::MemoryLayout {
        WasmBackend::layout(self)
    }
    fn id_to_addr_slice(&self) -> &[AbsoluteAddr] {
        WasmBackend::id_to_addr_slice(self)
    }
    fn id_to_event_slice(&self) -> &[WasmEventRef] {
        WasmBackend::id_to_event_slice(self)
    }
    fn num_events(&self) -> usize {
        WasmBackend::num_events(self)
    }
    fn clear_triggered_bits(&mut self) {
        WasmBackend::clear_triggered_bits(self)
    }
    fn mark_triggered_bit(&mut self, id: usize) {
        WasmBackend::mark_triggered_bit(self, id)
    }
    fn get_triggered_bits(&self) -> bit_set::BitSet {
        WasmBackend::get_triggered_bits(self)
    }
}

/// The runtime WASM backend.
pub struct WasmBackend {
    store: Store<()>,
    memory: Memory,
    comb_func: TypedFunc<(), i64>,
    event_funcs: HashMap<AbsoluteAddr, Vec<TypedFunc<(), i64>>>,
    eval_only_funcs: HashMap<AbsoluteAddr, Vec<TypedFunc<(), i64>>>,
    apply_funcs: HashMap<AbsoluteAddr, Vec<TypedFunc<(), i64>>>,
    event_map: HashMap<AbsoluteAddr, WasmEventRef>,
    eval_only_event_map: HashMap<AbsoluteAddr, WasmEventRef>,
    apply_event_map: HashMap<AbsoluteAddr, WasmEventRef>,
    id_to_addr: Vec<AbsoluteAddr>,
    id_to_event: Vec<WasmEventRef>,
    layout: MemoryLayout,
    options: SimulatorOptions,
}

impl WasmBackend {
    pub fn new(sir: &Program, options: &SimulatorOptions) -> Result<Self, crate::SimulatorError> {
        let engine = Engine::default();
        let layout = sir
            .layout
            .as_ref()
            .expect("layout must be built before backend")
            .clone();

        // Compile eval_comb
        let comb_wasm = wasm_codegen::compile_units(
            &sir.eval_comb,
            &layout,
            options.four_state,
            options.emit_triggers,
        );
        let comb_module = Module::new(&engine, &comb_wasm.bytes).map_err(|e| {
            crate::SimulatorError::from(format!("WASM compilation failed (eval_comb): {e:?}"))
        })?;

        // Compile event functions
        let mut event_modules = Vec::new();
        let mut eval_only_modules = Vec::new();
        let mut apply_modules = Vec::new();
        let mut event_map = HashMap::default();
        let mut eval_only_event_map = HashMap::default();
        let mut apply_event_map = HashMap::default();
        let mut id_to_addr = Vec::new();
        let mut addr_to_id = HashMap::default();
        let mut next_id = 0usize;

        let compile_ffs = |ff_map: &HashMap<
            AbsoluteAddr,
            Vec<crate::ir::ExecutionUnit<crate::ir::RegionedAbsoluteAddr>>,
        >,
                           modules: &mut Vec<(AbsoluteAddr, Module)>,
                           emap: &mut HashMap<AbsoluteAddr, WasmEventRef>,
                           addr_to_id: &mut HashMap<AbsoluteAddr, usize>,
                           next_id: &mut usize,
                           id_to_addr: &mut Vec<AbsoluteAddr>|
         -> Result<(), crate::SimulatorError> {
            for (clock, units) in ff_map {
                let canonical = sir.clock_domains.get(clock).copied().unwrap_or(*clock);
                let id = *addr_to_id.entry(canonical).or_insert_with(|| {
                    let id = *next_id;
                    *next_id += 1;
                    id_to_addr.push(canonical);
                    id
                });

                let wasm = wasm_codegen::compile_units(
                    units,
                    &layout,
                    options.four_state,
                    options.emit_triggers,
                );
                let module = Module::new(&engine, &wasm.bytes).map_err(|e| {
                    crate::SimulatorError::from(format!("WASM compilation failed (ff): {e:?}"))
                })?;
                let idx = modules.len();
                modules.push((canonical, module));
                emap.insert(
                    canonical,
                    WasmEventRef {
                        instance_idx: idx,
                        addr: canonical,
                        id,
                    },
                );
            }
            Ok(())
        };

        compile_ffs(
            &sir.eval_apply_ffs,
            &mut event_modules,
            &mut event_map,
            &mut addr_to_id,
            &mut next_id,
            &mut id_to_addr,
        )?;
        compile_ffs(
            &sir.eval_only_ffs,
            &mut eval_only_modules,
            &mut eval_only_event_map,
            &mut addr_to_id,
            &mut next_id,
            &mut id_to_addr,
        )?;
        compile_ffs(
            &sir.apply_ffs,
            &mut apply_modules,
            &mut apply_event_map,
            &mut addr_to_id,
            &mut next_id,
            &mut id_to_addr,
        )?;

        // Insert clock domain aliases
        for (alias, canonical) in &sir.clock_domains {
            if let Some(ev) = event_map.get(canonical).cloned() {
                event_map.insert(*alias, ev);
            }
            if let Some(ev) = eval_only_event_map.get(canonical).cloned() {
                eval_only_event_map.insert(*alias, ev);
            }
            if let Some(ev) = apply_event_map.get(canonical).cloned() {
                apply_event_map.insert(*alias, ev);
            }
        }

        // Pre-compute 4-state init regions
        let mut four_state_inits = Vec::new();
        if options.four_state {
            for (addr, &offset) in &layout.offsets {
                let width = layout.widths[addr];
                let is_4state = sir.module_variables[&sir.instance_module[&addr.instance_id]]
                    .get(&addr.var_id)
                    .map(|v| v.is_4state)
                    .unwrap_or(false);
                if is_4state {
                    four_state_inits.push((offset, get_byte_size(width)));
                }
            }
            for (addr, &rel_offset) in &layout.working_offsets {
                let offset = layout.working_base_offset + rel_offset;
                let width = layout.widths[addr];
                let is_4state = sir.module_variables[&sir.instance_module[&addr.instance_id]]
                    .get(&addr.var_id)
                    .map(|v| v.is_4state)
                    .unwrap_or(false);
                if is_4state {
                    four_state_inits.push((offset, get_byte_size(width)));
                }
            }
        }

        // Create store and shared memory
        let mut store = Store::new(&engine, ());
        let mem_pages = layout.merged_total_size.div_ceil(65536) as u64;
        let mem_pages = mem_pages.max(1);
        let memory = Memory::new(
            &mut store,
            wasmtime::MemoryType::new(mem_pages as u32, None),
        )
        .map_err(|e| crate::SimulatorError::from(format!("WASM memory creation failed: {e}")))?;

        // Initialize 4-state regions to X
        {
            let mem_data = memory.data_mut(&mut store);
            for &(offset, allocated_size) in &four_state_inits {
                // value bytes = 0xFF, mask bytes = 0xFF
                for i in 0..allocated_size {
                    if offset + i < mem_data.len() {
                        mem_data[offset + i] = 0xFF;
                    }
                    if offset + allocated_size + i < mem_data.len() {
                        mem_data[offset + allocated_size + i] = 0xFF;
                    }
                }
            }
        }

        // Helper: instantiate a WASM module with the shared memory.
        fn instantiate_module(
            engine: &Engine,
            store: &mut Store<()>,
            module: &Module,
            memory: &Memory,
        ) -> Result<TypedFunc<(), i64>, crate::SimulatorError> {
            let mut linker = Linker::new(engine);
            linker
                .define(&mut *store, "env", "memory", *memory)
                .map_err(|e| format!("WASM linker error: {e}"))?;
            let instance = linker
                .instantiate(&mut *store, module)
                .map_err(|e| format!("WASM instantiation error: {e}"))?;
            let func = instance
                .get_typed_func::<(), i64>(&mut *store, "run")
                .map_err(|e| format!("WASM function not found: {e}"))?;
            Ok(func)
        }

        let comb_func = instantiate_module(&engine, &mut store, &comb_module, &memory)?;

        let mut event_funcs: HashMap<AbsoluteAddr, Vec<TypedFunc<(), i64>>> = HashMap::default();
        for (addr, module) in &event_modules {
            let func = instantiate_module(&engine, &mut store, module, &memory)?;
            event_funcs.entry(*addr).or_default().push(func);
        }
        let mut eval_only_funcs: HashMap<AbsoluteAddr, Vec<TypedFunc<(), i64>>> =
            HashMap::default();
        for (addr, module) in &eval_only_modules {
            let func = instantiate_module(&engine, &mut store, module, &memory)?;
            eval_only_funcs.entry(*addr).or_default().push(func);
        }
        let mut apply_funcs: HashMap<AbsoluteAddr, Vec<TypedFunc<(), i64>>> = HashMap::default();
        for (addr, module) in &apply_modules {
            let func = instantiate_module(&engine, &mut store, module, &memory)?;
            apply_funcs.entry(*addr).or_default().push(func);
        }

        let id_to_event: Vec<WasmEventRef> =
            id_to_addr.iter().map(|addr| event_map[addr]).collect();

        Ok(Self {
            store,
            memory,
            comb_func,
            event_funcs,
            eval_only_funcs,
            apply_funcs,
            event_map,
            eval_only_event_map,
            apply_event_map,
            id_to_addr,
            id_to_event,
            layout,
            options: options.clone(),
        })
    }

    fn run_func(&mut self, func: &TypedFunc<(), i64>) -> Result<(), SimulatorErrorCode> {
        let res = func.call(&mut self.store, ()).unwrap_or(2);
        match res {
            0 => Ok(()),
            1 => Err(SimulatorErrorCode::DetectedTrueLoopCode(1)),
            code if code >= 2000 => Err(SimulatorErrorCode::DetectedTrueLoopCode(code)),
            _ => Err(SimulatorErrorCode::InternalError),
        }
    }

    pub fn eval_comb(&mut self) -> Result<(), SimulatorErrorCode> {
        let func = self.comb_func.clone();
        self.run_func(&func)
    }

    pub fn eval_apply_ff_at(&mut self, event: &WasmEventRef) -> Result<(), SimulatorErrorCode> {
        if let Some(funcs) = self.event_funcs.get(&event.addr).cloned() {
            for func in funcs {
                self.run_func(&func)?;
            }
        }
        Ok(())
    }

    pub fn eval_only_ff_at(&mut self, event: &WasmEventRef) -> Result<(), SimulatorErrorCode> {
        if let Some(funcs) = self.eval_only_funcs.get(&event.addr).cloned() {
            for func in funcs {
                self.run_func(&func)?;
            }
        }
        Ok(())
    }

    pub fn apply_ff_at(&mut self, event: &WasmEventRef) -> Result<(), SimulatorErrorCode> {
        if let Some(funcs) = self.apply_funcs.get(&event.addr).cloned() {
            for func in funcs {
                self.run_func(&func)?;
            }
        }
        Ok(())
    }

    pub fn resolve_signal(&self, addr: &AbsoluteAddr) -> SignalRef {
        let offset = self.layout.offsets[addr];
        let width = self.layout.widths[addr];
        let is_4state = self.layout.is_4states[addr];
        SignalRef {
            offset,
            width,
            is_4state,
        }
    }

    pub fn resolve_event(&self, addr: &AbsoluteAddr) -> WasmEventRef {
        self.event_map[addr]
    }

    pub fn resolve_event_opt(&self, addr: &AbsoluteAddr) -> Option<WasmEventRef> {
        self.event_map.get(addr).cloned()
    }

    pub fn resolve_eval_only_event(&self, addr: &AbsoluteAddr) -> Option<WasmEventRef> {
        self.eval_only_event_map.get(addr).cloned()
    }

    pub fn resolve_apply_event(&self, addr: &AbsoluteAddr) -> Option<WasmEventRef> {
        self.apply_event_map.get(addr).cloned()
    }

    pub fn set<T: Copy>(&mut self, signal: SignalRef, value: T) {
        let allocated_size = get_byte_size(signal.width);
        let provided_size = std::mem::size_of::<T>();
        assert!(provided_size <= allocated_size);

        let mem = self.memory.data_mut(&mut self.store);
        // Zero the allocated region
        for i in 0..allocated_size {
            mem[signal.offset + i] = 0;
        }
        // Write value
        let bytes =
            unsafe { std::slice::from_raw_parts(&value as *const T as *const u8, provided_size) };
        mem[signal.offset..signal.offset + provided_size].copy_from_slice(bytes);

        // Clear 4-state mask
        if self.options.four_state && signal.is_4state {
            for i in 0..allocated_size {
                mem[signal.offset + allocated_size + i] = 0;
            }
        }
    }

    pub fn set_wide(&mut self, signal: SignalRef, value: BigUint) {
        let allocated_size = get_byte_size(signal.width);
        let mut bytes = value.to_bytes_le();
        bytes.resize(allocated_size, 0u8);

        let mem = self.memory.data_mut(&mut self.store);
        mem[signal.offset..signal.offset + allocated_size]
            .copy_from_slice(&bytes[..allocated_size]);

        if self.options.four_state && signal.is_4state {
            for i in 0..allocated_size {
                mem[signal.offset + allocated_size + i] = 0;
            }
        }
    }

    pub fn set_four_state(&mut self, signal: SignalRef, value: BigUint, mask: BigUint) {
        let allocated_size = get_byte_size(signal.width);
        let mut v_bytes = value.to_bytes_le();
        v_bytes.resize(allocated_size, 0u8);

        let mem = self.memory.data_mut(&mut self.store);
        mem[signal.offset..signal.offset + allocated_size]
            .copy_from_slice(&v_bytes[..allocated_size]);

        if self.options.four_state && signal.is_4state {
            let mut m_bytes = mask.to_bytes_le();
            m_bytes.resize(allocated_size, 0u8);
            mem[signal.offset + allocated_size..signal.offset + 2 * allocated_size]
                .copy_from_slice(&m_bytes[..allocated_size]);
        }
    }

    pub fn get(&self, signal: SignalRef) -> BigUint {
        let byte_size = get_byte_size(signal.width);
        let mem = self.memory.data(&self.store);
        let slice = &mem[signal.offset..signal.offset + byte_size];
        let mut val = BigUint::from_bytes_le(slice);
        let extra_bits = byte_size * 8 - signal.width;
        if extra_bits > 0 {
            let mask = (BigUint::from(1u32) << signal.width) - 1u32;
            val &= mask;
        }
        val
    }

    pub fn get_as<T: Default + Copy>(&self, signal: SignalRef) -> T {
        let byte_size = get_byte_size(signal.width);
        let provided_size = std::mem::size_of::<T>();
        assert!(byte_size <= provided_size);

        let mem = self.memory.data(&self.store);
        let slice = &mem[signal.offset..signal.offset + byte_size];

        let mut val = T::default();
        unsafe {
            let val_ptr = &mut val as *mut T as *mut u8;
            std::ptr::copy_nonoverlapping(slice.as_ptr(), val_ptr, byte_size);
        }
        val
    }

    pub fn get_four_state(&self, signal: SignalRef) -> (BigUint, BigUint) {
        let byte_size = get_byte_size(signal.width);
        let mem = self.memory.data(&self.store);
        let v_slice = &mem[signal.offset..signal.offset + byte_size];
        let mut v_val = BigUint::from_bytes_le(v_slice);

        let mut m_val = if self.options.four_state && signal.is_4state {
            let m_slice = &mem[signal.offset + byte_size..signal.offset + 2 * byte_size];
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

    pub fn memory_as_ptr(&self) -> (*const u8, usize) {
        let data = self.memory.data(&self.store);
        (data.as_ptr(), self.layout.merged_total_size)
    }

    pub fn memory_as_mut_ptr(&mut self) -> (*mut u8, usize) {
        let data = self.memory.data_mut(&mut self.store);
        (data.as_mut_ptr(), self.layout.merged_total_size)
    }

    pub fn stable_region_size(&self) -> usize {
        self.layout.total_size
    }

    pub fn layout(&self) -> &MemoryLayout {
        &self.layout
    }

    pub fn id_to_addr_slice(&self) -> &[AbsoluteAddr] {
        &self.id_to_addr
    }

    pub fn id_to_event_slice(&self) -> &[WasmEventRef] {
        &self.id_to_event
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

    pub fn clear_triggered_bits(&mut self) {
        let offset = self.layout.triggered_bits_offset;
        let size = self.layout.triggered_bits_total_size;
        let mem = self.memory.data_mut(&mut self.store);
        for i in 0..size {
            mem[offset + i] = 0;
        }
    }

    pub fn mark_triggered_bit(&mut self, id: usize) {
        let byte_idx = id / 8;
        let bit_idx = id % 8;
        let offset = self.layout.triggered_bits_offset + byte_idx;
        let mem = self.memory.data_mut(&mut self.store);
        mem[offset] |= 1 << bit_idx;
    }

    pub fn get_triggered_bits(&self) -> bit_set::BitSet {
        let mut bits = bit_set::BitSet::with_capacity(self.num_events());
        let mem = self.memory.data(&self.store);
        let offset = self.layout.triggered_bits_offset;
        let size = self.layout.triggered_bits_total_size;

        for i in 0..size {
            let byte = mem[offset + i];
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

// Tests: crates/celox/tests/wasm_backend.rs
