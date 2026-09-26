#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

mod backend;
mod cases;
mod scalar;

#[cfg(feature = "emit")]
pub mod emit;
#[cfg(feature = "icarus")]
pub mod icarus;
#[cfg(any(feature = "verilator", feature = "icarus"))]
mod process;
#[cfg(any(feature = "verilator", feature = "icarus"))]
pub mod verification;
#[cfg(feature = "verilator")]
pub mod verilator;

pub use backend::{
    Backend, Design, Event, Factory, Instance, Signal, SignalPath, Simulator, Source,
};
pub use num_bigint::BigUint;
pub use scalar::Scalar;

/// Errors from a compiler or simulator adapter. Assertions fail by panicking,
/// just like ordinary Rust tests; adapter errors are included in that failure.
pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;

/// Language area used to select a subset of the suite.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Category {
    Combinational,
    Operators,
    Types,
    Arrays,
    Hierarchy,
    Sequential,
    FourState,
    Functions,
    ControlFlow,
    StandardLibrary,
    Regression,
}

/// Whether a case exercises simulation or rejection of an invalid design.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expectation {
    Simulation,
    CompilationError,
}

/// One reusable test. Names are stable `group::test_name` identifiers.
/// Backend-specific exclusions belong in the consuming project's runner.
pub struct TestCase {
    pub name: &'static str,
    pub category: Category,
    pub expectation: Expectation,
    run: fn(&mut Factory<'_>),
}

impl TestCase {
    /// Compile and run this case with a fresh backend supplied by `factory`.
    /// For `CompilationError`, the factory must return a compilation diagnostic;
    /// adapters must keep infrastructure failures distinct from language rejection.
    /// Panics on a failed assertion or adapter error. A factory may be invoked
    /// more than once when a case exercises several designs.
    pub fn run(&self, factory: &mut Factory<'_>) {
        (self.run)(factory);
    }
}

/// All cases in deterministic declaration order. Filter by category or name in the
/// host test runner; nothing is silently skipped by the suite itself.
pub fn cases() -> impl Iterator<Item = &'static TestCase> {
    cases::GROUPS.iter().flat_map(|group| group.iter())
}

/// Look up a case by its full stable name.
pub fn case(name: &str) -> Option<&'static TestCase> {
    cases().find(|case| case.name == name)
}
