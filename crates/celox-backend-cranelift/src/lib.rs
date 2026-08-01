//! Source-independent Cranelift translation and JIT compilation.

pub type HashMap<K, V> = fxhash::FxHashMap<K, V>;
pub type HashSet<K> = fxhash::FxHashSet<K>;

pub mod ir {
    pub use celox_design::{
        BinaryOp, DomainKind, InstanceId, RegionedAbsoluteAddrBase, SPARSE_WORKING_REGION,
        STABLE_REGION, StateAddr, TriggerIdWithKind, UnaryOp, WORKING_REGION,
    };
    pub use celox_sir::*;

    pub type AbsoluteAddr = celox_design::StateAddr;
    pub type RegionedAbsoluteAddr = celox_design::RegionedStateAddr;
}

pub mod backend {
    pub const MEM_SHIFT_THRESHOLD: usize = 4;

    pub mod memory_layout {
        pub use celox_state_layout::*;
        pub type MemoryLayout = celox_state_layout::MemoryLayout<celox_design::StateAddr>;
    }
}

pub mod optimizer {
    pub mod coalescing {
        pub use crate::cost_model;
        pub use crate::tail_call_split as pass_tail_call_split;
        pub use crate::tail_call_split::TailCallChunk;
    }
}

pub mod cost_model;
mod cranelift_options;
mod jit_engine;
pub mod tail_call_split;
mod translator;
mod wide_ops;

pub use cranelift_options::{CraneliftOptLevel, CraneliftOptions, RegallocAlgorithm};
pub use jit_engine::JitEngine;
pub use translator::SIRTranslator;

pub type MemoryLayout = celox_state_layout::MemoryLayout<celox_design::StateAddr>;
pub use celox_state_layout::get_byte_size;

/// Complete backend-owned policy required by SIR translation.
#[derive(Debug, Clone)]
pub struct CompileOptions {
    pub four_state: bool,
    pub emit_triggers: bool,
    pub cranelift: CraneliftOptions,
}
