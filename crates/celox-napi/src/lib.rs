#[cfg(not(target_arch = "wasm32"))]
mod cache;
mod errors;
mod gen_ts;
#[cfg(not(target_arch = "wasm32"))]
mod handle_backend;
mod options;
mod project;
#[cfg(not(target_arch = "wasm32"))]
mod runtime_event;
#[cfg(not(target_arch = "wasm32"))]
mod simulation_handle;
#[cfg(not(target_arch = "wasm32"))]
mod simulator_handle;
#[cfg(not(target_arch = "wasm32"))]
mod test_runner;
#[cfg(target_arch = "wasm32")]
mod wasm_handle;
#[cfg(not(target_arch = "wasm32"))]
use cache::*;
use errors::*;
pub use gen_ts::*;
#[cfg(not(target_arch = "wasm32"))]
use handle_backend::*;
pub use options::*;
use project::*;
#[cfg(not(target_arch = "wasm32"))]
pub use runtime_event::*;
#[cfg(not(target_arch = "wasm32"))]
pub use simulation_handle::*;
#[cfg(not(target_arch = "wasm32"))]
pub use simulator_handle::*;
#[cfg(not(target_arch = "wasm32"))]
pub use test_runner::*;
#[cfg(target_arch = "wasm32")]
pub use wasm_handle::*;

// The memory image of the WASM bridge; also built in host tests.
#[cfg(any(test, target_arch = "wasm32"))]
mod initial_memory;
mod layout;

use fxhash::FxHashMap as HashMap;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
#[expect(
    clippy::disallowed_types,
    reason = "cross-thread: the process-global JIT cache is shared by every JS thread (main and Node worker_threads) that loads the addon in one process"
)]
use std::sync::Mutex;

use napi::bindgen_prelude::*;
use napi_derive::napi;
use veryl_analyzer::{Analyzer, Context, attribute_table, ir::Ir, symbol_table};
use veryl_metadata::Metadata;
use veryl_parser::Parser;
use veryl_path::PathSet;

#[cfg(not(target_arch = "wasm32"))]
use celox::SimBackend;
#[cfg(not(target_arch = "wasm32"))]
use layout::{build_event_map, build_hierarchy_node, build_signal_layout};

/// Clear the process-global JIT compilation cache.
///
/// Call this when source files have changed and cached compiled code may be stale.
#[cfg(not(target_arch = "wasm32"))]
#[napi]
pub fn clear_jit_cache() {
    let mut cache = JIT_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache.clear();
}

/// Stub for wasm32: no JIT cache to clear.
#[cfg(target_arch = "wasm32")]
#[napi]
pub fn clear_jit_cache() {}

fn napi_bigint(value: u64) -> BigInt {
    BigInt {
        sign_bit: false,
        words: vec![value],
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
