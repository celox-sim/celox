//! Obligation generation from validated state-transition designs.
mod checker;
mod program;
pub use checker::{check, check_design};

mod specification;
pub use specification::check_specification;
mod spec_binding;

mod scoped_specification;
pub use scoped_specification::check_scoped_specification;
mod scoped_binding;

mod quantified_examples;

mod proof_program;
