pub mod backend;
pub use backend::{NativeBackend, NativeExecutionTiming, SharedNativeCode};
#[cfg(target_arch = "aarch64")]
pub use celox_backend_arm64::{jit_mem, scalar as emit};
pub use celox_backend_x86::native::*;
