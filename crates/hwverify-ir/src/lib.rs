//! Shared typed expressions and completely validated state-transition designs.
mod design;
pub mod lower;
mod term;
pub use design::*;
pub use lower::*;
pub use term::*;

#[cfg(test)]
mod validation_tests;
