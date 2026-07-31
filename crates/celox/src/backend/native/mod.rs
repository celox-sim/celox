//! Native x86-64 backend: custom ISel + register allocator + code emitter.
//!
//! Pipeline: SIR (bit-level SSA) → ISel → MIR (word-level SSA) → Spilling → Assignment → Emit

pub mod backend;
pub mod emit;
pub(crate) mod features;
pub mod isel;
pub mod jit_mem;
pub(crate) mod memory_effect;
pub mod mir;
pub(crate) mod mir_legalize;
pub(crate) mod mir_opt;
pub mod mir_verify;
pub mod regalloc;
mod sparse_write_state;
pub(crate) mod ssa_destroy;

pub use backend::{NativeBackend, SharedNativeCode};

fn enabled_by_default(name: &str) -> bool {
    let configured = std::env::var_os(name);
    if cfg!(test) {
        // Unit tests exercise individual passes in parallel. Keep their
        // historical isolated defaults without mutating process-global
        // environment variables; an explicit opt-in still exercises the
        // integrated production path.
        configured.is_some_and(|value| value != "0")
    } else {
        configured.is_none_or(|value| value != "0")
    }
}

pub(crate) fn native_tick_loop_enabled() -> bool {
    enabled_by_default("CELOX_NATIVE_TICK_LOOP")
}
