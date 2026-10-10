//! SystemVerilog frontend AST.
//!
//! This AST is intentionally smaller and more semantic than the raw
//! `sv-parser` CST, but it is still a language frontend structure rather than
//! Celox runtime IR.
//!
//! Public AST types and module construction live here. Child modules collect
//! declarations and processes, resolve types and constants, expand functions,
//! and normalize procedural expressions and assignments.

use fxhash::{FxHashMap as HashMap, FxHashSet as HashSet};
use std::{
    borrow::Cow,
    cell::RefCell,
    ops::{Deref, DerefMut},
    sync::Arc,
};

use sv_parser::{Locate, RefNode, SyntaxTree, unwrap_node};

use crate::{AnalyzerError, system_functions, typecheck};

mod array_compatibility;
mod array_parameters;
mod assignment_analysis;
mod case;
mod casts;
mod comb_process;
mod comb_rewrite;
pub(crate) mod const_functions;
mod constant_folding;
mod constants;
mod declarations;
mod dimensions;
mod dpi;
mod expressions;
mod ff_process;
mod functions;
mod generate;
mod imports;
mod inlining;
mod instances;
pub mod interfaces;
pub(crate) mod module_index;
pub mod packages;
mod packed_structs;
mod parameters;
mod patterns;
mod procedural;
mod scope;
mod scoped_map;
mod selects;
mod shared_map;
mod statements;
pub(crate) mod type_parameters;
mod types;
mod validation;

use array_compatibility::{
    check_unpacked_array_assignment, net_lvalue_unpacked_shape, selected_unpacked_shape,
    variable_lvalue_unpacked_shape,
};
use assignment_analysis::two_state_conditions_are_complements;
use case::expr_is_two_state;
use casts::{
    cast_is_supported, constant_cast_const_expr, constant_cast_is_supported, expr_type_from_type,
    resize_integral_literal_for_cast, resize_unbased_fill_literal_for_cast, runtime_cast_expr,
    runtime_constant_cast_const_expr,
};
use comb_process::comb_processes_from_module_node;
use comb_rewrite::{expr_static_width, whole_packed_lvalue};
use constant_folding::{
    fold_const_integral_expr_preserving_mask, simplify_constant_mux_conditions,
};
use constants::{
    binary_op_from_symbol, bind_generate_parameter, const_expr_from_constant_param_with_env,
    const_expr_from_expr, const_expr_from_param_expression, const_expr_from_ref_node,
    const_expr_from_ref_node_with_env, eval_ast_const_expr, expr_to_const, expr_to_lvalue_const,
    left_associate_expr_binary, next_genvar_value, primary_literal_text,
    substitute_assignment_constants_with_parameter_literals, substitute_const_expr_constants,
    substitute_const_expr_constants_preserving_enum_types, substitute_dimension_constants,
    substitute_expr_constants_with_parameter_literals, substitute_lvalue_constants,
    substitute_process_constants, substitute_process_constants_with_parameter_literals,
    unary_expr_from_symbol,
};
use declarations::{
    ScopeItem, identifier_locate, module_interface_from_node, module_name_from_node,
    module_non_port_items, module_parameter_port_list, package_declarations, parameter_name,
    parameters_from_module_node, ports_from_module_node, scope_declarations, scope_items,
    scope_name_from_node, signals_from_data_declaration, signals_from_module_node,
    signals_from_module_or_generate_item, type_alias_from_ref_node,
};
use dimensions::{
    enum_marker, extend_const_env_with_variable_types, function_packed_dimension_widths,
    function_param_packed_dimensions, insert_parameter_type_markers, local_parameter_marker,
    packed_dimensions_from_ports_and_signals, parameter_dimension_marker,
    parameter_dimensions_marker, parameter_marker, parameter_packed_dimensions,
    parameter_signed_element_marker, parameter_signed_marker, parameter_type_from_const_env,
    parameter_types_from_const_env, parameter_width_marker, size_system_function_expr_type,
    unpacked_dimension_widths, variable_bits_marker, variable_signed_marker,
    variable_size_function_width, variable_size_marker,
};
use expressions::{
    expr_from_expression, expr_from_expression_for_lvalue, expr_from_expression_with_types,
    expr_from_function_subroutine_call, expr_from_primary, expr_from_subroutine_call,
    expr_from_tf_call, expression_is_grouped, guard_zero_divisions, system_tf_call_parts,
};
use ff_process::ff_processes_from_module_node;
use functions::{
    function_from_declaration, function_local_packed_dimensions_from_block_item_iter,
    function_local_packed_dimensions_from_block_items,
    function_return_first_packed_dimension_width, function_return_is_2state, function_return_type,
    function_type_from_ref_node, functions_from_module_node, integer_atom_expr_type,
    procedural_truth_condition, tf_item_params, tf_params,
};
use inlining::{
    expand_expr_calls, expr_signedness, expr_signedness_with_return_types, substitute_expr_idents,
};
use instances::{
    collect_connected_nets, expr_ident_name, identifier_text, instances_from_module_node,
    node_source_text, reference_name,
};
use parameters::{
    apply_parameter_overrides, coerce_const_parameter_value, const_env_from_parameters,
    const_expr_from_i128, const_expr_to_expr, enum_member_constants_from_module_node,
    extend_const_env_with_parameters, format_typed_parameter_literal, infer_const_expr_type,
    infer_parameter_value_type, parameter_element_literal, parameter_value_env,
    parameters_from_ref_node, substitute_typed_parameter_literals,
};
use scoped_map::{ScopedMap, Signedness};
use selects::{
    add_expr, expr_select_from_select, indexed_select_base, net_lvalue_from_node,
    part_select_bounds, product_expr, variable_lvalue_from_node,
};
use shared_map::SharedMap;
use statements::{
    assignment_op_expr, coerce_procedural_assignment_rhs, expr_from_cond_predicate,
    expr_from_lvalue, lvalue_expr_type,
};
use types::{
    direction_from_port_direction, direction_from_ref_node, is_signed_from_ref_node,
    packed_ranges_from_ref_node_with_env, signed_element_depth_from_ref_node,
    type_alias_from_data_type, type_alias_from_data_type_or_implicit,
    type_aliases_from_module_node, type_aliases_from_module_node_with_env,
    type_from_net_port_header, type_from_ref_node, type_from_ref_node_with_env,
    type_from_variable_port_header, type_with_fallback_ranges_with_env, type_with_unpacked_ranges,
    unpacked_ranges_from_dimensions_with_env, unpacked_ranges_from_variable_dimensions_with_env,
    validate_unpacked_dimension_sizes,
};
use validation::{
    AlwaysKind, always_comb_body, always_kind, reject_silently_ignored_constructs,
    reject_unsupported_multidimensional_packed_bounds,
};

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
}

