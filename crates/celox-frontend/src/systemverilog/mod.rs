//! SystemVerilog source adapter.
//!
//! The public surface of this module is deliberately limited to source
//! analysis and scheduling.  Analyzer-native syntax and identities stay in
//! [`lowering`]; scheduled output crosses the frontend boundary through the
//! source-independent types in [`crate::shared`].

mod lowering;

pub use lowering::{FrontendError, prepare_external_hierarchy, schedule_sources};
