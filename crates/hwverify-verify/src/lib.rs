//! Obligation generation from validated state-transition designs.
mod checker;
mod program;
pub use checker::{check, check_design};

mod specification;
pub use specification::check_specification;
mod spec_binding;
