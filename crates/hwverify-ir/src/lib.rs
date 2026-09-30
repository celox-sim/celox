//! Shared typed expressions and completely validated state-transition designs.
mod design;
mod specification;
pub use specification::*;
pub mod lower;
mod term;
pub use design::*;
pub use lower::*;
pub use term::*;

#[cfg(test)]
mod validation_tests;