impl Source {
    pub fn from_syntax(syntax_tree: &SyntaxTree) -> Result<Self, AnalyzerError> {
        Self::from_syntax_with_module_parameter_overrides(syntax_tree, "", &HashMap::default())
    }

    pub fn from_syntax_with_module_parameter_overrides(
        syntax_tree: &SyntaxTree,
        module_name: &str,
        parameter_overrides: &HashMap<String, i128>,
    ) -> Result<Self, AnalyzerError> {
        let parameter_overrides = parameter_overrides
            .iter()
            .map(|(name, value)| (name.clone(), const_expr_from_i128(*value)))
            .collect();
        let interfaces = Self::module_interfaces_from_syntax(syntax_tree)?;
        let packages = packages::Packages::analyze(&[syntax_tree])?;
        let mut modules = Vec::new();
        for node in syntax_tree {
            match node {
                RefNode::ModuleDeclarationAnsi(module) => {
                    let imported = imports::imported_symbols(
                        RefNode::ModuleDeclarationAnsi(module),
                        syntax_tree,
                        &packages,
                    )?;
                    modules.push(Module::from_module_node_with_parameter_overrides(
                        module,
                        syntax_tree,
                        module_name,
                        &parameter_overrides,
                        &InterfaceLookup {
                            local: &interfaces,
                            extra: &ModuleInterfaces::default(),
                        },
                        Arc::new(imported),
                    )?);
                }
                RefNode::ModuleDeclarationNonansi(_) => {
                    return Err(AnalyzerError::Unsupported(
                        "non-ANSI module port declarations".to_string(),
                    ));
                }
                _ => {}
            }
        }

        Ok(Self { modules })
    }

