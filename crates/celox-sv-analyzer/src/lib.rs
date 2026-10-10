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
mod parsed;
pub mod procedural;
pub mod symbol;
pub mod syntax;
pub mod system_functions;
pub mod typecheck;

pub use ast::packages::Packages;
pub use ast::{ModuleInterface, ModuleInterfaces};
pub use ir::Ir;
pub use parsed::ParsedSource;

/// Internal marker used to defer division-by-zero state handling until the
/// simulator's two-state/four-state mode is known.
pub const DIV_ZERO_UNKNOWN_LITERAL: &str = "$celox_div_zero_unknown";

/// Errors reported by the SystemVerilog analyzer.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AnalyzerError {
    #[error("SystemVerilog parse error: {0}")]
    Parse(String),
    #[error("Unsupported SystemVerilog construct: {0}")]
    Unsupported(String),
    /// A memory file read by `$readmemh` or `$readmemb` is missing or invalid.
    #[error("Invalid $readmemh input: {0}")]
    MemoryFile(String),
    #[error("Duplicate module declaration: {name}")]
    DuplicateModule { name: String },
    #[error("Duplicate package declaration: {name}")]
    DuplicatePackage { name: String },
    #[error("Duplicate modport declaration in interface `{interface}`: {name}")]
    DuplicateModport { interface: String, name: String },
    #[error("Duplicate declaration in interface `{interface}`: {name}")]
    DuplicateInterfaceItem { interface: String, name: String },
    #[error("Interface `{interface}` has no parameter `{name}`")]
    UnknownInterfaceParameter { interface: String, name: String },
    #[error("Parameter `{name}` of interface `{interface}` is overridden more than once")]
    DuplicateInterfaceParameterOverride { interface: String, name: String },
    #[error("Modport `{modport}` of interface `{interface}` lists `{name}` more than once")]
    DuplicateModportItem {
        interface: String,
        modport: String,
        name: String,
    },
    #[error(
        "Modport `{modport}` of interface `{interface}` names `{name}`, which is not {expected}"
    )]
    UnknownModportItem {
        interface: String,
        modport: String,
        name: String,
        expected: &'static str,
    },
    #[error("Duplicate port declaration in module `{module}`: {name}")]
    DuplicatePort { module: String, name: String },
    #[error("Duplicate parameter declaration in module `{module}`: {name}")]
    DuplicateParameter { module: String, name: String },
    #[error("Duplicate instance declaration in module `{module}`: {name}")]
    DuplicateInstance { module: String, name: String },
    #[error("Generate block `{name}` in module `{module}` has the name of another declaration")]
    DuplicateGenerateScope { module: String, name: String },
    /// A package scope or an import names a package that is not declared.
    #[error("unknown package `{name}`")]
    UnknownPackage { name: String },
    /// `p::x`, or `import p::x;`, where package `p` declares no `x`.
    #[error("package `{package}` has no item `{name}`")]
    UnknownPackageItem { package: String, name: String },
    /// An import that IEEE 1800-2023 26.3 makes illegal, or a reference that
    /// matches names of two wildcard-imported packages.
    #[error("import of `{name}`: {detail}")]
    ImportConflict { name: String, detail: String },
    /// Packages that import or refer to each other.
    #[error("package `{name}` depends on itself")]
    PackageCycle { name: String },
    #[error("unknown top-level parameter override `{name}`")]
    UnknownParameterOverride { name: String },
    #[error("localparam override `{name}`")]
    LocalParameterOverride { name: String },
    /// A `$name` call that is not a system task or function Celox knows.
    #[error("unknown system task or function `{name}`")]
    UnknownSystemTf { name: String },
    /// A call of a system task or function that is not valid where it is,
    /// such as a task used as a value or a wrong number of arguments.
    #[error("invalid call of `{name}`: {detail}")]
    InvalidSystemTfCall { name: String, detail: String },
    /// An unpacked array assigned, passed as a subroutine argument, connected
    /// to a port, or used as an assignment pattern item where its type is not
    /// assignment compatible with the target array (IEEE 1800-2023 7.6, 10.8).
    #[error(
        "{context}: an unpacked array of type `{actual}` is not assignment compatible with `{target}`"
    )]
    IncompatibleUnpackedArray {
        context: String,
        actual: typecheck::UnpackedArrayType,
        target: typecheck::UnpackedArrayType,
    },
}

impl miette::Diagnostic for AnalyzerError {}

/// Roadmap issue for the SystemVerilog frontend, used for rejections that have
/// no dedicated tracking issue.
pub const SV_FRONTEND_TRACKING_ISSUE: u32 = 88;

