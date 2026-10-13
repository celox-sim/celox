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
#[cfg(feature = "host-runtime")]
mod runtime;
#[cfg(feature = "host-runtime")]
pub(crate) mod tiered;
pub use celox_backend_wasm as wasm_codegen;
pub use celox_runtime::backend as traits;
#[cfg(feature = "host-runtime")]
pub mod wasm_runtime;
#[cfg(feature = "host-runtime")]
pub(crate) use celox_runtime::RuntimeEventBuffer;
pub use celox_runtime::SimulatorErrorCode;
pub use memory_layout::{LayoutRequirements, MemoryLayout, MemoryLayoutMode, get_byte_size};
pub use traits::{EventHandle, SimBackend};

#[cfg(feature = "host-runtime")]
mod host {
    pub use super::interp::InterpBackend;
    pub use super::runtime::{EventRef, JitBackend, SharedJitCode};
    pub use super::tiered::{
        TieredBackend, TieredExecutionStats, TieredExecutionTier, TieredExecutionTiming,
        TieredPromotionStatus,
    };
    pub(crate) use celox_backend_cranelift::JitEngine;
    pub use celox_backend_cranelift::{
        CraneliftDiagnostics, CraneliftOptLevel, CraneliftOptions, RegallocAlgorithm,
    };

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
    let mut addresses = program.design.events.ordered_events.clone();
    let mut seen: crate::HashSet<_> = addresses.iter().copied().collect();
    // Preserve legacy/directly constructed programs whose event topology does
    // not list every kernel, after the semantic IDs used by trigger code.
    let mut extra: Vec<_> = program
        .sir
        .eval_apply_ffs
        .keys()
        .chain(program.sir.eval_only_ffs.keys())
        .chain(program.sir.apply_ffs.keys())
        .map(|address| program.design.events.canonical(*address))
        .filter(|address| seen.insert(*address))
        .collect();
    extra.sort_unstable();
    addresses.extend(extra);
    let ids = addresses
        .iter()
        .copied()
        .enumerate()
        .map(|(id, address)| (address, id))
        .collect();
    (ids, addresses)
}
