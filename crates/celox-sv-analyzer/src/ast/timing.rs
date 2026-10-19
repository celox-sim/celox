//! Native timing ownership diagnostics shared by process binders.
use super::*;

pub(super) fn disagreement() -> AnalyzerError {
    AnalyzerError::Parse("invalid native timing control ownership".into())
}
