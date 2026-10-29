#![cfg_attr(
    test,
    allow(
        clippy::disallowed_macros,
        reason = "thread-local counters exist only in unit tests; non-test library builds enforce the ban"
    )
)]
//! SystemVerilog analyzer for Celox.
//!
//! Native syntax and source storage belong to [`celox_sv_syntax`]. This crate
//! owns semantic binding, elaboration and analyzer IR over native syntax.
//! It does not depend on `celox` and must not know about SLT or SIR.

use std::path::Path;

use fxhash::FxHashMap as HashMap;
use thiserror::Error;

pub mod analyze;
pub mod ast;
pub mod ir;
mod parsed;
pub mod procedural;
mod source_tree;
pub mod symbol;
pub mod syntax;
pub mod system_functions;
pub mod typecheck;

pub use ast::const_functions::RuntimeConstantFunctions;
pub use ast::interfaces::{
    InterfaceMemberPath, InterfaceMemberPaths, NativeInterfacePlan, plan_interfaces,
    plan_interfaces_with_member_paths,
};
pub use ast::interfaces::{
    InterfacePrototype, interface_prototypes, interface_prototypes_for,
    interface_prototypes_with_options,
};
pub use ast::packages::Packages;
pub use ast::{ModuleInterface, ModuleInterfaces};
pub use ir::Ir;
pub use parsed::ParsedSource;

/// Internal marker used to defer division-by-zero state handling until the
/// simulator's two-state/four-state mode is known.
pub const DIV_ZERO_UNKNOWN_LITERAL: &str = "$celox_div_zero_unknown";

/// Language compatibility checks and implementation-defined timing defaults.
#[derive(Debug, Clone, Copy, Default)]
pub struct AnalysisOptions {
    pub allow_mixed_timescales: bool,
    /// Compatibility mode: return zero for every empty array reduction,
    /// rather than the product/bitwise-AND identity. Disabled by default.
    pub legacy_empty_reductions: bool,
    /// Default unit/precision exponents; precision also bounds global resolution.
    pub default_timescale: Option<(i32, i32)>,
}

/// Errors reported by the SystemVerilog analyzer.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AnalyzerError {
    /// Diagnostics discovered before a later analysis failure.
    #[error("{error}")]
    WithWarnings {
        #[source]
        error: Box<AnalyzerError>,
        warnings: Vec<ir::AnalyzerWarning>,
    },
    #[error("SystemVerilog parse error: {0}")]
    Parse(String),
    #[error("Unsupported SystemVerilog construct: {0}")]
    Unsupported(String),
    /// A class member is inaccessible from the lexical scope (IEEE 1800-2023 8.18).
    #[error("{visibility} class member `{owner}::{name}` is not accessible from this scope")]
    InaccessibleClassMember {
        owner: String,
        name: String,
        visibility: &'static str,
    },
    /// A dotted name whose first component names no variable or interface
    /// of the scope: a hierarchical name (IEEE 1800-2023 23.6) that the
    /// caller did not resolve to a signal of another instance.
    #[error("unresolved hierarchical reference `{path}`")]
    HierarchicalReference { path: String },
    /// A delay, event control or wait statement in a function, an
    /// `always_comb` or an `always_ff` body (IEEE 1800-2023 9.2.2.2,
    /// 9.2.2.4, 13.4).
    #[error("{construct} in {context}: only `initial` and `always` processes may suspend")]
    TimingControlOutsideProcess {
        construct: &'static str,
        context: &'static str,
    },
    /// A memory file read by `$readmemh` or `$readmemb` is missing or invalid.
    #[error("Invalid memory file input: {0}")]
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
    /// A handle of class `actual` assigned to a variable of class `target`
    /// that `actual` neither derives from nor implements; only `$cast` may
    /// assign it (IEEE 1800-2023 8.16, 8.26.5).
    #[error(
        "{context}: a handle of class `{actual}` is not assignment compatible with class `{target}`"
    )]
    IncompatibleClassHandle {
        context: String,
        actual: String,
        target: String,
    },
    /// `posedge`, `negedge` or `edge` of a real expression, which has no
    /// least significant bit to judge the edge on (IEEE 1800-2023 6.12).
    #[error("`{edge}` of a real expression: edge event controls do not apply to reals")]
    RealEdgeEvent { edge: &'static str },
    /// An item a program shall not contain: an `always` procedure, or a
    /// declaration or instance of a module, interface or program (IEEE
    /// 1800-2023 24.3).
    #[error("{item} in program `{program}`: a program shall not contain it (IEEE 1800-2023 24.3)")]
    IllegalProgramItem { program: String, item: String },
    /// An interface class that inherits parameters or typedefs named `name`
    /// from two different interface classes or specializations without
    /// declaring its own `name` (IEEE 1800-2023 8.26.6.2, 8.26.6.3).
    #[error(
        "interface class `{class}` inherits `{name}` from both `{first}` and `{second}` and must declare its own `{name}`"
    )]
    ConflictingInterfaceClassDeclaration {
        class: String,
        name: String,
        first: String,
        second: String,
    },
    /// The unadorned name of a parameterized class as the prefix of `::`
    /// outside the class and its out-of-block methods, where a
    /// specialization such as `C#()` is required (IEEE 1800-2023 8.25.1).
    #[error(
        "`{class}::` outside class `{class}`: a parameterized class needs a specialization such as `{class}#()::`"
    )]
    UnspecializedClassScope { class: String },
    /// A parameter value assignment of a class names a parameter that the
    /// parameter port list of the class does not declare, or declares local
    /// (IEEE 1800-2023 8.25).
    #[error("class `{class}` has no parameter `{name}` that a specialization may assign")]
    UnknownClassParameter { class: String, name: String },
    /// A `ref` or `const ref` formal bound to an actual, or used in a way,
    /// that IEEE 1800-2023 13.5.2 forbids.
    #[error("ref argument `{argument}` of `{subroutine}`: {detail}")]
    InvalidRefArgument {
        subroutine: String,
        argument: String,
        detail: String,
    },
    /// Actuals that cannot be bound to the formals of a task or function:
    /// a name it has no formal for, a formal bound twice, too many actuals,
    /// a positional actual after a named one (IEEE 1800-2023 13.5.4), or
    /// none for a formal without a default (13.5.3).
    #[error("arguments of `{subroutine}`: {detail}")]
    InvalidCallArguments { subroutine: String, detail: String },
    /// A DPI export declaration that IEEE 1800-2023 35.7 forbids.
    #[error("DPI-C export of `{name}`: {detail}")]
    InvalidDpiExport { name: String, detail: String },
}