    pub fn from_syntax_module_with_parameter_overrides(
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
    pub fn from_syntax_module_with_parameter_expr_overrides(
        syntax_tree: &SyntaxTree,
        module_name: &str,
        parameter_overrides: &HashMap<String, ConstExpr>,
        extra_interfaces: &ModuleInterfaces,
    ) -> Result<Self, AnalyzerError> {
        let index = module_index::ModuleIndex::new(syntax_tree)?;
        let packages = packages::Packages::analyze(&[syntax_tree])?;
        Self::from_indexed_syntax_module(
            syntax_tree,
            &index,
            module_name,
            parameter_overrides,
            extra_interfaces,
            &packages,
        )
    }

    /// `packages` are the packages the module may use, from any source.
    pub(crate) fn from_indexed_syntax_module(
        syntax_tree: &SyntaxTree,
        index: &module_index::ModuleIndex,
        module_name: &str,
        parameter_overrides: &HashMap<String, ConstExpr>,
        extra_interfaces: &ModuleInterfaces,
        packages: &packages::Packages,
    ) -> Result<Self, AnalyzerError> {
        let interfaces = InterfaceLookup {
            local: &index.interfaces,
            extra: extra_interfaces,
        };
        let mut modules = Vec::new();
        for node in index.nodes(syntax_tree, module_name) {
            match node {
                RefNode::ModuleDeclarationAnsi(module) => {
                    let node = RefNode::ModuleDeclarationAnsi(module);
                    let imported = imports::imported_symbols(node.clone(), syntax_tree, packages)?;
                    modules.push(Module::from_module_node_with_parameter_overrides(
                        node,
                        syntax_tree,
                        module_name,
                        parameter_overrides,
                        &interfaces,
                        Arc::new(imported),
                    )?);
                }
                RefNode::ModuleDeclarationNonansi(module) => {
                    let node = RefNode::ModuleDeclarationNonansi(module);
                    if module_name_from_node(node, syntax_tree)? == module_name {
                        return Err(AnalyzerError::Unsupported(
                            "non-ANSI module port declarations".to_string(),
                        ));
                    }
                }
                _ => {}
            }
        }
        Ok(Self { modules })
    }

    pub fn module_names_from_syntax(
        syntax_tree: &SyntaxTree,
    ) -> Result<Vec<String>, AnalyzerError> {
        let mut names = Vec::new();
        for node in syntax_tree {
            match node {
                RefNode::ModuleDeclarationAnsi(module) => names.push(module_name_from_node(
                    RefNode::ModuleDeclarationAnsi(module),
                    syntax_tree,
                )?),
                RefNode::ModuleDeclarationNonansi(module) => names.push(module_name_from_node(
                    RefNode::ModuleDeclarationNonansi(module),
                    syntax_tree,
                )?),
                _ => {}
            }
        }
        Ok(names)
    }

    /// The positional interface of every ANSI module declared in `syntax_tree`.
    pub fn module_interfaces_from_syntax(
        syntax_tree: &SyntaxTree,
    ) -> Result<ModuleInterfaces, AnalyzerError> {
        let mut interfaces = ModuleInterfaces::default();
        for node in syntax_tree {
            if let RefNode::ModuleDeclarationAnsi(module) = node {
                let node = RefNode::ModuleDeclarationAnsi(module);
                let name = module_name_from_node(node.clone(), syntax_tree)?;
                interfaces.insert(name, module_interface_from_node(node, syntax_tree)?);
            }
        }
        Ok(interfaces)
    }

    pub fn modules(&self) -> &[Module] {
        &self.modules
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Module {
    name: String,
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
    /// The package parameters the module sees, by qualified name and by the
    /// names its imports bind.
    imported_parameters: Vec<Parameter>,
    /// The symbols of a package, for the scopes that use it.
    symbols: Option<scope::ScopeSymbols>,
}

impl Module {
    /// Make the module's constant functions available to the constant
    /// evaluator until the guard is dropped.
    pub(crate) fn install_constant_functions(&self) -> impl Drop {
        const_functions::install(self.constant_functions.clone())
    }

    /// Analyze a module or a package. The scope starts from the `imported`
    /// symbols of the packages it uses.
    fn from_module_node_with_parameter_overrides<'a>(
        node: impl Into<RefNode<'a>>,
        syntax_tree: &SyntaxTree,
        override_module_name: &str,
        parameter_overrides: &HashMap<String, ConstExpr>,
        interfaces: &InterfaceLookup<'_>,
        imported: Arc<scope::ScopeSymbols>,
    ) -> Result<Self, AnalyzerError> {
        let node = node.into();
        let is_package = matches!(node, RefNode::PackageDeclaration(_));
        let name = scope_name_from_node(node.clone(), syntax_tree)?;
        let _imported = scope::install(imported.clone());
        let with_imported = |mut functions: const_functions::ConstantFunctions| {
            functions.extend_missing(&imported.constant_functions);
            functions
        };
        let mut type_aliases = type_aliases_from_module_node(node.clone(), syntax_tree)?;
        // Constant functions see the module constants known so far; they are
        // collected again once the parameters are known.
        let _constant_functions =
            const_functions::install(with_imported(const_functions::module_constant_functions(
                node.clone(),
                syntax_tree,
                &imported.const_env,
                &type_aliases,
                &imported.parameter_values,
            )));
        let empty_parameter_overrides = HashMap::default();
        let applicable_parameter_overrides = if name == override_module_name {
            parameter_overrides
        } else {
            &empty_parameter_overrides
        };
        let mut parameters = parameters_from_module_node(
            node.clone(),
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
        const_env.extend(const_env_from_parameters(&parameters));
        type_aliases =
            type_aliases_from_module_node_with_env(node.clone(), syntax_tree, &const_env)?;
        let mut enum_constants = enum_member_constants_from_module_node(
            node.clone(),
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
        type_aliases =
            type_aliases_from_module_node_with_env(node.clone(), syntax_tree, &const_env)?;
        // Constant casts are evaluated while parameter syntax is lowered.
        // Repeat that lowering after enum constants become available so a
        // cast operand such as `byte_t'(ENUM_MEMBER)` is not permanently
        // discarded during the initial pass. Materialize only enum-member
        // references so exported parameter expressions retain dependencies
        // on other parameters for later specialization.
        parameters = parameters_from_module_node(
            node.clone(),
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
        extend_const_env_with_parameters(&mut const_env, &parameters);
        type_aliases =
            type_aliases_from_module_node_with_env(node.clone(), syntax_tree, &const_env)?;
        const_functions::replace(with_imported(const_functions::module_constant_functions(
            node.clone(),
            syntax_tree,
            &const_env,
            &type_aliases,
            &parameter_value_env_with_imported(&parameters, &const_env, &imported),
        )));
        extend_const_env_with_parameters(&mut const_env, &parameters);

        match reject_silently_ignored_constructs(
            node.clone(),
            syntax_tree,
            &const_env,
            &type_aliases,
            &parameter_packed_dimensions_with_imported(&parameters, &imported).into(),
            &parameter_value_env(&parameters, &const_env),
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
                    ports_from_module_node(node.clone(), syntax_tree, &const_env, &type_aliases)?;
                let preliminary_signals =
                    signals_from_module_node(node.clone(), syntax_tree, &const_env, &type_aliases)?;
                extend_const_env_with_variable_types(
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
                parameters = parameters_from_module_node(
                    node.clone(),
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
                extend_const_env_with_parameters(&mut const_env, &parameters);
                type_aliases =
                    type_aliases_from_module_node_with_env(node.clone(), syntax_tree, &const_env)?;
                reject_silently_ignored_constructs(
                    node.clone(),
                    syntax_tree,
                    &const_env,
                    &type_aliases,
                    &parameter_packed_dimensions_with_imported(&parameters, &imported).into(),
                    &parameter_value_env(&parameters, &const_env),
                )?;
            }
            Err(error) => return Err(error),
        }
        let ports = ports_from_module_node(node.clone(), syntax_tree, &const_env, &type_aliases)?;
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
        let mut signals =
            signals_from_module_node(node.clone(), syntax_tree, &const_env, &type_aliases)?;
        let array_parameters = array_parameters::array_parameters_from_module_node(
            node.clone(),
            syntax_tree,
            &const_env,
            &type_aliases,
        )?;
        signals.extend(
            array_parameters
                .iter()
                .map(|parameter| parameter.signal.clone()),
        );
        // The array parameters of the packages the module uses are constant
        // signals of the module, like its own.
        signals.extend(imported.signals.iter().cloned());
        for r#type in ports
            .iter()
            .map(Port::r#type)
            .chain(signals.iter().map(Signal::r#type))
        {
            validate_unpacked_dimension_sizes(r#type.unpacked_ranges(), &const_env)?;
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
        packed_dimensions.parameter_values =
            parameter_value_env_with_imported(&parameters, &const_env, &imported).into();
        packed_dimensions
            .parameter_values
            .retain(|name, _| !const_env.contains_key(name));
        packed_dimensions.extend(parameter_packed_dimensions_with_imported(
            &parameters,
            &imported,
        ));
        // Instance connections may call subroutines, of the module or of the
        // packages it uses, with untyped assignment patterns.
        let (mut subroutine_params, mut subroutine_shapes) = procedural::subroutine_argument_names(
            node.clone(),
            syntax_tree,
            &const_env,
            &type_aliases,
        )?;
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
        packed_dimensions.subroutine_param_shapes = Arc::new(subroutine_shapes.clone());
        let mut instances = instances_from_module_node(
            node.clone(),
            syntax_tree,
            &const_env,
            &packed_dimensions,
            interfaces,
        )?;
        let mut instance_names = HashSet::default();
        if let Some(instance) = instances
            .iter()
            .filter(|instance| {
                instance
                    .condition()
                    .and_then(|condition| eval_ast_const_expr(condition, &const_env))
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
        let scope_heads: HashSet<&str> = signals
            .iter()
            .map(Signal::name)
            .chain(instances.iter().map(Instance::name))
            .filter_map(generate_scope_head)
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
        reject_unsupported_multidimensional_packed_bounds(&ports, &signals, &const_env)?;
        let mut parameter_values =
            parameter_value_env_with_imported(&parameters, &const_env, &imported);
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
        let mut functions =
            functions_from_module_node(node.clone(), syntax_tree, &const_env, &packed_dimensions)?;
        for (name, function) in &imported.functions {
            functions
                .entry(name.clone())
                .or_insert_with(|| function.clone());
        }
        for (name, metadata) in &imported.function_return_types {
            packed_dimensions
                .function_return_types
                .entry(name.clone())
                .or_insert(*metadata);
        }
        packed_dimensions
            .function_return_types
            .extend(functions.iter().map(|(name, function)| {
                (
                    name.clone(),
                    FunctionReturnMetadata {
                        width: function.return_width,
                        first_packed_dimension_width: function.return_first_packed_dimension_width,
                        signed: function.return_signed,
                        is_2state: function.return_is_2state,
                    },
                )
            }));
        let mut dpi_imports = dpi::dpi_imports_from_module_node(
            node.clone(),
            syntax_tree,
            &const_env,
            &type_aliases,
        )?;
        packed_dimensions
            .function_return_types
            .extend(dpi_imports.iter().filter_map(|import| {
                let r#type = import.return_type()?;
                Some((
                    import.name().to_string(),
                    FunctionReturnMetadata {
                        width: Some(r#type.width()),
                        first_packed_dimension_width: (r#type.width() > 1)
                            .then_some(r#type.width()),
                        signed: r#type.is_signed(),
                        is_2state: !r#type.is_4state(),
                    },
                ))
            }));
        packed_dimensions.functions = Arc::new(functions.clone());
        packed_dimensions.expression_signedness = expression_signedness.clone().into();
        for instance in &mut instances {
            for connection in &mut instance.port_connections {
                connection.actual_expr = connection.actual_expr.take().map(|expr| {
                    let expr = expand_expr_calls(expr, &functions, &expression_signedness, 0, true);
                    substitute_expr_constants_with_parameter_literals(
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
        };
        let mut subroutines = procedural::subroutines_from_module_node(
            node.clone(),
            syntax_tree,
            &const_env,
            &packed_dimensions,
            &parameter_values,
            &mut body_state,
        )?;
        let mut comb_processes = comb_processes_from_module_node(
            node.clone(),
            syntax_tree,
            &const_env,
            &packed_dimensions,
            &parameter_values,
            &mut body_state,
        )?
        .into_iter()
        .map(|process| {
            substitute_process_constants_with_parameter_literals(
                process,
                &const_env,
                &parameter_values,
            )
        })
        .collect::<Vec<_>>();
        let mut ff_processes = ff_processes_from_module_node(
            node.clone(),
            syntax_tree,
            &const_env,
            &parameter_values,
            &packed_dimensions,
            &mut body_state,
        )?;
        let mut initial_processes = Vec::new();
        for parameter in &array_parameters {
            initial_processes.push(parameter.initial_process(syntax_tree, &packed_dimensions)?);
        }
        initial_processes.extend(imported.initial_processes.iter().cloned());
        initial_processes.extend(procedural::initial_processes_from_module_node(
            node.clone(),
            syntax_tree,
            &const_env,
            &packed_dimensions,
            &parameter_values,
            &mut body_state,
        )?);
        let procedurally_written = procedural::written_names(
            comb_processes
                .iter()
                .flat_map(|process| process.body.iter())
                .chain(ff_processes.iter().flat_map(|process| process.body.iter())),
        );
        let mut connected_nets = HashSet::default();
        for connection in instances
            .iter()
            .flat_map(|instance| instance.port_connections())
        {
            if let Some(expr) = connection.actual_expr() {
                collect_connected_nets(expr, &mut connected_nets);
            }
        }
        if let Some(signal) = signals.iter().find(|signal| {
            signal.is_net()
                && !procedurally_written.contains(signal.name())
                && !connected_nets.contains(signal.name())
        }) {
            return Err(AnalyzerError::Unsupported(format!(
                "undriven net declaration `{}`",
                signal.name()
            )));
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
                for assignment in &mut process.assignments {
                    assignment.rhs = rename.expr(assignment.rhs.clone());
                }
            }
            for process in &mut ff_processes {
                process.body = body(std::mem::take(&mut process.body));
            }
            for process in &mut initial_processes {
                process.body = body(std::mem::take(&mut process.body));
            }
            for subroutine in &mut subroutines {
                *subroutine = rename.subroutine(subroutine.clone());
            }
            for instance in &mut instances {
                for connection in &mut instance.port_connections {
                    connection.actual_expr =
                        connection.actual_expr.take().map(|expr| rename.expr(expr));
                }
            }
        }
        let assignments = comb_processes
            .iter()
            .flat_map(|process| process.assignments().iter().cloned())
            .collect();
        // A package keeps its symbols for the scopes that use it.
        let symbols = is_package.then(|| scope::ScopeSymbols {
            const_env: const_env.clone(),
            parameter_values: parameter_values.clone(),
            parameters: parameters.clone(),
            type_aliases: type_aliases.clone(),
            enum_constants: enum_constants.clone(),
            functions: functions.clone(),
            function_return_types: packed_dimensions.function_return_types.clone(),
            constant_functions: const_functions::installed(),
            subroutine_params: subroutine_params.clone(),
            subroutine_shapes: subroutine_shapes.clone(),
            subroutines: subroutines.clone(),
            locals: locals.clone(),
            dpi_imports: dpi_imports.clone(),
            // A package's signals are its array parameters and constant
            // variables, initialized by its initial processes.
            signals: signals.clone(),
            initial_processes: initial_processes.clone(),
            aliases: imported.aliases.clone(),
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

        Ok(Self {
            name,
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
            constant_functions: const_functions::installed(),
            imported_parameters,
            symbols,
        })
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
}

const MAX_GENERATE_LOOP_EXPANSION: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parameter {
    name: String,
    value: Option<ConstExpr>,
    declared_width: Option<usize>,
    packed_ranges: Vec<PackedRange>,
    declared_signed: Option<bool>,
    declared_is_2state: bool,
    has_declared_type: bool,
    is_local: bool,
    /// See [`Type::signed_element_depth`].
    signed_element_depth: Option<usize>,
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
            packed_ranges: Vec::new(),
            declared_signed,
            declared_is_2state,
            has_declared_type,
            is_local,
            signed_element_depth: None,
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
        constants: &HashMap<String, i128>,
        parameter_types: &HashMap<String, ExprType>,
    ) -> Option<i128> {
        let mut value =
            substitute_typed_parameter_literals(self.value()?.clone(), constants, parameter_types);
        if self.declared_is_2state {
            value = ConstExpr::Unary {
                op: UnaryOp::ToTwoState,
                expr: Box::new(value),
            };
        }
        let mut value = typecheck::eval_const_expr(&value.into(), constants)?;
        if let Some(width) = self.declared_width {
            value =
                coerce_const_parameter_value(value, width, self.declared_signed.unwrap_or(false));
        }
        Some(value)
    }

    pub(crate) fn resolved_value_with_literals(
        &self,
        constants: &HashMap<String, i128>,
        parameter_types: &HashMap<String, ExprType>,
        literals: &HashMap<String, Expr>,
    ) -> Option<i128> {
        self.resolved_value(constants, parameter_types).or_else(|| {
            let literal = self.resolved_literal(constants, parameter_types, literals)?;
            eval_ast_const_expr(&expr_to_const(literal)?, constants)
        })
    }

    pub(crate) fn resolved_literal(
        &self,
        constants: &HashMap<String, i128>,
        parameter_types: &HashMap<String, ExprType>,
        literals: &HashMap<String, Expr>,
    ) -> Option<Expr> {
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
        let value = parameter_value_env(std::slice::from_ref(&parameter), &evaluation_constants)
            .remove(self.name())?;
        let value = substitute_expr_idents(value, literals);
        let value = fold_const_integral_expr_preserving_mask(value, &evaluation_constants);
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

    pub(crate) fn declared_width(&self) -> Option<usize> {
        self.declared_width
    }

    pub(crate) fn declared_signed(&self) -> Option<bool> {
        self.declared_signed
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Port {
    name: String,
    direction: PortDirection,
    r#type: Type,
    is_net: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signal {
    name: String,
    r#type: Type,
    is_net: bool,
}

impl Signal {
    fn new(name: String, r#type: Type) -> Self {
        Self {
            name,
            r#type,
            is_net: false,
        }
    }

    fn new_net(name: String, r#type: Type) -> Self {
        Self {
            name,
            r#type,
            is_net: true,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn r#type(&self) -> &Type {
        &self.r#type
    }

    pub(crate) fn is_net(&self) -> bool {
        self.is_net
    }
}

impl Port {
    fn new(name: String, direction: PortDirection, r#type: Type, is_net: bool) -> Self {
        Self {
            name,
            direction,
            r#type,
            is_net,
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
    /// The source text of a data type, for a `parameter type` override.
    type_text: Option<String>,
}

impl ParameterOverride {
    fn new(name: String, value: Option<ConstExpr>) -> Self {
        Self {
            name,
            value,
            type_text: None,
        }
    }

    fn type_override(name: String, type_text: String) -> Self {
        Self {
            name,
            value: None,
            type_text: Some(type_text),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn value(&self) -> Option<&ConstExpr> {
        self.value.as_ref()
    }

    pub fn type_text(&self) -> Option<&str> {
        self.type_text.as_deref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortConnection {
    formal: String,
    actual: String,
    actual_expr: Option<Expr>,
}

impl PortConnection {
    fn new(formal: String, actual: String, actual_expr: Option<Expr>) -> Self {
        Self {
            formal,
            actual,
            actual_expr,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Type {
    kind: TypeKind,
    is_signed: bool,
    packed_ranges: Vec<PackedRange>,
    unpacked_ranges: Vec<UnpackedRange>,
    members: Vec<packed_structs::PackedMember>,
    /// How many leading packed dimensions select an element of a signed
    /// named type, which is signed although the whole array is not (IEEE
    /// 1800-2023 7.4.1).
    signed_element_depth: Option<usize>,
}

impl Type {
    fn implicit() -> Self {
        Self {
            kind: TypeKind::Implicit,
            is_signed: false,
            packed_ranges: Vec::new(),
            unpacked_ranges: Vec::new(),
            members: Vec::new(),
            signed_element_depth: None,
        }
    }

    fn new(kind: TypeKind) -> Self {
        Self {
            kind,
            is_signed: false,
            packed_ranges: Vec::new(),
            unpacked_ranges: Vec::new(),
            members: Vec::new(),
            signed_element_depth: None,
        }
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeKind {
    Bit,
    Logic,
    Reg,
    Implicit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConstExpr {
    Literal(String),
    Ident(String),
    Select {
        expr: Box<ConstExpr>,
        bit: Box<ConstExpr>,
    },
    Function {
        name: String,
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

thread_local! {
    static NEXT_CALL_SITE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Run one analysis step with call sites numbered from zero, so that
/// analyzing the same source always gives the same sites.
pub(crate) fn with_call_sites<T>(f: impl FnOnce() -> T) -> T {
    let saved = NEXT_CALL_SITE.with(|next| next.replace(0));
    let result = f();
    NEXT_CALL_SITE.with(|next| next.set(saved));
    result
}

impl ConstExpr {
    /// A call of `name`; a user subroutine call gets a site of its own.
    fn call(name: String, args: Vec<ConstExpr>) -> Self {
        let site = (!name.starts_with('$'))
            .then(|| NEXT_CALL_SITE.with(|next| next.replace(next.get() + 1)));
        ConstExpr::Function { name, args, site }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    kind: CombProcessKind,
    condition: Option<ConstExpr>,
    assignments: Vec<Assignment>,
    body: Vec<Stmt>,
}

impl CombProcess {
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
            kind,
            condition,
            assignments,
            body,
        }
    }

    fn procedural(kind: CombProcessKind, condition: Option<ConstExpr>, body: Vec<Stmt>) -> Self {
        Self {
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfProcess {
    events: Vec<FfEvent>,
    body: Vec<Stmt>,
}

impl FfProcess {
    fn new(events: Vec<FfEvent>, body: Vec<Stmt>) -> Self {
        Self { events, body }
    }

    pub fn events(&self) -> &[FfEvent] {
        &self.events
    }

    pub fn body(&self) -> &[Stmt] {
        &self.body
    }
}

/// An `initial` process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitialProcess {
    condition: Option<ConstExpr>,
    body: Vec<Stmt>,
    initializer: bool,
}

impl InitialProcess {
    pub fn condition(&self) -> Option<&ConstExpr> {
        self.condition.as_ref()
    }

    /// Whether this process holds variable declaration initializers.
    pub fn is_initializer(&self) -> bool {
        self.initializer
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
    return_signed: bool,
    return_is_2state: bool,
}

/// How a function argument is passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParamDirection {
    Input,
    Output,
    Inout,
}

impl ParamDirection {
    /// Whether the call writes the actual argument back.
    fn is_written(self) -> bool {
        !matches!(self, ParamDirection::Input)
    }

    fn from_tf_port(direction: &sv_parser::TfPortDirection) -> Option<Self> {
        match direction {
            sv_parser::TfPortDirection::PortDirection(direction) => match &**direction {
                sv_parser::PortDirection::Input(_) => Some(ParamDirection::Input),
                sv_parser::PortDirection::Output(_) => Some(ParamDirection::Output),
                sv_parser::PortDirection::Inout(_) => Some(ParamDirection::Inout),
                sv_parser::PortDirection::Ref(_) => None,
            },
            sv_parser::TfPortDirection::ConstRef(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FunctionParam {
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

#[derive(Debug, Clone, PartialEq, Eq)]
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
        name: String,
        args: Vec<Expr>,
    },
    /// `expr inside { items }`: true when `expr` matches any item.
    Inside {
        expr: Box<Expr>,
        items: Vec<InsideItem>,
    },
}

/// One item of an `inside` set.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    dimensions.extend(parameter_packed_dimensions(parameters));
    dimensions
}

/// The literal values of `parameters`, and of the package parameters and
/// enum members a scope imports.
fn parameter_value_env_with_imported(
    parameters: &[Parameter],
    const_env: &HashMap<String, i128>,
    imported: &scope::ScopeSymbols,
) -> HashMap<String, Expr> {
    let mut values = parameter_value_env(parameters, const_env);
    for (name, value) in &imported.parameter_values {
        values.entry(name.clone()).or_insert_with(|| value.clone());
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
    packed: Vec<PackedDimension>,
    unpacked: Vec<UnpackedDimension>,
    signed: bool,
    is_2state: bool,
    members: Vec<packed_structs::PackedMember>,
    /// See [`Type::signed_element_depth`].
    signed_element_depth: Option<usize>,
}

type VariablePackedDimensions = HashMap<String, VariableDimensions>;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct FunctionReturnMetadata {
    width: Option<usize>,
    first_packed_dimension_width: Option<usize>,
    signed: bool,
    is_2state: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct PackedDimensions {
    variables: ScopedMap<VariableDimensions>,
    const_env: SharedMap<i128>,
    type_aliases: HashMap<String, Type>,
    function_return_types: HashMap<String, FunctionReturnMetadata>,
    functions: Arc<HashMap<String, Function>>,
    parameter_values: SharedMap<Expr>,
    expression_signedness: ScopedMap<bool>,
    constant_indexed_base: bool,
    /// The current lexical scope has already collected its visible declarations.
    /// Preliminary parameter/function/range lowering must keep this false.
    scope_types_complete: bool,
    /// The declared shape of each argument of each subroutine, for
    /// assignment patterns passed as arguments.
    subroutine_param_shapes: Arc<HashMap<String, Vec<VariableDimensions>>>,
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
            type_aliases: type_aliases.clone(),
            function_return_types: HashMap::default(),
            functions: Arc::default(),
            parameter_values: SharedMap::default(),
            expression_signedness: ScopedMap::default(),
            constant_indexed_base: false,
            scope_types_complete: false,
            subroutine_param_shapes: Arc::default(),
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
