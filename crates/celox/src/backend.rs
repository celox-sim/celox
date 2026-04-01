#[cfg(not(target_arch = "wasm32"))]
mod jit_engine;
mod memory_layout;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod native;
#[cfg(not(target_arch = "wasm32"))]
mod runtime;
pub mod traits;
#[cfg(not(target_arch = "wasm32"))]
mod translator;
#[allow(dead_code, unused_variables, unused_imports)]
pub mod wasm_codegen;
#[cfg(not(target_arch = "wasm32"))]
pub mod wasm_runtime;
#[cfg(not(target_arch = "wasm32"))]
mod wide_ops;

#[cfg(not(target_arch = "wasm32"))]
pub(crate) use jit_engine::JitEngine;
pub use memory_layout::{MemoryLayout, get_byte_size};
#[cfg(not(target_arch = "wasm32"))]
pub use runtime::SharedJitCode;
#[cfg(not(target_arch = "wasm32"))]
pub use runtime::{EventRef, JitBackend};
pub use traits::SimulatorErrorCode;
#[cfg(not(target_arch = "wasm32"))]
pub use traits::{EventHandle, SimBackend};
#[cfg(target_arch = "wasm32")]
pub use traits::EventHandle;
#[cfg(not(target_arch = "wasm32"))]
pub(super) use translator::SIRTranslator;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use translator::core::MEM_SHIFT_THRESHOLD;
#[cfg(target_arch = "wasm32")]
pub(crate) const MEM_SHIFT_THRESHOLD: usize = 4;
