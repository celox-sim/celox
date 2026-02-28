use thiserror::Error;
#[derive(Error, Debug)]
pub enum SimulatorError {
    #[error(transparent)]
    SIRParser(crate::ParserError),
    #[error("Runtime error: {0}")]
    Runtime(#[from] crate::RuntimeErrorCode),
    #[error("JIT Code generation error: {0}")]
    Codegen(String),
}
