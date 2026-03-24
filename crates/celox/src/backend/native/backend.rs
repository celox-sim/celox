//! NativeBackend: SimBackend implementation using the custom x86-64 backend.
//!
//! Mirrors the structure of JitBackend but compiles through
//! ISel → MIR → regalloc → x86-64 emit instead of Cranelift.

use std::sync::Arc;

use bit_set::BitSet;
use num_bigint::BigUint;

use crate::ir::{AbsoluteAddr, Program, SignalRef};
use crate::{HashMap, SimulatorError, SimulatorOptions};

use super::super::{MemoryLayout, get_byte_size};
use super::super::traits::SimulatorErrorCode;
use super::{emit, isel, jit_mem, regalloc};

// ────────────────────────────────────────────────────────────────
// Event handle
// ────────────────────────────────────────────────────────────────

/// JIT function type: `fn(state: *mut u8) -> i64`
pub type NativeSimFunc = unsafe extern "sysv64" fn(*mut u8) -> i64;

/// Compiled event handle for native backend.
/// Holds the function pointer directly — no indirection at call time.
#[derive(Clone, Copy)]
pub struct NativeEventRef {
    pub func: NativeSimFunc,
    pub addr: AbsoluteAddr,
    pub id: usize,
}

impl std::fmt::Debug for NativeEventRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeEventRef")
            .field("func", &(self.func as usize))
            .field("addr", &self.addr)
            .field("id", &self.id)
            .finish()
    }
}

impl super::super::EventHandle for NativeEventRef {
    fn id(&self) -> usize {
        self.id
    }
    fn addr(&self) -> AbsoluteAddr {
        self.addr
    }
}

// ────────────────────────────────────────────────────────────────
// Shared compiled code
// ────────────────────────────────────────────────────────────────

struct CompiledCode {
    comb_func: NativeSimFunc,
    /// Keep JitCode alive so the mmap regions remain valid.
    _jit_codes: Vec<jit_mem::JitCode>,

    event_map: HashMap<AbsoluteAddr, NativeEventRef>,
    eval_only_event_map: HashMap<AbsoluteAddr, NativeEventRef>,
    apply_event_map: HashMap<AbsoluteAddr, NativeEventRef>,
    id_to_addr: Vec<AbsoluteAddr>,
    id_to_event: Vec<NativeEventRef>,
    layout: MemoryLayout,
}

// Safety: JitCode contains Mmap which is Send+Sync after creation.
unsafe impl Send for CompiledCode {}
unsafe impl Sync for CompiledCode {}

// ────────────────────────────────────────────────────────────────
// Compilation
// ────────────────────────────────────────────────────────────────

fn codegen_err(msg: String) -> SimulatorError {
    SimulatorError::new(crate::simulator::SimulatorErrorKind::Codegen(msg))
}

fn compile_units(
    units: &[crate::ir::ExecutionUnit<crate::ir::RegionedAbsoluteAddr>],
    layout: &MemoryLayout,
) -> Result<jit_mem::JitCode, SimulatorError> {
    if units.is_empty() {
        // Empty function: just return 0
        let mut empty_func = super::mir::MFunction::new(super::mir::VRegAllocator::new(), vec![]);
        let mut block = super::mir::MBlock::new(super::mir::BlockId(0));
        block.push(super::mir::MInst::Return);
        empty_func.push_block(block);
        let empty_result = emit::emit(&empty_func, &regalloc::AssignmentMap::default(), 0)
            .map_err(|e| codegen_err(format!("emit error: {e}")))?;
        return jit_mem::JitCode::new(&empty_result.code)
            .map_err(|e| codegen_err(format!("mmap error: {e}")));
    }

    // For now, compile only the first EU (multi-EU support needs chaining)
    let eu = &units[0];
    let mut mfunc = isel::lower_execution_unit(eu, layout);
    let regalloc_result = regalloc::run_regalloc(&mut mfunc);
    let emit_result = emit::emit(&mfunc, &regalloc_result.assignment, regalloc_result.spill_frame_size)
        .map_err(|e| codegen_err(format!("emit error: {e}")))?;
    jit_mem::JitCode::new(&emit_result.code)
        .map_err(|e| codegen_err(format!("mmap error: {e}")))
}

