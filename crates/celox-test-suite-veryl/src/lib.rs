#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

mod backend;
mod cases;
mod scalar;
pub mod script;

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

/// One reusable test. Names are stable `group::test_name` identifiers.
/// Backend-specific exclusions belong in the consuming project's runner.
pub struct TestCase {
    pub name: &'static str,
    pub category: Category,
    pub expectation: Expectation,
    pub tags: &'static [TestTag],
    body: Body,
}

enum Body {
    Script(&'static script::ScriptCase),
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
    /// Panics on a failed assertion or adapter error. A factory may be invoked
    /// more than once when a case exercises several designs.
    pub fn run(&self, factory: &mut Factory<'_>) {
        match &self.body {
            Body::Script(case) => run_script(case, factory),
        }
    }

    /// The case's script in the [`script`] language, which an adapter can
    /// run without the Rust driver (for example as a generated testbench).
    pub fn script(&self) -> &'static script::ScriptCase {
        match &self.body {
            Body::Script(case) => case,
        }
    }
}

impl script::ScriptCase {
    /// The design this case compiles.
    pub fn design(&self) -> Design {
        Design {
            sources: self
                .sources
                .iter()
                .map(|(path, parts)| Source {
                    text: parts
                        .iter()
                        .map(|part| match part {
                            script::ast::SourcePart::Text(text) => text.clone(),
                            script::ast::SourcePart::Std(path) => std_source(path),
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                    path: path.clone(),
                })
                .collect(),
            top: self.top.clone(),
            four_state: self.four_state,
        }
    }
}

/// The text of a Veryl standard library file such as `fifo/fifo.veryl`.
fn std_source(path: &str) -> String {
    use std::path::{Path, PathBuf};
    veryl_std::expand().expect("failed to expand veryl-std sources");
    let rel: PathBuf = path.split('/').collect();
    let paths = veryl_std::paths(Path::new("")).expect("failed to resolve veryl-std sources");
    let src = paths
        .iter()
        .find(|candidate| candidate.src.ends_with(&rel))
        .unwrap_or_else(|| panic!("veryl-std source not found: {path}"));
    std::fs::read_to_string(&src.src)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", src.src.display()))
}

fn run_script(case: &script::ScriptCase, factory: &mut Factory<'_>) {
    let compiled = factory(&case.design());
    if case.expectation == Expectation::CompilationError {
        match compiled {
            Err(error) if error.is::<CompilationRejected>() => {}
            Err(error) => panic!(
                "compile {}: expected language rejection, got adapter failure: {error}",
                case.name
            ),
            Ok(_) => panic!("invalid design was accepted: {}", case.name),
        }
        return;
    }
    let backend = compiled.unwrap_or_else(|error| panic!("compile {}: {error}", case.name));
    let mut sim = Simulator::new(backend);
    script::interp::run(case, &mut sim);
}

/// All cases in deterministic declaration order. Filter by category or name in the
/// host test runner; nothing is silently skipped by the suite itself.
pub fn cases() -> impl Iterator<Item = &'static TestCase> {
    static CASES: std::sync::OnceLock<Vec<&'static TestCase>> = std::sync::OnceLock::new();
    CASES
        .get_or_init(|| {
            cases::GROUPS
                .iter()
                .flat_map(|group| script_cases(group.file, group.text))
                .collect()
        })
        .iter()
        .copied()
}

fn script_cases(file: &str, text: &str) -> Vec<&'static TestCase> {
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
                body: Body::Script(case),
            }))
        })
        .collect()
}

/// Look up a case by its full stable name.
pub fn case(name: &str) -> Option<&'static TestCase> {
    cases().find(|case| case.name == name)
}
