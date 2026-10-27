#[cfg(feature = "host-runtime")]
pub(crate) mod compile_cancel;
#[cfg(feature = "host-runtime")]
pub(crate) mod interp;
#[cfg(feature = "host-runtime")]
pub(crate) mod lanes;
#[cfg(feature = "host-runtime")]
pub(crate) mod memory_image;
pub(crate) mod memory_layout;
#[cfg(all(
    feature = "host-runtime",
    any(
        target_arch = "x86_64",
        feature = "arm64-codegen",
        target_arch = "aarch64"
    )
))]
pub(crate) mod native;
#[cfg(feature = "cranelift")]
mod runtime;
#[cfg(feature = "host-runtime")]
pub(crate) mod tiered;
pub use celox_backend_wasm as wasm_codegen;
pub use celox_runtime::backend as traits;
#[cfg(feature = "wasm-runtime")]
pub mod wasm_runtime;
#[cfg(feature = "host-runtime")]
pub(crate) use celox_runtime::RuntimeEventBuffer;
pub use celox_runtime::SimulatorErrorCode;
pub use memory_layout::{LayoutRequirements, MemoryLayout, MemoryLayoutMode, get_byte_size};
pub use traits::{EventHandle, SimBackend};

#[cfg(feature = "host-runtime")]
mod host {
    pub use super::interp::InterpBackend;
    #[cfg(feature = "cranelift")]
    pub use super::runtime::{EventRef, JitBackend, SharedJitCode};
    pub use super::tiered::{
        TieredBackend, TieredExecutionStats, TieredExecutionTier, TieredExecutionTiming,
        TieredPromotionStatus,
    };
    pub use celox_backend_common::cranelift_options::{
        CraneliftDiagnostics, CraneliftOptLevel, CraneliftOptions, RegallocAlgorithm,
    };
    #[cfg(feature = "cranelift")]
    pub(crate) use celox_backend_cranelift::JitEngine;

    #[cfg(any(
        target_arch = "x86_64",
        feature = "arm64-codegen",
        target_arch = "aarch64"
    ))]
    pub use celox_backend_x86::{NativeDiagnostics, NativeDumpOptions, X86BackendOptions};
}

#[cfg(feature = "host-runtime")]
pub use host::*;

/// Trigger instructions use the semantic event order. Every execution engine
/// must assign those same IDs rather than enumerating hash-map keys.
#[cfg(feature = "host-runtime")]
fn event_ids(
    program: &crate::ir::LaidOutProgram,
) -> (
    crate::HashMap<crate::ir::AbsoluteAddr, usize>,
    Vec<crate::ir::AbsoluteAddr>,
) {
    let addresses = crate::edge_plans::event_addresses(program);
    let ids = addresses
        .iter()
        .copied()
        .enumerate()
        .map(|(id, address)| (address, id))
        .collect();
    (ids, addresses)
}