fn compile_program(
    sir: &Program,
    _options: &SimulatorOptions,
) -> Result<CompiledCode, SimulatorError> {
    let layout = MemoryLayout::build(sir, false); // TODO: four_state support
    let mut all_jit_codes: Vec<jit_mem::JitCode> = Vec::new();

    // Compile eval_comb
    let comb_jit = compile_units(&sir.eval_comb, &layout)?;
    let comb_func = comb_jit.fn_ptr;
    all_jit_codes.push(comb_jit);

    // Compile FF units
    let mut next_id = 0usize;
    let mut id_to_addr = Vec::new();
    let mut id_to_event = Vec::new();
    let mut event_map = HashMap::default();
    let mut eval_only_event_map = HashMap::default();
    let mut apply_event_map = HashMap::default();

    let compile_ff_group = |ff_map: &HashMap<
        AbsoluteAddr,
        Vec<crate::ir::ExecutionUnit<crate::ir::RegionedAbsoluteAddr>>,
    >,
                                 all_codes: &mut Vec<jit_mem::JitCode>,
                                 event_map_out: &mut HashMap<AbsoluteAddr, NativeEventRef>,
                                 next_id: &mut usize,
                                 id_to_addr: &mut Vec<AbsoluteAddr>,
                                 id_to_event: &mut Vec<NativeEventRef>|
     -> Result<(), SimulatorError> {
        for (addr, units) in ff_map {
            let code = compile_units(units, &layout)?;
            let func = code.fn_ptr;
            all_codes.push(code);

            let id = *next_id;
            *next_id += 1;

            let event = NativeEventRef {
                func,
                addr: *addr,
                id,
            };
            event_map_out.insert(*addr, event);

            if !id_to_addr.contains(addr) {
                id_to_addr.push(*addr);
                id_to_event.push(event);
            }
        }
        Ok(())
    };

    compile_ff_group(
        &sir.eval_apply_ffs, &mut all_jit_codes, &mut event_map,
        &mut next_id, &mut id_to_addr, &mut id_to_event,
    )?;
    compile_ff_group(
        &sir.eval_only_ffs, &mut all_jit_codes, &mut eval_only_event_map,
        &mut next_id, &mut id_to_addr, &mut id_to_event,
    )?;
    compile_ff_group(
        &sir.apply_ffs, &mut all_jit_codes, &mut apply_event_map,
        &mut next_id, &mut id_to_addr, &mut id_to_event,
    )?;

    Ok(CompiledCode {
        comb_func,
        _jit_codes: all_jit_codes,
        event_map,
        eval_only_event_map,
        apply_event_map,
        id_to_addr,
        id_to_event,
        layout,
    })
}

// ────────────────────────────────────────────────────────────────
// NativeBackend
// ────────────────────────────────────────────────────────────────

pub struct NativeBackend {
    compiled: Arc<CompiledCode>,
    memory: Vec<u64>,
}

impl NativeBackend {
    pub fn new(
        sir: &Program,
        options: &SimulatorOptions,
    ) -> Result<Self, SimulatorError> {
        let compiled = Arc::new(compile_program(sir, options)?);
        let mem_size_words =
            (compiled.layout.merged_total_size + compiled.layout.triggered_bits_total_size + 7) / 8;
        let memory = vec![0u64; mem_size_words + 1]; // +1 for safety
        Ok(Self { compiled, memory })
    }

    fn mem_ptr(&self) -> *const u8 {
        self.memory.as_ptr() as *const u8
    }

    fn mem_mut_ptr(&mut self) -> *mut u8 {
        self.memory.as_mut_ptr() as *mut u8
    }

    fn mem_bytes(&self) -> &[u8] {
        let ptr = self.mem_ptr();
        let len = self.memory.len() * 8;
        unsafe { std::slice::from_raw_parts(ptr, len) }
    }

    fn mem_bytes_mut(&mut self) -> &mut [u8] {
        let ptr = self.mem_mut_ptr();
        let len = self.memory.len() * 8;
        unsafe { std::slice::from_raw_parts_mut(ptr, len) }
    }

    fn call_func(
        memory: &mut [u64],
        func: NativeSimFunc,
    ) -> Result<(), SimulatorErrorCode> {
        let ptr = memory.as_mut_ptr() as *mut u8;
        let ret = unsafe { func(ptr) };
        if ret == 0 {
            Ok(())
        } else {
            Err(SimulatorErrorCode::InternalError)
        }
    }
}

impl super::super::SimBackend for NativeBackend {
    type Event = NativeEventRef;

    fn eval_comb(&mut self) -> Result<(), SimulatorErrorCode> {
        Self::call_func(&mut self.memory, self.compiled.comb_func)
    }

    fn eval_apply_ff_and_comb(
        &mut self,
        event: NativeEventRef,
    ) -> Result<(), SimulatorErrorCode> {
        Self::call_func(&mut self.memory, event.func)?;
        Self::call_func(&mut self.memory, self.compiled.comb_func)
    }

    fn eval_apply_ff_at(&mut self, event: NativeEventRef) -> Result<(), SimulatorErrorCode> {
        Self::call_func(&mut self.memory, event.func)
    }

    fn eval_only_ff_at(&mut self, event: NativeEventRef) -> Result<(), SimulatorErrorCode> {
        Self::call_func(&mut self.memory, event.func)
    }

    fn apply_ff_at(&mut self, event: NativeEventRef) -> Result<(), SimulatorErrorCode> {
        Self::call_func(&mut self.memory, event.func)
    }

