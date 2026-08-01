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

/// Source-independent vocabulary consumed by instruction selection.
pub mod ir {
    pub use celox_design::{
        AbsoluteAddrBase, BinaryOp, DomainKind, InstanceId, RegionedAbsoluteAddrBase,
        RuntimeEventKind, SPARSE_WORKING_REGION, STABLE_REGION, StateAddr, TriggerIdWithKind,
        UnaryOp, WORKING_REGION,
    };
    pub use celox_sir::*;

    pub type AbsoluteAddr = celox_design::StateAddr;
    pub type RegionedAbsoluteAddr = celox_design::RegionedStateAddr;
    pub type SirProgram = celox_sir::SirProgram<AbsoluteAddr, RegionedAbsoluteAddr>;
}

pub mod timing {
    pub fn now() -> std::time::Instant {
        std::time::Instant::now()
    }
}

/// Compatibility namespace retained while the moved implementation is made
/// internally idiomatic. It contains backend contracts only.
pub mod backend {
    pub mod memory_layout {
        pub use celox_state_layout::*;
        pub type MemoryLayout = celox_state_layout::MemoryLayout<celox_design::StateAddr>;
    }

    pub use memory_layout::MemoryLayout;
    pub mod native;
}

pub use backend::native::*;

/// Test-only access to the SIR cleanup used by ISel regression fixtures.
#[cfg(test)]
pub mod optimizer {
    pub mod coalescing {
        pub use celox_sir_opt::coalescing::pass_eliminate_working_round_trip;
    }
}
