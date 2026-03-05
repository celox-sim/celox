use std::fmt;

/// Render a `miette::Diagnostic` to a plain-text string (no ANSI colors).
///
/// Uses `GraphicalReportHandler` with `ThemeStyles::none()` so the output is
/// safe for NAPI / JS error messages.
pub fn render_diagnostic(diag: &dyn miette::Diagnostic) -> String {
    use miette::{GraphicalReportHandler, GraphicalTheme, ThemeCharacters, ThemeStyles};

    let theme = GraphicalTheme {
        characters: ThemeCharacters::unicode(),
        styles: ThemeStyles::none(),
    };
    let handler = GraphicalReportHandler::new_themed(theme)
        .with_links(false)
        .with_width(120);
    let mut buf = String::new();
    match handler.render_report(&mut buf, diag) {
        Ok(()) => buf,
        // Fallback: if rendering fails, use Display
        Err(_) => diag.to_string(),
    }
}

/// The specific kind of simulator error.
#[derive(Debug)]
pub enum SimulatorErrorKind {
    SIRParser(crate::ParserError),
    Analyzer(Vec<veryl_analyzer::AnalyzerError>),
    Runtime(crate::RuntimeErrorCode),
    Codegen(String),
}

/// A simulator error that may also carry accumulated analyzer warnings.
#[derive(Debug)]
pub struct SimulatorError {
    kind: Box<SimulatorErrorKind>,
    warnings: Vec<veryl_analyzer::AnalyzerError>,
}

impl SimulatorError {
    /// Create a new `SimulatorError` with no warnings.
    pub fn new(kind: SimulatorErrorKind) -> Self {
        Self {
            kind: Box::new(kind),
            warnings: Vec::new(),
        }
    }

    /// Attach warnings to this error.
    pub fn with_warnings(mut self, warnings: Vec<veryl_analyzer::AnalyzerError>) -> Self {
        self.warnings = warnings;
        self
    }

    /// Returns a reference to the error kind.
    pub fn kind(&self) -> &SimulatorErrorKind {
        &self.kind
    }

    /// Returns accumulated analyzer warnings.
    pub fn warnings(&self) -> &[veryl_analyzer::AnalyzerError] {
        &self.warnings
    }
}

impl fmt::Display for SimulatorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind.as_ref() {
            SimulatorErrorKind::SIRParser(e) => f.write_str(&render_diagnostic(e))?,
            SimulatorErrorKind::Analyzer(errors) => {
                for (i, e) in errors.iter().enumerate() {
                    if i > 0 {
                        f.write_str("\n")?;
                    }
                    f.write_str(&render_diagnostic(e))?;
                }
            }
            SimulatorErrorKind::Runtime(e) => write!(f, "Runtime error: {e}")?,
            SimulatorErrorKind::Codegen(msg) => write!(f, "JIT Code generation error: {msg}")?,
        }
        if !self.warnings.is_empty() {
            f.write_str("\n\n--- warnings ---\n\n")?;
            for (i, w) in self.warnings.iter().enumerate() {
                if i > 0 {
                    f.write_str("\n")?;
                }
                f.write_str(&render_diagnostic(w))?;
            }
        }
        Ok(())
    }
}

impl std::error::Error for SimulatorError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self.kind.as_ref() {
            SimulatorErrorKind::SIRParser(e) => Some(e),
            SimulatorErrorKind::Runtime(e) => Some(e),
            _ => None,
        }
    }
}

impl From<crate::RuntimeErrorCode> for SimulatorError {
    fn from(e: crate::RuntimeErrorCode) -> Self {
        SimulatorError::new(SimulatorErrorKind::Runtime(e))
    }
}

impl From<crate::ParserError> for SimulatorError {
    fn from(e: crate::ParserError) -> Self {
        SimulatorError::new(SimulatorErrorKind::SIRParser(e))
    }
}

impl From<String> for SimulatorError {
    fn from(msg: String) -> Self {
        SimulatorError::new(SimulatorErrorKind::Codegen(msg))
    }
}