    fn resolve_signal(&self, addr: &AbsoluteAddr) -> SignalRef {
        let layout = &self.compiled.layout;
        let offset = layout.offsets.get(addr).copied().unwrap_or(0);
        let width = layout.widths.get(addr).copied().unwrap_or(0);
        SignalRef {
            offset,
            width,
            is_4state: false,
        }
    }

    fn resolve_event(&self, addr: &AbsoluteAddr) -> NativeEventRef {
        *self
            .compiled
            .event_map
            .get(addr)
            .unwrap_or_else(|| panic!("event not found for {:?}", addr))
    }

    fn resolve_event_opt(&self, addr: &AbsoluteAddr) -> Option<NativeEventRef> {
        self.compiled.event_map.get(addr).copied()
    }

    fn resolve_eval_only_event(&self, addr: &AbsoluteAddr) -> Option<NativeEventRef> {
        self.compiled.eval_only_event_map.get(addr).copied()
    }

    fn resolve_apply_event(&self, addr: &AbsoluteAddr) -> Option<NativeEventRef> {
        self.compiled.apply_event_map.get(addr).copied()
    }

    fn set<T: Copy>(&mut self, signal: SignalRef, val: T) {
        let bs = get_byte_size(signal.width);
        let bytes = self.mem_bytes_mut();
        let val_bytes =
            unsafe { std::slice::from_raw_parts(&val as *const T as *const u8, std::mem::size_of::<T>()) };
        let copy_len = val_bytes.len().min(bs);
        bytes[signal.offset..signal.offset + copy_len].copy_from_slice(&val_bytes[..copy_len]);
    }

    fn set_wide(&mut self, signal: SignalRef, val: BigUint) {
        let bs = get_byte_size(signal.width);
        let bytes = self.mem_bytes_mut();
        let val_bytes = val.to_bytes_le();
        let copy_len = val_bytes.len().min(bs);
        bytes[signal.offset..signal.offset + bs].fill(0);
        bytes[signal.offset..signal.offset + copy_len].copy_from_slice(&val_bytes[..copy_len]);
    }

    fn set_four_state(&mut self, signal: SignalRef, val: BigUint, _mask: BigUint) {
        self.set_wide(signal, val);
    }

    fn get(&self, signal: SignalRef) -> BigUint {
        let bs = get_byte_size(signal.width);
        let bytes = self.mem_bytes();
        BigUint::from_bytes_le(&bytes[signal.offset..signal.offset + bs])
    }

    fn get_as<T: Default + Copy>(&self, signal: SignalRef) -> T {
        let bs = get_byte_size(signal.width);
        let bytes = self.mem_bytes();
        let mut val = T::default();
        let val_bytes = unsafe {
            std::slice::from_raw_parts_mut(&mut val as *mut T as *mut u8, std::mem::size_of::<T>())
        };
        let copy_len = val_bytes.len().min(bs);
        val_bytes[..copy_len].copy_from_slice(&bytes[signal.offset..signal.offset + copy_len]);
        val
    }

    fn get_four_state(&self, signal: SignalRef) -> (BigUint, BigUint) {
        (self.get(signal), BigUint::from(0u32))
    }

    fn memory_as_ptr(&self) -> (*const u8, usize) {
        (self.mem_ptr(), self.memory.len() * 8)
    }

    fn memory_as_mut_ptr(&mut self) -> (*mut u8, usize) {
        (self.mem_mut_ptr(), self.memory.len() * 8)
    }

    fn stable_region_size(&self) -> usize {
        self.compiled.layout.total_size
    }

    fn layout(&self) -> &MemoryLayout {
        &self.compiled.layout
    }

    fn id_to_addr_slice(&self) -> &[AbsoluteAddr] {
        &self.compiled.id_to_addr
    }

    fn id_to_event_slice(&self) -> &[NativeEventRef] {
        &self.compiled.id_to_event
    }

    fn num_events(&self) -> usize {
        self.compiled.id_to_event.len()
    }

    fn clear_triggered_bits(&mut self) {
        let offset = self.compiled.layout.triggered_bits_offset;
        let size = self.compiled.layout.triggered_bits_total_size;
        let bytes = self.mem_bytes_mut();
        bytes[offset..offset + size].fill(0);
    }

    fn mark_triggered_bit(&mut self, id: usize) {
        let offset = self.compiled.layout.triggered_bits_offset;
        let byte_idx = offset + id / 8;
        let bit_idx = id % 8;
        self.mem_bytes_mut()[byte_idx] |= 1 << bit_idx;
    }

    fn get_triggered_bits(&self) -> BitSet {
        let offset = self.compiled.layout.triggered_bits_offset;
        let size = self.compiled.layout.triggered_bits_total_size;
        let bytes = self.mem_bytes();
        let mut bs = BitSet::with_capacity(size * 8);
        for i in 0..size * 8 {
            let byte_idx = offset + i / 8;
            let bit_idx = i % 8;
            if bytes[byte_idx] & (1 << bit_idx) != 0 {
                bs.insert(i);
            }
        }
        bs
    }
}
