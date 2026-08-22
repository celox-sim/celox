#[cfg(feature = "host-runtime")]
pub(crate) mod interp;
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
