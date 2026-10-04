//! Obligation generation from validated state-transition designs.
mod checker;
mod program;
pub use checker::{check, check_design, check_editor_request, validate_proof_metadata};

mod specification;
pub use specification::check_specification;
mod spec_binding;

mod scoped_specification;
pub use scoped_specification::check_scoped_specification;
mod scoped_binding;

mod quantified_examples;

pub mod lemma_candidate;
pub mod proof_program;
