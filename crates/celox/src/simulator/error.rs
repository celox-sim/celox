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

#[derive(Debug)]
pub enum SimulatorError {
    SIRParser(crate::ParserError),
    Analyzer(Vec<veryl_analyzer::AnalyzerError>),
    Runtime(crate::RuntimeErrorCode),
    Codegen(String),
}

impl fmt::Display for SimulatorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SimulatorError::SIRParser(e) => write!(f, "{e}"),
            SimulatorError::Analyzer(errors) => {
                for (i, e) in errors.iter().enumerate() {
                    if i > 0 {
                        f.write_str("\n")?;
                    }
                    f.write_str(&render_diagnostic(e))?;
                }
                Ok(())
            }
            SimulatorError::Runtime(e) => write!(f, "Runtime error: {e}"),
            SimulatorError::Codegen(msg) => write!(f, "JIT Code generation error: {msg}"),
        }
    }
}

impl std::error::Error for SimulatorError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SimulatorError::SIRParser(e) => Some(e),
            SimulatorError::Runtime(e) => Some(e),
            _ => None,
        }
    }
}

impl From<crate::RuntimeErrorCode> for SimulatorError {
    fn from(e: crate::RuntimeErrorCode) -> Self {
        SimulatorError::Runtime(e)
    }
}
