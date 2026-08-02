pub(crate) mod memory_layout;
#[cfg(target_arch = "x86_64")]
pub(crate) mod native;
#[cfg(not(target_arch = "wasm32"))]
mod runtime;
pub use celox_backend_wasm as wasm_codegen;
pub use celox_runtime::backend as traits;
#[cfg(not(target_arch = "wasm32"))]
pub mod wasm_runtime;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use celox_backend_cranelift::JitEngine;
#[cfg(not(target_arch = "wasm32"))]
pub use celox_backend_cranelift::{
    CraneliftDiagnostics, CraneliftOptLevel, CraneliftOptions, RegallocAlgorithm,
};
#[cfg(target_arch = "x86_64")]
pub use celox_backend_x86::{NativeDiagnostics, NativeDumpOptions, X86BackendOptions};
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use celox_runtime::RuntimeEventBuffer;
#[cfg(not(target_arch = "wasm32"))]
pub use celox_runtime::SimulatorErrorCode;
pub use memory_layout::{LayoutRequirements, MemoryLayout, MemoryLayoutMode, get_byte_size};
#[cfg(not(target_arch = "wasm32"))]
pub use runtime::SharedJitCode;
#[cfg(not(target_arch = "wasm32"))]
pub use runtime::{EventRef, JitBackend};
#[cfg(target_arch = "wasm32")]
pub use traits::EventHandle;
#[cfg(not(target_arch = "wasm32"))]
pub use traits::{EventHandle, SimBackend};
