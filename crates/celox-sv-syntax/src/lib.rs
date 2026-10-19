//! SystemVerilog source storage, lexical analysis, preprocessing and native
//! declaration syntax. Whole-source validation remains a separate contract.
//!
//! This layer has no elaboration or simulator dependencies. Tokens retain their
//! spelling location and macro expansion chain instead of acquiring locations
//! in a rewritten source string.

mod lexer;
mod nodes;
mod parser;
mod preprocess;
mod source;

pub use lexer::{Token, TokenKind, lex};
pub use nodes::*;
pub use parser::{Declarations, ParseError, ParseErrorKind};
pub use preprocess::{IncludePolicy, Options, Preprocessed, Preprocessor};
pub use source::{Error, Expansion, ExpansionId, FileId, Origin, SourceFile, SourceMap, Span};
