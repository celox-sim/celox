use crate::ir::cfg::SirCfgError;
use crate::ir::verify::SirVerifyError;
use crate::optimizer::StateSsaError;
use thiserror::Error;

/// Stable category for a failed SIR optimization operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OptimizationErrorKind {
    InvalidInput,
    ControlFlow,
    StateSsa,
    Verification,
    Invariant,
}

/// Structured failure from a fallible SIR optimization operation.
///
/// The concrete internal analysis error remains available through
/// [`std::error::Error::source`] without becoming part of the public API.
#[derive(Debug, Error)]
#[error("{stage}: {detail}")]
pub struct OptimizationError {
    kind: OptimizationErrorKind,
    stage: &'static str,
    detail: String,
    #[source]
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl OptimizationError {
    pub fn kind(&self) -> OptimizationErrorKind {
        self.kind
    }

    pub fn stage(&self) -> &'static str {
        self.stage
    }

    pub(crate) fn invalid_input(stage: &'static str, detail: impl Into<String>) -> Self {
        Self::without_source(OptimizationErrorKind::InvalidInput, stage, detail)
    }

    pub(crate) fn invariant(stage: &'static str, detail: impl Into<String>) -> Self {
        Self::without_source(OptimizationErrorKind::Invariant, stage, detail)
    }

    pub(crate) fn control_flow(stage: &'static str, source: SirCfgError) -> Self {
        Self::with_source(
            OptimizationErrorKind::ControlFlow,
            stage,
            source.to_string(),
            source,
        )
    }

    pub(crate) fn state_ssa(stage: &'static str, source: StateSsaError) -> Self {
        Self::with_source(
            OptimizationErrorKind::StateSsa,
            stage,
            source.to_string(),
            source,
        )
    }

    pub(crate) fn verification(stage: &'static str, source: SirVerifyError) -> Self {
        Self::with_source(
            OptimizationErrorKind::Verification,
            stage,
            source.to_string(),
            source,
        )
    }

    fn without_source(
        kind: OptimizationErrorKind,
        stage: &'static str,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            stage,
            detail: detail.into(),
            source: None,
        }
    }

    fn with_source<E>(
        kind: OptimizationErrorKind,
        stage: &'static str,
        detail: String,
        source: E,
    ) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        Self {
            kind,
            stage,
            detail,
            source: Some(Box::new(source)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use crate::ir::cfg::SirCfgError;

    use super::{OptimizationError, OptimizationErrorKind};

    #[test]
    fn preserves_error_category_stage_and_source() {
        let error = OptimizationError::control_flow(
            "fused comb dead-store elimination",
            SirCfgError::Empty,
        );

        assert_eq!(error.kind(), OptimizationErrorKind::ControlFlow);
        assert_eq!(error.stage(), "fused comb dead-store elimination");
        assert_eq!(
            error.to_string(),
            "fused comb dead-store elimination: SIR CFG has no blocks"
        );
        assert_eq!(error.source().unwrap().to_string(), "SIR CFG has no blocks");
        assert!(error.source().unwrap().is::<SirCfgError>());
    }
}
