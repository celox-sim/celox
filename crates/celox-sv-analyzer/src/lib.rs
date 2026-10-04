//! SystemVerilog analyzer for Celox.
//!
//! This crate intentionally mirrors the role of `veryl-analyzer`: it owns
//! SystemVerilog syntax parsing, semantic analysis, elaboration, and analyzer
//! IR. It does not depend on `celox` and must not know about SLT or SIR.

// Rustdoc's auto-trait analysis traverses sv-parser's deeply nested syntax tree.
#![recursion_limit = "512"]

use std::path::Path;

use fxhash::FxHashMap as HashMap;
use thiserror::Error;

pub mod analyze;
pub mod ast;
pub mod ir;
pub mod symbol;
pub mod syntax;
pub mod typecheck;

pub use ast::packages::PackageSource;
pub use ast::{ModuleInterface, ModuleInterfaces};
pub use ir::Ir;

/// Internal marker used to defer division-by-zero state handling until the
/// simulator's two-state/four-state mode is known.
pub const DIV_ZERO_UNKNOWN_LITERAL: &str = "$celox_div_zero_unknown";

/// Errors reported by the SystemVerilog analyzer.
#[derive(Debug, Error)]
pub enum AnalyzerError {
    #[error("SystemVerilog parse error: {0}")]
    Parse(String),
    #[error("Unsupported SystemVerilog construct: {0}")]
    Unsupported(String),
    #[error("Duplicate module declaration: {name}")]
    DuplicateModule { name: String },
    #[error("Duplicate port declaration in module `{module}`: {name}")]
    DuplicatePort { module: String, name: String },
    #[error("Duplicate parameter declaration in module `{module}`: {name}")]
    DuplicateParameter { module: String, name: String },
    #[error("Duplicate instance declaration in module `{module}`: {name}")]
    DuplicateInstance { module: String, name: String },
}

impl miette::Diagnostic for AnalyzerError {}

/// Roadmap issue for the SystemVerilog frontend, used for rejections that have
/// no dedicated tracking issue.
pub const SV_FRONTEND_TRACKING_ISSUE: u32 = 88;

/// Dedicated tracking issues, keyed by the leading text of the construct name
/// carried by [`AnalyzerError::Unsupported`].
const UNSUPPORTED_CONSTRUCT_ISSUES: &[(&str, u32)] = &[
    ("blocking assignment inside always_ff", 421),
    ("initial construct", 425),
    ("non-ANSI module port declarations", 426),
    ("ref port direction", 427),
    ("always and always_latch processes", 431),
    ("non-zero-based multidimensional packed range", 438),
    ("variable declaration initializer", 439),
    (
        "unpacked struct, union, or unsupported packed struct member",
        440,
    ),
    ("procedural loop inside always_ff", 441),
    ("wildcard port connection", 442),
    ("mixed clock-edge polarities for one signal", 443),
    ("delayed continuous assignment", 444),
    ("duplicate internal signal", 445),
    ("loop-generate unroll limit exceeded", 448),
    ("concatenated always_ff assignment target", 450),
    ("iff-qualified always_ff event", 452),
    ("nonblocking assignment inside always_comb", 453),
    ("genvar update operator", 455),
    ("reduction operator in parameter expression", 456),
    ("gate primitive instantiation", 457),
    ("procedural loop inside always_comb", 459),
    ("undriven net declaration", 460),
    ("non-integer module parameter override", 461),
    ("always_ff event expression", 464),
    ("selected or composite assignment inside function", 466),
    ("mixed reset-edge polarities for one signal", 471),
];

impl AnalyzerError {
    /// GitHub issue that tracks support for the construct this error rejects.
    pub fn tracking_issue(&self) -> u32 {
        let Self::Unsupported(construct) = self else {
            return SV_FRONTEND_TRACKING_ISSUE;
        };
        UNSUPPORTED_CONSTRUCT_ISSUES
            .iter()
            .find(|(prefix, _)| construct.starts_with(prefix))
            .map_or(SV_FRONTEND_TRACKING_ISSUE, |&(_, issue)| issue)
    }
}

/// Parse and analyze a SystemVerilog source string.
pub fn analyze_source(code: &str, path: &Path) -> Result<Ir, AnalyzerError> {
    let syntax_tree = syntax::parse_source(code, path)?;
    let source = ast::Source::from_syntax(&syntax_tree)?;
    analyze::analyze_source(source)
}

