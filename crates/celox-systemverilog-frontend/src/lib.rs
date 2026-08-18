//! SystemVerilog source adapter for the source-neutral frontend core.

mod lowering;

pub use lowering::{FrontendError, prepare_external_hierarchy, schedule_sources};
