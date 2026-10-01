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
    cell::RefCell,
    ops::{Deref, DerefMut},
    sync::Arc,
};

use sv_parser::{Locate, RefNode, SyntaxTree, unwrap_node};

use crate::{AnalyzerError, typecheck};

mod assignment_analysis;
mod case;
mod casts;
mod comb_process;
mod comb_rewrite;
mod constant_folding;
mod constants;
mod declarations;
mod dimensions;
mod expressions;
mod ff_process;
mod functions;
mod generate;
mod inlining;
mod instances;
mod packed_structs;
mod parameters;
mod selects;
mod statements;
mod types;
mod validation;

use assignment_analysis::{
    conditional_chain_has_complementary_final_predicate,
    definitely_assigned_comb_targets_statement_or_null, lvalue_bit_range, lvalue_is_covered_by,
    two_state_conditions_are_complements,
};
use case::{
    conditional_assignments_from_case_statement, expr_is_two_state, mark_condition_context,
    mark_exhaustive_fallback, two_state_case_item_reachability,
};
use casts::{
    cast_is_supported, cast_zero_type, constant_cast_const_expr, constant_cast_is_supported,
    expr_type_from_type, resize_integral_literal_for_cast, resize_unbased_fill_literal_for_cast,
    runtime_constant_cast_const_expr,
};
use comb_process::{comb_processes_from_module_node, fold_conditional_assignment_over};
use comb_rewrite::{
    comb_previous_value_placeholder, expr_contains_comb_previous_value,
    expr_references_overlapping_lvalue, expr_static_width, lvalues_overlap,
    overlapping_value_before, selected_value_after_write, substitute_intermediate_comb_value_reads,
    whole_packed_lvalue,
};
use constant_folding::{
    fold_const_integral_expr_preserving_mask, simplify_constant_mux_conditions,
};
use constants::{
    binary_op_from_symbol, bind_generate_parameter, const_expr_from_constant_param_with_env,
    const_expr_from_expr, const_expr_from_param_expression, const_expr_from_ref_node,
    const_expr_from_ref_node_with_env, eval_ast_const_expr, expr_to_const, expr_to_lvalue_const,
    left_associate_expr_binary, next_genvar_value, primary_literal_text,
    substitute_assignment_constants, substitute_assignment_constants_with_parameter_literals,
    substitute_const_expr_constants, substitute_const_expr_constants_preserving_enum_types,
    substitute_dimension_constants, substitute_expr_constants_with_parameter_literals,
    substitute_lvalue_constants, substitute_process_constants,
    substitute_process_constants_with_parameter_literals, unary_expr_from_symbol,
};
use declarations::{
    identifier_locate, module_name_from_node, module_non_port_items, module_parameter_port_list,
    module_scope_items, package_or_generate_declaration_from_module_item,
    package_or_generate_declaration_from_non_port_item, parameter_name,
    parameters_from_module_node, ports_from_module_node, signals_from_data_declaration,
    signals_from_module_node, signals_from_module_or_generate_item, type_alias_from_ref_node,
};
use dimensions::{
    enum_marker, extend_const_env_with_variable_types, function_packed_dimension_widths,
    function_param_packed_dimensions, insert_parameter_type_markers, local_parameter_marker,
    packed_dimensions_from_ports_and_signals, parameter_marker, parameter_packed_dimensions,
    parameter_signed_marker, parameter_types_from_const_env, parameter_width_marker,
    size_system_function_expr_type, unpacked_dimension_widths, variable_bits_marker,
    variable_signed_marker, variable_size_function_width, variable_size_marker,
};
use expressions::{
    expr_from_expression, expr_from_expression_with_types, expr_from_function_subroutine_call,
    expr_from_primary, expression_is_grouped, guard_zero_divisions,
};
use ff_process::ff_processes_from_module_node;
use functions::{
    case_item_condition, function_from_declaration,
    function_local_packed_dimensions_from_block_item_iter,
    function_local_packed_dimensions_from_block_items,
    function_return_first_packed_dimension_width, function_return_is_2state, function_return_type,
    function_type_from_ref_node, functions_from_module_node, integer_atom_expr_type,
    procedural_truth_condition, tf_item_params, tf_params,
};
use inlining::{
    expand_assignment_calls, expand_expr_calls, expand_ff_process_calls, expand_process_calls,
    expr_signedness_with_return_types, substitute_expr_idents,
};
use instances::{
    connection_references_net, expr_ident_name, identifier_text, instances_from_module_node,
};
use parameters::{
    apply_parameter_overrides, coerce_const_parameter_value, const_env_from_parameters,
    const_expr_from_i128, const_expr_to_expr, enum_member_constants_from_module_node,
    extend_const_env_with_parameters, format_typed_parameter_literal, infer_const_expr_type,
    infer_parameter_value_type, parameter_value_env, parameters_from_ref_node,
    substitute_typed_parameter_literals,
};
use selects::{
    add_expr, expr_select_from_select, indexed_select_base, net_lvalue_from_node,
    packed_index_offset, part_select_bounds, product_expr, variable_lvalue_from_node,
};
use statements::{
    assignment_op_expr, coerce_procedural_assignment_rhs, combine_expr_condition_terms,
    conditional_assignments_from_statement, conditional_assignments_from_statement_or_null,
    expr_from_cond_predicate, expr_from_lvalue, lvalue_expr_type,
};
use types::{
    direction_from_port_direction, direction_from_ref_node, is_signed_from_ref_node,
    packed_ranges_from_ref_node_with_env, type_alias_from_data_type,
    type_alias_from_data_type_or_implicit, type_aliases_from_module_node,
    type_aliases_from_module_node_with_env, type_from_net_port_header, type_from_ref_node,
    type_from_ref_node_with_env, type_from_variable_port_header,
    type_with_fallback_ranges_with_env, type_with_unpacked_ranges,
    unpacked_ranges_from_dimensions_with_env, unpacked_ranges_from_variable_dimensions_with_env,
    validate_unpacked_dimension_sizes,
};
use validation::{
    reject_silently_ignored_constructs, reject_unsupported_multidimensional_packed_bounds,
    static_for_loop_initial_value, static_for_loop_iterations,
};

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
        let mut modules = Vec::new();
        for node in syntax_tree {
            match node {
                RefNode::ModuleDeclarationAnsi(module) => {
                    modules.push(Module::from_module_node_with_parameter_overrides(
                        module,
                        syntax_tree,
                        module_name,
                        &parameter_overrides,
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
        )
    }

    pub fn from_syntax_module_with_parameter_expr_overrides(
        syntax_tree: &SyntaxTree,
        module_name: &str,
        parameter_overrides: &HashMap<String, ConstExpr>,
    ) -> Result<Self, AnalyzerError> {
        let mut modules = Vec::new();
        for node in syntax_tree {
            match node {
                RefNode::ModuleDeclarationAnsi(module) => {
                    let node = RefNode::ModuleDeclarationAnsi(module);
                    if module_name_from_node(node.clone(), syntax_tree)? != module_name {
                        continue;
                    }
                    modules.push(Module::from_module_node_with_parameter_overrides(
                        node,
                        syntax_tree,
                        module_name,
                        parameter_overrides,
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
}

impl Module {
    fn from_module_node_with_parameter_overrides<'a>(
        node: impl Into<RefNode<'a>>,
        syntax_tree: &SyntaxTree,
        override_module_name: &str,
        parameter_overrides: &HashMap<String, ConstExpr>,
    ) -> Result<Self, AnalyzerError> {
        let node = node.into();
        let name = module_name_from_node(node.clone(), syntax_tree)?;
        let mut type_aliases = type_aliases_from_module_node(node.clone(), syntax_tree)?;
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
            &HashMap::default(),
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
        let mut const_env = const_env_from_parameters(&parameters);
        type_aliases =
            type_aliases_from_module_node_with_env(node.clone(), syntax_tree, &const_env)?;
        let enum_constants = enum_member_constants_from_module_node(
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

        match reject_silently_ignored_constructs(
            node.clone(),
            syntax_tree,
            &const_env,
            &type_aliases,
            &parameter_packed_dimensions(&parameters),
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
                    &parameter_packed_dimensions(&parameters),
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
        let signals =
            signals_from_module_node(node.clone(), syntax_tree, &const_env, &type_aliases)?;
        for r#type in ports
            .iter()
            .map(Port::r#type)
            .chain(signals.iter().map(Signal::r#type))
        {
            validate_unpacked_dimension_sizes(r#type.unpacked_ranges(), &const_env)?;
        }
        if let Some(parameter) = parameters.iter().find(|parameter| {
            ports.iter().any(|port| port.name() == parameter.name())
                || signals
                    .iter()
                    .any(|signal| signal.name() == parameter.name())
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
        packed_dimensions.parameter_values = parameter_value_env(&parameters, &const_env);
        packed_dimensions
            .parameter_values
            .retain(|name, _| !const_env.contains_key(name));
        packed_dimensions.extend(parameter_packed_dimensions(&parameters));
        let mut instances =
            instances_from_module_node(node.clone(), syntax_tree, &const_env, &packed_dimensions)?;
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
        reject_unsupported_multidimensional_packed_bounds(&ports, &signals, &const_env)?;
        let mut parameter_values = parameter_value_env(&parameters, &const_env);
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
        let functions =
            functions_from_module_node(node.clone(), syntax_tree, &const_env, &packed_dimensions)?;
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
        packed_dimensions.functions = Arc::new(functions.clone());
        packed_dimensions.expression_signedness = Arc::new(expression_signedness.clone());
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
        let comb_processes = comb_processes_from_module_node(
            node.clone(),
            syntax_tree,
            &const_env,
            &packed_dimensions,
            &functions,
            &expression_signedness,
            &parameter_values,
        )?
        .into_iter()
        .map(|process| expand_process_calls(process, &functions, &expression_signedness))
        .map(|process| {
            substitute_process_constants_with_parameter_literals(
                process,
                &const_env,
                &parameter_values,
            )
        })
        .collect::<Vec<_>>();
        let ff_processes = ff_processes_from_module_node(
            node.clone(),
            syntax_tree,
            &const_env,
            &parameter_values,
            &packed_dimensions,
        )?
        .into_iter()
        .map(|process| {
            expand_ff_process_calls(
                process,
                &functions,
                &expression_signedness,
                &const_env,
                &parameter_values,
            )
        })
        .collect::<Vec<_>>();
        if let Some(signal) = signals.iter().find(|signal| {
            signal.is_net()
                && !comb_processes.iter().any(|process| {
                    process
                        .assignments()
                        .iter()
                        .any(|assignment| assignment.lhs() == signal.name())
                })
                && !ff_processes.iter().any(|process| {
                    process
                        .assignments()
                        .iter()
                        .any(|assignment| assignment.assignment().lhs() == signal.name())
                })
                && !instances.iter().any(|instance| {
                    instance.port_connections().iter().any(|connection| {
                        connection
                            .actual_expr()
                            .is_some_and(|expr| connection_references_net(expr, signal.name()))
                    })
                })
        }) {
            return Err(AnalyzerError::Unsupported(format!(
                "undriven net declaration `{}`",
                signal.name()
            )));
        }
        let assignments = comb_processes
            .iter()
            .flat_map(|process| process.assignments().iter().cloned())
            .collect();

        Ok(Self {
            name,
            parameters,
            ports,
            signals,
            instances,
            assignments,
            comb_processes,
            ff_processes,
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
}

const MAX_STATIC_PROCEDURAL_LOOP_EXPANSION: usize = 10_000;

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
    ) -> Self {
        Self {
            module_name,
            name,
            parameter_names,
            parameter_overrides,
            condition,
            port_names,
            port_connections,
        }
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
}

impl ParameterOverride {
    fn new(name: String, value: Option<ConstExpr>) -> Self {
        Self { name, value }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn value(&self) -> Option<&ConstExpr> {
        self.value.as_ref()
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
}

impl Type {
    fn implicit() -> Self {
        Self {
            kind: TypeKind::Implicit,
            is_signed: false,
            packed_ranges: Vec::new(),
            unpacked_ranges: Vec::new(),
            members: Vec::new(),
        }
    }

    fn new(kind: TypeKind) -> Self {
        Self {
            kind,
            is_signed: false,
            packed_ranges: Vec::new(),
            unpacked_ranges: Vec::new(),
            members: Vec::new(),
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
}

impl CombProcess {
    fn new(
        kind: CombProcessKind,
        condition: Option<ConstExpr>,
        assignments: Vec<Assignment>,
    ) -> Self {
        Self {
            kind,
            condition,
            assignments,
        }
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
    assignments: Vec<ConditionalAssignment>,
}

impl FfProcess {
    fn new(events: Vec<FfEvent>, assignments: Vec<ConditionalAssignment>) -> Self {
        Self {
            events,
            assignments,
        }
    }

    pub fn events(&self) -> &[FfEvent] {
        &self.events
    }

    pub fn assignments(&self) -> &[ConditionalAssignment] {
        &self.assignments
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
    fn new(condition: Option<Expr>, assignment: Assignment) -> Self {
        Self {
            condition,
            assignment,
            exhaustive_fallback_start: None,
            guard_boundary: None,
            path_epochs: Vec::new(),
        }
    }

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
    body: Expr,
    return_width: Option<usize>,
    return_first_packed_dimension_width: Option<usize>,
    return_signed: bool,
    return_is_2state: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FunctionParam {
    name: String,
    width: Option<usize>,
    signed: bool,
    is_2state: bool,
    packed_dimensions: Vec<PackedDimension>,
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
}

/// Enum member constants collected from module-level `typedef enum`
/// declarations.
#[derive(Default)]
struct EnumMemberConstants {
    /// Evaluated values for the module constant environment.
    numbers: HashMap<String, i128>,
    /// Resolved literal expressions for process-expression substitution.
    exprs: HashMap<String, Expr>,
    /// Base width and signedness retained for early constant substitution.
    types: HashMap<String, ExprType>,
}

#[derive(Clone, Copy)]
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
    variables: VariablePackedDimensions,
    const_env: HashMap<String, i128>,
    type_aliases: HashMap<String, Type>,
    function_return_types: HashMap<String, FunctionReturnMetadata>,
    functions: Arc<HashMap<String, Function>>,
    parameter_values: HashMap<String, Expr>,
    expression_signedness: Arc<HashMap<String, bool>>,
    constant_indexed_base: bool,
}

impl PackedDimensions {
    fn new(
        variables: VariablePackedDimensions,
        const_env: &HashMap<String, i128>,
        type_aliases: &HashMap<String, Type>,
    ) -> Self {
        Self {
            variables,
            const_env: const_env.clone(),
            type_aliases: type_aliases.clone(),
            function_return_types: HashMap::default(),
            functions: Arc::default(),
            parameter_values: HashMap::default(),
            expression_signedness: Arc::default(),
            constant_indexed_base: false,
        }
    }
}

impl Deref for PackedDimensions {
    type Target = VariablePackedDimensions;

    fn deref(&self) -> &Self::Target {
        &self.variables
    }
}

impl DerefMut for PackedDimensions {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.variables
    }
}

const MAX_DYNAMIC_SELECT_EXPANSION: u128 = 4_096;