impl miette::Diagnostic for AnalyzerError {}

/// Roadmap issue for the SystemVerilog frontend, used for rejections that have
/// no dedicated tracking issue.
pub const SV_FRONTEND_TRACKING_ISSUE: u32 = 88;

/// Dedicated tracking issues, keyed by the leading text of the construct name
/// carried by [`AnalyzerError::Unsupported`].
const UNSUPPORTED_CONSTRUCT_ISSUES: &[(&str, u32)] = &[
    ("non-ANSI port expressions", 1953),
    ("ref port direction", 427),
    ("nonblocking assignment inside always_latch", 431),
    (
        "unpacked struct, union, or unsupported packed struct member",
        440,
    ),
    ("wildcard port connection", 442),
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
    ("nonconstant module parameter override", 461),
    ("always_ff event expression", 464),
    ("package net", 1146),
];

impl AnalyzerError {
    pub(crate) fn with_warnings(self, found: Vec<ir::AnalyzerWarning>) -> Self {
        if found.is_empty() {
            return self;
        }
        match self {
            Self::WithWarnings {
                error,
                mut warnings,
            } => {
                ast::warnings::append(&mut warnings, found);
                Self::WithWarnings { error, warnings }
            }
            error => Self::WithWarnings {
                error: Box::new(error),
                warnings: found,
            },
        }
    }

    /// Diagnostics retained when semantic analysis failed.
    pub fn warnings(&self) -> &[ir::AnalyzerWarning] {
        match self {
            Self::WithWarnings { warnings, .. } => warnings,
            _ => &[],
        }
    }

    /// The underlying source error, without its separately reported diagnostics.
    pub fn into_error(self) -> Self {
        match self {
            Self::WithWarnings { error, .. } => error.into_error(),
            error => error,
        }
    }

    /// GitHub issue that tracks support for the construct this error rejects.
    pub fn tracking_issue(&self) -> u32 {
        if let Self::WithWarnings { error, .. } = self {
            return error.tracking_issue();
        }
        let Self::Unsupported(construct) = self else {
            return SV_FRONTEND_TRACKING_ISSUE;
        };
        UNSUPPORTED_CONSTRUCT_ISSUES
            .iter()
            .find(|(prefix, _)| construct.starts_with(prefix))
            .map_or(SV_FRONTEND_TRACKING_ISSUE, |&(_, issue)| issue)
    }
}

/// Time declarations for synthetic foreign declaration files, preserving the
/// authored design's explicit/default settings and simulation precision.
pub fn generated_time_declarations(
    sources: &[(&str, &Path)],
    roots: Option<&[&str]>,
) -> Result<String, AnalyzerError> {
    ast::generated_time_declarations(sources, roots)
}

