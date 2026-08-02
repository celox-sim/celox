//! x86-64 backend pipeline: SIR → MIR → allocation → machine code.

pub mod emit;
pub mod features;
pub mod isel;
pub mod jit_mem;
pub mod memory_effect;
pub mod mir;
pub mod mir_legalize;
pub mod mir_opt;
pub mod mir_verify;
pub mod regalloc;
mod sparse_write_state;
pub mod ssa_destroy;
pub mod x86_slp;
