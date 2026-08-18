//! SystemVerilog frontend adapter for Celox.

mod lowering;

pub use lowering::{FrontendError, prepare_external_hierarchy, schedule_sources};
