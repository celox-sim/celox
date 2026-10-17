use std::path::Path;
use std::sync::Arc;

use celox_sv_analyzer::ir::SourceSpan;

/// A non-fatal diagnostic of the SystemVerilog frontend. The design still
/// compiles; the simulator reports these with its compilation warnings.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{kind}")]
pub struct FrontendWarning {
    pub kind: FrontendWarningKind,
    /// The construct that warns, unless the analyzer created it or its file
    /// cannot be read.
    pub location: Option<WarningLocation>,
}

/// What a [`FrontendWarning`] reports.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, thiserror::Error)]
pub enum FrontendWarningKind {
    /// A memory file without address directives holds a different number of
    /// words than the address range of `$readmemh`/`$readmemb`
    /// (IEEE 1800-2023 21.4).
    #[error("`{task}` reading {file}: {detail}")]
    MemoryFileWordCount {
        task: String,
        file: String,
        detail: String,
    },
    /// `$exit` outside a program block, which IEEE 1800-2023 24.7 ignores.
    /// Celox has no program blocks, so every call is ignored.
    #[error("`$exit` outside a program block is ignored")]
    ExitOutsideProgram,
}

/// Where a warning is in the source.
#[derive(Debug, Clone)]
pub struct WarningLocation {
    pub path: String,
    /// The 1-based line of the start of the construct.
    pub line: usize,
    /// The 1-based column, in characters, of the start of the construct.
    pub column: usize,
    source: miette::NamedSource<Arc<str>>,
    span: miette::SourceSpan,
}

impl WarningLocation {
    fn new(path: String, text: Arc<str>, span: &SourceSpan) -> Option<Self> {
        let before = text.get(..span.start)?;
        text.get(span.start..span.end)?;
        let line_start = before.rfind('\n').map_or(0, |newline| newline + 1);
        Some(Self {
            line: before.matches('\n').count() + 1,
            column: before[line_start..].chars().count() + 1,
            source: miette::NamedSource::new(&path, text),
            span: (span.start..span.end).into(),
            path,
        })
    }
}

/// A warning found while lowering, with the span of its construct.
pub(crate) type PendingWarning = (FrontendWarningKind, Option<SourceSpan>);

/// Appends `found` to `warnings` in source order, once for each construct:
/// each specialization of a module and each lowering attempt of a process
/// repeats the warnings of the same source. Spans are resolved against
/// `sources`, or else the file at their path.
pub(crate) fn append_unique_warnings(
    sources: &[(&str, &Path)],
    warnings: &mut Vec<FrontendWarning>,
    mut found: Vec<PendingWarning>,
) {
    found.sort_by(|(left_kind, left), (right_kind, right)| {
        (left, left_kind).cmp(&(right, right_kind))
    });
    found.dedup();
    let mut texts = fxhash::FxHashMap::<std::path::PathBuf, Option<Arc<str>>>::default();
    for (kind, span) in found {
        let location = span.and_then(|span| {
            let text = texts
                .entry(span.path.clone())
                .or_insert_with(|| {
                    sources
                        .iter()
                        .find(|(_, path)| *path == span.path)
                        .map(|(code, _)| Arc::from(*code))
                        .or_else(|| std::fs::read_to_string(&span.path).ok().map(Arc::from))
                })
                .clone();
            WarningLocation::new(span.path.display().to_string(), text?, &span)
        });
        warnings.push(FrontendWarning { kind, location });
    }
}

impl miette::Diagnostic for FrontendWarning {
    fn code<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        Some(Box::new(match self.kind {
            FrontendWarningKind::MemoryFileWordCount { .. } => "memory_file_word_count",
            FrontendWarningKind::ExitOutsideProgram => "exit_outside_program",
        }))
    }

    fn severity(&self) -> Option<miette::Severity> {
        Some(miette::Severity::Warning)
    }

    fn help<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        Some(Box::new(match self.kind {
            FrontendWarningKind::MemoryFileWordCount { .. } => {
                "give one word per address of the range, or place the words with `@address` directives"
            }
            FrontendWarningKind::ExitOutsideProgram => {
                "end the simulation with `$finish`, or remove the call"
            }
        }))
    }

    fn source_code(&self) -> Option<&dyn miette::SourceCode> {
        self.location
            .as_ref()
            .map(|location| &location.source as &dyn miette::SourceCode)
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = miette::LabeledSpan> + '_>> {
        let location = self.location.as_ref()?;
        Some(Box::new(std::iter::once(
            miette::LabeledSpan::new_with_span(Some("called here".to_string()), location.span),
        )))
    }
}
