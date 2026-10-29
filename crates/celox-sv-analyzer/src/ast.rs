//! SystemVerilog frontend AST.
//!
//! This semantic language frontend is separate from Celox runtime IR.
//!
//! Public AST types and module construction live here. Child modules collect
//! declarations and processes, resolve types and constants, expand functions,
//! and normalize procedural expressions and assignments.

use fxhash::{FxHashMap as HashMap, FxHashSet as HashSet};
use std::{
    borrow::Cow,
    cell::RefCell,
    ops::{Deref, DerefMut},
    path::Path,
    sync::Arc,
};

use crate::source_tree::SourceTree as SyntaxTree;

/// State owned by one analysis session, shared explicitly with nested scopes.
#[derive(Default)]
pub(super) struct AnalysisState {
    pub(crate) const_functions: const_functions::State,
    foreign: std::rc::Rc<classes::foreign::Registry>,
    foreign_arguments: RefCell<Vec<(String, classes::specializations::BoundArguments)>>,
    foreign_names: RefCell<Vec<(String, classes::specializations::BoundArguments, String)>>,
    foreign_error: RefCell<Option<AnalyzerError>>,
    scope: scope::State,
    interfaces: interface_ast::State,
    dimensions: dimensions::State,
    root_subroutines: std::cell::Cell<bool>,
    inline_randomizations: RefCell<Vec<classes::constraints::Inline>>,
    next_call_site: std::rc::Rc<std::cell::Cell<usize>>,
    timescales: timescales::State,
    diagnostics: RefCell<Vec<warnings::AnalyzerWarning>>,
    /// Revision of the imported scope, the analyzed package, the active
    /// interface instances and the root-subroutine mode.
    revision: generate::Revision,
    generate_items: generate::Memo,
    /// Classes laid out so far, by name.
    classes: std::cell::RefCell<HashMap<String, std::rc::Rc<classes::Class>>>,
    /// The constants and types of the scope that declares each class, which
    /// its parameter defaults and body see (IEEE 1800-2023 8.25).
    class_declaration_scopes:
        std::cell::RefCell<HashMap<String, (HashMap<String, i128>, HashMap<String, Type>)>>,
    /// The classes declared in a generate block that elaboration repeats,
    /// which would be a class of their own in each repetition (27.4).
    class_declarations_repeated: std::cell::RefCell<HashSet<String>>,
    /// Set once the class methods of the scope being analyzed have their
    /// signatures, after which a new specialization would have none.
    class_specializations_frozen: std::cell::Cell<bool>,
    /// The class specializations being made, which their own declarations
    /// may name before they are complete.
    class_specializations_building: std::cell::RefCell<Vec<String>>,
    /// How many class specializations are being made, one inside another.
    class_specialization_depth: std::cell::Cell<usize>,
    /// The specializations of classes made so far, in order.
    class_specializations: std::cell::RefCell<
        Vec<(
            String,
            std::rc::Rc<classes::specializations::Specialization>,
        )>,
    >,
}

/// Syntax and specialization bindings for one scope within an analysis session.
/// Child modules share the session and receive their own parameter bindings.
struct AnalysisContext<'a> {
    syntax_tree: &'a SyntaxTree,
    state: std::rc::Rc<AnalysisState>,
    parameter_overrides: Cow<'a, HashMap<String, ConstExpr>>,
}

impl<'a> AnalysisContext<'a> {
    #[cfg(test)]
    fn new(
        syntax_tree: &'a SyntaxTree,
        parameter_overrides: &'a HashMap<String, ConstExpr>,
    ) -> Self {
        Self {
            syntax_tree,
            state: std::rc::Rc::default(),
            parameter_overrides: Cow::Borrowed(parameter_overrides),
        }
    }

    fn unspecialized(syntax_tree: &'a SyntaxTree) -> Self {
        Self {
            syntax_tree,
            state: std::rc::Rc::default(),
            parameter_overrides: Cow::Owned(HashMap::default()),
        }
    }

    fn with_packages(syntax_tree: &'a SyntaxTree, packages: &packages::Packages) -> Self {
        Self {
            syntax_tree,
            state: std::rc::Rc::new(AnalysisState {
                next_call_site: packages.call_sites.clone(),
                foreign: packages.foreign.clone(),
                ..AnalysisState::default()
            }),
            parameter_overrides: Cow::Owned(HashMap::default()),
        }
    }

    fn child<'b>(
        &self,
        syntax_tree: &'b SyntaxTree,
        parameter_overrides: &'b HashMap<String, ConstExpr>,
    ) -> AnalysisContext<'b> {
        AnalysisContext {
            syntax_tree,
            state: self.state.clone(),
            parameter_overrides: Cow::Borrowed(parameter_overrides),
        }
    }
}

impl Deref for AnalysisContext<'_> {
    type Target = SyntaxTree;

    fn deref(&self) -> &Self::Target {
        self.syntax_tree
    }
}

use crate::procedural::ParamDirection;
pub use crate::system_functions::{
    ArrayQuery, Callee, ContainerKind, ContainerLayout, ContainerOp, ObjectOp, StringOp,
    SystemFunction, SystemTask,
};
use crate::{AnalyzerError, system_functions, typecheck};

mod array_compatibility;
mod array_parameters;
mod assignment_analysis;
mod bound_patterns;
mod case;
mod casts;
mod classes;
mod comb_process;
mod processes;
pub use bound_patterns::BoundPattern;
mod comb_rewrite;
pub(crate) mod const_functions;
mod constant_binding;
mod constant_folding;
mod constants;
mod declarations;
mod dimensions;
mod dpi;
mod enums;
mod expression_binding;
mod expressions;
mod ff_process;
mod functions;
mod generate;
pub(crate) mod imports;
mod inlining;
mod instances;
mod interface_ast;
mod interface_binding;
pub mod interfaces;
mod lifetimes;
pub(crate) mod module_index;
mod package_owners;
pub mod packages;
mod packed_structs;
mod parameters;
mod port_owners;
mod timing;
mod type_binding;
pub(crate) use parameters::unbounded_parameter_marker;
mod array_reductions;
mod containers;
mod patterns;
mod procedural;
mod scope;
mod scoped_map;
mod selects;
mod shared_map;
mod statements;
mod subroutine_owners;
pub(crate) mod timescales;
pub(crate) mod type_parameters;
mod types;
mod unpacked_structs;
pub(crate) mod validation;
pub(crate) mod warnings;

use array_compatibility::selected_unpacked_shape;
use assignment_analysis::two_state_conditions_are_complements;
use case::expr_is_two_state;
use casts::{
    expr_type_from_type, resize_integral_literal_for_cast, resize_unbased_fill_literal_for_cast,
};
use comb_process::comb_processes_from_scope;
use comb_rewrite::{expr_static_width, whole_packed_lvalue};
use constant_folding::{
    fold_const_integral_expr_preserving_mask, simplify_constant_mux_conditions,
};
use constants::{
    bind_generate_parameter, eval_ast_const_expr, expr_to_const, expr_to_index_const,
    substitute_assignment_constants_with_parameter_literals, substitute_const_expr_constants,
    substitute_const_expr_constants_preserving_enum_types, substitute_dimension_constants,
    substitute_expr_constants_with_parameter_literals, substitute_lvalue_constants,
    substitute_process_constants, substitute_process_constants_with_parameter_literals,
};
use declarations::{
    module_interface_from_scope, parameters_from_scope, ports_from_scope, signals_from_body_item,
    signals_from_scope,
};
use dimensions::{
    enum_marker, extend_const_env_with_variable_types, function_packed_dimension_widths,
    insert_parameter_type_markers, local_parameter_marker,
    packed_dimensions_from_ports_and_signals, parameter_dimension_marker,
    parameter_dimensions_marker, parameter_marker, parameter_packed_dimensions,
    parameter_rank_marker, parameter_signed_element_marker, parameter_signed_marker,
    parameter_type_from_const_env, parameter_types_from_const_env, parameter_width_marker,
    unpacked_dimension_widths, variable_bits_marker, variable_dimensions_marker,
    variable_signed_marker, variable_size_function_width, variable_size_marker,
    variable_unpacked_dimensions_marker,
};
use expressions::guard_zero_divisions;
use ff_process::ff_processes_from_scope;
use functions::{functions_from_scope, procedural_truth_condition};
use inlining::{expand_expr_calls, expr_signedness_with_return_types, substitute_expr_idents};
pub(crate) use instances::normalize_identifier;
use instances::{expr_ident_name, instances_from_scope};
use parameters::{
    apply_parameter_overrides, coerce_const_parameter_value, const_env_from_parameters,
    const_expr_from_i128, const_expr_to_expr, enum_member_constants_from_scope,
    extend_const_env_with_parameters, format_typed_parameter_literal, infer_const_expr_type,
    infer_parameter_value_type, parameter_element_literal, parameter_value_env,
    substitute_typed_parameter_literals,
};
use scoped_map::{ScopedMap, Signedness};
use selects::{add_expr, product_expr};
use shared_map::SharedMap;
use statements::{
    assignment_op_expr, coerce_procedural_assignment_rhs, expr_from_lvalue, lvalue_expr_type,
};
use types::{
    type_aliases_from_scope, type_aliases_from_scope_with_env, type_with_unpacked_ranges,
    validate_unpacked_dimension_sizes,
};
use validation::{AlwaysKind, always_comb_body, always_kind, reject_silently_ignored_constructs};

/// A procedural statement of the analyzer AST.
pub type Stmt = crate::procedural::StmtBase<Expr, LValue>;

/// The result of converting syntax that Celox may not be able to represent.
type Converted<T> = Result<T, AnalyzerError>;

/// The error for a construct the analyzer cannot represent.
fn unsupported(construct: impl Into<String>) -> AnalyzerError {
    AnalyzerError::Unsupported(construct.into())
}
pub type LocalVariable = crate::procedural::LocalVariableBase<crate::ir::Type>;
pub type Subroutine = crate::procedural::SubroutineBase<Expr, LValue, crate::ir::Type>;
pub type SubroutineParam = crate::procedural::SubroutineParamBase<Expr, crate::ir::Type>;

/// The positional interface of a module: its ports in declaration order and
/// the parameters of its `#(...)` list that an instantiation may override.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModuleInterface {
    pub ports: Vec<String>,
    pub parameters: Vec<String>,
}

/// The interface of each module, by module name.
pub type ModuleInterfaces = HashMap<String, ModuleInterface>;

/// Local declarations take precedence over interfaces supplied by other files.
struct InterfaceLookup<'a> {
    local: &'a ModuleInterfaces,
    extra: &'a ModuleInterfaces,
}

impl InterfaceLookup<'_> {
    fn get(&self, name: &str) -> Option<&ModuleInterface> {
        self.local.get(name).or_else(|| self.extra.get(name))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    modules: Vec<Module>,
    diagnostics: Vec<warnings::AnalyzerWarning>,
}

/// Overrides supplied by one instantiation. Types have been resolved in its scope.
pub(crate) struct ModuleParameterBindings<'a> {
    pub declarations_only: bool,
    pub values: &'a HashMap<String, ConstExpr>,
    pub types: &'a [(String, Type)],
    pub patterns: &'a [(String, BoundPattern)],
    pub hierarchical: &'a [HierarchicalSignal],
}

/// A signal of another instance that hierarchical names of the analyzed
/// module denote (IEEE 1800-2023 23.6), resolved by the caller against the
/// elaborated hierarchy. The module refers to it by a local signal `name`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HierarchicalSignal {
    /// The names that denote the signal, as [`crate::ParsedSource::hierarchical_names`]
    /// spells them.
    pub paths: Vec<String>,
    pub name: String,
    pub r#type: Type,
    pub is_net: bool,
    /// The names denote the module's own signal `name`, which they alias.
    pub local: bool,
}

impl Source {
    pub(crate) fn from_syntax(syntax_tree: &SyntaxTree) -> Result<Self, AnalyzerError> {
        Self::from_syntax_with_module_parameter_overrides(syntax_tree, "", &HashMap::default())
    }

