use celox_sir::verify::SirVerifyError;
use celox_slt::{SLTNodeFactsError, scheduler::SchedulerError};
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct SourceLocation {
    pub path: String,
    pub text: String,
    pub span: miette::SourceSpan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoweringPhase {
    FfLowering,
    CombLowering,
    SimulatorParser,
}

impl std::fmt::Display for LoweringPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FfLowering => write!(f, "FF lowering"),
            Self::CombLowering => write!(f, "comb lowering"),
            Self::SimulatorParser => write!(f, "simulator parser"),
        }
    }
}

#[derive(Error, Debug)]
pub enum ParserError {
    #[error(transparent)]
    Scheduler(SchedulerError<String>),
    #[error("{error}")]
    SchedulerWithLocation {
        error: SchedulerError<String>,
        source_locations: Vec<SourceLocation>,
    },
    #[error("Unsupported in {phase}: {feature} [tracking issue #{issue}] ({detail})")]
    Unsupported {
        issue: u32,
        phase: LoweringPhase,
        feature: &'static str,
        detail: String,
        source_location: Option<SourceLocation>,
    },
    #[error("Illegal in current context: {feature} ({detail})")]
    IllegalContext {
        feature: &'static str,
        detail: String,
        source_location: Option<SourceLocation>,
    },
    #[error("Top module `{name}` not found in IR")]
    TopNotFound { name: String },
    #[error("Top module `{name}` is generic and cannot be used as a top-level module")]
    GenericTop { name: String },
    #[error("SIR verification failed {phase} in {group} unit {unit}: {error}")]
    SirVerify {
        phase: &'static str,
        group: &'static str,
        unit: usize,
        #[source]
        error: SirVerifyError,
    },
    #[error("SLT verification failed {phase}: {error}")]
    SltVerify {
        phase: &'static str,
        #[source]
        error: SLTNodeFactsError,
    },
    #[error("SLT construction failed: {0}")]
    SltConstruction(#[from] SLTNodeFactsError),
}

impl ParserError {
    pub fn unsupported(
        issue: u32,
        phase: LoweringPhase,
        feature: &'static str,
        detail: impl Into<String>,
        source_location: Option<SourceLocation>,
    ) -> Self {
        Self::Unsupported {
            issue,
            phase,
            feature,
            detail: detail.into(),
            source_location,
        }
    }

    pub fn illegal_context(
        feature: &'static str,
        detail: impl Into<String>,
        source_location: Option<SourceLocation>,
    ) -> Self {
        Self::IllegalContext {
            feature,
            detail: detail.into(),
            source_location,
        }
    }
}
