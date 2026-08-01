use celox_sir::verify::SirVerifyError;
use celox_slt::{SLTNodeFactsError, scheduler::SchedulerError};
use thiserror::Error;
use veryl_analyzer::multi_sources::{MultiSources, Source};
use veryl_parser::token_range::TokenRange;

/// Source location information for rich error diagnostics.
#[derive(Debug)]
pub struct SourceLocation {
    pub source: MultiSources,
    pub span: miette::SourceSpan,
}

impl SourceLocation {
    pub fn from_token(token: &TokenRange) -> Self {
        let path = token.beg.source.to_string();
        let text = token.beg.source.get_text();
        Self {
            source: MultiSources {
                sources: vec![Source { path, text }],
            },
            span: token.into(),
        }
    }

    fn path(&self) -> Option<&str> {
        self.source
            .sources
            .first()
            .map(|source| source.path.as_str())
    }
}

/// The compilation phase where an unsupported feature was encountered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoweringPhase {
    FfLowering,
    CombLowering,
    SimulatorParser,
}

impl std::fmt::Display for LoweringPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoweringPhase::FfLowering => write!(f, "FF lowering"),
            LoweringPhase::CombLowering => write!(f, "comb lowering"),
            LoweringPhase::SimulatorParser => write!(f, "simulator parser"),
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

    #[error(
        "Unresolved type width for variable `{variable}` in module `{module}`: \
             width cannot be determined at compile time (type: {typ})"
    )]
    UnresolvedWidth {
        module: String,
        variable: String,
        typ: String,
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
        token: Option<&TokenRange>,
    ) -> Self {
        ParserError::Unsupported {
            issue,
            phase,
            feature,
            detail: detail.into(),
            source_location: token.map(SourceLocation::from_token),
        }
    }

    pub fn illegal_context(
        feature: &'static str,
        detail: impl Into<String>,
        token: Option<&TokenRange>,
    ) -> Self {
        ParserError::IllegalContext {
            feature,
            detail: detail.into(),
            source_location: token.map(SourceLocation::from_token),
        }
    }

    pub fn unresolved_width(
        module: &veryl_analyzer::ir::Module,
        var: &veryl_analyzer::ir::Variable,
        typ: impl Into<String>,
    ) -> Self {
        ParserError::UnresolvedWidth {
            module: module.name.to_string(),
            variable: var.path.to_string(),
            typ: typ.into(),
            source_location: Some(SourceLocation::from_token(&var.token)),
        }
    }
}

impl miette::Diagnostic for ParserError {
    fn code<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        match self {
            ParserError::Unsupported { phase, .. } => Some(Box::new(format!(
                "unsupported_{}",
                match phase {
                    LoweringPhase::FfLowering => "ff_lowering",
                    LoweringPhase::CombLowering => "comb_lowering",
                    LoweringPhase::SimulatorParser => "simulator_parser",
                }
            ))),
            ParserError::IllegalContext { .. } => Some(Box::new("illegal_context")),
            ParserError::UnresolvedWidth { .. } => Some(Box::new("unresolved_width")),
            ParserError::Scheduler(_) | ParserError::SchedulerWithLocation { .. } => {
                Some(Box::new("scheduler"))
            }
            ParserError::TopNotFound { .. } => Some(Box::new("top_not_found")),
            ParserError::GenericTop { .. } => Some(Box::new("generic_top")),
            ParserError::SirVerify { .. } => Some(Box::new("sir_verify")),
            ParserError::SltVerify { .. } | ParserError::SltConstruction(_) => {
                Some(Box::new("slt_verify"))
            }
        }
    }

    fn severity(&self) -> Option<miette::Severity> {
        Some(miette::Severity::Error)
    }

    fn source_code(&self) -> Option<&dyn miette::SourceCode> {
        let location = match self {
            ParserError::Unsupported {
                source_location, ..
            }
            | ParserError::IllegalContext {
                source_location, ..
            }
            | ParserError::UnresolvedWidth {
                source_location, ..
            } => source_location.as_ref(),
            ParserError::SchedulerWithLocation {
                source_locations, ..
            } => source_locations.first(),
            _ => None,
        };
        location.map(|location| &location.source as &dyn miette::SourceCode)
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = miette::LabeledSpan> + '_>> {
        let location = match self {
            ParserError::Unsupported {
                source_location, ..
            }
            | ParserError::IllegalContext {
                source_location, ..
            }
            | ParserError::UnresolvedWidth {
                source_location, ..
            } => source_location.as_ref(),
            _ => None,
        };
        if let Some(location) = location {
            return Some(Box::new(std::iter::once(
                miette::LabeledSpan::new_with_span(
                    Some("Error location".to_string()),
                    location.span,
                ),
            )));
        }

        match self {
            ParserError::SchedulerWithLocation {
                source_locations, ..
            } => {
                let first_path = source_locations.first().and_then(SourceLocation::path)?;
                let labels = source_locations
                    .iter()
                    .filter(move |location| location.path() == Some(first_path))
                    .map(|location| {
                        miette::LabeledSpan::new_with_span(
                            Some("loop participant".to_string()),
                            location.span,
                        )
                    })
                    .collect::<Vec<_>>();
                if labels.is_empty() {
                    None
                } else {
                    Some(Box::new(labels.into_iter()))
                }
            }
            _ => None,
        }
    }
}
