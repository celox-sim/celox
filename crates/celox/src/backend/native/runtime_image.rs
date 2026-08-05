//! Runtime-only loading and instantiation of compiler-produced native images.

use std::sync::Arc;

use celox_runtime::{DesignReflection, ReflectionSignal, SignalRef};

use crate::{NativeBackend, SharedNativeCode, SimBackend, SimulatorError};

use super::{NativeImageContainerError, NativeProgramImage};

/// Failure while discovering or attaching a compiler-produced native image.
#[derive(Debug, thiserror::Error)]
pub enum NativeProgramLoadError {
    #[error(transparent)]
    Container(#[from] NativeImageContainerError),
    #[error("no native program image is attached to the runtime")]
    MissingImage,
    #[error("failed to attach native machine code: {0}")]
    Attach(#[source] SimulatorError),
}

/// One independently mutable instance of a precompiled native program.
///
/// Construction needs only a serialized [`NativeProgramImage`]. Source text,
/// frontend lookup tables, SIR, and compiler layout artifacts are not retained.
pub struct NativeProgramInstance {
    shared: Arc<SharedNativeCode>,
    backend: NativeBackend,
}

impl NativeProgramInstance {
    /// Attach an already-decoded program image and allocate fresh state.
    pub fn from_image(image: NativeProgramImage) -> Result<Self, NativeProgramLoadError> {
        let shared =
            Arc::new(SharedNativeCode::from_image(image).map_err(NativeProgramLoadError::Attach)?);
        let backend = NativeBackend::from_shared(Arc::clone(&shared));
        Ok(Self { shared, backend })
    }

    /// Discover a program image appended to arbitrary runtime bytes.
    pub fn from_attached_bytes(bytes: &[u8]) -> Result<Self, NativeProgramLoadError> {
        let appended = NativeProgramImage::discover_appended(bytes)?
            .ok_or(NativeProgramLoadError::MissingImage)?;
        Self::from_image(appended.image)
    }

    /// Discover the program image appended to the running executable.
    pub fn from_current_executable() -> Result<Self, NativeProgramLoadError> {
        let appended = NativeProgramImage::discover_in_current_executable()?
            .ok_or(NativeProgramLoadError::MissingImage)?;
        Self::from_image(appended.image)
    }

    /// Elaborated hierarchy and signal metadata embedded by the compiler.
    pub fn reflection(&self) -> &DesignReflection {
        self.shared.program_image().reflection()
    }

    /// Resolve a fully qualified signal name such as `Top.clock`.
    pub fn signal(&self, full_name: &str) -> Option<&ReflectionSignal> {
        self.reflection()
            .signal_by_name(full_name)
            .map(|(_, signal)| signal)
    }

    /// Resolve only the compact state handle for a fully qualified signal.
    pub fn signal_ref(&self, full_name: &str) -> Option<SignalRef> {
        self.signal(full_name).map(|signal| signal.signal)
    }

    /// Execute combinational logic after one or more foreign writes.
    pub fn eval_comb(&mut self) -> Result<(), celox_runtime::SimulatorErrorCode> {
        self.backend.eval_comb()
    }

    /// Direct access used by foreign-interface adapters for value and event
    /// operations. The compiler is not involved in these calls.
    pub fn backend(&self) -> &NativeBackend {
        &self.backend
    }

    pub fn backend_mut(&mut self) -> &mut NativeBackend {
        &mut self.backend
    }
}
