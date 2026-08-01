pub(crate) mod memory_layout;
#[cfg(target_arch = "x86_64")]
pub(crate) mod native;
#[cfg(not(target_arch = "wasm32"))]
mod runtime;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod runtime_event_buffer;
pub mod traits;
pub use celox_backend_wasm as wasm_codegen;
#[cfg(not(target_arch = "wasm32"))]
pub mod wasm_runtime;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use celox_backend_cranelift::JitEngine;
#[cfg(not(target_arch = "wasm32"))]
pub use celox_backend_cranelift::{CraneliftOptLevel, CraneliftOptions, RegallocAlgorithm};
#[cfg(target_arch = "x86_64")]
pub use celox_backend_x86::X86BackendOptions;
pub use memory_layout::{LayoutRequirements, MemoryLayout, MemoryLayoutMode, get_byte_size};
#[cfg(not(target_arch = "wasm32"))]
pub use runtime::SharedJitCode;
#[cfg(not(target_arch = "wasm32"))]
pub use runtime::{EventRef, JitBackend};
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use runtime_event_buffer::RuntimeEventBuffer;
#[cfg(target_arch = "wasm32")]
pub use traits::EventHandle;
pub use traits::SimulatorErrorCode;
#[cfg(not(target_arch = "wasm32"))]
pub use traits::{EventHandle, SimBackend};
