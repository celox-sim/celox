mod builder;
#[cfg(feature = "host-runtime")]
pub(crate) mod checkpoint;
mod error;
#[cfg(feature = "host-runtime")]
mod heap;
pub(crate) mod mixed;
#[cfg(feature = "host-runtime")]
mod state_file;

#[cfg(all(
    feature = "host-runtime",
    any(
        target_arch = "x86_64",
        feature = "arm64-codegen",
        target_arch = "aarch64"
    )
))]
pub use builder::NativeCompilation;
#[cfg(feature = "host-runtime")]
pub use builder::{DeadStorePolicy, SimulatorBuilder, SimulatorOptions, TierPromotion};
pub use builder::{
    compile_frontend_to_sir, compile_to_sir, compile_to_sir_with_diagnostic_policy,
    compile_to_sir_with_latch_policy,
};
pub use builder::{compile_mixed_to_sir, compile_sv_to_sir};
#[cfg(feature = "host-runtime")]
pub use checkpoint::{Checkpoint, CheckpointError, StateImage};
pub use error::render_diagnostic;
pub use error::{CodegenError, CompilationWarning, SimulatorError, SimulatorErrorKind};
#[cfg(feature = "host-runtime")]
pub use state_file::{StateError, StateMismatch, StateSchema};

#[cfg(feature = "host-runtime")]
mod host;

#[cfg(feature = "host-runtime")]
pub use host::*;

/// Human-readable diagnostics are separate from the byte-exact event payload.
#[cfg(feature = "host-runtime")]
pub(crate) fn message_diagnostic(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => bytes.escape_ascii().to_string(),
    }
}

#[cfg(feature = "host-runtime")]
pub(crate) use host::native_io;
