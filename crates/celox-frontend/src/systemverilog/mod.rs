//! SystemVerilog source adapter.
//!
//! The public surface of this module is deliberately limited to source
//! analysis and scheduling.  Analyzer-native syntax and identities stay in
//! [`lowering`]; lowering uses [`crate::shared::SourceVarId`] directly and
//! scheduled output crosses the frontend boundary through [`crate::shared`].

pub use celox_systemverilog_frontend::{
    FrontendError, prepare_external_hierarchy, schedule_sources,
};