/// Generate timing declarations using the selected compatibility policy.
pub fn generated_time_declarations_with_options(
    sources: &[(&str, &Path)],
    roots: Option<&[&str]>,
    options: AnalysisOptions,
) -> Result<String, AnalyzerError> {
    ast::timescales::generated_source_declarations(sources, roots, options)
}

/// Parse and analyze a SystemVerilog source string.
pub fn analyze_source(code: &str, path: &Path) -> Result<Ir, AnalyzerError> {
    let syntax_tree = source_tree::parse_source(code, path)?;
    let warnings = ast::warnings::source(&syntax_tree);
    collecting_warnings(warnings, || {
        if syntax_tree.declarations().all_units().any(|unit| {
            unit.kind == celox_sv_syntax::UnitKind::Interface
                && unit.port_style == celox_sv_syntax::PortStyle::Ansi
                && !unit.external
        }) {
            return analyze_interface_source(code, path, "", &HashMap::default());
        }
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
    let syntax_tree = source_tree::parse_source(code, path)?;
    let warnings = ast::warnings::source(&syntax_tree);
    collecting_warnings(warnings, || {
        if syntax_tree.declarations().all_units().any(|unit| {
            unit.kind == celox_sv_syntax::UnitKind::Interface
                && unit.port_style == celox_sv_syntax::PortStyle::Ansi
                && !unit.external
        }) {
            return analyze_interface_source(code, path, module_name, parameter_overrides);
        }
        let source = ast::Source::from_syntax_with_module_parameter_overrides(
            &syntax_tree,
            module_name,
            parameter_overrides,
        )?;
        analyze::analyze_source(source)
    })
}

fn analyze_interface_source(
    code: &str,
    path: &Path,
    override_module: &str,
    overrides: &HashMap<String, i128>,
) -> Result<Ir, AnalyzerError> {
    let source = ParsedSource::parse(code, path)?;
    let packages = ParsedSource::analyze_packages(&[&source])?;
    let plan = &packages.interface_plan;
    let mut names = source
        .module_names()
        .iter()
        .filter(|name| {
            plan.modules.contains_key(*name)
                || !plan
                    .modules
                    .values()
                    .any(|binding| binding.original == **name)
        })
        .cloned()
        .collect::<Vec<_>>();
    names.extend(
        plan.modules
            .keys()
            .filter(|name| !source.module_names().contains(name))
            .cloned(),
    );
    let mut modules = Vec::new();
    let mut warnings = packages.warnings().to_vec();
    for name in names {
        let values = if name == override_module
            || plan
                .modules
                .get(&name)
                .is_some_and(|binding| binding.original == override_module)
        {
            overrides
                .iter()
                .map(|(name, value)| {
                    (
                        name.clone(),
                        if *value < 0 {
                            ir::ConstExpr::Unary {
                                op: ir::UnaryOp::Minus,
                                expr: Box::new(ir::ConstExpr::Literal(
                                    value.unsigned_abs().to_string(),
                                )),
                            }
                        } else {
                            ir::ConstExpr::Literal(value.to_string())
                        },
                    )
                })
                .collect()
        } else {
            HashMap::default()
        };
        let ir = source.analyze_module(
            &name,
            &values,
            &[],
            &[],
            &ModuleInterfaces::default(),
            &packages,
        )?;
        modules.extend(ir.modules().iter().cloned());
        warnings.extend(ir.warnings().iter().cloned());
    }
    Ok(Ir::new(modules).with_warnings(warnings))
}

/// Publish diagnostics as owned result data, on either exit path.
fn collecting_warnings(
    warnings: Vec<ir::AnalyzerWarning>,
    analysis: impl FnOnce() -> Result<Ir, AnalyzerError>,
) -> Result<Ir, AnalyzerError> {
    finish_analysis(analysis(), warnings)
}

pub(crate) fn finish_analysis(
    result: Result<Ir, AnalyzerError>,
    warnings: Vec<ir::AnalyzerWarning>,
) -> Result<Ir, AnalyzerError> {
    match result {
        Ok(ir) => Ok(ir.with_warnings(warnings)),
        Err(error) => Err(error.with_warnings(warnings)),
    }
}

/// Return the module names declared in a SystemVerilog source without
/// performing semantic lowering of their bodies or port declarations.
pub fn source_module_names(code: &str, path: &Path) -> Result<Vec<String>, AnalyzerError> {
    let syntax_tree = source_tree::parse_source(code, path)?;
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
    let syntax_tree = source_tree::parse_source(code, path)?;
    let warnings = ast::warnings::components(&syntax_tree)
        .remove(module_name)
        .unwrap_or_default();
    collecting_warnings(warnings, || {
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
    Ok(ParsedSource::parse(code, path)?.module_interfaces().clone())
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
        &[],
        &ModuleInterfaces::default(),
        packages,
    )
}
