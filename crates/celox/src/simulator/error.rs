use thiserror::Error;
#[derive(Error, Debug)]
pub enum SimulatorError {
    #[error(transparent)]
    SIRParser(crate::ParserError),
    #[error("{}", .0.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("\n"))]
    Analyzer(Vec<veryl_analyzer::AnalyzerError>),
    #[error("Runtime error: {0}")]
    Runtime(#[from] crate::RuntimeErrorCode),
    #[error("JIT Code generation error: {0}")]
    Codegen(String),
}
