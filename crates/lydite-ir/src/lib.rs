//! Shared typed expressions and completely validated state-transition designs.
mod design;
mod scoped_specification;
mod specification;
pub use scoped_specification::*;
pub use specification::*;
pub mod lower;
mod term;
pub use design::*;
pub use lower::*;
pub use term::*;

#[cfg(test)]
mod validation_tests;

mod quantified_examples;
pub use quantified_examples::*;
