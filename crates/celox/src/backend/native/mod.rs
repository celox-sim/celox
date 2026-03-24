//! Native x86-64 backend: custom ISel + register allocator + code emitter.
//!
//! Pipeline: SIR (bit-level SSA) → ISel → MIR (word-level SSA) → Spilling → Assignment → Emit

pub mod backend;
pub mod emit;
pub mod isel;
pub mod jit_mem;
pub mod mir;
pub mod regalloc;

pub use backend::NativeBackend;
