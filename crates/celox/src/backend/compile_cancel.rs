//! Cooperative cancellation for background compilation pipelines.
//!
//! A [`CompileCancel`] is a shared flag observed at coarse pipeline
//! boundaries (per compile task, per execution unit, and between lowering
//! phases). An uncancelled run pays one predictable branch per boundary and
//! nothing else; the simulation hot path never touches this type.
//! Cancellation is cooperative: the unit in flight finishes, then the
//! pipeline unwinds with [`CodegenError::Cancelled`](crate::CodegenError::Cancelled)
//! so the caller keeps whatever partial results it already collected.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Shared cancellation flag for a compilation pipeline.
#[derive(Clone)]
pub(crate) struct CompileCancel {
    flag: Arc<AtomicBool>,
}

impl CompileCancel {
    pub(crate) fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Request cancellation. Idempotent and safe from any thread.
    pub(crate) fn cancel(&self) {
        self.flag.store(true, Ordering::Release);
    }

    /// Whether cancellation has been requested.
    pub(crate) fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }
}

/// Whether an optional token has been cancelled; `None` tokens (plain,
/// uncancellable compiles) never cancel.
pub(crate) fn cancelled(cancel: Option<&CompileCancel>) -> bool {
    cancel.is_some_and(CompileCancel::is_cancelled)
}

/// The error a compile pipeline returns when it observes cancellation.
pub(crate) fn cancelled_error() -> crate::SimulatorError {
    crate::SimulatorError::new(crate::SimulatorErrorKind::Codegen(
        crate::CodegenError::Cancelled,
    ))
}
