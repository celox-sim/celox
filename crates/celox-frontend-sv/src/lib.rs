//! SystemVerilog frontend adapter for Celox.

mod lowering;

pub use celox_sv_analyzer::AnalyzerError;
pub use lowering::{FrontendError, prepare_external_hierarchy, schedule_sources};
