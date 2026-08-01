//! Self-contained x86-64 code-generation kernel.
//!
//! The crate consumes source-independent SIR plus a finalized physical state
//! layout. It does not know about the Veryl frontend, simulator facade, or
//! testbench runtime.

pub type HashMap<K, V> = fxhash::FxHashMap<K, V>;
pub type HashSet<K> = fxhash::FxHashSet<K>;

#[derive(Debug, Clone, Copy)]
pub struct X86BackendOptions {
    pub slp: bool,
}

impl Default for X86BackendOptions {
    fn default() -> Self {
        Self { slp: true }
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

pub mod timing {
    pub fn now() -> std::time::Instant {
        std::time::Instant::now()
    }
}

#[path = "backend/native/mod.rs"]
pub mod native;

pub use native::*;
