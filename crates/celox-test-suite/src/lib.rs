#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

mod backend;
mod scalar;
pub mod script;
pub mod sv;
pub mod veryl;

#[cfg(feature = "external")]
mod frontend;
#[cfg(feature = "external")]
pub mod icarus;
#[cfg(feature = "external")]
mod process;
#[cfg(feature = "external")]
pub mod verification;
#[cfg(feature = "external")]
pub mod verilator;

pub use backend::{
    Backend, Design, Event, Factory, Instance, Signal, SignalPath, Simulator, Source,
};
#[cfg(feature = "external")]
pub use frontend::{Frontend, PreparedDesign};
pub use num_bigint::BigUint;
pub use scalar::Scalar;

/// Errors from a compiler or simulator adapter. Assertions fail by panicking,
/// just like ordinary Rust tests; adapter errors are included in that failure.
pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;

/// Explicit language rejection returned by a compiler factory.
///
/// Return `Err(CompilationRejected(diagnostic).into())` only when the compiler
/// rejects the input HDL. Missing tools, I/O errors, timeouts, unsupported
/// adapter operations, and compiler panics must remain ordinary errors.
/// Only this marker satisfies [`Expectation::CompilationError`].
#[derive(Debug)]
pub struct CompilationRejected(pub String);

impl std::fmt::Display for CompilationRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CompilationRejected {}

/// Language area used to select a subset of a suite.
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

/// Reviewed expectations that go beyond SystemVerilog's portable requirements.
/// These tags describe the oracle, independently of tool-specific exclusions.
/// An untagged case is not a certification of SV conformance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TestTag {
    EvaluationOrder,
    AssignmentPatternEvaluation,
    DeferredFunctionEffects,
    EagerAssertionMessages,
    TwoStateZeroDivision,
    TwoStateInitialization,
}

impl TestTag {
    /// Stable machine-readable identifier, independent of Rust variant names.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EvaluationOrder => "evaluation_order",
            Self::AssignmentPatternEvaluation => "assignment_pattern_evaluation",
            Self::DeferredFunctionEffects => "deferred_function_effects",
            Self::EagerAssertionMessages => "eager_assertion_messages",
            Self::TwoStateZeroDivision => "two_state_zero_division",
            Self::TwoStateInitialization => "two_state_initialization",
        }
    }

    /// Why the tagged oracle needs more than the SV requirements.
    pub const fn reason(self) -> &'static str {
        match self {
            Self::EvaluationOrder => {
                "Requires a particular evaluation order for effectful arguments or expression operands; SV does not guarantee that order (IEEE 1800-2023 13.5, 11.4.2)."
            }
            Self::AssignmentPatternEvaluation => {
                "Requires source-order evaluation or a particular evaluation count for effectful assignment-pattern items; SV does not guarantee these choices (IEEE 1800-2023 10.9.1, 11.4.2)."
            }
            Self::DeferredFunctionEffects => {
                "Requires Celox's deferred FF function effects so subsequent statements read pre-edge state; SV subroutine copy-out is blocking (IEEE 1800-2023 4.9.7)."
            }
            Self::EagerAssertionMessages => {
                "Requires message argument effects even on a successful assertion; SV executes the fail action only on failure (IEEE 1800-2023 16.3)."
            }
            Self::TwoStateZeroDivision => {
                "Requires the suite's totalized two-state division/remainder result of zero, including logic destinations; SV arithmetic produces X before any two-state destination conversion (IEEE 1800-2023 11.3.4, 11.4.3)."
            }
            Self::TwoStateInitialization => {
                "Explicitly requires the suite's zero-initialized two-state storage even for emitted four-state logic; SV default initialization depends on the declared type (IEEE 1800-2023 6.8, Table 6-7)."
            }
        }
    }
}

/// Resolves a `(std PATH)` source part to the text of a standard library file.
pub type StdResolver = fn(&str) -> String;

/// One reusable test. Names are stable `group::test_name` identifiers.
/// Backend-specific exclusions belong in the consuming project's runner.
pub struct TestCase {
    pub name: &'static str,
    pub category: Category,
    pub expectation: Expectation,
    pub tags: &'static [TestTag],
    script: &'static script::ScriptCase,
    std_resolver: StdResolver,
}

impl TestCase {
    /// Whether a reviewed assertion relies on behavior beyond portable SV.
    /// Untagged cases may still rely on the suite's general adapter contract.
    pub fn has_stronger_than_sv_expectations(&self) -> bool {
        !self.tags.is_empty()
    }

    /// Compile and run this case with a fresh backend supplied by `factory`.
    /// For `CompilationError`, the factory must return [`CompilationRejected`].
    /// Every other error fails the case, including infrastructure failures.
    /// Panics on a failed assertion or adapter error.
    pub fn run(&self, factory: &mut Factory<'_>) {
        let compiled = factory(&self.design());
        if self.expectation == Expectation::CompilationError {
            match compiled {
                Err(error) if error.is::<CompilationRejected>() => {}
                Err(error) => panic!(
                    "compile {}: expected language rejection, got adapter failure: {error}",
                    self.name
                ),
                Ok(_) => panic!("invalid design was accepted: {}", self.name),
            }
            return;
        }
        let backend = compiled.unwrap_or_else(|error| panic!("compile {}: {error}", self.name));
        let mut sim = Simulator::new(backend);
        script::interp::run(self.script, &mut sim);
    }

    /// The case's script in the [`script`] language, which an adapter can
    /// run without the Rust driver (for example as a generated testbench).
    pub fn script(&self) -> &'static script::ScriptCase {
        self.script
    }

    /// The design this case compiles.
    pub fn design(&self) -> Design {
        self.script.design(self.std_resolver)
    }
}

impl script::ScriptCase {
    /// The design this case compiles, with `(std PATH)` parts read through
    /// `std_resolver`.
    pub fn design(&self, std_resolver: StdResolver) -> Design {
        Design {
            sources: self
                .sources
                .iter()
                .map(|(path, parts)| Source {
                    text: parts
                        .iter()
                        .map(|part| match part {
                            script::ast::SourcePart::Text(text) => text.clone(),
                            script::ast::SourcePart::Std(path) => std_resolver(path),
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                    path: path.clone(),
                })
                .collect(),
            top: self.top.clone(),
            four_state: self.four_state,
            parameters: self.parameters.clone(),
            timed: self.is_timed(),
        }
    }
}

/// A resolver for suites without a standard library: any `(std PATH)` part
/// is an error.
pub fn no_std_library(path: &str) -> String {
    panic!("this suite has no standard library (std \"{path}\")")
}

/// Parse a script group into cases that live for the rest of the program.
/// `file` names the group in diagnostics. Panics on a malformed script: the
/// groups are compiled into the suite crates.
pub fn load_group(file: &str, text: &str, std_resolver: StdResolver) -> Vec<&'static TestCase> {
    let (_, parsed) = script::ast::group(text).unwrap_or_else(|error| panic!("{file}:{error}"));
    parsed
        .into_iter()
        .map(|case| -> &'static TestCase {
            let case: &'static script::ScriptCase = Box::leak(Box::new(case));
            Box::leak(Box::new(TestCase {
                name: &case.name,
                category: case.category,
                expectation: case.expectation,
                tags: &case.tags,
                script: case,
                std_resolver,
            }))
        })
        .collect()
}
