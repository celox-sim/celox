//! Native x86-64 backend: custom ISel + register allocator + code emitter.
//!
//! Pipeline: SIR (bit-level SSA) → ISel → MIR (word-level SSA) → Spilling → Assignment → Emit

pub mod backend;
pub mod emit;
pub mod isel;
pub mod jit_mem;
pub mod mir;
pub(crate) mod mir_legalize;
pub(crate) mod mir_opt;
pub mod mir_verify;
pub mod regalloc;
pub(crate) mod ssa_destroy;

pub use backend::{NativeBackend, SharedNativeCode};