    pub(crate) fn from_syntax_with_module_parameter_overrides(
        syntax_tree: &SyntaxTree,
        module_name: &str,
        parameter_overrides: &HashMap<String, i128>,
    ) -> Result<Self, AnalyzerError> {
        let packages = packages::Packages::analyze(&[(syntax_tree, Path::new(""))])?;
        let context = AnalysisContext::with_packages(syntax_tree, &packages);
        let syntax_tree = &context;
        let parameter_overrides = parameter_overrides
            .iter()
            .map(|(name, value)| (name.clone(), const_expr_from_i128(*value)))
            .collect();
        let interfaces = Self::module_interfaces_from_syntax(syntax_tree)?;
        let _timescales = timescales::install_for_tree(
            &syntax_tree.state.timescales,
            packages.timescales.clone(),
            syntax_tree,
        )?;
        let mut modules = Vec::new();
        for handle in syntax_tree.root_scopes()? {
            let scoped = syntax_tree.scoped(handle)?;
            let scoped_context = context.child(&scoped, &parameter_overrides);
            let syntax_tree = &scoped_context;
            let unit = syntax_tree.root_unit()?;
            if !unit.kind.is_module_like() {
                continue;
            }
            match unit.port_style {
                celox_sv_syntax::PortStyle::Ansi => {
                    let imported = imports::imported_symbols(
                        syntax_tree,
                        &packages,
                        packages.unit(Path::new("")),
                    )?;
                    modules.push(
                        Module::from_scope_with_parameter_overrides(
                            syntax_tree,
                            module_name,
                            &parameter_overrides,
                            &InterfaceLookup {
                                local: &interfaces,
                                extra: &ModuleInterfaces::default(),
                            },
                            Arc::new(imported),
                        )
                        .map_err(|error| {
                            error.with_warnings(context.state.diagnostics.borrow().clone())
                        })?,
                    );
                }
                celox_sv_syntax::PortStyle::NonAnsi => {
                    return Err(AnalyzerError::Unsupported(
                        "non-ANSI port expressions".to_string(),
                    ));
                }
                _ => {}
            }
        }

        Ok(Self {
            modules,
            diagnostics: context.state.diagnostics.borrow().clone(),
        })
    }

    pub(crate) fn from_syntax_module_with_parameter_overrides(
        syntax_tree: &SyntaxTree,
        module_name: &str,
        parameter_overrides: &HashMap<String, i128>,
    ) -> Result<Self, AnalyzerError> {
        let parameter_overrides = parameter_overrides
            .iter()
            .map(|(name, value)| (name.clone(), const_expr_from_i128(*value)))
            .collect();
        Self::from_syntax_module_with_parameter_expr_overrides(
            syntax_tree,
            module_name,
            &parameter_overrides,
            &ModuleInterfaces::default(),
        )
    }

    /// `extra_interfaces` describes modules declared in other sources; the
    /// modules of `syntax_tree` are always known.
    pub(crate) fn from_syntax_module_with_parameter_expr_overrides(
        syntax_tree: &SyntaxTree,
        module_name: &str,
        parameter_overrides: &HashMap<String, ConstExpr>,
        extra_interfaces: &ModuleInterfaces,
    ) -> Result<Self, AnalyzerError> {
        let index = module_index::ModuleIndex::new(syntax_tree)?;
        let packages = packages::Packages::analyze(&[(syntax_tree, Path::new(""))])?;
        Self::from_indexed_syntax_module(
            syntax_tree,
            Path::new(""),
            &index,
            module_name,
            ModuleParameterBindings {
                declarations_only: false,
                values: parameter_overrides,
                types: &[],
                patterns: &[],
                hierarchical: &[],
            },
            extra_interfaces,
            &packages,
        )
    }

    /// `packages` are the packages the module may use, from any source, and
    /// the compilation units of the sources; `path` names the source of
    /// `syntax_tree`.
    pub(crate) fn from_indexed_syntax_module(
        syntax_tree: &SyntaxTree,
        path: &Path,
        index: &module_index::ModuleIndex,
        module_name: &str,
        bindings: ModuleParameterBindings<'_>,
        extra_interfaces: &ModuleInterfaces,
        packages: &packages::Packages,
    ) -> Result<Self, AnalyzerError> {
        let context = AnalysisContext::with_packages(syntax_tree, packages);
        let syntax_tree = &context;
        let _timescales = timescales::install_for_tree(
            &syntax_tree.state.timescales,
            packages.timescales.clone(),
            syntax_tree,
        )?;
        let interfaces = InterfaceLookup {
            local: &index.interfaces,
            extra: extra_interfaces,
        };
        let mut modules = Vec::new();
        let interface_plan = packages.interface_plan.modules.get(module_name);
        let original_name = interface_plan.map_or(module_name, |plan| plan.original.as_str());
        for scope_tree in index.scopes(original_name) {
            let scope_context = context.child(scope_tree, bindings.values);
            let syntax_tree = &scope_context;
            match syntax_tree.root_unit()?.port_style {
                celox_sv_syntax::PortStyle::Ansi => {
                    let mut imported =
                        imports::imported_symbols(syntax_tree, packages, packages.unit(path))?;
                    imported.declarations_only = bindings.declarations_only;
                    let own_types = bindings
                        .types
                        .iter()
                        .filter(|(name, _)| {
                            !interface_plan.is_some_and(|plan| {
                                plan.ports
                                    .iter()
                                    .any(|port| name.starts_with(&format!("{}$", port.name)))
                            })
                        })
                        .cloned()
                        .collect::<Vec<_>>();
                    imported.type_parameter_bindings =
                        type_parameters::bind(syntax_tree, &own_types)?;
                    imported.pattern_parameter_bindings =
                        type_parameters::bind_patterns(syntax_tree, bindings.patterns)?;
                    // A hierarchical name reads and writes the local signal
                    // bound to the other instance's object.
                    for signal in bindings.hierarchical {
                        for path in &signal.paths {
                            imported
                                .interface_members
                                .insert(path.clone(), signal.name.clone());
                        }
                        if signal.local {
                            imported.local_aliases.extend(signal.paths.iter().cloned());
                            continue;
                        }
                        imported.state_signals.push(Signal {
                            is_const: false,
                            name: signal.name.clone(),
                            r#type: signal.r#type.clone(),
                            is_net: signal.is_net,
                            unresolved_net: false,
                            package_variable: None,
                            hierarchical: signal.paths.first().cloned(),
                        });
                    }
                    if let Some(mut module) = interface_binding::analyze_module(
                        syntax_tree,
                        path,
                        packages,
                        interface_binding::Bindings {
                            values: bindings.values,
                            types: bindings.types,
                            export_symbols: false,
                            plan: interface_plan,
                        },
                        &interfaces,
                        imported.clone(),
                    )
                    .map_err(|error| {
                        error.with_warnings(context.state.diagnostics.borrow().clone())
                    })? {
                        module.name = module_name.to_string();
                        modules.push(module);
                        continue;
                    }
                    modules.push(
                        Module::from_scope_with_parameter_overrides(
                            syntax_tree,
                            module_name,
                            bindings.values,
                            &interfaces,
                            Arc::new(imported),
                        )
                        .map_err(|error| {
                            error.with_warnings(context.state.diagnostics.borrow().clone())
                        })?,
                    );
                }
                celox_sv_syntax::PortStyle::NonAnsi if syntax_tree.root_name()? == module_name => {
                    return Err(AnalyzerError::Unsupported(
                        "non-ANSI port expressions".to_string(),
                    ));
                }
                _ => {}
            }
        }
        Ok(Self {
            modules,
            diagnostics: context.state.diagnostics.borrow().clone(),
        })
    }

    pub(crate) fn module_names_from_syntax(
        syntax_tree: &SyntaxTree,
    ) -> Result<Vec<String>, AnalyzerError> {
        let mut names = Vec::new();
        for handle in syntax_tree.root_scopes()? {
            let scoped = syntax_tree.scoped(handle)?;
            let syntax_tree = &scoped;
            let unit = syntax_tree.root_unit()?;
            if unit.kind.is_module_like() && unit.port_style != celox_sv_syntax::PortStyle::Wildcard
            {
                names.push(syntax_tree.root_name()?);
            }
        }
        Ok(names)
    }

    /// The positional interface of every ANSI module declared in `syntax_tree`.
    pub(crate) fn module_interfaces_from_syntax(
        syntax_tree: &SyntaxTree,
    ) -> Result<ModuleInterfaces, AnalyzerError> {
        let mut interfaces = ModuleInterfaces::default();
        for handle in syntax_tree.root_scopes()? {
            let scoped = syntax_tree.scoped(handle)?;
            let syntax_tree = &scoped;
            let unit = syntax_tree.root_unit()?;
            if unit.kind.is_module_like() && unit.port_style == celox_sv_syntax::PortStyle::Ansi {
                let name = syntax_tree.root_name()?;
                let context = AnalysisContext::unspecialized(syntax_tree);
                interfaces.insert(name, module_interface_from_scope(&context)?);
            }
        }
        Ok(interfaces)
    }

    pub fn modules(&self) -> &[Module] {
        &self.modules
    }

