use std::{
    fmt,
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
};

/// A source file in one preprocessing session.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct FileId(pub(crate) usize);

/// A UTF-8 byte range in an original source file.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct Span {
    pub file: FileId,
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn range(self) -> Range<usize> {
        self.start..self.end
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct ExpansionId(pub(crate) usize);

/// Spelling location plus the expansion that introduced a token, if any.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct Origin {
    pub spelling: Span,
    pub expansion: Option<ExpansionId>,
}

#[derive(Clone, Debug)]
pub struct Expansion {
    pub name: String,
    pub invocation: Origin,
    pub definition: Option<Span>,
}

#[derive(Clone, Debug)]
pub struct SourceFile {
    pub path: PathBuf,
    pub text: Arc<str>,
    line_starts: Vec<usize>,
    logical_lines: Vec<(usize, usize, PathBuf)>,
}

impl SourceFile {
    /// One-based physical line and byte column. `line directives do not alter
    /// physical spelling locations.
    pub fn line_column(&self, offset: usize) -> (usize, usize) {
        let line = self
            .line_starts
            .partition_point(|start| *start <= offset)
            .saturating_sub(1);
        (line + 1, offset - self.line_starts[line] + 1)
    }
}

#[derive(Clone, Debug, Default)]
pub struct SourceMap {
    files: Vec<SourceFile>,
    expansions: Vec<Expansion>,
}

impl SourceMap {
    pub fn add(&mut self, path: impl AsRef<Path>, text: impl Into<Arc<str>>) -> FileId {
        let text = text.into();
        let mut line_starts = vec![0];
        line_starts.extend(
            text.bytes()
                .enumerate()
                .filter_map(|(i, b)| (b == b'\n').then_some(i + 1)),
        );
        let id = FileId(self.files.len());
        self.files.push(SourceFile {
            path: path.as_ref().to_owned(),
            text,
            line_starts,
            logical_lines: Vec::new(),
        });
        id
    }

    pub fn file(&self, id: FileId) -> &SourceFile {
        &self.files[id.0]
    }
    pub fn text(&self, span: Span) -> &str {
        &self.file(span.file).text[span.range()]
    }
    pub fn expansion(&self, id: ExpansionId) -> &Expansion {
        &self.expansions[id.0]
    }

    /// Logical filename/line after `line directives, separate from the physical
    /// source span. Columns remain physical byte columns.
    pub fn logical_location(&self, span: Span) -> (&Path, usize, usize) {
        let file = self.file(span.file);
        let (line, column) = file.line_column(span.start);
        let Some((physical, logical, path)) = file
            .logical_lines
            .iter()
            .rev()
            .find(|(physical, _, _)| *physical <= line)
        else {
            return (&file.path, line, column);
        };
        (path, logical + (line - physical), column)
    }

    pub(crate) fn set_line(&mut self, span: Span, line: usize, path: PathBuf) -> Result<(), Error> {
        let physical = self.file(span.file).line_column(span.start).0 + 1;
        let remaining = self
            .file(span.file)
            .line_starts
            .len()
            .saturating_sub(physical);
        if line.checked_add(remaining).is_none() {
            return Err(self.error(
                Origin {
                    spelling: span,
                    expansion: None,
                },
                "logical line number exceeds the supported range",
            ));
        }
        self.files[span.file.0]
            .logical_lines
            .push((physical, line, path));
        Ok(())
    }

    pub(crate) fn add_expansion(&mut self, expansion: Expansion) -> ExpansionId {
        let id = ExpansionId(self.expansions.len());
        self.expansions.push(expansion);
        id
    }

    /// Outermost invocation for diagnostics; the original spelling remains
    /// available separately through `origin.spelling`.
    pub fn invocation(&self, mut origin: Origin) -> Span {
        while let Some(id) = origin.expansion {
            origin = self.expansion(id).invocation;
        }
        origin.spelling
    }

    pub(crate) fn expansion_depth(&self, mut origin: Origin) -> usize {
        let mut depth = 0;
        while let Some(id) = origin.expansion {
            depth += 1;
            origin = self.expansion(id).invocation;
        }
        depth
    }

    pub(crate) fn error(&self, origin: Origin, message: impl Into<String>) -> Error {
        let span = self.invocation(origin);
        let (path, line, column) = self.logical_location(span);
        Error {
            message: message.into(),
            origin,
            path: path.to_owned(),
            line,
            column,
        }
    }
}

/// An error always refers to original source, including expanded macro text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Error {
    pub message: String,
    pub origin: Origin,
    pub path: PathBuf,
    pub line: usize,
    pub column: usize,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}:{}: {}",
            self.path.display(),
            self.line,
            self.column,
            self.message
        )
    }
}
impl std::error::Error for Error {}
