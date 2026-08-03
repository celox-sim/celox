use std::fmt;
use thiserror::Error;

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

fn render_analyzer_error(error: &veryl_analyzer::AnalyzerError) -> String {
    let mut rendered = render_diagnostic(error);
    if let veryl_analyzer::AnalyzerError::UnresolvableGenericExpression { identifier, .. } = error {
        rendered.push_str("\nhelp: if this is a module `param ");
        rendered.push_str(identifier);
        rendered.push_str(
            ": type`, declare it as a module generic parameter like `module ModuleName::<",
        );
        rendered.push_str(identifier);
        rendered.push_str(": type>` instead");
    }
    rendered
}

/// The specific kind of simulator error.
#[derive(Debug)]
pub enum SimulatorErrorKind {
    SIRParser(crate::ParserError),
    Analyzer(Vec<veryl_analyzer::AnalyzerError>),
    Runtime(crate::RuntimeErrorCode),
    Codegen(CodegenError),
}

/// Structured failure while preparing executable simulator code.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CodegenError {
    #[cfg(feature = "host-runtime")]
    #[error("{0}")]
    Cranelift(
        #[from]
        #[source]
        celox_backend_cranelift::CraneliftError,
    ),

    #[error("{context}: {source}")]
    Optimization {
        context: &'static str,
        #[source]
        source: celox_sir_opt::OptimizationError,
    },

    #[error("{phase}: {source}")]
    SirVerification {
        phase: String,
        #[source]
        source: celox_sir::verify::SirVerifyError,
    },

    #[cfg(all(
        feature = "host-runtime",
        any(
            target_arch = "x86_64",
            all(target_arch = "aarch64", feature = "experimental-arm64-backend")
        )
    ))]
    #[error("native emission failed: {source}")]
    NativePipeline {
        #[source]
        source: crate::backend::native::emit::ChainedEmitError,
    },

    #[cfg(all(
        feature = "host-runtime",
        any(
            target_arch = "x86_64",
            all(target_arch = "aarch64", feature = "experimental-arm64-backend")
        )
    ))]
    #[error("native emission failed: {source}")]
    NativeEmission {
        #[source]
        source: crate::backend::native::emit::EmitError,
    },

    #[cfg(all(
        feature = "host-runtime",
        any(
            target_arch = "x86_64",
            all(target_arch = "aarch64", feature = "experimental-arm64-backend")
        )
    ))]
    #[error("failed to allocate executable native memory: {source}")]
    NativeMemory {
        #[source]
        source: std::io::Error,
    },

    #[cfg(feature = "host-runtime")]
    #[error("WASM {stage} failed: {source}")]
    Wasm {
        stage: &'static str,
        #[source]
        source: wasmtime::Error,
    },

    #[cfg(feature = "host-runtime")]
    #[error("{message}")]
    Message { message: String },
}

#[cfg(feature = "host-runtime")]
impl CodegenError {
    pub(crate) fn message(message: impl Into<String>) -> Self {
        Self::Message {
            message: message.into(),
        }
    }
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
                    f.write_str(&render_analyzer_error(e))?;
                }
            }
            SimulatorErrorKind::Runtime(e) => write!(f, "Runtime error: {e}")?,
            SimulatorErrorKind::Codegen(error) => write!(f, "JIT Code generation error: {error}")?,
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
            SimulatorErrorKind::Codegen(error) => Some(error),
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

impl From<CodegenError> for SimulatorError {
    fn from(error: CodegenError) -> Self {
        SimulatorError::new(SimulatorErrorKind::Codegen(error))
    }
}

#[cfg(feature = "host-runtime")]
impl From<celox_backend_cranelift::CraneliftError> for SimulatorError {
    fn from(error: celox_backend_cranelift::CraneliftError) -> Self {
        CodegenError::from(error).into()
    }
}

#[cfg(all(test, feature = "host-runtime"))]
mod tests {
    use std::error::Error as _;

    use super::{CodegenError, SimulatorError, SimulatorErrorKind};

    #[test]
    fn codegen_error_is_available_as_the_simulator_error_source() {
        let error = SimulatorError::from(CodegenError::message("compile worker stopped"));

        assert!(matches!(error.kind(), SimulatorErrorKind::Codegen(_)));
        assert_eq!(
            error.to_string(),
            "JIT Code generation error: compile worker stopped"
        );
        assert_eq!(
            error.source().unwrap().to_string(),
            "compile worker stopped"
        );
    }

    #[test]
    fn simulator_error_keeps_the_backend_source_chain() {
        let backend = celox_backend_cranelift::CraneliftError::NativeTarget {
            message: "unsupported test target",
        };
        let error = SimulatorError::from(backend);

        let codegen = error.source().expect("codegen source");
        assert!(codegen.to_string().contains("native target"));
        let backend = codegen.source().expect("backend source");
        assert!(backend.to_string().contains("unsupported test target"));
    }
}