    pub(crate) fn warnings(&self) -> Vec<warnings::AnalyzerWarning> {
        let mut warnings = self.diagnostics.clone();
        // A program's functions run in its threads, where `$exit` applies.
        for module in self.modules.iter().filter(|module| !module.program) {
            warnings::append(&mut warnings, module.constant_functions.warnings());
        }
        warnings
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Module {
    name: String,
    /// A program (IEEE 1800-2023 24.3), whose processes run in the reactive
    /// region set.
    program: bool,
    parameters: Vec<Parameter>,
    ports: Vec<Port>,
    signals: Vec<Signal>,
    instances: Vec<Instance>,
    assignments: Vec<Assignment>,
    comb_processes: Vec<CombProcess>,
    ff_processes: Vec<FfProcess>,
    initial_processes: Vec<InitialProcess>,
    locals: Vec<LocalVariable>,
    subroutines: Vec<Subroutine>,
    dpi_imports: Vec<crate::ir::DpiImport>,
    /// The functions its constant expressions may call.
    constant_functions: const_functions::ConstantFunctions,
    /// Expression bodies used to publish symbolic declaration contracts.
    symbolic_functions: HashMap<String, Function>,
    declaration_subroutines: Vec<crate::ir::Subroutine>,
    /// The package parameters the module sees, by qualified name and by the
    /// names its imports bind.
    imported_parameters: Vec<Parameter>,
    /// Elaborated interface scopes, including those without data members.
    interface_scope_names: Vec<String>,
    /// The symbols of a package, for the scopes that use it.
    symbols: Option<scope::ScopeSymbols>,
}

impl Module {
    pub(crate) fn declaration_expr(
        &self,
        state: &AnalysisState,
        expr: &ConstExpr,
        signedness: &HashMap<String, bool>,
    ) -> ConstExpr {
        let expanded = expand_expr_calls(
            const_expr_to_expr(expr.clone()),
            &self.symbolic_functions,
            signedness,
            0,
            true,
        );
        expr_to_index_const(state, expanded, &PackedDimensions::default())
            .unwrap_or_else(|| expr.clone())
    }

    pub(crate) fn declaration_subroutines(&self) -> &[crate::ir::Subroutine] {
        &self.declaration_subroutines
    }

    /// Make the module's constant functions available to the constant
    /// evaluator until the guard is dropped.
    pub(crate) fn constant_function_state(&self) -> AnalysisState {
        let state = AnalysisState::default();
        const_functions::replace(&state.const_functions, self.constant_functions.clone());
        state
    }

    /// Analyze a module or a package. The scope starts from the `imported`
    /// symbols of the packages it uses.
    fn from_scope_with_parameter_overrides(
        syntax_tree: &AnalysisContext<'_>,
        override_module_name: &str,
        parameter_overrides: &HashMap<String, ConstExpr>,
        interfaces: &InterfaceLookup<'_>,
        imported: Arc<scope::ScopeSymbols>,
    ) -> Result<Self, AnalyzerError> {
        Self::from_scope_with_symbols(
            syntax_tree,
            override_module_name,
            parameter_overrides,
            interfaces,
            imported,
            false,
        )
    }

    fn from_scope_with_symbols(
        syntax_tree: &AnalysisContext<'_>,
        override_module_name: &str,
        parameter_overrides: &HashMap<String, ConstExpr>,
        interfaces: &InterfaceLookup<'_>,
        imported: Arc<scope::ScopeSymbols>,
        export_symbols: bool,
    ) -> Result<Self, AnalyzerError> {
        Self::from_selected_scope(
            syntax_tree,
            override_module_name,
            parameter_overrides,
            interfaces,
            imported,
            export_symbols,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn from_selected_scope(
        syntax_tree: &AnalysisContext<'_>,
        override_module_name: &str,
        parameter_overrides: &HashMap<String, ConstExpr>,
        interfaces: &InterfaceLookup<'_>,
        imported: Arc<scope::ScopeSymbols>,
        export_symbols: bool,
        header_ports: Option<&port_owners::Selection<'_>>,
    ) -> Result<Self, AnalyzerError> {
        let registry = syntax_tree.state.foreign.clone();
        for _ in 0..1024 {
            let revision = registry.revision();
            let mut symbols = (*imported).clone();
            registry.extend(syntax_tree, &mut symbols);
            let declarations_only = symbols.declarations_only;
            let result = Self::from_selected_scope_once(
                syntax_tree,
                override_module_name,
                parameter_overrides,
                interfaces,
                Arc::new(symbols),
                export_symbols || registry.has_templates(),
                header_ports,
            );
            if let Ok(module) = &result
                && !declarations_only
                && let Some(symbols) = &module.symbols
            {
                registry.finish(syntax_tree, symbols)?;
            }
            if registry.revision() == revision {
                return result;
            }
            syntax_tree.state.classes.borrow_mut().clear();
            syntax_tree.state.class_specializations.borrow_mut().clear();
            syntax_tree.state.foreign_error.borrow_mut().take();
            syntax_tree.state.foreign_names.borrow_mut().clear();
        }
        Err(unsupported(
            "foreign class specialization discovery did not converge",
        ))
    }

    fn from_selected_scope_once(
        syntax_tree: &AnalysisContext<'_>,
        override_module_name: &str,
        parameter_overrides: &HashMap<String, ConstExpr>,
        interfaces: &InterfaceLookup<'_>,
        imported: Arc<scope::ScopeSymbols>,
        export_symbols: bool,
        header_ports: Option<&port_owners::Selection<'_>>,
    ) -> Result<Self, AnalyzerError> {
        // The scope makes its class specializations until its class methods
        // have signatures.
        syntax_tree.state.class_specializations_frozen.set(false);
        let unit = syntax_tree.scope_handle().unit(syntax_tree.declarations());
        let is_package = unit.is_none_or(|unit| unit.kind == celox_sv_syntax::UnitKind::Package);
        let program = unit.is_some_and(|unit| unit.kind == celox_sv_syntax::UnitKind::Program);
        let name = unit.map_or_else(
            || packages::UNIT.to_string(),
            |unit| instances::normalize_identifier(&unit.name.spelling),
        );
        let empty_parameter_overrides = HashMap::default();
        let applicable_parameter_overrides = if name == override_module_name {
            parameter_overrides
        } else {
            &empty_parameter_overrides
        };
        let specialization =
            syntax_tree.child(syntax_tree.syntax_tree, applicable_parameter_overrides);
        let syntax_tree = &specialization;
        // The collectors below each elaborate the same generate scopes.
        let _elaborations = generate::memoize(syntax_tree);
        let _imported = scope::install(&syntax_tree.state, imported.clone());
        let with_imported = |mut functions: const_functions::ConstantFunctions| {
            functions.extend_missing(&imported.constant_functions);
            functions
        };
        let mut type_aliases = type_aliases_from_scope(syntax_tree)?;
        // Constant functions see the module constants known so far; they are
        // collected again once the parameters are known.
        let _constant_functions = const_functions::install(
            &syntax_tree.state,
            with_imported(const_functions::module_constant_functions(
                syntax_tree,
                &imported.const_env,
                &type_aliases,
                &imported.parameter_values,
            )),
        );
        let mut parameters = parameters_from_scope(
            syntax_tree,
            &type_aliases,
            &imported.const_env,
            applicable_parameter_overrides,
        )?;
        let mut parameter_names = HashSet::default();
        if let Some(parameter) = parameters
            .iter()
            .find(|parameter| !parameter_names.insert(parameter.name()))
        {
            return Err(AnalyzerError::DuplicateParameter {
                module: name,
                name: parameter.name().to_string(),
            });
        }
        if name == override_module_name {
            apply_parameter_overrides(&mut parameters, parameter_overrides)?;
        }
        let mut const_env = imported.const_env.clone();
        const_env.extend(const_env_from_parameters(&syntax_tree.state, &parameters));
        type_aliases = type_aliases_from_scope_with_env(syntax_tree, &const_env)?;
        let mut enum_constants = enum_member_constants_from_scope(
            syntax_tree,
            &const_env,
            &type_aliases,
            applicable_parameter_overrides,
        )?;
        for (name, value) in &enum_constants.numbers {
            const_env.entry(name.clone()).or_insert(*value);
            const_env.insert(enum_marker(name), *value);
            if let Some(r#type) = enum_constants.types.get(name) {
                insert_parameter_type_markers(&mut const_env, name, *r#type);
            }
        }
        enum_constants.extend_missing(&imported.enum_constants);
        // Alias ranges may themselves contain casts sized by enum members.
        // Rebuild them before assigning declared parameter widths.
        type_aliases = type_aliases_from_scope_with_env(syntax_tree, &const_env)?;
        // Constant casts are evaluated while parameter syntax is lowered.
        // Repeat that lowering after enum constants become available so a
        // cast operand such as `byte_t'(ENUM_MEMBER)` is not permanently
        // discarded during the initial pass. Materialize only enum-member
        // references so exported parameter expressions retain dependencies
        // on other parameters for later specialization.
        parameters = parameters_from_scope(
            syntax_tree,
            &type_aliases,
            &const_env,
            applicable_parameter_overrides,
        )?;
        for parameter in &mut parameters {
            parameter.value = parameter.value.take().map(|value| {
                substitute_typed_parameter_literals(
                    value,
                    &enum_constants.numbers,
                    &enum_constants.types,
                )
            });
        }
        if name == override_module_name {
            apply_parameter_overrides(&mut parameters, parameter_overrides)?;
        }
        extend_const_env_with_parameters(&syntax_tree.state, &mut const_env, &parameters);
        type_aliases = type_aliases_from_scope_with_env(syntax_tree, &const_env)?;
        // Contract dependency analysis must see module parameter references
        // before the normal procedural lowering substitutes default literals.
        let symbolic_parameters = parameters
            .iter()
            .map(|parameter| parameter.name().to_string())
            .collect();
        // Class parameter defaults are bound where the class is declared.
        classes::specializations::declare_scopes(syntax_tree, &const_env, &type_aliases);
        let declaration_subroutines =
            with_imported(const_functions::symbolic_module_constant_functions(
                syntax_tree,
                &const_env,
                &type_aliases,
                &imported.parameter_values,
                &symbolic_parameters,
            ))
            .declarations();
        const_functions::replace(
            &syntax_tree.state.const_functions,
            with_imported(const_functions::module_constant_functions(
                syntax_tree,
                &const_env,
                &type_aliases,
                &parameter_value_env_with_imported(
                    &syntax_tree.state,
                    &parameters,
                    &const_env,
                    &imported,
                ),
            )),
        );
        extend_const_env_with_parameters(&syntax_tree.state, &mut const_env, &parameters);

        let mut array_parameters =
            array_parameters::array_parameters_from_scope(syntax_tree, &const_env, &type_aliases)?;
        for parameter in &array_parameters {
            const_env.insert(
                parameters::unbounded_parameter_marker(&parameter.signal.name),
                0,
            );
        }

        if !imported.declarations_only {
            match reject_silently_ignored_constructs(
                syntax_tree,
                &const_env,
                &type_aliases,
                &parameter_packed_dimensions_with_imported(&parameters, &imported).into(),
                &parameter_value_env_with_imported(
                    &syntax_tree.state,
                    &parameters,
                    &const_env,
                    &imported,
                ),
            ) {
                Ok(()) => {}
                // A parameter initializer may inspect a port or signal type
                // through `$bits`/`$size`. Collect declarations only when that
                // missing type metadata is the remaining validation failure, so
                // unsupported constructs keep their original diagnostics.
                Err(AnalyzerError::Unsupported(construct))
                    if construct == "constant cast expression" =>
                {
                    let preliminary_ports =
                        ports_from_scope(syntax_tree, &const_env, &type_aliases, header_ports)?;
                    let preliminary_signals =
                        signals_from_scope(syntax_tree, &const_env, &type_aliases)?;
                    extend_const_env_with_variable_types(
                        &syntax_tree.state,
                        &mut const_env,
                        preliminary_ports
                            .iter()
                            .map(|port| (port.name(), port.r#type()))
                            .chain(
                                preliminary_signals
                                    .iter()
                                    .map(|signal| (signal.name(), signal.r#type())),
                            ),
                    );
                    parameters = parameters_from_scope(
                        syntax_tree,
                        &type_aliases,
                        &const_env,
                        applicable_parameter_overrides,
                    )?;
                    for parameter in &mut parameters {
                        parameter.value = parameter.value.take().map(|value| {
                            substitute_typed_parameter_literals(
                                value,
                                &enum_constants.numbers,
                                &enum_constants.types,
                            )
                        });
                    }
                    if name == override_module_name {
                        apply_parameter_overrides(&mut parameters, parameter_overrides)?;
                    }
                    extend_const_env_with_parameters(
                        &syntax_tree.state,
                        &mut const_env,
                        &parameters,
                    );
                    type_aliases = type_aliases_from_scope_with_env(syntax_tree, &const_env)?;
                    array_parameters = array_parameters::array_parameters_from_scope(
                        syntax_tree,
                        &const_env,
                        &type_aliases,
                    )?;
                    reject_silently_ignored_constructs(
                        syntax_tree,
                        &const_env,
                        &type_aliases,
                        &parameter_packed_dimensions_with_imported(&parameters, &imported).into(),
                        &parameter_value_env_with_imported(
                            &syntax_tree.state,
                            &parameters,
                            &const_env,
                            &imported,
                        ),
                    )?;
                }
                Err(error) => return Err(error),
            }
        }
        let mut ports = ports_from_scope(syntax_tree, &const_env, &type_aliases, header_ports)?;
        ports.extend(imported.ports.iter().cloned());
        let mut port_names = HashSet::default();
        if let Some(port) = ports.iter().find(|port| !port_names.insert(port.name())) {
            return Err(AnalyzerError::DuplicatePort {
                module: name,
                name: port.name().to_string(),
            });
        }
        if ports
            .iter()
            .any(|port| port.direction() == PortDirection::Ref)
        {
            return Err(AnalyzerError::Unsupported("ref port direction".to_string()));
        }
        // The static properties of each class specialization are variables
        // of the module (IEEE 1800-2023 8.9, 8.25).
        classes::specializations::collect(
            syntax_tree,
            &PackedDimensions::new(HashMap::default(), &const_env, &type_aliases),
        );
        let mut signals = signals_from_scope(syntax_tree, &const_env, &type_aliases)?;
        signals.extend(
            array_parameters
                .iter()
                .map(|parameter| parameter.signal.clone()),
        );
        // The array parameters of the packages the module uses are constant
        // signals of the module, like its own.
        signals.extend(imported.signals.iter().cloned());
        signals.extend(imported.state_signals.iter().cloned());
        for r#type in ports
            .iter()
            .map(Port::r#type)
            .chain(signals.iter().map(Signal::r#type))
        {
            validate_unpacked_dimension_sizes(
                &syntax_tree.state,
                r#type.unpacked_ranges(),
                &const_env,
            )?;
        }
        let signal_names: HashSet<_> = signals.iter().map(Signal::name).collect();
        if let Some(parameter) = parameters.iter().find(|parameter| {
            port_names.contains(parameter.name()) || signal_names.contains(parameter.name())
        }) {
            return Err(AnalyzerError::Unsupported(format!(
                "parameter name collides with port or signal `{}`",
                parameter.name()
            )));
        }
        let mut packed_dimensions =
            packed_dimensions_from_ports_and_signals(&ports, &signals, &const_env, &type_aliases);
        // Four-state parameter values cannot be represented by the numeric
        // environment. Keep their expressions for constant case analysis.
        packed_dimensions.parameter_values = parameter_value_env_with_imported(
            &syntax_tree.state,
            &parameters,
            &const_env,
            &imported,
        )
        .into();
        packed_dimensions
            .parameter_values
            .retain(|name, _| !const_env.contains_key(name));
        packed_dimensions.extend(parameter_packed_dimensions_with_imported(
            &parameters,
            &imported,
        ));
        // Instance connections may call subroutines, of the module or of the
        // packages it uses, with untyped assignment patterns.
        let (mut subroutine_params, mut subroutine_shapes, mut subroutine_directions) =
            procedural::subroutine_argument_names(syntax_tree, &const_env, &type_aliases)?;
        for (name, params) in &imported.subroutine_params {
            subroutine_params
                .entry(name.clone())
                .or_insert_with(|| params.clone());
        }
        for (name, shapes) in &imported.subroutine_shapes {
            subroutine_shapes
                .entry(name.clone())
                .or_insert_with(|| shapes.clone());
        }
        // The subroutines of the packages the module uses, by their qualified
        // names and by the names its imports give them.
        for subroutine in &imported.subroutines {
            subroutine_directions
                .entry(subroutine.name.clone())
                .or_insert_with(|| {
                    subroutine
                        .params
                        .iter()
                        .map(|param| param.direction)
                        .collect()
                });
        }
        for (name, target) in &imported.aliases {
            if let Some(directions) = subroutine_directions.get(target).cloned() {
                subroutine_directions
                    .entry(name.clone())
                    .or_insert(directions);
            }
        }
        let dpi_items = generate::items(syntax_tree, &const_env, &type_aliases)?;
        let mut dpi_imports = dpi::imports(&dpi_items, syntax_tree.declarations())?;
        for import in &dpi_imports {
            subroutine_params.insert(
                import.name().to_string(),
                import
                    .arguments()
                    .iter()
                    .map(|arg| arg.name().to_string())
                    .collect(),
            );
            subroutine_directions.insert(
                import.name().to_string(),
                vec![crate::procedural::ParamDirection::Input; import.arguments().len()],
            );
        }
        packed_dimensions.subroutine_param_shapes = Arc::new(subroutine_shapes.clone());
        packed_dimensions.subroutine_directions = Arc::new(subroutine_directions.clone());
        packed_dimensions.subroutine_param_names = Arc::new(subroutine_params.clone());
        // Array parameters retain a constant value as well as their signal layout.
        // Bind their values in declaration order before child parameter arguments.
        for process in &imported.initial_processes {
            for statement in &process.body {
                if let Stmt::Assign {
                    lhs: LValue::Ident(name),
                    rhs,
                    ..
                } = statement
                {
                    packed_dimensions
                        .array_parameter_values
                        .insert(name.clone(), rhs.clone());
                }
            }
        }
        let mut array_initial_processes = Vec::new();
        let mut packed_pattern_values = HashMap::default();
        for parameter in &array_parameters {
            let process = parameter.initial_process(syntax_tree, &packed_dimensions)?;
            if let Stmt::Assign { rhs, .. } = &process.body[0] {
                packed_dimensions
                    .array_parameter_values
                    .insert(parameter.signal.name.clone(), rhs.clone());
                let name = parameter.signal.name();
                let shape = packed_dimensions.get(name).expect("array parameter shape");
                if shape.unpacked.is_empty()
                    && !shape.real
                    && !shape.string
                    && shape.class.is_none()
                {
                    // An integral value parameter is self-determined before
                    // conversion at a receiving declaration (6.20.2). The
                    // pattern's concatenation must retain its declared sign.
                    let signed = shape.signed;
                    let is_2state = shape.is_2state;
                    let width = expr_static_width(
                        &syntax_tree.state,
                        &Expr::Ident(name.to_string()),
                        &packed_dimensions,
                    )
                    .ok_or_else(|| unsupported("packed pattern parameter width"))?;
                    let mut value = Expr::Resize {
                        expr: Box::new(rhs.clone()),
                        width,
                        signed,
                    };
                    if is_2state {
                        value = Expr::Unary {
                            op: UnaryOp::ToTwoState,
                            expr: Box::new(value),
                        };
                    }
                    let value = fold_const_integral_expr_preserving_mask(
                        &syntax_tree.state,
                        value,
                        &packed_dimensions.const_env,
                    );
                    packed_dimensions
                        .parameter_values
                        .insert(name.to_string(), value.clone());
                    packed_pattern_values.insert(name.to_string(), value);
                }
            }
            array_initial_processes.push(process);
        }
        let mut instances =
            instances_from_scope(syntax_tree, &const_env, &packed_dimensions, interfaces)?;
        let mut instance_names = HashSet::default();
        if let Some(instance) = instances
            .iter()
            .filter(|instance| {
                instance
                    .condition()
                    .and_then(|condition| {
                        eval_ast_const_expr(&syntax_tree.state, condition, &const_env)
                    })
                    .is_none_or(|value| value != 0)
            })
            .find(|instance| !instance_names.insert(instance.name()))
        {
            return Err(AnalyzerError::DuplicateInstance {
                module: name,
                name: instance.name().to_string(),
            });
        }
        // A generate block shares its name space with the module's other
        // declarations (IEEE 1800-2023 27.6, 23.9).
        // The members of a variable of unpacked structure type are named
        // like declarations of a scope of the variable's name.
        let aggregates: HashSet<&str> = signals
            .iter()
            .filter(|signal| signal.is_aggregate())
            .map(Signal::name)
            .collect();
        let scope_heads: HashSet<&str> = signals
            .iter()
            .filter(|signal| signal.hierarchical.is_none())
            .map(Signal::name)
            .chain(instances.iter().map(Instance::name))
            .filter_map(generate_scope_head)
            .filter(|head| !aggregates.contains(head))
            .collect();
        if let Some(conflict) = ports
            .iter()
            .map(Port::name)
            .chain(signals.iter().map(Signal::name))
            .chain(instances.iter().map(Instance::name))
            .find(|conflict| scope_heads.contains(conflict))
        {
            return Err(AnalyzerError::DuplicateGenerateScope {
                module: name,
                name: conflict.to_string(),
            });
        }
        let mut parameter_values = parameter_value_env_with_imported(
            &syntax_tree.state,
            &parameters,
            &const_env,
            &imported,
        );
        parameter_values.extend(packed_pattern_values);
        for (name, value) in &enum_constants.exprs {
            parameter_values
                .entry(name.clone())
                .or_insert_with(|| value.clone());
        }
        let mut expression_signedness = ports
            .iter()
            .map(|port| (port.name().to_string(), port.r#type().is_signed()))
            .chain(
                signals
                    .iter()
                    .map(|signal| (signal.name().to_string(), signal.r#type().is_signed())),
            )
            .collect::<HashMap<_, _>>();
        expression_signedness.extend(
            parameter_types_from_const_env(&const_env)
                .into_iter()
                .map(|(name, r#type)| (name, r#type.signed)),
        );
        let mut functions = functions_from_scope(syntax_tree, &const_env, &packed_dimensions)?;
        for (name, function) in &imported.functions {
            functions
                .entry(name.clone())
                .or_insert_with(|| function.clone());
        }
        for (name, metadata) in &imported.function_return_types {
            packed_dimensions
                .function_return_types
                .entry(name.clone())
                .or_insert_with(|| metadata.clone());
        }
        packed_dimensions
            .function_return_types
            .extend(functions.iter().map(|(name, function)| {
                (
                    name.clone(),
                    FunctionReturnMetadata {
                        arguments: Some(function_argument_metadata(&function.params)),
                        width: function.return_width,
                        first_packed_dimension_width: function.return_first_packed_dimension_width,
                        dimensions: function.return_dimensions,
                        signed: function.return_signed,
                        is_2state: function.return_is_2state,
                        string: function.return_string,
                        pattern_type: function.return_type.clone(),
                        enum_domain: function.return_enum_domain.clone(),
                    },
                )
            }));
        packed_dimensions
            .function_return_types
            .extend(dpi_imports.iter().filter_map(|import| {
                let r#type = import.return_type()?;
                Some((
                    import.name().to_string(),
                    FunctionReturnMetadata {
                        arguments: Some(dpi_argument_metadata(import)),
                        width: Some(r#type.width()),
                        first_packed_dimension_width: (r#type.width() > 1)
                            .then_some(r#type.width()),
                        dimensions: Some(usize::from(r#type.width() > 1)),
                        signed: r#type.is_signed(),
                        is_2state: !r#type.is_4state(),
                        string: false,
                        pattern_type: None,
                        enum_domain: None,
                    },
                ))
            }));
        // Class methods are subroutines `class::method` (IEEE 1800-2023 8.6).
        dimensions::all_class_signatures(syntax_tree, &mut packed_dimensions);
        packed_dimensions.functions = Arc::new(functions.clone());
        packed_dimensions.expression_signedness = expression_signedness.clone().into();
        for instance in &mut instances {
            for connection in &mut instance.port_connections {
                if let Some(pattern) = &mut connection.bound_pattern {
                    pattern.resolve_types(&packed_dimensions)?;
                }
                connection.map_expressions(|expr| {
                    let expr = expand_expr_calls(expr, &functions, &expression_signedness, 0, true);
                    substitute_expr_constants_with_parameter_literals(
                        &syntax_tree.state,
                        expr,
                        &const_env,
                        &enum_constants.exprs,
                    )
                });
            }
        }
        // Bodies now have the active module's declarations and function types.
        // Declaration-time queries still use syntax discovery while metadata
        // is incomplete; generated/procedural scopes overlay this complete base.
        packed_dimensions.scope_types_complete = true;
        let mut locals = Vec::new();
        let mut local_counter = 0usize;
        let mut body_state = procedural::BodyState {
            locals: &mut locals,
            counter: &mut local_counter,
            subroutine_params: &subroutine_params,
            subroutine_directions: &subroutine_directions,
            automatic: lifetimes::automatic_by_default(syntax_tree),
            statics: Vec::new(),
            explicit_statics: HashSet::default(),
            static_initializers: Vec::new(),
            constant_evaluation: false,
        };
        let (mut subroutines, mut comb_processes, mut ff_processes, mut initial_processes) =
            if imported.declarations_only {
                (Vec::new(), Vec::new(), Vec::new(), Vec::new())
            } else {
                let subroutines = procedural::subroutines_from_scope(
                    syntax_tree,
                    &const_env,
                    &packed_dimensions,
                    &parameter_values,
                    &mut body_state,
                )?;
                let mut comb_processes = comb_processes_from_scope(
                    syntax_tree,
                    &const_env,
                    &packed_dimensions,
                    &parameter_values,
                    &mut body_state,
                )?
                .into_iter()
                .map(|process| {
                    substitute_process_constants_with_parameter_literals(
                        &syntax_tree.state,
                        process,
                        &const_env,
                        &parameter_values,
                    )
                })
                .collect::<Vec<_>>();
                let mut ff_processes = ff_processes_from_scope(
                    syntax_tree,
                    &const_env,
                    &parameter_values,
                    &packed_dimensions,
                    &mut body_state,
                )?;
                comb_processes.extend(imported.comb_processes.iter().cloned());
                ff_processes.extend(imported.ff_processes.iter().cloned());
                let mut initial_processes = array_initial_processes;
                initial_processes.extend(imported.initial_processes.iter().cloned());
                initial_processes.extend(procedural::initial_processes_from_scope(
                    syntax_tree,
                    &const_env,
                    &packed_dimensions,
                    &parameter_values,
                    &mut body_state,
                )?);
                (subroutines, comb_processes, ff_processes, initial_processes)
            };
        let (inline_subroutines, inline_randomizations) =
            procedural::randomization::inline_subroutines(
                syntax_tree,
                &mut body_state,
                &packed_dimensions,
            )?;
        subroutines.extend(inline_subroutines);
        // Static locals of the processes that keep their value are variables
        // of the module, initialized once with the declaration initializers.
        let statics: HashMap<String, Type> = body_state.statics.drain(..).collect();
        let (mut kept, mut static_initializers) = lifetimes::keep_static_locals(
            &syntax_tree.state,
            &statics,
            &subroutine_directions,
            comb_processes
                .iter_mut()
                .map(|process| (&mut process.body, false))
                .chain(
                    ff_processes
                        .iter_mut()
                        .map(|process| (&mut process.body, false)),
                )
                .chain(
                    initial_processes
                        .iter_mut()
                        .map(|process| (&mut process.body, !process.repeats)),
                ),
            Some(&body_state.explicit_statics),
        )?;
        for subroutine in &subroutines {
            kept.extend(subroutine.persistent_locals.iter().cloned());
        }
        // Statics of subroutines are initialized first: a run-time
        // initializer of a process local may call the subroutine.
        static_initializers.splice(0..0, body_state.static_initializers.drain(..));
        locals.retain(|local| !kept.contains(&local.name));
        for name in kept {
            signals.push(Signal::new(name.clone(), statics[&name].clone()));
        }
        if !static_initializers.is_empty() {
            // Declaration initializers run before the other initial blocks.
            let first_block = initial_processes
                .iter()
                .position(|process| !process.is_initializer())
                .unwrap_or(initial_processes.len());
            // They may read module variables, so they also stay after those
            // initializers when processes are reordered by creation.
            let creation_order = initial_processes
                .iter()
                .filter(|process| process.is_initializer())
                .map(|process| process.creation_order)
                .max()
                .unwrap_or(0);
            initial_processes.insert(
                first_block,
                InitialProcess {
                    creation_order,
                    site: None,
                    condition: None,
                    body: static_initializers,
                    kind: celox_design::ProcessKind::Initializer,
                    validation_body: Vec::new(),
                    synthetic: false,
                    repeats: false,
                },
            );
        }
        // A call of a name an import binds calls the package subroutine or
        // DPI import. Calls of subroutines declared in a generate block were
        // already bound to those.
        let calls: HashMap<String, String> = imported
            .aliases
            .iter()
            .filter(|(_, target)| {
                imported
                    .subroutines
                    .iter()
                    .any(|subroutine| subroutine.name == **target)
                    || imported
                        .dpi_imports
                        .iter()
                        .any(|import| import.name() == target.as_str())
            })
            .map(|(name, target)| (name.clone(), target.clone()))
            .collect();
        if !calls.is_empty() {
            let rename = scope::Renamer { names: &calls };
            let body = |body: Vec<Stmt>| body.into_iter().map(|stmt| rename.stmt(stmt)).collect();
            for process in &mut comb_processes {
                process.body = body(std::mem::take(&mut process.body));
                process.sensitivity_body = body(std::mem::take(&mut process.sensitivity_body));
                for assignment in &mut process.assignments {
                    assignment.rhs = rename.expr(assignment.rhs.clone());
                }
            }
            for process in &mut ff_processes {
                process.body = body(std::mem::take(&mut process.body));
            }
            for process in &mut initial_processes {
                process.body = body(std::mem::take(&mut process.body));
                process.validation_body = body(std::mem::take(&mut process.validation_body));
            }
            for subroutine in &mut subroutines {
                *subroutine = rename.subroutine(subroutine.clone());
            }
            for instance in &mut instances {
                for connection in &mut instance.port_connections {
                    connection.map_expressions(|expr| rename.expr(expr));
                }
            }
        }
        let nets = signals
            .iter()
            .filter(|signal| signal.is_net())
            .map(Signal::name)
            .chain(ports.iter().filter(|port| port.is_net).map(Port::name))
            .collect();
        procedural::references::validate(
            subroutines.iter().chain(imported.subroutines.iter()),
            &comb_processes,
            &ff_processes,
            &initial_processes,
            &subroutine_directions,
            &subroutine_params,
            &nets,
        )?;
        procedural::timed_tasks::classify(
            subroutines.iter().chain(imported.subroutines.iter()),
            &mut comb_processes,
            &mut ff_processes,
            &mut initial_processes,
        )?;
        let assignments = comb_processes
            .iter()
            .flat_map(|process| process.assignments().iter().cloned())
            .collect();
        // A package keeps its symbols for the scopes that use it.
        let symbols = (is_package || export_symbols).then(|| scope::ScopeSymbols {
            package_scopes: imported.package_scopes.clone(),
            classes: classes::scope_classes(syntax_tree, &const_env),
            class_member_types: classes::specializations::export_member_types(syntax_tree),
            inline_randomizations,
            declarations_only: imported.declarations_only,
            type_parameter_bindings: HashMap::default(),
            pattern_parameter_bindings: HashMap::default(),
            const_env: const_env.clone(),
            parameter_values: parameter_values.clone(),
            parameters: parameters.clone(),
            // A class of the scope is a type of it (IEEE 1800-2023 8.3).
            type_aliases: type_aliases
                .iter()
                .map(|(name, ty)| (name.clone(), ty.clone()))
                .chain(classes::scope_class_types(syntax_tree))
                .collect(),
            enum_constants: enum_constants.clone(),
            functions: functions.clone(),
            function_return_types: (*packed_dimensions.function_return_types).clone(),
            constant_functions: const_functions::installed(&syntax_tree.state.const_functions),
            subroutine_params: subroutine_params.clone(),
            subroutine_shapes: subroutine_shapes.clone(),
            subroutines: subroutines.clone(),
            locals: locals.clone(),
            dpi_imports: dpi_imports.clone(),
            // A package's signals are its array parameters and constant
            // variables, initialized by its initial processes.
            signals: signals.clone(),
            ports: Vec::new(),
            state_signals: Vec::new(),
            initial_processes: initial_processes.clone(),
            comb_processes: comb_processes.clone(),
            ff_processes: ff_processes.clone(),
            aliases: imported.aliases.clone(),
            interface_members: imported.interface_members.clone(),
            local_aliases: imported.local_aliases.clone(),
            interface_types: imported.interface_types.clone(),
            interface_arrays: imported.interface_arrays.clone(),
            interface_scopes: imported.interface_scopes.clone(),
            interface_declarations: imported.interface_declarations.clone(),
            interface_instances: imported.interface_instances.clone(),
            generate_imports: HashMap::default(),
        });
        // The subroutines, locals and DPI imports of the packages the module
        // uses are lowered with it.
        for subroutine in &imported.subroutines {
            if !subroutines
                .iter()
                .any(|known| known.name == subroutine.name)
            {
                subroutines.push(subroutine.clone());
            }
        }
        for local in &imported.locals {
            if !locals.iter().any(|known| known.name == local.name) {
                locals.push(local.clone());
            }
        }
        for import in &imported.dpi_imports {
            if !dpi_imports
                .iter()
                .any(|known| known.name() == import.name())
            {
                dpi_imports.push(import.clone());
            }
        }
        let mut imported_parameters = imported.parameters.clone();
        imported_parameters
            .retain(|parameter| !parameters.iter().any(|local| local.name == parameter.name));

        // Expression construction tracks only whole-variable assignments.
        // Leave calls of functions with selected/concatenated writes symbolic
        // in declaration contracts rather than publishing a partial body.
        // Expression inlining also sizes formal and return conversions at the
        // default specialization; symbolic signature widths cannot be inlined.
        for subroutine in &declaration_subroutines {
            let mut selected_write = false;
            for statement in &subroutine.body {
                statement.walk(&mut |statement| {
                    selected_write |= matches!(
                        statement,
                        crate::procedural::StmtBase::Assign {
                            lhs: crate::ir::LValue::Select { .. },
                            ..
                        } | crate::procedural::StmtBase::AssignConcat { .. }
                    );
                });
            }
            let symbolic_type = subroutine
                .return_type
                .as_ref()
                .is_some_and(|ty| ty.has_symbolic_ranges(&syntax_tree.state.const_functions))
                || subroutine.params.iter().any(|param| {
                    param
                        .r#type
                        .has_symbolic_ranges(&syntax_tree.state.const_functions)
                });
            if (selected_write || symbolic_type)
                && let Some(function) = functions.get_mut(&subroutine.name)
            {
                function.body = None;
            }
        }

        // A native default value is not evidence of a representable symbolic
        // body. Keep failed declaration lowerings visible to contract checking.
        for function in functions.values_mut() {
            if !declaration_subroutines
                .iter()
                .any(|subroutine| subroutine.name == function.name)
            {
                function.body = None;
            }
        }

        let mut interface_scope_names = imported
            .interface_types
            .keys()
            .chain(imported.interface_arrays.keys())
            .cloned()
            .collect::<Vec<_>>();
        interface_scope_names.sort_unstable();
        interface_scope_names.dedup();

        Ok(Self {
            name,
            program,
            interface_scope_names,
            parameters,
            ports,
            signals,
            instances,
            assignments,
            comb_processes,
            ff_processes,
            initial_processes,
            locals,
            subroutines,
            dpi_imports,
            constant_functions: const_functions::installed(&syntax_tree.state.const_functions),
            symbolic_functions: functions,
            declaration_subroutines,
            imported_parameters,
            symbols,
        })
    }

    pub(crate) fn interface_scope_names(&self) -> &[String] {
        &self.interface_scope_names
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn ports(&self) -> &[Port] {
        &self.ports
    }

    pub fn signals(&self) -> &[Signal] {
        &self.signals
    }

    /// The package parameters the module sees.
    pub fn imported_parameters(&self) -> &[Parameter] {
        &self.imported_parameters
    }

    pub fn parameters(&self) -> &[Parameter] {
        &self.parameters
    }

    pub fn instances(&self) -> &[Instance] {
        &self.instances
    }

    pub fn assignments(&self) -> &[Assignment] {
        &self.assignments
    }

    pub fn comb_processes(&self) -> &[CombProcess] {
        &self.comb_processes
    }

    pub fn ff_processes(&self) -> &[FfProcess] {
        &self.ff_processes
    }

    pub fn initial_processes(&self) -> &[InitialProcess] {
        &self.initial_processes
    }

    pub fn locals(&self) -> &[LocalVariable] {
        &self.locals
    }

    pub fn subroutines(&self) -> &[Subroutine] {
        &self.subroutines
    }

    pub fn dpi_imports(&self) -> &[crate::ir::DpiImport] {
        &self.dpi_imports
    }

    /// Whether this is a program (IEEE 1800-2023 24.3).
    pub fn is_program(&self) -> bool {
        self.program
    }
}

pub(crate) fn generated_time_declarations(
    sources: &[(&str, &std::path::Path)],
    roots: Option<&[&str]>,
) -> Result<String, AnalyzerError> {
    timescales::generated_source_declarations(sources, roots, crate::AnalysisOptions::default())
}

const MAX_GENERATE_LOOP_EXPANSION: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parameter {
    floating: Option<TypeKind>,
    name: String,
    value: Option<ConstExpr>,
    declared_width: Option<usize>,
    packed_ranges: Vec<PackedRange>,
    members: Vec<packed_structs::PackedMember>,
    declared_signed: Option<bool>,
    declared_is_2state: bool,
    has_declared_type: bool,
    is_local: bool,
    /// See [`Type::signed_element_depth`].
    signed_element_depth: Option<usize>,
    /// The enumerated type the parameter is declared with.
    enum_domain: Option<Arc<enums::EnumDomain>>,
}

impl Parameter {
    fn new(
        name: String,
        value: Option<ConstExpr>,
        declared_width: Option<usize>,
        declared_signed: Option<bool>,
        declared_is_2state: bool,
        has_declared_type: bool,
        is_local: bool,
    ) -> Self {
        Self {
            name,
            value,
            declared_width,
            floating: None,
            packed_ranges: Vec::new(),
            members: Vec::new(),
            declared_signed,
            declared_is_2state,
            has_declared_type,
            is_local,
            signed_element_depth: None,
            enum_domain: None,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn value(&self) -> Option<&ConstExpr> {
        self.value.as_ref()
    }

    pub(crate) fn resolved_value(
        &self,
        state: &AnalysisState,
        constants: &HashMap<String, i128>,
        parameter_types: &HashMap<String, ExprType>,
    ) -> Option<i128> {
        if self.floating.is_some() {
            return None;
        }
        let mut value =
            substitute_typed_parameter_literals(self.value()?.clone(), constants, parameter_types);
        if self.declared_is_2state {
            value = ConstExpr::Unary {
                op: UnaryOp::ToTwoState,
                expr: Box::new(value),
            };
        }
        let value: crate::ir::ConstExpr = value.into();
        let environment = const_functions::Environment {
            constants,
            functions: &state.const_functions,
        };
        let mut value = if self.has_declared_type && typecheck::const_expr_has_real_type(&value) {
            // An explicitly integral parameter supplies an integer assignment
            // context. Untyped real parameters retain their floating type.
            typecheck::round_real_to_i128(typecheck::eval_real_const_expr(&value, &environment)?)
        } else {
            typecheck::eval_const_expr_in_env(&value, &environment)?
        };
        if let Some(width) = self.declared_width {
            value =
                coerce_const_parameter_value(value, width, self.declared_signed.unwrap_or(false));
        }
        Some(value)
    }

    /// Resolve once, retaining a nonnumeric literal for the caller to publish.
    /// Numeric results take precedence, including numbers recovered from a
    /// literal after two-state conversion or dependency substitution.
    pub(crate) fn resolved_value_and_literal(
        &self,
        state: &AnalysisState,
        constants: &HashMap<String, i128>,
        parameter_types: &HashMap<String, ExprType>,
        literals: &HashMap<String, Expr>,
    ) -> (Option<i128>, Option<Expr>) {
        if self.floating.is_some() {
            return (
                None,
                self.resolved_literal(state, constants, parameter_types, literals),
            );
        }
        if let Some(value) = self.resolved_value(state, constants, parameter_types) {
            return (Some(value), None);
        }
        let literal = self.resolved_literal(state, constants, parameter_types, literals);
        let value = literal.as_ref().and_then(|literal| {
            eval_ast_const_expr(state, &expr_to_const(state, literal.clone())?, constants)
        });
        if value.is_some() {
            (value, None)
        } else {
            (None, literal)
        }
    }

    pub(crate) fn resolved_literal(
        &self,
        state: &AnalysisState,
        constants: &HashMap<String, i128>,
        parameter_types: &HashMap<String, ExprType>,
        literals: &HashMap<String, Expr>,
    ) -> Option<Expr> {
        if let Some(kind) = self.floating {
            return parameters::real_literal(self.value()?, kind, constants, literals);
        }
        #[cfg(test)]
        parameters::LITERAL_RESOLUTIONS.with(|calls| calls.set(calls.get() + 1));
        if self
            .value()
            .is_some_and(|value| parameters::is_unbounded(value, constants))
        {
            return Some(Expr::Literal("$".into()));
        }
        // A previous elaboration pass may have left this declaration's numeric
        // value in the environment. Evaluate its initializer, not that value.
        let evaluation_constants = if constants.contains_key(self.name()) {
            #[cfg(test)]
            parameters::LITERAL_ENV_COPIES.with(|copies| copies.set(copies.get() + 1));
            let mut masked = constants.clone();
            masked.remove(self.name());
            Cow::Owned(masked)
        } else {
            Cow::Borrowed(constants)
        };
        let mut parameter = self.clone();
        parameter.value = self.value.clone().map(|value| {
            substitute_typed_parameter_literals(value, &evaluation_constants, parameter_types)
        });
        if let Some(ty) = self.resolved_type(parameter_types) {
            parameter.declared_width = Some(ty.width);
            parameter.declared_signed = Some(ty.signed);
        }
        let value = parameter_value_env(
            state,
            std::slice::from_ref(&parameter),
            &evaluation_constants,
        )
        .remove(self.name())?;
        let value = substitute_expr_idents(value, literals);
        let value = fold_const_integral_expr_preserving_mask(state, value, &evaluation_constants);
        matches!(value, Expr::Literal(_)).then_some(value)
    }

    pub(crate) fn resolved_type(
        &self,
        parameter_types: &HashMap<String, ExprType>,
    ) -> Option<ExprType> {
        let inferred = self.value().and_then(|value| {
            infer_parameter_value_type(value, self.has_declared_type, parameter_types)
        });
        let width = self
            .declared_width
            .or(inferred.map(|r#type| r#type.width))?;
        let signed = self
            .declared_signed
            .or(inferred.map(|r#type| r#type.signed))
            .unwrap_or(false);
        Some(ExprType { width, signed })
    }

    pub(crate) fn packed_ranges(&self) -> &[PackedRange] {
        &self.packed_ranges
    }

    pub(crate) fn declared_width(&self) -> Option<usize> {
        self.declared_width
    }

    pub(crate) fn declared_signed(&self) -> Option<bool> {
        self.declared_signed
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Port {
    randomizable: bool,
    name: String,
    direction: PortDirection,
    r#type: Type,
    is_net: bool,
    unresolved_net: bool,
    default: Option<ConstExpr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signal {
    is_const: bool,
    name: String,
    r#type: Type,
    is_net: bool,
    unresolved_net: bool,
    /// The package variable the signal denotes, as `(package, variable)`.
    package_variable: Option<(String, String)>,
    /// The hierarchical name of the signal of another instance the signal
    /// denotes (IEEE 1800-2023 23.6).
    hierarchical: Option<String>,
}

impl Signal {
    fn new(name: String, r#type: Type) -> Self {
        Self {
            name,
            r#type,
            is_net: false,
            unresolved_net: false,
            is_const: false,
            package_variable: None,
            hierarchical: None,
        }
    }

    fn new_net(name: String, r#type: Type) -> Self {
        Self {
            name,
            r#type,
            is_net: true,
            unresolved_net: false,
            is_const: false,
            package_variable: None,
            hierarchical: None,
        }
    }

    /// The hierarchical name of the signal of another instance the signal
    /// denotes, as written.
    pub fn hierarchical(&self) -> Option<&str> {
        self.hierarchical.as_deref()
    }

    /// The package variable (IEEE 1800-2023 26.2) the signal denotes, as
    /// `(package, variable)`: every module that uses it shares that object.
    pub fn package_variable(&self) -> Option<(&str, &str)> {
        self.package_variable
            .as_ref()
            .map(|(package, variable)| (package.as_str(), variable.as_str()))
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// A variable of unpacked structure type, which only names its leaf
    /// variables.
    pub(crate) fn is_aggregate(&self) -> bool {
        self.r#type.unpacked_struct.is_some()
    }

    pub fn r#type(&self) -> &Type {
        &self.r#type
    }

    pub(crate) fn is_net(&self) -> bool {
        self.is_net
    }

    /// Whether this is an unresolved, single-driver `uwire` net (6.6.2).
    pub(crate) fn is_unresolved_net(&self) -> bool {
        self.unresolved_net
    }

    pub(crate) fn with_unresolved_net(mut self, unresolved: bool) -> Self {
        self.unresolved_net = unresolved;
        self
    }
}

impl Port {
    fn new(name: String, direction: PortDirection, r#type: Type, is_net: bool) -> Self {
        Self {
            name,
            direction,
            r#type,
            randomizable: !is_net,
            is_net,
            unresolved_net: false,
            default: None,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn direction(&self) -> PortDirection {
        self.direction
    }

    pub fn r#type(&self) -> &Type {
        &self.r#type
    }

    pub fn is_net(&self) -> bool {
        self.is_net
    }

    /// Whether this is an unresolved, single-driver `uwire` net (6.6.2).
    pub(crate) fn is_unresolved_net(&self) -> bool {
        self.unresolved_net
    }

    pub(crate) fn with_unresolved_net(mut self, unresolved: bool) -> Self {
        self.unresolved_net = unresolved;
        self
    }

    pub(crate) fn with_default(mut self, default: Option<ConstExpr>) -> Self {
        self.default = default;
        self
    }

    pub fn default(&self) -> Option<&ConstExpr> {
        self.default.as_ref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instance {
    module_name: String,
    name: String,
    parameter_names: Vec<String>,
    parameter_overrides: Vec<ParameterOverride>,
    condition: Option<ConstExpr>,
    port_names: Vec<String>,
    port_connections: Vec<PortConnection>,
    /// The declared `[left:right]` bounds of an instance array.
    array_range: Option<(i128, i128)>,
}

impl Instance {
    fn new(
        module_name: String,
        name: String,
        parameter_names: Vec<String>,
        parameter_overrides: Vec<ParameterOverride>,
        condition: Option<ConstExpr>,
        port_names: Vec<String>,
        port_connections: Vec<PortConnection>,
        array_range: Option<(i128, i128)>,
    ) -> Self {
        Self {
            module_name,
            name,
            parameter_names,
            parameter_overrides,
            condition,
            port_names,
            port_connections,
            array_range,
        }
    }

    pub fn array_len(&self) -> Option<usize> {
        self.array_range
            .and_then(|(left, right)| usize::try_from(left.abs_diff(right)).ok()?.checked_add(1))
    }

    /// The declared `[left:right]` bounds of an instance array, if this is one.
    pub fn array_range(&self) -> Option<(i128, i128)> {
        self.array_range
    }

    pub fn module_name(&self) -> &str {
        &self.module_name
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn parameter_names(&self) -> &[String] {
        &self.parameter_names
    }

    pub fn parameter_overrides(&self) -> &[ParameterOverride] {
        &self.parameter_overrides
    }

    pub fn condition(&self) -> Option<&ConstExpr> {
        self.condition.as_ref()
    }

    pub fn port_names(&self) -> &[String] {
        &self.port_names
    }

    pub fn port_connections(&self) -> &[PortConnection] {
        &self.port_connections
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParameterOverride {
    name: String,
    value: Option<ConstExpr>,
    /// The data type resolved in the instantiating scope.
    bound_type: Option<Type>,
    bound_pattern: Option<BoundPattern>,
}

impl ParameterOverride {
    fn new(name: String, value: Option<ConstExpr>) -> Self {
        Self {
            name,
            value,
            bound_type: None,
            bound_pattern: None,
        }
    }

    fn type_override(name: String, bound_type: Type) -> Self {
        Self {
            name,
            value: None,
            bound_type: Some(bound_type),
            bound_pattern: None,
        }
    }

    /// A constant assignment pattern, evaluated in the caller scope.
    pub fn bound_pattern(&self) -> Option<&BoundPattern> {
        self.bound_pattern.as_ref()
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn value(&self) -> Option<&ConstExpr> {
        self.value.as_ref()
    }

    pub fn bound_type(&self) -> Option<&Type> {
        self.bound_type.as_ref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortConnection {
    formal: String,
    actual: String,
    actual_expr: Option<Expr>,
    bound_pattern: Option<BoundPattern>,
}

impl PortConnection {
    fn new(formal: String, actual: String, actual_expr: Option<Expr>) -> Self {
        Self {
            formal,
            actual,
            actual_expr,
            bound_pattern: None,
        }
    }

    /// An untyped assignment pattern with caller-owned values. The receiving
    /// port supplies its element types after parameter specialization.
    pub fn bound_pattern(&self) -> Option<&BoundPattern> {
        self.bound_pattern.as_ref()
    }

    fn map_expressions(&mut self, mut map: impl FnMut(Expr) -> Expr) {
        self.actual_expr = self.actual_expr.take().map(&mut map);
        if let Some(pattern) = &mut self.bound_pattern {
            pattern.map_expressions(&mut map);
        }
    }

    pub fn formal(&self) -> &str {
        &self.formal
    }

    pub fn actual(&self) -> &str {
        &self.actual
    }

    pub fn actual_expr(&self) -> Option<&Expr> {
        self.actual_expr.as_ref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortDirection {
    Input,
    Output,
    Inout,
    Ref,
    Unspecified,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Type {
    container: Option<Arc<ContainerType>>,
    is_enum: bool,
    /// Resolved members of a scalar enum; None if its domain is unsupported.
    enum_domain: Option<Arc<enums::EnumDomain>>,
    /// A packed array whose elements are of an enumerated type, whose
    /// methods are not supported on its elements.
    packed_enum: bool,
    kind: TypeKind,
    is_signed: bool,
    packed_ranges: Vec<PackedRange>,
    unpacked_ranges: Vec<UnpackedRange>,
    members: Vec<packed_structs::PackedMember>,
    /// How many leading packed dimensions select an element of a signed
    /// named type, which is signed although the whole array is not (IEEE
    /// 1800-2023 7.4.1).
    signed_element_depth: Option<usize>,
    /// The class of an object handle type.
    class: Option<Arc<str>>,
    /// The typedef that names an enumeration, struct or union type.
    nominal: Nominal,
    /// The members of an unpacked structure type (IEEE 1800-2023 7.2).
    unpacked_struct: Option<Arc<unpacked_structs::UnpackedStruct>>,
}

/// The declaration that names an enumeration, struct or union type, which
/// tells it apart from another type of the same structure (IEEE 1800-2023
/// 6.19, 6.22.1) where a class specialization is keyed by its type
/// arguments (8.25). Type equality does not compare it.
#[derive(Debug, Clone, Default)]
struct Nominal(Option<Arc<str>>);

impl PartialEq for Nominal {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for Nominal {}

impl std::hash::Hash for Nominal {
    fn hash<H: std::hash::Hasher>(&self, _: &mut H) {}
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ContainerType {
    key: Option<Type>,
    layout: system_functions::ContainerLayout,
    element: Type,
    /// The record that holds an element of unpacked structure type which is
    /// not kept as a flat value.
    record: Option<system_functions::Record>,
}

impl Type {
    fn implicit() -> Self {
        Self {
            is_enum: false,
            enum_domain: None,
            packed_enum: false,
            kind: TypeKind::Implicit,
            is_signed: false,
            packed_ranges: Vec::new(),
            unpacked_ranges: Vec::new(),
            members: Vec::new(),
            signed_element_depth: None,
            class: None,
            container: None,
            nominal: Nominal::default(),
            unpacked_struct: None,
        }
    }

    fn new(kind: TypeKind) -> Self {
        Self {
            kind,
            is_enum: false,
            enum_domain: None,
            packed_enum: false,
            is_signed: false,
            packed_ranges: Vec::new(),
            unpacked_ranges: Vec::new(),
            members: Vec::new(),
            signed_element_depth: None,
            class: None,
            container: None,
            unpacked_struct: None,
            nominal: Nominal::default(),
        }
    }

    /// Name an enumeration, struct or union type after typedef `name`,
    /// unless an earlier typedef already named it (`typedef A B` names the
    /// type of `A`).
    fn name_declaration(&mut self, name: impl FnOnce() -> String) {
        if (self.is_enum || !self.members.is_empty() || self.unpacked_struct.is_some())
            && self.nominal.0.is_none()
        {
            self.nominal = Nominal(Some(name().into()));
        }
    }

    /// The typedef that names this enumeration, struct or union type.
    fn declaration_name(&self) -> Option<&str> {
        self.nominal.0.as_deref()
    }

    /// The members of an unpacked structure type.
    pub(in crate::ast) fn unpacked_struct(&self) -> Option<&Arc<unpacked_structs::UnpackedStruct>> {
        self.unpacked_struct.as_ref()
    }

    /// The class of an object handle type.
    pub fn class(&self) -> Option<&str> {
        self.class.as_deref()
    }

    pub fn kind(&self) -> TypeKind {
        self.kind
    }

    pub fn is_signed(&self) -> bool {
        self.is_signed
    }

    pub fn packed_ranges(&self) -> &[PackedRange] {
        &self.packed_ranges
    }

    pub fn unpacked_ranges(&self) -> &[UnpackedRange] {
        &self.unpacked_ranges
    }

    /// How many leading packed dimensions select an element of a signed
    /// named type (IEEE 1800-2023 7.4.1).
    pub fn signed_element_depth(&self) -> Option<usize> {
        self.signed_element_depth
    }

    /// A type with constant `(left, right)` packed and unpacked bounds.
    pub fn resolved(
        kind: TypeKind,
        is_signed: bool,
        signed_element_depth: Option<usize>,
        packed: &[(i128, i128)],
        unpacked: &[(i128, i128)],
    ) -> Self {
        let literal = |value: i128| ConstExpr::Literal(value.to_string());
        Self {
            is_signed,
            signed_element_depth,
            packed_ranges: packed
                .iter()
                .map(|&(left, right)| PackedRange::new(literal(left), literal(right)))
                .collect(),
            unpacked_ranges: unpacked
                .iter()
                .map(|&(left, right)| UnpackedRange::new(literal(left), literal(right)))
                .collect(),
            ..Self::new(kind)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TypeKind {
    Real,
    Shortreal,
    /// IEEE 1800-2023 6.16: a dynamically sized string, held as a heap handle.
    String,
    /// IEEE 1800-2023 8.4: a handle of a class object, `null` or an object
    /// in the heap.
    Object,
    /// A scheduler-owned process reference (9.7), independent of the heap.
    Process,
    Bit,
    Logic,
    Reg,
    Implicit,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PackedRange {
    left: ConstExpr,
    right: ConstExpr,
}

impl PackedRange {
    fn new(left: ConstExpr, right: ConstExpr) -> Self {
        Self { left, right }
    }

    pub fn left(&self) -> &ConstExpr {
        &self.left
    }

    pub fn right(&self) -> &ConstExpr {
        &self.right
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UnpackedRange {
    left: ConstExpr,
    right: ConstExpr,
    size: Option<ConstExpr>,
}

impl UnpackedRange {
    fn new(left: ConstExpr, right: ConstExpr) -> Self {
        Self {
            left,
            right,
            size: None,
        }
    }

    fn sized(size: ConstExpr) -> Self {
        Self {
            left: ConstExpr::Literal("0".to_string()),
            right: ConstExpr::Binary {
                left: Box::new(size.clone()),
                op: BinaryOp::Sub,
                right: Box::new(ConstExpr::Literal("1".to_string())),
            },
            size: Some(size),
        }
    }

    pub fn left(&self) -> &ConstExpr {
        &self.left
    }

    pub fn right(&self) -> &ConstExpr {
        &self.right
    }

    fn size(&self) -> Option<&ConstExpr> {
        self.size.as_ref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ConstExpr {
    Literal(String),
    Ident(String),
    Select {
        expr: Box<ConstExpr>,
        bit: Box<ConstExpr>,
    },
    /// A typed selection used inside another selection's index.
    SelectRange {
        expr: Box<ConstExpr>,
        msb: Box<ConstExpr>,
        lsb: Box<ConstExpr>,
        signed: bool,
    },
    /// An integral cast whose width and signedness remain independent of
    /// the enclosing index arithmetic.
    Resize {
        expr: Box<ConstExpr>,
        width: usize,
        signed: bool,
    },
    Function {
        callee: Callee,
        args: Vec<ConstExpr>,
        /// Tells a user subroutine call apart from a call written alike: a
        /// select repeats its index in its bounds and range checks, and the
        /// copies of one call share its site, so the call runs once.
        site: Option<usize>,
    },
    Unary {
        op: UnaryOp,
        expr: Box<ConstExpr>,
    },
    Binary {
        left: Box<ConstExpr>,
        op: BinaryOp,
        right: Box<ConstExpr>,
    },
    Mux {
        condition: Box<ConstExpr>,
        then_expr: Box<ConstExpr>,
        else_expr: Box<ConstExpr>,
    },
}

impl ConstExpr {
    /// Effectful calls get distinct sites; analyzer-created copies retain them.
    fn call(state: &AnalysisState, callee: Callee, args: Vec<ConstExpr>) -> Self {
        let site = callee
            .is_effectful()
            .then(|| state.next_call_site.replace(state.next_call_site.get() + 1));
        ConstExpr::Function { callee, args, site }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    Plus,
    Minus,
    BitNot,
    LogicNot,
    ToTwoState,
    RedAnd,
    RedOr,
    RedXor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    Shl,
    Shr,
    Sar,
    BitAnd,
    BitOr,
    BitXor,
    LogicAnd,
    LogicOr,
    Eq,
    Ne,
    EqCase,
    NeCase,
    EqWildcard,
    NeWildcard,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    lhs: LValue,
    rhs: Expr,
}

impl Assignment {
    fn new(lhs: LValue, rhs: Expr) -> Self {
        Self { lhs, rhs }
    }

    pub fn lhs(&self) -> &str {
        self.lhs.name()
    }

    pub fn lhs_value(&self) -> &LValue {
        &self.lhs
    }

    pub fn rhs(&self) -> &Expr {
        &self.rhs
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LValue {
    Ident(String),
    Select {
        name: String,
        msb: ConstExpr,
        lsb: ConstExpr,
        signed: bool,
        array_slice_width: Option<ConstExpr>,
        array_slice_reversed: bool,
        /// Whether this selection names a two-state packed struct member.
        is_2state: bool,
    },
}

impl LValue {
    pub fn name(&self) -> &str {
        match self {
            LValue::Ident(name) | LValue::Select { name, .. } => name,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CombProcess {
    creation_order: usize,
    site: Option<crate::procedural::SourceSpan>,
    /// Where the procedure is written, for an `always @*` that becomes a
    /// process when it calls a timed task.
    process_site: Option<crate::procedural::ProcessSite>,
    kind: CombProcessKind,
    condition: Option<ConstExpr>,
    assignments: Vec<Assignment>,
    body: Vec<Stmt>,
    sensitivity_body: Vec<Stmt>,
}

impl CombProcess {
    pub fn site(&self) -> Option<&crate::procedural::SourceSpan> {
        self.site.as_ref()
    }

    /// Reads removed from executable branches still affect implicit sensitivity.
    pub fn sensitivity_body(&self) -> &[Stmt] {
        &self.sensitivity_body
    }

    /// Position among the elaborated module items, shared across process kinds.
    pub fn creation_order(&self) -> usize {
        self.creation_order
    }

    fn new(
        kind: CombProcessKind,
        condition: Option<ConstExpr>,
        assignments: Vec<Assignment>,
    ) -> Self {
        let body = assignments
            .iter()
            .map(|assignment| Stmt::Assign {
                lhs: assignment.lhs.clone(),
                rhs: assignment.rhs.clone(),
                nonblocking: false,
            })
            .collect();
        Self {
            creation_order: 0,
            site: None,
            process_site: None,
            sensitivity_body: Vec::new(),
            kind,
            condition,
            assignments,
            body,
        }
    }

    fn procedural(kind: CombProcessKind, condition: Option<ConstExpr>, body: Vec<Stmt>) -> Self {
        Self {
            creation_order: 0,
            site: None,
            process_site: None,
            sensitivity_body: Vec::new(),
            kind,
            condition,
            assignments: Vec::new(),
            body,
        }
    }

    pub fn body(&self) -> &[Stmt] {
        &self.body
    }

    pub fn kind(&self) -> CombProcessKind {
        self.kind
    }

    pub fn condition(&self) -> Option<&ConstExpr> {
        self.condition.as_ref()
    }

    pub fn assignments(&self) -> &[Assignment] {
        &self.assignments
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CombProcessKind {
    ContinuousAssign,
    AlwaysComb,
    AlwaysLatch,
    AlwaysStar,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfProcess {
    always_ff: bool,
    suspendable: bool,
    creation_order: usize,
    site: Option<crate::procedural::ProcessSite>,
    events: Vec<FfEvent>,
    body: Vec<Stmt>,
}

impl FfProcess {
    pub fn is_always_ff(&self) -> bool {
        self.always_ff
    }

    /// Position among the elaborated module items, shared across process kinds.
    pub fn creation_order(&self) -> usize {
        self.creation_order
    }

    pub fn site(&self) -> Option<&crate::procedural::ProcessSite> {
        self.site.as_ref()
    }

    fn new(events: Vec<FfEvent>, body: Vec<Stmt>) -> Self {
        Self {
            always_ff: false,
            suspendable: false,
            creation_order: 0,
            site: None,
            events,
            body,
        }
    }

    pub fn events(&self) -> &[FfEvent] {
        &self.events
    }

    pub fn body(&self) -> &[Stmt] {
        &self.body
    }
}

/// A scheduled procedure, including initial/always processes and final procedures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitialProcess {
    creation_order: usize,
    site: Option<crate::procedural::ProcessSite>,
    condition: Option<ConstExpr>,
    body: Vec<Stmt>,
    kind: celox_design::ProcessKind,
    /// Folded-away statements retained only for final-procedure legality checks.
    validation_body: Vec<Stmt>,
    synthetic: bool,
    /// An `always` process: its body runs forever.
    repeats: bool,
}

impl InitialProcess {
    /// Position among the elaborated module items, shared across process kinds.
    pub fn creation_order(&self) -> usize {
        self.creation_order
    }

    pub fn site(&self) -> Option<&crate::procedural::ProcessSite> {
        self.site.as_ref()
    }

    pub fn condition(&self) -> Option<&ConstExpr> {
        self.condition.as_ref()
    }

    /// The procedure's simulation phase.
    pub fn kind(&self) -> celox_design::ProcessKind {
        self.kind
    }

    /// A final procedure runs once when the simulation finishes (9.2.3).
    pub fn is_final(&self) -> bool {
        self.kind == celox_design::ProcessKind::Final
    }

    /// Compiler-generated storage initialization, without a user static process.
    pub fn is_synthetic(&self) -> bool {
        self.synthetic
    }

    /// Whether this process holds variable declaration initializers.
    pub fn is_initializer(&self) -> bool {
        self.kind == celox_design::ProcessKind::Initializer
    }

    /// Whether this is an `always` process, which runs its body forever and
    /// never finishes on its own.
    pub fn repeats(&self) -> bool {
        self.repeats
    }

    pub fn body(&self) -> &[Stmt] {
        &self.body
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfEdge {
    Pos,
    Neg,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfEvent {
    edge: FfEdge,
    signal: String,
}

impl FfEvent {
    fn new(edge: FfEdge, signal: String) -> Self {
        Self { edge, signal }
    }

    pub fn edge(&self) -> FfEdge {
        self.edge
    }

    pub fn signal(&self) -> &str {
        &self.signal
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConditionalAssignment {
    condition: Option<Expr>,
    assignment: Assignment,
    exhaustive_fallback_start: Option<usize>,
    /// Statement slot at which the controlling if/case chain was evaluated.
    guard_boundary: Option<usize>,
    /// Nested branch paths, from innermost to outermost.
    path_epochs: Vec<usize>,
}

impl ConditionalAssignment {
    pub fn condition(&self) -> Option<&Expr> {
        self.condition.as_ref()
    }

    pub fn assignment(&self) -> &Assignment {
        &self.assignment
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Function {
    /// The aggregate return type, frozen in its declaration scope.
    return_type: Option<Type>,
    return_real: bool,
    /// Whether the function returns a `string`.
    return_string: bool,
    /// Whether the function returns a class handle.
    return_handle: bool,
    name: String,
    params: Vec<FunctionParam>,
    /// The function's value as one expression of its arguments, or `None`
    /// for a function or task written as statements: a call of it is not
    /// expanded into an expression.
    body: Option<Expr>,
    /// For each `output` / `inout` parameter, its value when the body ends,
    /// in terms of the input parameters.
    outputs: Vec<(String, Expr)>,
    return_width: Option<usize>,
    return_first_packed_dimension_width: Option<usize>,
    return_dimensions: Option<usize>,
    return_signed: bool,
    return_is_2state: bool,
    /// The members of an enumerated return type, for its methods.
    return_enum_domain: Option<Arc<enums::EnumDomain>>,
}

fn function_argument_metadata(params: &[FunctionParam]) -> Arc<[FunctionArgumentMetadata]> {
    params
        .iter()
        .map(|param| FunctionArgumentMetadata {
            name: param.name.clone(),
            direction: param.direction,
            has_default: param.has_default,
        })
        .collect()
}

fn dpi_argument_metadata(import: &crate::ir::DpiImport) -> Arc<[FunctionArgumentMetadata]> {
    import
        .arguments()
        .iter()
        .map(|argument| FunctionArgumentMetadata {
            name: argument.name().to_string(),
            direction: ParamDirection::Input,
            has_default: false,
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FunctionParam {
    has_default: bool,
    direction: ParamDirection,
    name: String,
    width: Option<usize>,
    signed: bool,
    is_2state: bool,
    packed_dimensions: Vec<PackedDimension>,
    /// See [`Type::signed_element_depth`].
    signed_element_depth: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FunctionLocalType {
    width: usize,
    signed: bool,
    is_2state: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Expr {
    Ident(String),
    Literal(String),
    Select {
        expr: Box<Expr>,
        msb: ConstExpr,
        lsb: ConstExpr,
        signed: bool,
    },
    Concat(Vec<Expr>),
    RepeatConcat {
        count: ConstExpr,
        parts: Vec<Expr>,
    },
    Resize {
        expr: Box<Expr>,
        width: usize,
        signed: bool,
    },
    Unary {
        op: UnaryOp,
        expr: Box<Expr>,
    },
    Binary {
        left: Box<Expr>,
        op: BinaryOp,
        right: Box<Expr>,
    },
    Mux {
        condition: Box<Expr>,
        then_expr: Box<Expr>,
        else_expr: Box<Expr>,
    },
    Call {
        callee: Callee,
        /// Missing actuals retain their formal slots for declaration-scope defaults.
        args: Vec<Option<Expr>>,
        /// Formal positions in source evaluation/copy-out order; empty means positional.
        arg_order: Vec<usize>,
    },
    /// `expr inside { items }`: true when `expr` matches any item.
    Inside {
        expr: Box<Expr>,
        items: Vec<InsideItem>,
    },
}

/// One item of an `inside` set.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum InsideItem {
    /// A value matched with wildcard equality (`==?`).
    Value(Expr),
    /// An inclusive range `[low:high]`.
    Range { low: Expr, high: Expr },
}

impl InsideItem {
    /// The operand expressions of the item.
    fn exprs(&self) -> Vec<&Expr> {
        match self {
            InsideItem::Value(value) => vec![value],
            InsideItem::Range { low, high } => vec![low, high],
        }
    }

    /// Rebuild the item with `f` applied to each operand.
    fn map(self, f: &mut impl FnMut(Expr) -> Expr) -> InsideItem {
        match self {
            InsideItem::Value(value) => InsideItem::Value(f(value)),
            InsideItem::Range { low, high } => InsideItem::Range {
                low: f(low),
                high: f(high),
            },
        }
    }
}

/// Enum member constants collected from module-level `typedef enum`
/// declarations.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct EnumMemberConstants {
    /// Evaluated values for the module constant environment.
    numbers: HashMap<String, i128>,
    /// Resolved literal expressions for process-expression substitution.
    exprs: HashMap<String, Expr>,
    /// Base width and signedness retained for early constant substitution.
    types: HashMap<String, ExprType>,
}

impl EnumMemberConstants {
    /// Add the members of `other` that are not known already.
    fn extend_missing(&mut self, other: &EnumMemberConstants) {
        for (name, value) in &other.numbers {
            self.numbers.entry(name.clone()).or_insert(*value);
        }
        for (name, value) in &other.exprs {
            self.exprs
                .entry(name.clone())
                .or_insert_with(|| value.clone());
        }
        for (name, value) in &other.types {
            self.types.entry(name.clone()).or_insert(*value);
        }
    }
}

/// The declared dimensions of `parameters` and of the package parameters a
/// scope imports, so that selects use their declared ranges.
fn parameter_packed_dimensions_with_imported(
    parameters: &[Parameter],
    imported: &scope::ScopeSymbols,
) -> VariablePackedDimensions {
    let mut dimensions = parameter_packed_dimensions(&imported.parameters);
    // A local scalar or implicitly typed parameter also shadows an imported
    // packed parameter, even though it contributes no explicit packed ranges.
    for parameter in parameters {
        dimensions.remove(parameter.name());
    }
    dimensions.extend(parameter_packed_dimensions(parameters));
    dimensions
}

/// The literal values of `parameters`, and of the package parameters and
/// enum members a scope imports.
fn parameter_value_env_with_imported(
    state: &AnalysisState,
    parameters: &[Parameter],
    const_env: &HashMap<String, i128>,
    imported: &scope::ScopeSymbols,
) -> HashMap<String, Expr> {
    let mut values = parameter_value_env(state, parameters, const_env);
    let local_names: HashSet<_> = parameters.iter().map(Parameter::name).collect();
    for (name, value) in &imported.parameter_values {
        if !local_names.contains(name.as_str()) {
            values.entry(name.clone()).or_insert_with(|| value.clone());
        }
    }
    values
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExprType {
    pub(crate) width: usize,
    pub(crate) signed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PackedDimension {
    left: ConstExpr,
    right: ConstExpr,
    width: ConstExpr,
    normalize_single: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UnpackedDimension {
    left: ConstExpr,
    right: ConstExpr,
    width: ConstExpr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VariableDimensions {
    container: Option<Arc<ContainerType>>,
    randomizable: bool,
    enum_domain: Option<Arc<enums::EnumDomain>>,
    /// See [`Type::packed_enum`].
    packed_enum: bool,
    real: bool,
    /// A `string`: each element is a heap handle, not packed bits.
    string: bool,
    /// The class of an object handle variable.
    class: Option<String>,
    packed: Vec<PackedDimension>,
    unpacked: Vec<UnpackedDimension>,
    signed: bool,
    is_2state: bool,
    members: Vec<packed_structs::PackedMember>,
    /// See [`Type::signed_element_depth`].
    signed_element_depth: Option<usize>,
    /// A variable of unpacked structure type: the names of its members
    /// are variables (see [`unpacked_structs`]).
    unpacked_struct: Option<Arc<unpacked_structs::UnpackedStruct>>,
}

type VariablePackedDimensions = HashMap<String, VariableDimensions>;

#[derive(Debug, Clone, PartialEq, Eq)]
struct FunctionArgumentMetadata {
    name: String,
    direction: ParamDirection,
    has_default: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct FunctionReturnMetadata {
    /// Validate calls in type queries without retaining or executing bodies.
    arguments: Option<Arc<[FunctionArgumentMetadata]>>,
    width: Option<usize>,
    first_packed_dimension_width: Option<usize>,
    dimensions: Option<usize>,
    signed: bool,
    is_2state: bool,
    /// The function returns a `string`.
    string: bool,
    /// Declared aggregate type, available before function bodies are inlined.
    pattern_type: Option<Type>,
    /// The members of an enumerated return type, for its methods.
    enum_domain: Option<Arc<enums::EnumDomain>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct PackedDimensions {
    variables: ScopedMap<VariableDimensions>,
    const_env: SharedMap<i128>,
    type_aliases: SharedMap<Type>,
    function_return_types: SharedMap<FunctionReturnMetadata>,
    functions: Arc<HashMap<String, Function>>,
    parameter_values: SharedMap<Expr>,
    /// Constant array initializers for binding child parameter values.
    array_parameter_values: SharedMap<Expr>,
    expression_signedness: ScopedMap<bool>,
    constant_indexed_base: bool,
    /// The current lexical scope has already collected its visible declarations.
    /// Preliminary parameter/function/range lowering must keep this false.
    scope_types_complete: bool,
    /// Calls are checked for legality while their operands remain unexecuted.
    type_query: bool,
    /// The declared shape of each argument of each subroutine, for
    /// assignment patterns passed as arguments.
    subroutine_param_shapes: Arc<HashMap<String, Vec<VariableDimensions>>>,
    subroutine_directions: Arc<lifetimes::Directions>,
    subroutine_param_names: Arc<HashMap<String, Vec<String>>>,
    /// The class whose method is being bound.
    class_scope: Option<String>,
}

impl PackedDimensions {
    fn new(
        variables: impl Into<ScopedMap<VariableDimensions>>,
        const_env: &HashMap<String, i128>,
        type_aliases: &HashMap<String, Type>,
    ) -> Self {
        Self {
            variables: variables.into(),
            const_env: const_env.clone().into(),
            type_aliases: type_aliases.clone().into(),
            function_return_types: SharedMap::default(),
            functions: Arc::default(),
            parameter_values: SharedMap::default(),
            array_parameter_values: SharedMap::default(),
            expression_signedness: ScopedMap::default(),
            constant_indexed_base: false,
            scope_types_complete: false,
            type_query: false,
            subroutine_param_shapes: Arc::default(),
            subroutine_directions: Arc::default(),
            subroutine_param_names: Arc::default(),
            class_scope: None,
        }
    }
}

impl Deref for PackedDimensions {
    type Target = ScopedMap<VariableDimensions>;

    fn deref(&self) -> &Self::Target {
        &self.variables
    }
}

impl DerefMut for PackedDimensions {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.variables
    }
}

impl Signedness for PackedDimensions {
    fn signedness(&self, name: &str) -> Option<bool> {
        self.get(name).map(|dimensions| dimensions.signed)
    }
}

/// The outermost generate scope of a scoped name such as `g.x` or `g[0].x`,
/// or `None` for a name declared directly in the module. An escaped scope
/// component ends at its terminating space.
fn generate_scope_head(name: &str) -> Option<&str> {
    let end = if name.starts_with('\\') {
        name.find(' ')? + 1
    } else {
        name.find(['.', '['])?
    };
    Some(&name[..end])
}
