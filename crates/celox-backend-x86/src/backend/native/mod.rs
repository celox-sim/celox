//! x86-64 backend pipeline: SIR → MIR → allocation → machine code.

#[cfg(any(target_arch = "x86_64", feature = "cross-codegen"))]
pub mod emit;
#[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
pub mod features;
pub mod isel;
#[cfg(any(target_arch = "x86_64", feature = "cross-codegen"))]
pub mod jit_mem;
pub mod memory_effect;
pub mod mir;
pub mod mir_legalize;
pub mod mir_opt;
pub mod mir_verify;
#[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
pub mod regalloc;
pub mod scalar_pipeline;
mod sparse_write_state;
#[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
pub mod ssa_destroy;
#[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
pub mod x86_slp;
