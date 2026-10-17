/// A non-fatal diagnostic of the SystemVerilog frontend. The design still
/// compiles; the simulator reports these with its compilation warnings.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, thiserror::Error)]
pub enum FrontendWarning {
    /// A memory file without address directives holds a different number of
    /// words than the address range of `$readmemh`/`$readmemb`
    /// (IEEE 1800-2023 21.4).
    #[error("`{task}` reading {file}: {detail}")]
    MemoryFileWordCount {
        task: String,
        file: String,
        detail: String,
    },
}

impl miette::Diagnostic for FrontendWarning {
    fn code<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        Some(Box::new(match self {
            Self::MemoryFileWordCount { .. } => "memory_file_word_count",
        }))
    }

    fn severity(&self) -> Option<miette::Severity> {
        Some(miette::Severity::Warning)
    }

    fn help<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        Some(Box::new(match self {
            Self::MemoryFileWordCount { .. } => {
                "give one word per address of the range, or place the words with `@address` directives"
            }
        }))
    }
}
