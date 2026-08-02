//! Self-contained x86-64 code-generation kernel.
//!
//! The crate consumes source-independent SIR plus a finalized physical state
//! layout. It does not know about the Veryl frontend, simulator facade, or
//! testbench runtime.

pub type HashMap<K, V> = fxhash::FxHashMap<K, V>;
pub type HashSet<K> = fxhash::FxHashSet<K>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeDumpOptions {
    pub block: usize,
    pub label: Option<String>,
    pub stage: Option<String>,
    pub dump_sir: bool,
    pub mir_limit: usize,
}

impl Default for NativeDumpOptions {
    fn default() -> Self {
        Self {
            block: 0,
            label: None,
            stage: None,
            dump_sir: true,
            mir_limit: 64,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NativeDiagnostics {
    pub phase_timing: bool,
    pub regalloc_timing: bool,
    pub regalloc_stats: bool,
    pub mir_stats: bool,
    pub mir_block_stats: bool,
    pub verify_sir: bool,
    pub verify_mir: bool,
    pub verify_mir_passes: bool,
    pub verify_regalloc: bool,
    pub isel_trace_regs: Vec<usize>,
    pub dump: Option<NativeDumpOptions>,
    pub perf_map: bool,
}

#[derive(Debug, Clone)]
pub struct X86BackendOptions {
    pub slp: bool,
    pub native_tick_loop: bool,
    pub diagnostics: NativeDiagnostics,
}

impl Default for X86BackendOptions {
    fn default() -> Self {
        Self {
            slp: true,
            native_tick_loop: true,
            diagnostics: NativeDiagnostics::default(),
        }
    }
}

pub use celox_design::{
    AbsoluteAddrBase, BinaryOp, DomainKind, InstanceId, RegionedAbsoluteAddrBase, RuntimeEventKind,
    SPARSE_WORKING_REGION, STABLE_REGION, StateAddr, TriggerIdWithKind, UnaryOp, WORKING_REGION,
};
pub use celox_sir::*;

pub type AbsoluteAddr = celox_design::StateAddr;
pub type RegionedAbsoluteAddr = celox_design::RegionedStateAddr;
pub type SirProgram = celox_sir::SirProgram<AbsoluteAddr, RegionedAbsoluteAddr>;
pub type MemoryLayout = celox_state_layout::MemoryLayout<AbsoluteAddr>;

/// Remaining iterations for an in-function native tick loop.
pub const STATE_HEADER_NATIVE_LOOP_REMAINING_OFFSET: usize = 8;
/// Runtime-event sequence observed when a native tick batch starts.
pub const STATE_HEADER_NATIVE_LOOP_EVENT_SEQ_OFFSET: usize = 24;

const _: () = {
    assert!(
        celox_state_layout::STATE_HEADER_RUNTIME_EVENT_ADDR_OFFSET + 8
            <= STATE_HEADER_NATIVE_LOOP_REMAINING_OFFSET
    );
    assert!(
        STATE_HEADER_NATIVE_LOOP_REMAINING_OFFSET + 8
            <= celox_state_layout::STATE_HEADER_COMB_CAPTURE_ENABLED_ADDR_OFFSET
    );
    assert!(
        celox_state_layout::STATE_HEADER_COMB_CAPTURE_ENABLED_ADDR_OFFSET + 8
            <= STATE_HEADER_NATIVE_LOOP_EVENT_SEQ_OFFSET
    );
    assert!(STATE_HEADER_NATIVE_LOOP_EVENT_SEQ_OFFSET + 8 <= celox_state_layout::STATE_HEADER_SIZE);
};

pub mod timing {
    pub fn now() -> std::time::Instant {
        std::time::Instant::now()
    }
}

#[path = "backend/native/mod.rs"]
pub mod native;

pub use native::*;
