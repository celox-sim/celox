#![doc = include_str!("../README.md")]
//! Aplite: bounded finite Boolean, bit-vector and array constraint solving.
//!
//! The solver runs in process, without an external SMT executable. Unsupported
//! expressions and exhausted resource budgets return [`Verdict::Unknown`].
//! SAT models are replayed against the original formula; UNSAT trusts the solver.
//! This crate does not provide uniform sampling of satisfying assignments.
pub mod term;
pub use term::*;
pub type Env = std::collections::BTreeMap<String, Term>;
pub mod finite;
pub use finite::{
    Assumption, Limits, Outcome, PreparedQuery, Scalar, SearchHint, Verdict, solve, solve_seeded,
    solve_with_hint,
};

#[cfg(test)]
mod seeded_tests;

#[cfg(test)]
mod sampling_oracle;