/// Parse and analyze a SystemVerilog source with parameter overrides applied
/// to one module before generate elaboration.
pub fn analyze_source_with_module_parameter_overrides(
    code: &str,
    path: &Path,
    module_name: &str,
    parameter_overrides: &HashMap<String, i128>,
) -> Result<Ir, AnalyzerError> {
    let syntax_tree = syntax::parse_source(code, path)?;
    let source = ast::Source::from_syntax_with_module_parameter_overrides(
        &syntax_tree,
        module_name,
        parameter_overrides,
    )?;
    analyze::analyze_source(source)
}

/// Return the module names declared in a SystemVerilog source without
/// performing semantic lowering of their bodies or port declarations.
pub fn source_module_names(code: &str, path: &Path) -> Result<Vec<String>, AnalyzerError> {
    let syntax_tree = syntax::parse_source(code, path)?;
    ast::Source::module_names_from_syntax(&syntax_tree)
}

/// Return whether implicit nets are enabled when each module is declared.
#[doc(hidden)]
pub fn source_module_implicit_net_permissions(
    code: &str,
    path: &Path,
) -> Result<Vec<(String, bool)>, AnalyzerError> {
    syntax::source_module_implicit_net_permissions(code, path)
}

/// Analyze only one module from a source file, applying its parameter
/// overrides before generate elaboration.
pub fn analyze_source_module_with_parameter_overrides(
    code: &str,
    path: &Path,
    module_name: &str,
    parameter_overrides: &HashMap<String, i128>,
) -> Result<Ir, AnalyzerError> {
    let syntax_tree = syntax::parse_source(code, path)?;
    let source = ast::Source::from_syntax_module_with_parameter_overrides(
        &syntax_tree,
        module_name,
        parameter_overrides,
    )?;
    analyze::analyze_source(source)
}

/// Analyze only one module from a source file while preserving the literal
/// types of its parameter override expressions.
///
/// `interfaces` describes modules declared in other sources, so that
/// positional port and parameter connections to them can be resolved.
pub fn analyze_source_module_with_parameter_expr_overrides(
    code: &str,
    path: &Path,
    module_name: &str,
    parameter_overrides: &HashMap<String, ir::ConstExpr>,
    interfaces: &ModuleInterfaces,
) -> Result<Ir, AnalyzerError> {
    let syntax_tree = syntax::parse_source(code, path)?;
    let parameter_overrides = parameter_overrides
        .iter()
        .map(|(name, value)| (name.clone(), value.clone().into()))
        .collect();
    let source = ast::Source::from_syntax_module_with_parameter_expr_overrides(
        &syntax_tree,
        module_name,
        &parameter_overrides,
        &interfaces.clone().into_iter().collect(),
    )?;
    analyze::analyze_source(source)
}

/// The positional interface (ports and overridable parameters) of every
/// module declared in a source, without analyzing the module bodies.
pub fn source_module_interfaces(
    code: &str,
    path: &Path,
) -> Result<ModuleInterfaces, AnalyzerError> {
    let syntax_tree = syntax::parse_source(code, path)?;
    ast::Source::module_interfaces_from_syntax(&syntax_tree)
}

#[cfg(test)]
mod tests;

/// The packages declared in a source, rewritten so that their items can be
/// inlined into the modules that use them.
pub fn source_packages(code: &str, path: &Path) -> Result<Vec<PackageSource>, AnalyzerError> {
    let syntax_tree = syntax::parse_source(code, path)?;
    ast::packages::source_packages(code, &syntax_tree)
}

/// The source of `module_name` with the packages it uses inlined, or `None`
/// when it uses no package. Source positions before the module's `endmodule`
/// are unchanged.
pub fn inline_module_packages(
    code: &str,
    path: &Path,
    module_name: &str,
    packages: &HashMap<String, PackageSource>,
) -> Result<Option<String>, AnalyzerError> {
    let syntax_tree = syntax::parse_source(code, path)?;
    ast::packages::inline_packages(code, &syntax_tree, module_name, packages)
}

/// The source of `module_name` with each `parameter type` in `overrides`
/// (`(name, data type text)`) bound to its data type, or `None` when it has no
/// such parameter.
pub fn apply_module_type_parameters(
    code: &str,
    path: &Path,
    module_name: &str,
    overrides: &[(String, String)],
) -> Result<Option<String>, AnalyzerError> {
    let syntax_tree = syntax::parse_source(code, path)?;
    ast::packages::apply_type_parameter_overrides(code, &syntax_tree, module_name, overrides)
}