/// Dedicated tracking issues, keyed by the leading text of the construct name
/// carried by [`AnalyzerError::Unsupported`].
const UNSUPPORTED_CONSTRUCT_ISSUES: &[(&str, u32)] = &[
    ("non-ANSI module port declarations", 426),
    ("ref port direction", 427),
    ("always and always_latch processes", 431),
    ("non-zero-based multidimensional packed range", 438),
    (
        "unpacked struct, union, or unsupported packed struct member",
        440,
    ),
    ("wildcard port connection", 442),
    ("mixed clock-edge polarities for one signal", 443),
    ("delayed continuous assignment", 444),
    ("duplicate internal signal", 445),
    ("streaming concatenation", 447),
    ("loop-generate unroll limit exceeded", 448),
    ("iff-qualified always_ff event", 452),
    ("nonblocking assignment inside always_comb", 453),
    ("genvar update operator", 455),
    ("reduction operator in parameter expression", 456),
    ("gate primitive instantiation", 457),
    ("undriven net declaration", 460),
    ("non-integer module parameter override", 461),
    ("always_ff event expression", 464),
    ("mixed reset-edge polarities for one signal", 471),
    ("package net", 1146),
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

/// Rewrite `sources` so that they no longer declare or use interfaces, or
/// return `None` when no source declares one. See [`ast::interfaces`].
pub fn elaborate_interfaces(
    sources: &[(&str, &Path)],
) -> Result<Option<Vec<String>>, AnalyzerError> {
    ast::interfaces::elaborate_interfaces(sources)
}

/// Parse and analyze a SystemVerilog source string.
pub fn analyze_source(code: &str, path: &Path) -> Result<Ir, AnalyzerError> {
    ast::with_call_sites(|| {
        let syntax_tree = syntax::parse_source(code, path)?;
        let source = ast::Source::from_syntax(&syntax_tree)?;
        analyze::analyze_source(source)
    })
}

/// Parse and analyze a SystemVerilog source with parameter overrides applied
/// to one module before generate elaboration.
pub fn analyze_source_with_module_parameter_overrides(
    code: &str,
    path: &Path,
    module_name: &str,
    parameter_overrides: &HashMap<String, i128>,
) -> Result<Ir, AnalyzerError> {
    ast::with_call_sites(|| {
        let syntax_tree = syntax::parse_source(code, path)?;
        let source = ast::Source::from_syntax_with_module_parameter_overrides(
            &syntax_tree,
            module_name,
            parameter_overrides,
        )?;
        analyze::analyze_source(source)
    })
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
    ast::with_call_sites(|| {
        let syntax_tree = syntax::parse_source(code, path)?;
        let source = ast::Source::from_syntax_module_with_parameter_overrides(
            &syntax_tree,
            module_name,
            parameter_overrides,
        )?;
        analyze::analyze_source(source)
    })
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
    ParsedSource::parse(code, path)?.analyze_module_with_parameter_expr_overrides(
        module_name,
        parameter_overrides,
        interfaces,
    )
}

/// The positional interface (ports and overridable parameters) of every
/// module declared in a source, without analyzing the module bodies.
pub fn source_module_interfaces(
    code: &str,
    path: &Path,
) -> Result<ModuleInterfaces, AnalyzerError> {
    ast::with_call_sites(|| {
        let syntax_tree = syntax::parse_source(code, path)?;
        ast::Source::module_interfaces_from_syntax(&syntax_tree)
    })
}

#[cfg(test)]
mod tests;

/// Analyze the packages declared in `sources`, each once, after the packages
/// it depends on.
pub fn analyze_packages(sources: &[(&str, &Path)]) -> Result<Packages, AnalyzerError> {
    let parsed = sources
        .iter()
        .map(|(code, path)| ParsedSource::parse(code, path))
        .collect::<Result<Vec<_>, _>>()?;
    ParsedSource::analyze_packages(&parsed.iter().collect::<Vec<_>>())
}

/// Analyze one module of a source with parameter overrides applied before
/// generate elaboration. The module may use `packages`, which may be
/// declared in other sources.
pub fn analyze_source_module_with_packages(
    code: &str,
    path: &Path,
    module_name: &str,
    parameter_overrides: &HashMap<String, i128>,
    packages: &Packages,
) -> Result<Ir, AnalyzerError> {
    let overrides = parameter_overrides
        .iter()
        .map(|(name, value)| {
            let literal = ir::ConstExpr::Literal(value.unsigned_abs().to_string());
            let value = if *value < 0 {
                ir::ConstExpr::Unary {
                    op: ir::UnaryOp::Minus,
                    expr: Box::new(literal),
                }
            } else {
                literal
            };
            (name.clone(), value)
        })
        .collect();
    ParsedSource::parse(code, path)?.analyze_module(
        module_name,
        &overrides,
        &[],
        &ModuleInterfaces::default(),
        packages,
    )
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
    ast::type_parameters::apply_type_parameter_overrides(code, &syntax_tree, module_name, overrides)
}
