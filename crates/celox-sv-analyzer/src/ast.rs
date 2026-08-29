//! SystemVerilog frontend AST.
//!
//! This AST is intentionally smaller and more semantic than the raw
//! `sv-parser` CST, but it is still a language frontend structure rather than
//! Celox runtime IR.

use fxhash::{FxHashMap as HashMap, FxHashSet as HashSet};
use std::ops::{Deref, DerefMut};

use sv_parser::{Locate, RefNode, SyntaxTree, unwrap_node};

use crate::{AnalyzerError, typecheck};

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
        reject_silently_ignored_constructs(node.clone(), syntax_tree, &const_env, &type_aliases)?;
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
        let packed_dimensions =
            packed_dimensions_from_ports_and_signals(&ports, &signals, &const_env, &type_aliases);
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
        for instance in &mut instances {
            for connection in &mut instance.port_connections {
                connection.actual_expr = connection.actual_expr.take().map(|expr| {
                    expand_expr_calls(expr, &functions, &expression_signedness, 0, true)
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
                        matches!(
                            connection.actual_expr(),
                            Some(Expr::Ident(name)) if name == signal.name()
                        )
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

fn module_name_from_node(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
) -> Result<String, AnalyzerError> {
    let id = unwrap_node!(node, ModuleIdentifier)
        .ok_or_else(|| AnalyzerError::Unsupported("module without identifier".to_string()))?;
    let locate = identifier_locate(id)
        .ok_or_else(|| AnalyzerError::Unsupported("unsupported module identifier".to_string()))?;
    syntax_tree
        .get_str(&locate)
        .map(str::to_string)
        .ok_or_else(|| AnalyzerError::Unsupported("invalid module identifier span".to_string()))
}

fn reject_unsupported_multidimensional_packed_bounds(
    ports: &[Port],
    signals: &[Signal],
    const_env: &HashMap<String, i128>,
) -> Result<(), AnalyzerError> {
    let unsupported = ports
        .iter()
        .map(Port::r#type)
        .chain(signals.iter().map(Signal::r#type))
        .any(|r#type| {
            let ranges = r#type.packed_ranges();
            ranges.len() > 1
                && ranges.iter().any(|range| {
                    let left = eval_ast_const_expr(range.left(), const_env);
                    let right = eval_ast_const_expr(range.right(), const_env);
                    !matches!(
                        (left, right),
                        (Some(left), Some(right))
                            if (right == 0 && left >= 0) || (left == 0 && right >= 0)
                    )
                })
        });
    if unsupported {
        Err(AnalyzerError::Unsupported(
            "non-zero-based multidimensional packed range".to_string(),
        ))
    } else {
        Ok(())
    }
}

fn static_for_loop_iterations(
    loop_statement: &sv_parser::LoopStatement,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
) -> Option<(String, Vec<i128>)> {
    let sv_parser::LoopStatement::For(loop_statement) = loop_statement else {
        return None;
    };
    let (initialization, _, condition, _, step) = &loop_statement.nodes.1.nodes.1;
    let (name, initial_value) = for_loop_initialization(initialization.as_ref()?, syntax_tree)
        .and_then(|(name, value)| Some((name, eval_ast_const_expr(&value, const_env)?)))?;
    let initial_value = coerce_for_loop_index_value(initial_value)?;
    let condition = const_expr_from_expr(condition.as_ref()?, syntax_tree)?;
    let steps = step.as_ref()?.nodes.0.contents();
    let [step] = steps.as_slice() else {
        return None;
    };

    let mut values = Vec::new();
    let mut value = initial_value;
    for _ in 0..10_000 {
        let mut loop_env = const_env.clone();
        loop_env.insert(name.clone(), value);
        insert_parameter_type_markers(
            &mut loop_env,
            &name,
            ExprType {
                width: 32,
                signed: true,
            },
        );
        let condition_value = eval_ast_const_expr(&condition, &loop_env)?;
        if condition_value == 0 {
            return Some((name, values));
        }
        values.push(value);
        let next = next_for_loop_value(step, &name, value, syntax_tree, &loop_env);
        value = coerce_for_loop_index_value(next?)?;
    }

    let mut loop_env = const_env.clone();
    loop_env.insert(name.clone(), value);
    insert_parameter_type_markers(
        &mut loop_env,
        &name,
        ExprType {
            width: 32,
            signed: true,
        },
    );
    (eval_ast_const_expr(&condition, &loop_env) == Some(0)).then_some((name, values))
}

fn coerce_for_loop_index_value(value: i128) -> Option<i128> {
    const MODULUS: i128 = 1i128 << 32;
    const SIGN_BIT: i128 = 1i128 << 31;
    let value = value.rem_euclid(MODULUS);
    Some(if value >= SIGN_BIT {
        value - MODULUS
    } else {
        value
    })
}

fn validate_static_for_loops_in_statement_or_null(
    statement: &sv_parser::StatementOrNull,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
) -> Result<(), AnalyzerError> {
    if let sv_parser::StatementOrNull::Statement(statement) = statement {
        validate_static_for_loops_in_statement(statement, syntax_tree, const_env)?;
    }
    Ok(())
}

fn validate_static_for_loops_in_statement(
    statement: &sv_parser::Statement,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
) -> Result<(), AnalyzerError> {
    match &statement.nodes.2 {
        sv_parser::StatementItem::ProceduralTimingControlStatement(timing) => {
            validate_static_for_loops_in_statement_or_null(
                &timing.nodes.1,
                syntax_tree,
                const_env,
            )?;
        }
        sv_parser::StatementItem::SeqBlock(block) => {
            for statement in &block.nodes.3 {
                validate_static_for_loops_in_statement_or_null(statement, syntax_tree, const_env)?;
            }
        }
        sv_parser::StatementItem::ConditionalStatement(conditional) => {
            validate_static_for_loops_in_statement_or_null(
                &conditional.nodes.3,
                syntax_tree,
                const_env,
            )?;
            for (_, _, _, branch) in &conditional.nodes.4 {
                validate_static_for_loops_in_statement_or_null(branch, syntax_tree, const_env)?;
            }
            if let Some((_, branch)) = &conditional.nodes.5 {
                validate_static_for_loops_in_statement_or_null(branch, syntax_tree, const_env)?;
            }
        }
        sv_parser::StatementItem::CaseStatement(case) => {
            let sv_parser::CaseStatement::Normal(case) = &**case else {
                return Ok(());
            };
            for item in std::iter::once(&case.nodes.3).chain(case.nodes.4.iter()) {
                let statement = match item {
                    sv_parser::CaseItem::NonDefault(item) => &item.nodes.2,
                    sv_parser::CaseItem::Default(item) => &item.nodes.2,
                };
                validate_static_for_loops_in_statement_or_null(statement, syntax_tree, const_env)?;
            }
        }
        sv_parser::StatementItem::LoopStatement(loop_statement) => {
            let (name, values) = static_for_loop_iterations(loop_statement, syntax_tree, const_env)
                .ok_or_else(|| {
                    AnalyzerError::Unsupported("procedural loop inside always_ff".to_string())
                })?;
            let sv_parser::LoopStatement::For(loop_statement) = &**loop_statement else {
                unreachable!();
            };
            for value in values {
                let mut loop_env = const_env.clone();
                loop_env.insert(name.clone(), value);
                insert_parameter_type_markers(
                    &mut loop_env,
                    &name,
                    ExprType {
                        width: 32,
                        signed: true,
                    },
                );
                validate_static_for_loops_in_statement_or_null(
                    &loop_statement.nodes.2,
                    syntax_tree,
                    &loop_env,
                )?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn for_loop_initialization(
    initialization: &sv_parser::ForInitialization,
    syntax_tree: &SyntaxTree,
) -> Option<(String, ConstExpr)> {
    match initialization {
        sv_parser::ForInitialization::Declaration(declaration) => {
            let declarations = declaration.nodes.0.contents();
            let [declaration] = declarations.as_slice() else {
                return None;
            };
            if !for_loop_index_type_is_supported(&declaration.nodes.1) {
                return None;
            }
            let assignments = declaration.nodes.2.contents();
            let [assignment] = assignments.as_slice() else {
                return None;
            };
            let name = identifier_text(RefNode::VariableIdentifier(&assignment.0), syntax_tree)?;
            let value = const_expr_from_expr(&assignment.2, syntax_tree)?;
            Some((name, value))
        }
        // An assignment-style initializer targets a variable in the enclosing
        // procedural scope. Its initialization and loop steps are observable
        // assignments, so it cannot be treated as a declaration-scoped
        // compile-time unrolling constant.
        sv_parser::ForInitialization::ListOfVariableAssignments(_) => None,
    }
}

fn for_loop_index_type_is_supported(data_type: &sv_parser::DataType) -> bool {
    matches!(
        data_type,
        sv_parser::DataType::Atom(data_type)
            if matches!(
                data_type.nodes.0,
                sv_parser::IntegerAtomType::Int(_) | sv_parser::IntegerAtomType::Integer(_)
            ) && !matches!(data_type.nodes.1, Some(sv_parser::Signing::Unsigned(_)))
    )
}

fn cast_target_type(
    casting_type: &sv_parser::CastingType,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Option<ExprType> {
    if let Some(r#type) = integer_atom_expr_type(RefNode::CastingType(casting_type)) {
        return Some(r#type);
    }
    match casting_type {
        sv_parser::CastingType::SimpleType(simple_type) => {
            let sv_parser::SimpleType::PsTypeIdentifier(identifier) = simple_type.as_ref() else {
                return None;
            };
            let name = identifier_text(RefNode::TypeIdentifier(&identifier.nodes.1), syntax_tree)?;
            if let Some(r#type) = type_aliases.get(&name) {
                expr_type_from_type(r#type, const_env)
            } else {
                Some(ExprType {
                    width: usize::try_from(*const_env.get(&name)?).ok()?.max(1),
                    signed: false,
                })
            }
        }
        sv_parser::CastingType::ConstantPrimary(primary) => {
            if let Some(r#type) =
                size_system_function_expr_type(primary, syntax_tree, const_env, type_aliases)
            {
                return Some(r#type);
            }
            let target = const_expr_from_ref_node(RefNode::ConstantPrimary(primary), syntax_tree)?;
            if let ConstExpr::Ident(name) = &target {
                if let Some(r#type) = type_aliases.get(name) {
                    return expr_type_from_type(r#type, const_env);
                }
            }
            let width = eval_ast_const_expr(&target, const_env)?;
            Some(ExprType {
                width: usize::try_from(width).ok()?.max(1),
                signed: false,
            })
        }
        _ => None,
    }
}

fn size_system_function_expr_type(
    primary: &sv_parser::ConstantPrimary,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Option<ExprType> {
    let sv_parser::ConstantPrimary::ConstantFunctionCall(call) = primary else {
        return None;
    };
    let sv_parser::SubroutineCall::SystemTfCall(system_call) = &call.nodes.0.nodes.0 else {
        return None;
    };
    let (name, r#type) = match &**system_call {
        sv_parser::SystemTfCall::ArgDataType(call) => {
            let name = syntax_tree.get_str(&call.nodes.0.nodes.0)?;
            let data_type = &call.nodes.1.nodes.1.0;
            let r#type = match data_type {
                sv_parser::DataType::Type(data_type) => {
                    let name =
                        identifier_text(RefNode::TypeIdentifier(&data_type.nodes.1), syntax_tree)?;
                    type_aliases.get(&name).cloned()
                }
                _ => type_from_ref_node(RefNode::DataType(data_type), syntax_tree),
            }?;
            (name, r#type)
        }
        // sv-parser classifies an unqualified typedef argument as an
        // expression because its grammar cannot know whether the identifier
        // names a type. Resolve that ambiguity from the module alias table.
        sv_parser::SystemTfCall::ArgExpression(call) => {
            let name = syntax_tree.get_str(&call.nodes.0.nodes.0)?;
            let arguments = call.nodes.1.nodes.1.0.contents();
            if arguments.len() != 1 {
                return None;
            }
            let argument = arguments[0].as_ref()?;
            let ConstExpr::Ident(alias) = const_expr_from_expr(argument, syntax_tree)? else {
                return None;
            };
            (name, type_aliases.get(&alias)?.clone())
        }
        sv_parser::SystemTfCall::ArgOptionl(_) => return None,
    };
    // $bits covers every unpacked and packed dimension; $size covers only
    // the outermost dimension, which is unpacked when one is present.
    let first_dimension_only = name == "$size";
    if name != "$bits" && !first_dimension_only {
        return None;
    }
    if !first_dimension_only {
        let unpacked_width = r#type
            .unpacked_ranges()
            .iter()
            .try_fold(1usize, |width, range| {
                let left = eval_ast_const_expr(range.left(), const_env)?;
                let right = eval_ast_const_expr(range.right(), const_env)?;
                width.checked_mul(usize::try_from(left.abs_diff(right)).ok()?.checked_add(1)?)
            })?;
        let packed_width = r#type
            .packed_ranges()
            .iter()
            .try_fold(1usize, |width, range| {
                let left = eval_ast_const_expr(range.left(), const_env)?;
                let right = eval_ast_const_expr(range.right(), const_env)?;
                width.checked_mul(usize::try_from(left.abs_diff(right)).ok()?.checked_add(1)?)
            })?;
        return Some(ExprType {
            width: unpacked_width.checked_mul(packed_width)?.max(1),
            signed: r#type.is_signed(),
        });
    }
    let range = r#type
        .unpacked_ranges()
        .first()
        .map(|range| (range.left(), range.right()))
        .or_else(|| {
            r#type
                .packed_ranges()
                .first()
                .map(|range| (range.left(), range.right()))
        });
    let Some((left, right)) = range else {
        return Some(ExprType {
            width: 1,
            signed: r#type.is_signed(),
        });
    };
    let left = eval_ast_const_expr(left, const_env)?;
    let right = eval_ast_const_expr(right, const_env)?;
    let width = usize::try_from(left.abs_diff(right)).ok()?.checked_add(1)?;
    Some(ExprType {
        width: width.max(1),
        signed: r#type.is_signed(),
    })
}

fn expr_type_from_type(r#type: &Type, const_env: &HashMap<String, i128>) -> Option<ExprType> {
    if !r#type.unpacked_ranges().is_empty() {
        return None;
    }
    let width = r#type
        .packed_ranges()
        .iter()
        .try_fold(1usize, |width, range| {
            let left = eval_ast_const_expr(range.left(), const_env)?;
            let right = eval_ast_const_expr(range.right(), const_env)?;
            width.checked_mul(usize::try_from(left.abs_diff(right)).ok()?.checked_add(1)?)
        })?;
    Some(ExprType {
        width: width.max(1),
        signed: r#type.is_signed(),
    })
}

fn cast_is_supported(
    cast: &sv_parser::Cast,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> bool {
    cast_zero_type(cast, syntax_tree, const_env, type_aliases).is_some()
}

fn constant_cast_is_supported(
    cast: &sv_parser::ConstantCast,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> bool {
    constant_cast_const_expr(cast, syntax_tree, const_env, type_aliases).is_some()
}

fn cast_zero_type(
    cast: &sv_parser::Cast,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Option<ExprType> {
    let ConstExpr::Literal(literal) = const_expr_from_expr(&cast.nodes.2.nodes.1, syntax_tree)?
    else {
        return None;
    };
    let literal = typecheck::parse_integral_literal(&literal)?;
    if literal.value != 0u8.into() || literal.mask != 0u8.into() {
        return None;
    }
    let target_type = cast_target_type(&cast.nodes.0, syntax_tree, const_env, type_aliases)?;
    let signed =
        if casting_type_is_numeric_size(&cast.nodes.0, syntax_tree, const_env, type_aliases) {
            literal.signed
        } else {
            target_type.signed
        };
    Some(ExprType {
        signed,
        ..target_type
    })
}

fn constant_cast_const_expr(
    cast: &sv_parser::ConstantCast,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Option<ConstExpr> {
    let operand = const_expr_from_ref_node_with_env(
        RefNode::ConstantExpression(&cast.nodes.2.nodes.1),
        syntax_tree,
        const_env,
        type_aliases,
    )?;
    let operand_type = infer_const_expr_type(&operand, &parameter_types_from_const_env(const_env))?;
    let literal = if let ConstExpr::Literal(literal) = &operand {
        typecheck::parse_integral_literal(literal)?
    } else {
        let operand_value = eval_ast_const_expr(&operand, const_env)?;
        let operand_literal =
            format_typed_parameter_literal(operand_value, operand_type.width, operand_type.signed);
        typecheck::parse_integral_literal(&operand_literal)?
    };
    let target_type = cast_target_type(&cast.nodes.0, syntax_tree, const_env, type_aliases)?;
    // A numeric size cast keeps the source expression's signedness; a type
    // cast takes the target type's signedness.
    let signed =
        if casting_type_is_numeric_size(&cast.nodes.0, syntax_tree, const_env, type_aliases) {
            literal.signed
        } else {
            target_type.signed
        };
    let resized = match &operand {
        ConstExpr::Literal(value) => {
            resize_unbased_fill_literal_for_cast(value, target_type.width, signed).unwrap_or_else(
                || resize_integral_literal_for_cast(literal, target_type.width, signed),
            )
        }
        _ => resize_integral_literal_for_cast(literal, target_type.width, signed),
    };
    let resized = if cast_target_is_two_state(&cast.nodes.0, syntax_tree, const_env, type_aliases) {
        two_state_integral_literal(&resized, target_type.width, signed)?
    } else {
        resized
    };
    Some(ConstExpr::Literal(resized))
}

fn cast_target_is_two_state(
    casting_type: &sv_parser::CastingType,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> bool {
    if type_from_ref_node_with_env(
        RefNode::CastingType(casting_type),
        syntax_tree,
        const_env,
        type_aliases,
    )
    .is_some_and(|r#type| r#type.kind() == TypeKind::Bit)
    {
        return true;
    }
    match casting_type {
        sv_parser::CastingType::SimpleType(simple_type) => {
            let sv_parser::SimpleType::PsTypeIdentifier(identifier) = simple_type.as_ref() else {
                return false;
            };
            identifier_text(RefNode::TypeIdentifier(&identifier.nodes.1), syntax_tree)
                .and_then(|name| type_aliases.get(&name))
                .is_some_and(|r#type| r#type.kind() == TypeKind::Bit)
        }
        sv_parser::CastingType::ConstantPrimary(primary) => {
            let Some(ConstExpr::Ident(name)) =
                const_expr_from_ref_node(RefNode::ConstantPrimary(primary), syntax_tree)
            else {
                return false;
            };
            type_aliases
                .get(&name)
                .is_some_and(|r#type| r#type.kind() == TypeKind::Bit)
        }
        _ => false,
    }
}

fn two_state_integral_literal(value: &str, width: usize, signed: bool) -> Option<String> {
    let mut literal = typecheck::parse_integral_literal(value)?;
    let keep = (num_bigint::BigUint::from(1usize) << width) - num_bigint::BigUint::from(1usize);
    literal.value &= &keep ^ &literal.mask;
    literal.mask = num_bigint::BigUint::default();
    Some(resize_integral_literal_for_cast(literal, width, signed))
}

fn casting_type_is_numeric_size(
    casting_type: &sv_parser::CastingType,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> bool {
    match casting_type {
        sv_parser::CastingType::ConstantPrimary(_) => true,
        sv_parser::CastingType::SimpleType(simple_type) => {
            let sv_parser::SimpleType::PsTypeIdentifier(identifier) = simple_type.as_ref() else {
                return false;
            };
            let Some(name) =
                identifier_text(RefNode::TypeIdentifier(&identifier.nodes.1), syntax_tree)
            else {
                return false;
            };
            !type_aliases.contains_key(&name) && const_env.contains_key(&name)
        }
        _ => false,
    }
}

fn resize_unbased_fill_literal_for_cast(value: &str, width: usize, signed: bool) -> Option<String> {
    let normalized = value.trim().to_ascii_lowercase();
    let mut chars = normalized.chars();
    (chars.next()? == '\'' && chars.clone().count() == 1).then_some(())?;
    let fill = chars.next()?;
    matches!(fill, '0' | '1' | 'x' | 'z' | '?').then_some(())?;
    let signing = if signed { "s" } else { "" };
    Some(format!(
        "{width}'{signing}b{}",
        fill.to_string().repeat(width)
    ))
}

fn resize_integral_literal_for_cast(
    literal: typecheck::IntegralLiteral,
    width: usize,
    signed: bool,
) -> String {
    let mut value = literal.value;
    let mut mask = literal.mask;
    if literal.signed && literal.width > 0 && literal.width < width {
        let extension = ((num_bigint::BigUint::from(1usize) << (width - literal.width))
            - num_bigint::BigUint::from(1usize))
            << literal.width;
        if value.bit((literal.width - 1) as u64) {
            value |= &extension;
        }
        if mask.bit((literal.width - 1) as u64) {
            mask |= extension;
        }
    }
    let keep = (num_bigint::BigUint::from(1usize) << width) - num_bigint::BigUint::from(1usize);
    value &= &keep;
    mask &= keep;
    let signing = if signed { "s" } else { "" };
    if mask == num_bigint::BigUint::default() {
        return format!("{width}'{signing}d{value}");
    }
    let bits = (0..width)
        .rev()
        .map(|bit| {
            if mask.bit(bit as u64) {
                if value.bit(bit as u64) { 'x' } else { 'z' }
            } else if value.bit(bit as u64) {
                '1'
            } else {
                '0'
            }
        })
        .collect::<String>();
    format!("{width}'{signing}b{bits}")
}

fn for_loop_variable_lvalue_name(
    lvalue: &sv_parser::VariableLvalue,
    syntax_tree: &SyntaxTree,
) -> Option<String> {
    let sv_parser::VariableLvalue::Identifier(identifier) = lvalue else {
        return None;
    };
    identifier_text(
        RefNode::HierarchicalVariableIdentifier(&identifier.nodes.1),
        syntax_tree,
    )
}

fn next_for_loop_value(
    step: &sv_parser::ForStepAssignment,
    name: &str,
    value: i128,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
) -> Option<i128> {
    match step {
        sv_parser::ForStepAssignment::OperatorAssignment(step) => {
            if for_loop_variable_lvalue_name(&step.nodes.0, syntax_tree)? != name {
                return None;
            }
            let operator = syntax_tree.get_str(&step.nodes.1.nodes.0)?.trim();
            let rhs = const_expr_from_expr(&step.nodes.2, syntax_tree)?;
            if operator == "=" {
                return eval_ast_const_expr(&rhs, const_env);
            }
            let op = match operator {
                "+=" => BinaryOp::Add,
                "-=" => BinaryOp::Sub,
                "*=" => BinaryOp::Mul,
                "/=" => BinaryOp::Div,
                "%=" => BinaryOp::Mod,
                _ => return None,
            };
            let lhs = ConstExpr::Literal(format_typed_parameter_literal(value, 32, true));
            let expression = ConstExpr::Binary {
                left: Box::new(lhs),
                op,
                right: Box::new(rhs),
            };
            eval_ast_const_expr(&expression, const_env)
        }
        sv_parser::ForStepAssignment::IncOrDecExpression(step) => {
            let (lvalue, operator) = match &**step {
                sv_parser::IncOrDecExpression::Prefix(step) => (&step.nodes.2, &step.nodes.0),
                sv_parser::IncOrDecExpression::Suffix(step) => (&step.nodes.0, &step.nodes.2),
            };
            if for_loop_variable_lvalue_name(lvalue, syntax_tree)? != name {
                return None;
            }
            match syntax_tree.get_str(&operator.nodes.0)? {
                "++" => value.checked_add(1),
                "--" => value.checked_sub(1),
                _ => None,
            }
        }
        sv_parser::ForStepAssignment::FunctionSubroutineCall(_) => None,
    }
}

fn reject_silently_ignored_constructs(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Result<(), AnalyzerError> {
    let inactive_nodes = inactive_conditional_generate_nodes(node.clone(), syntax_tree, const_env);
    let has_leaking_conditional_generate_local =
        conditional_generate_has_leaking_local(node.clone(), syntax_tree);
    reject_duplicate_conditional_generate_locals(node.clone(), syntax_tree)?;
    for child in node {
        if inactive_nodes.iter().any(|inactive| inactive == &child) {
            continue;
        }
        match child {
            RefNode::Cast(cast)
                if !cast_is_supported(cast, syntax_tree, const_env, type_aliases) =>
            {
                return Err(AnalyzerError::Unsupported("cast expression".to_string()));
            }
            RefNode::ConstantCast(cast)
                if !constant_cast_is_supported(cast, syntax_tree, const_env, type_aliases) =>
            {
                return Err(AnalyzerError::Unsupported("constant cast expression".to_string()));
            }
            RefNode::AnsiPortDeclarationNet(port) if port.nodes.3.is_some() => {
                return Err(AnalyzerError::Unsupported(
                    "ANSI port default value".to_string(),
                ));
            }
            RefNode::AnsiPortDeclarationVariable(port) if port.nodes.3.is_some() => {
                return Err(AnalyzerError::Unsupported(
                    "ANSI port default value".to_string(),
                ));
            }
            RefNode::AnsiPortDeclarationVariable(port)
                if RefNode::AnsiPortDeclarationVariable(port)
                    .into_iter()
                    .any(|node| matches!(node, RefNode::DataTypeEnum(_))) =>
            {
                return Err(AnalyzerError::Unsupported("enum port".to_string()));
            }
            RefNode::AnsiPortDeclarationNet(port)
                if RefNode::AnsiPortDeclarationNet(port)
                    .into_iter()
                    .any(|node| matches!(node, RefNode::DataTypeEnum(_))) =>
            {
                return Err(AnalyzerError::Unsupported("enum port".to_string()));
            }
            RefNode::AlwaysConstruct(always) => {
                if matches!(
                    always.nodes.0,
                    sv_parser::AlwaysKeyword::Always(_) | sv_parser::AlwaysKeyword::AlwaysLatch(_)
                ) {
                    return Err(AnalyzerError::Unsupported(
                        "always and always_latch processes".to_string(),
                    ));
                }
                let body = RefNode::Statement(&always.nodes.1);
                if matches!(always.nodes.0, sv_parser::AlwaysKeyword::AlwaysFf(_))
                    && body
                        .clone()
                        .into_iter()
                        .any(|node| matches!(node, RefNode::BlockingAssignment(_)))
                {
                    return Err(AnalyzerError::Unsupported(
                        "blocking assignment inside always_ff".to_string(),
                    ));
                }
                if matches!(always.nodes.0, sv_parser::AlwaysKeyword::AlwaysFf(_)) {
                    validate_static_for_loops_in_statement(&always.nodes.1, syntax_tree, const_env)?;
                }
                if matches!(always.nodes.0, sv_parser::AlwaysKeyword::AlwaysFf(_))
                    && body.clone().into_iter().any(|node| {
                        matches!(
                            node,
                            RefNode::CaseStatement(case)
                                if !matches!(
                                    case,
                                    sv_parser::CaseStatement::Normal(case)
                                        if matches!(case.nodes.1, sv_parser::CaseKeyword::Case(_))
                                )
                        )
                    })
                {
                    return Err(AnalyzerError::Unsupported(
                        "casez, casex, or pattern case inside always_ff".to_string(),
                    ));
                }
                if matches!(always.nodes.0, sv_parser::AlwaysKeyword::AlwaysComb(_))
                    && body
                        .clone()
                        .into_iter()
                        .any(|node| matches!(node, RefNode::LoopStatement(_)))
                {
                    return Err(AnalyzerError::Unsupported(
                        "procedural loop inside always_comb".to_string(),
                    ));
                }
                if matches!(always.nodes.0, sv_parser::AlwaysKeyword::AlwaysComb(_))
                    && body
                        .clone()
                        .into_iter()
                        .any(|node| matches!(node, RefNode::NonblockingAssignment(_)))
                {
                    return Err(AnalyzerError::Unsupported(
                        "nonblocking assignment inside always_comb".to_string(),
                    ));
                }
                if matches!(always.nodes.0, sv_parser::AlwaysKeyword::AlwaysFf(_))
                    && body.clone().into_iter().any(|node| {
                        matches!(
                            node,
                            RefNode::EventExpressionExpression(event)
                                if event.nodes.2.is_some()
                        )
                    })
                {
                    return Err(AnalyzerError::Unsupported(
                        "iff-qualified always_ff event".to_string(),
                    ));
                }
                if matches!(always.nodes.0, sv_parser::AlwaysKeyword::AlwaysFf(_))
                    && body
                        .clone()
                        .into_iter()
                        .any(|node| matches!(node, RefNode::VariableLvalueLvalue(_)))
                {
                    return Err(AnalyzerError::Unsupported(
                        "concatenated always_ff assignment target".to_string(),
                    ));
                }
                if body
                    .into_iter()
                    .any(|node| matches!(node, RefNode::DataDeclaration(_)))
                {
                    let detail = if matches!(
                        always.nodes.0,
                        sv_parser::AlwaysKeyword::AlwaysComb(_)
                    ) {
                        "block-local declaration inside always_comb"
                    } else {
                        "procedural local data declaration"
                    };
                    return Err(AnalyzerError::Unsupported(detail.to_string()));
                }
            }
            RefNode::NetDeclAssignment(assignment) if assignment.nodes.2.is_some() => {
                return Err(AnalyzerError::Unsupported(
                    "net declaration assignment".to_string(),
                ));
            }
            RefNode::LoopGenerateConstruct(generate) => {
                if RefNode::LoopGenerateConstruct(generate)
                    .into_iter()
                    .any(|node| matches!(node, RefNode::ModuleInstantiation(_)))
                {
                    return Err(AnalyzerError::Unsupported(
                        "module instantiation inside loop-generate".to_string(),
                    ));
                }
                if generate_block_has_data_declaration(&generate.nodes.2) {
                    return Err(AnalyzerError::Unsupported(
                        "local data declaration inside loop-generate".to_string(),
                    ));
                }
                if RefNode::LoopGenerateConstruct(generate)
                    .into_iter()
                    .any(|node| {
                        matches!(
                            node,
                            RefNode::AlwaysConstruct(always)
                                if matches!(always.nodes.0, sv_parser::AlwaysKeyword::AlwaysFf(_))
                        )
                    })
                {
                    return Err(AnalyzerError::Unsupported(
                        "always_ff inside loop-generate".to_string(),
                    ));
                }
            }
            RefNode::InitialConstruct(_) => {
                return Err(AnalyzerError::Unsupported("initial construct".to_string()));
            }
            RefNode::FinalConstruct(_) => {
                return Err(AnalyzerError::Unsupported("final construct".to_string()));
            }
            RefNode::ElaborationSystemTask(_) => {
                return Err(AnalyzerError::Unsupported(
                    "elaboration system task".to_string(),
                ));
            }
            RefNode::BindDirective(_) => {
                return Err(AnalyzerError::Unsupported("bind directive".to_string()));
            }
            RefNode::SpecifyBlock(_) => {
                return Err(AnalyzerError::Unsupported("specify block".to_string()));
            }
            RefNode::MintypmaxExpressionTernary(_)
            | RefNode::ConstantMintypmaxExpressionTernary(_) => {
                return Err(AnalyzerError::Unsupported(
                    "mintypmax expression".to_string(),
                ));
            }
            RefNode::ConcurrentAssertionItem(_) => {
                return Err(AnalyzerError::Unsupported(
                    "concurrent assertion".to_string(),
                ));
            }
            RefNode::ProceduralAssertionStatement(_) => {
                return Err(AnalyzerError::Unsupported(
                    "procedural assertion statement".to_string(),
                ));
            }
            RefNode::ConditionalGenerateConstruct(
                sv_parser::ConditionalGenerateConstruct::Case(_),
            ) => {
                return Err(AnalyzerError::Unsupported(
                    "case-generate construct".to_string(),
                ));
            }
            RefNode::ConditionalGenerateConstruct(
                sv_parser::ConditionalGenerateConstruct::If(generate),
            ) if generate_block_has_type_declaration(&generate.nodes.2)
                || generate.nodes.3.as_ref().is_some_and(|(_, block)| {
                    generate_block_has_type_declaration(block)
                }) =>
            {
                return Err(AnalyzerError::Unsupported(
                    "type declaration inside conditional-generate".to_string(),
                ));
            }
            RefNode::ConditionalGenerateConstruct(sv_parser::ConditionalGenerateConstruct::If(
                generate,
            )) if (generate_block_has_data_declaration(&generate.nodes.2)
                && generate
                    .nodes
                    .3
                    .as_ref()
                    .is_some_and(|(_, block)| generate_block_has_data_declaration(block)))
                || has_leaking_conditional_generate_local =>
            {
                return Err(AnalyzerError::Unsupported(
                    "local data declaration inside conditional-generate".to_string(),
                ));
            }
            RefNode::VariableDeclAssignmentVariable(assignment) if assignment.nodes.2.is_some() => {
                return Err(AnalyzerError::Unsupported(
                    "variable declaration initializer".to_string(),
                ));
            }
            RefNode::IndexedRange(_) | RefNode::ConstantIndexedRange(_) => {
                return Err(AnalyzerError::Unsupported(
                    "indexed part-select".to_string(),
                ));
            }
            RefNode::DataTypeStructUnion(_) => {
                return Err(AnalyzerError::Unsupported(
                    "packed struct or union type".to_string(),
                ));
            }
            RefNode::ConstantFunctionCall(call)
                if matches!(
                    &call.nodes.0.nodes.0,
                    sv_parser::SubroutineCall::TfCall(call) if call.nodes.2.is_some()
                ) =>
            {
                return Err(AnalyzerError::Unsupported(
                    "user constant function call".to_string(),
                ));
            }
            RefNode::NamedPortConnectionAsterisk(_) => {
                return Err(AnalyzerError::Unsupported(
                    "wildcard port connection".to_string(),
                ));
            }
            RefNode::ContinuousAssign(sv_parser::ContinuousAssign::Net(assign))
                if assign.nodes.2.is_some() =>
            {
                return Err(AnalyzerError::Unsupported(
                    "delayed continuous assignment".to_string(),
                ));
            }
            RefNode::ContinuousAssign(sv_parser::ContinuousAssign::Variable(assign))
                if assign.nodes.1.is_some() =>
            {
                return Err(AnalyzerError::Unsupported(
                    "delayed continuous assignment".to_string(),
                ));
            }
            RefNode::FunctionDeclaration(function)
                if function_has_static_local_state(function) =>
            {
                return Err(AnalyzerError::Unsupported(
                    "static function-local state".to_string(),
                ));
            }
            RefNode::FunctionDeclaration(function)
                if RefNode::FunctionDeclaration(function)
                    .into_iter()
                    .any(|node| {
                        matches!(
                            node,
                            RefNode::ConditionalStatement(statement)
                                if statement.nodes.5.is_none()
                                    && RefNode::ConditionalStatement(statement)
                                        .into_iter()
                                        .any(|node| matches!(node, RefNode::JumpStatement(
                                            sv_parser::JumpStatement::Return(_)
                                        )))
                        )
                    }) =>
            {
                return Err(AnalyzerError::Unsupported(
                    "conditional function return without else".to_string(),
                ));
            }
            RefNode::FunctionDeclaration(function)
                if RefNode::FunctionDeclaration(function)
                    .into_iter()
                    .any(non_input_function_port) =>
            {
                return Err(AnalyzerError::Unsupported(
                    "output or inout function argument".to_string(),
                ));
            }
            RefNode::FunctionDeclaration(function)
                if RefNode::FunctionDeclaration(function).into_iter().any(|node| {
                    matches!(
                        node,
                        RefNode::CaseStatement(case)
                            if !matches!(
                                case,
                                sv_parser::CaseStatement::Normal(case)
                                    if matches!(case.nodes.1, sv_parser::CaseKeyword::Case(_))
                            )
                    )
                }) =>
            {
                return Err(AnalyzerError::Unsupported(
                    "casez or casex inside function".to_string(),
                ));
            }
            RefNode::FunctionDeclaration(function)
                if RefNode::FunctionDeclaration(function)
                    .into_iter()
                    .any(|node| matches!(node, RefNode::BlockingAssignment(assignment) if blocking_assignment_has_non_plain_lvalue(assignment))) =>
            {
                return Err(AnalyzerError::Unsupported(
                    "selected or composite assignment inside function".to_string(),
                ));
            }
            RefNode::GateInstantiation(_) => {
                return Err(AnalyzerError::Unsupported(
                    "gate primitive instantiation".to_string(),
                ));
            }
            RefNode::DefparamAssignment(_) => {
                return Err(AnalyzerError::Unsupported(
                    "defparam assignment".to_string(),
                ));
            }
            RefNode::NetType(
                sv_parser::NetType::Supply0(_)
                | sv_parser::NetType::Supply1(_)
                | sv_parser::NetType::Tri0(_)
                | sv_parser::NetType::Tri1(_),
            ) => {
                return Err(AnalyzerError::Unsupported(
                    "pull or supply net type".to_string(),
                ));
            }
            RefNode::NetType(sv_parser::NetType::Trireg(_)) => {
                return Err(AnalyzerError::Unsupported(
                    "trireg charge storage".to_string(),
                ));
            }
            RefNode::PackedDimensionRange(range)
                if const_expr_from_ref_node_with_env(
                    RefNode::ConstantExpression(&range.nodes.0.nodes.1.nodes.0),
                    syntax_tree,
                    const_env,
                    type_aliases,
                )
                .is_none()
                    || const_expr_from_ref_node_with_env(
                        RefNode::ConstantExpression(&range.nodes.0.nodes.1.nodes.2),
                        syntax_tree,
                        const_env,
                        type_aliases,
                    )
                    .is_none() =>
            {
                return Err(AnalyzerError::Unsupported(
                    "unsupported packed range".to_string(),
                ));
            }
            RefNode::PackageImportDeclaration(_) | RefNode::PackageScope(_) => {
                return Err(AnalyzerError::Unsupported(
                    "package-dependent systemverilog module".to_string(),
                ));
            }
            RefNode::ParamAssignment(parameter)
                if RefNode::ParamAssignment(parameter).into_iter().any(|node| {
                    matches!(
                        node,
                        RefNode::ConstantExpression(
                            sv_parser::ConstantExpression::Unary(unary)
                        ) if syntax_tree
                            .get_str(&unary.nodes.0.nodes.0.nodes.0)
                            .is_some_and(|op| op == "~")
                            && matches!(
                                const_expr_from_ref_node(
                                    RefNode::ConstantPrimary(&unary.nodes.2),
                                    syntax_tree,
                                ),
                                Some(ConstExpr::Ident(_))
                            )
                    )
                }) =>
            {
                return Err(AnalyzerError::Unsupported(
                    "width-dependent complement in parameter expression".to_string(),
                ));
            }
            RefNode::ParamAssignment(parameter)
                if RefNode::ParamAssignment(parameter).into_iter().any(|node| {
                    matches!(
                        node,
                        RefNode::UnaryOperator(operator)
                            if syntax_tree
                                .get_str(&operator.nodes.0)
                                .is_some_and(|op| matches!(op, "&" | "|" | "^" | "~&" | "~|" | "~^" | "^~"))
                    )
                }) =>
            {
                return Err(AnalyzerError::Unsupported(
                    "reduction operator in parameter expression".to_string(),
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

fn inactive_conditional_generate_nodes<'a>(
    node: RefNode<'a>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
) -> Vec<RefNode<'a>> {
    let mut selections = Vec::new();
    for item in module_non_port_items(node) {
        generate_selections_from_non_port_item(item, syntax_tree, const_env, &mut selections);
    }
    selections
        .into_iter()
        .filter(|(_, selected)| !selected)
        .flat_map(|(block, _)| RefNode::GenerateBlock(block))
        .collect()
}

fn record_generate_block_selection<'a>(
    selections: &mut Vec<(&'a sv_parser::GenerateBlock, bool)>,
    block: &'a sv_parser::GenerateBlock,
    selected: bool,
) {
    if let Some((_, previously_selected)) = selections
        .iter_mut()
        .find(|(candidate, _)| *candidate == block)
    {
        *previously_selected |= selected;
    } else {
        selections.push((block, selected));
    }
}

fn generate_selections_from_non_port_item<'a>(
    item: &'a sv_parser::NonPortModuleItem,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    selections: &mut Vec<(&'a sv_parser::GenerateBlock, bool)>,
) {
    match item {
        sv_parser::NonPortModuleItem::GenerateRegion(region) => {
            for item in &region.nodes.1 {
                generate_selections_from_generate_item(item, syntax_tree, const_env, selections);
            }
        }
        sv_parser::NonPortModuleItem::ModuleOrGenerateItem(item) => {
            generate_selections_from_module_or_generate_item(
                item,
                syntax_tree,
                const_env,
                selections,
            );
        }
        _ => {}
    }
}

fn generate_selections_from_generate_item<'a>(
    item: &'a sv_parser::GenerateItem,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    selections: &mut Vec<(&'a sv_parser::GenerateBlock, bool)>,
) {
    if let sv_parser::GenerateItem::ModuleOrGenerateItem(item) = item {
        generate_selections_from_module_or_generate_item(item, syntax_tree, const_env, selections);
    }
}

fn generate_selections_from_module_or_generate_item<'a>(
    item: &'a sv_parser::ModuleOrGenerateItem,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    selections: &mut Vec<(&'a sv_parser::GenerateBlock, bool)>,
) {
    let sv_parser::ModuleOrGenerateItem::ModuleItem(item) = item else {
        return;
    };
    match &item.nodes.1 {
        sv_parser::ModuleCommonItem::ConditionalGenerateConstruct(generate) => {
            generate_selections_from_conditional_generate(
                generate,
                syntax_tree,
                const_env,
                selections,
            );
        }
        sv_parser::ModuleCommonItem::LoopGenerateConstruct(generate) => {
            generate_selections_from_loop_generate(generate, syntax_tree, const_env, selections);
        }
        _ => {}
    }
}

fn generate_selections_from_conditional_generate<'a>(
    generate: &'a sv_parser::ConditionalGenerateConstruct,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    selections: &mut Vec<(&'a sv_parser::GenerateBlock, bool)>,
) {
    let sv_parser::ConditionalGenerateConstruct::If(generate) = generate else {
        return;
    };
    let condition = const_expr_from_ref_node(
        RefNode::ConstantExpression(&generate.nodes.1.nodes.1),
        syntax_tree,
    )
    .and_then(|condition| eval_ast_const_expr(&condition, const_env));
    let then_selected = condition.map(|condition| condition != 0).unwrap_or(true);
    record_generate_block_selection(selections, &generate.nodes.2, then_selected);
    if then_selected {
        generate_selections_from_generate_block(
            &generate.nodes.2,
            syntax_tree,
            const_env,
            selections,
        );
    }
    if let Some((_, block)) = &generate.nodes.3 {
        let else_selected = condition.map(|condition| condition == 0).unwrap_or(true);
        record_generate_block_selection(selections, block, else_selected);
        if else_selected {
            generate_selections_from_generate_block(block, syntax_tree, const_env, selections);
        }
    }
}

fn generate_selections_from_loop_generate<'a>(
    generate: &'a sv_parser::LoopGenerateConstruct,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    selections: &mut Vec<(&'a sv_parser::GenerateBlock, bool)>,
) {
    let Some(name) = identifier_text(
        RefNode::GenvarIdentifier(&generate.nodes.1.nodes.1.0.nodes.1),
        syntax_tree,
    ) else {
        record_generate_block_selection(selections, &generate.nodes.2, true);
        generate_selections_from_generate_block(
            &generate.nodes.2,
            syntax_tree,
            const_env,
            selections,
        );
        return;
    };
    let Some(init) = const_expr_from_ref_node(
        RefNode::ConstantExpression(&generate.nodes.1.nodes.1.0.nodes.3),
        syntax_tree,
    )
    .and_then(|init| eval_ast_const_expr(&init, const_env)) else {
        record_generate_block_selection(selections, &generate.nodes.2, true);
        generate_selections_from_generate_block(
            &generate.nodes.2,
            syntax_tree,
            const_env,
            selections,
        );
        return;
    };
    let Some(condition) = const_expr_from_ref_node(
        RefNode::ConstantExpression(&generate.nodes.1.nodes.1.2.nodes.0),
        syntax_tree,
    ) else {
        record_generate_block_selection(selections, &generate.nodes.2, true);
        generate_selections_from_generate_block(
            &generate.nodes.2,
            syntax_tree,
            const_env,
            selections,
        );
        return;
    };

    let mut value = init;
    let mut selected = false;
    for _ in 0..10_000 {
        let mut loop_env = const_env.clone();
        loop_env.insert(name.clone(), value);
        let Some(condition_value) = eval_ast_const_expr(&condition, &loop_env) else {
            record_generate_block_selection(selections, &generate.nodes.2, true);
            generate_selections_from_generate_block(
                &generate.nodes.2,
                syntax_tree,
                &loop_env,
                selections,
            );
            return;
        };
        if condition_value == 0 {
            break;
        }
        selected = true;
        generate_selections_from_generate_block(
            &generate.nodes.2,
            syntax_tree,
            &loop_env,
            selections,
        );
        let Some(next) =
            next_genvar_value(value, &generate.nodes.1.nodes.1.4, syntax_tree, &loop_env)
        else {
            break;
        };
        value = next;
    }
    record_generate_block_selection(selections, &generate.nodes.2, selected);
}

fn generate_selections_from_generate_block<'a>(
    block: &'a sv_parser::GenerateBlock,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    selections: &mut Vec<(&'a sv_parser::GenerateBlock, bool)>,
) {
    match block {
        sv_parser::GenerateBlock::GenerateItem(item) => {
            generate_selections_from_generate_item(item, syntax_tree, const_env, selections);
        }
        sv_parser::GenerateBlock::Multiple(block) => {
            let mut block_env = const_env.clone();
            for item in &block.nodes.3 {
                if add_localparams_from_generate_item(item, syntax_tree, &mut block_env) {
                    continue;
                }
                generate_selections_from_generate_item(item, syntax_tree, &block_env, selections);
            }
        }
    }
}

fn blocking_assignment_has_non_plain_lvalue(assignment: &sv_parser::BlockingAssignment) -> bool {
    let lvalue = match assignment {
        sv_parser::BlockingAssignment::Variable(assignment) => &assignment.nodes.0,
        sv_parser::BlockingAssignment::OperatorAssignment(assignment) => &assignment.nodes.0,
        _ => return true,
    };
    let sv_parser::VariableLvalue::Identifier(identifier) = lvalue else {
        return true;
    };
    let select = &identifier.nodes.2;
    select.nodes.0.is_some() || !select.nodes.1.nodes.0.is_empty() || select.nodes.2.is_some()
}

fn non_input_function_port(node: RefNode<'_>) -> bool {
    let direction = match node {
        RefNode::TfPortItem(port) => port.nodes.1.as_ref(),
        RefNode::TfPortDeclaration(port) => Some(&port.nodes.1),
        _ => return false,
    };
    match direction {
        None => false,
        Some(sv_parser::TfPortDirection::PortDirection(direction)) => {
            !matches!(&**direction, sv_parser::PortDirection::Input(_))
        }
        Some(sv_parser::TfPortDirection::ConstRef(_)) => true,
    }
}

fn function_has_static_local_state(function: &sv_parser::FunctionDeclaration) -> bool {
    let function_is_static = !matches!(function.nodes.1, Some(sv_parser::Lifetime::Automatic(_)));
    let local_is_static = |item: &sv_parser::BlockItemDeclaration| {
        let sv_parser::BlockItemDeclaration::Data(item) = item else {
            return false;
        };
        let sv_parser::DataDeclaration::Variable(variable) = &item.nodes.1 else {
            return false;
        };
        match &variable.nodes.2 {
            Some(sv_parser::Lifetime::Automatic(_)) => false,
            Some(sv_parser::Lifetime::Static(_)) => true,
            None => function_is_static,
        }
    };
    match &function.nodes.2 {
        sv_parser::FunctionBodyDeclaration::WithPort(body) => {
            body.nodes.5.iter().any(local_is_static)
        }
        sv_parser::FunctionBodyDeclaration::WithoutPort(body) => body.nodes.4.iter().any(|item| {
            matches!(
                item,
                sv_parser::TfItemDeclaration::BlockItemDeclaration(item)
                    if local_is_static(item)
            )
        }),
    }
}

fn apply_parameter_overrides(
    parameters: &mut [Parameter],
    overrides: &HashMap<String, ConstExpr>,
) -> Result<(), AnalyzerError> {
    if let Some(name) = overrides
        .keys()
        .find(|name| !parameters.iter().any(|parameter| parameter.name() == *name))
    {
        return Err(AnalyzerError::Unsupported(format!(
            "unknown top-level parameter override `{name}`"
        )));
    }
    if let Some(name) = overrides.keys().find(|name| {
        parameters
            .iter()
            .any(|parameter| parameter.name() == *name && parameter.is_local)
    }) {
        return Err(AnalyzerError::Unsupported(format!(
            "localparam override `{name}`"
        )));
    }
    for parameter in parameters {
        if let Some(value) = overrides.get(parameter.name()) {
            parameter.value = Some(value.clone());
        }
    }
    Ok(())
}

fn const_expr_from_i128(value: i128) -> ConstExpr {
    if value < 0 {
        ConstExpr::Unary {
            op: UnaryOp::Minus,
            expr: Box::new(ConstExpr::Literal(value.unsigned_abs().to_string())),
        }
    } else {
        ConstExpr::Literal(value.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parameter {
    name: String,
    value: Option<ConstExpr>,
    declared_width: Option<usize>,
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
}

impl Type {
    fn implicit() -> Self {
        Self {
            kind: TypeKind::Implicit,
            is_signed: false,
            packed_ranges: Vec::new(),
            unpacked_ranges: Vec::new(),
        }
    }

    fn new(kind: TypeKind) -> Self {
        Self {
            kind,
            is_signed: false,
            packed_ranges: Vec::new(),
            unpacked_ranges: Vec::new(),
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

fn identifier_locate(node: RefNode<'_>) -> Option<Locate> {
    match unwrap_node!(node, SimpleIdentifier, EscapedIdentifier) {
        Some(RefNode::SimpleIdentifier(identifier)) => Some(identifier.nodes.0),
        Some(RefNode::EscapedIdentifier(identifier)) => Some(identifier.nodes.0),
        _ => None,
    }
}

fn ports_from_module_node(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Result<Vec<Port>, AnalyzerError> {
    let mut ports = Vec::new();
    let mut inherited_direction = PortDirection::Unspecified;
    let mut inherited_type = Type::implicit();
    for child in node {
        match child {
            RefNode::AnsiPortDeclarationNet(port) => {
                let header = port.nodes.0.as_ref();
                let direction = header
                    .and_then(|header| direction_from_ref_node(header.into()))
                    .unwrap_or(inherited_direction);
                let r#type = match header {
                    Some(header) => {
                        type_from_net_port_header(header, syntax_tree, const_env, type_aliases)
                            .ok_or_else(|| {
                                AnalyzerError::Unsupported("unsupported port data type".to_string())
                            })?
                    }
                    None => inherited_type.clone(),
                };
                let r#type = type_with_fallback_ranges_with_env(
                    r#type,
                    RefNode::AnsiPortDeclarationNet(port),
                    syntax_tree,
                    const_env,
                    type_aliases,
                );
                let inherited_type_base = r#type.clone();
                let r#type = type_with_unpacked_ranges(
                    r#type,
                    unpacked_ranges_from_dimensions_with_env(
                        &port.nodes.2,
                        syntax_tree,
                        const_env,
                        type_aliases,
                    )?,
                );
                let name = port_name(RefNode::PortIdentifier(&port.nodes.1), syntax_tree)?;
                inherited_direction = direction;
                inherited_type = inherited_type_base;
                ports.push(Port::new(name, direction, r#type, true));
            }
            RefNode::AnsiPortDeclarationVariable(port) => {
                let header = port.nodes.0.as_ref();
                let direction = header
                    .and_then(|header| direction_from_ref_node(header.into()))
                    .unwrap_or(inherited_direction);
                let r#type = match header {
                    Some(header) => {
                        type_from_variable_port_header(header, syntax_tree, const_env, type_aliases)
                            .ok_or_else(|| {
                                AnalyzerError::Unsupported("unsupported port data type".to_string())
                            })?
                    }
                    None => inherited_type.clone(),
                };
                let r#type = type_with_fallback_ranges_with_env(
                    r#type,
                    RefNode::AnsiPortDeclarationVariable(port),
                    syntax_tree,
                    const_env,
                    type_aliases,
                );
                let inherited_type_base = r#type.clone();
                let r#type = type_with_unpacked_ranges(
                    r#type,
                    unpacked_ranges_from_variable_dimensions_with_env(
                        &port.nodes.2,
                        syntax_tree,
                        const_env,
                        type_aliases,
                    )?,
                );
                let name = port_name(RefNode::PortIdentifier(&port.nodes.1), syntax_tree)?;
                inherited_direction = direction;
                inherited_type = inherited_type_base;
                ports.push(Port::new(name, direction, r#type, false));
            }
            RefNode::AnsiPortDeclarationParen(port) => {
                let explicit_direction = port.nodes.0.as_ref().map(direction_from_port_direction);
                let direction = explicit_direction.unwrap_or(inherited_direction);
                let r#type = if explicit_direction.is_some() {
                    Type::implicit()
                } else {
                    inherited_type.clone()
                };
                let name = port_name(RefNode::PortIdentifier(&port.nodes.2), syntax_tree)?;
                inherited_direction = direction;
                inherited_type = r#type.clone();
                ports.push(Port::new(name, direction, r#type, false));
            }
            _ => {}
        }
    }
    Ok(ports)
}

fn parameters_from_module_node(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    type_aliases: &HashMap<String, Type>,
    base_const_env: &HashMap<String, i128>,
    parameter_overrides: &HashMap<String, ConstExpr>,
) -> Result<Vec<Parameter>, AnalyzerError> {
    let mut parameters = Vec::new();
    if let Some(parameter_port_list) = module_parameter_port_list(node.clone()) {
        parameters_from_ref_node(
            parameter_port_list.clone(),
            syntax_tree,
            &mut parameters,
            false,
            base_const_env,
            type_aliases,
            parameter_overrides,
        )?;
        let mut local_parameters = Vec::new();
        for child in parameter_port_list {
            if let RefNode::LocalParameterDeclaration(localparam) = child {
                parameters_from_ref_node(
                    RefNode::LocalParameterDeclaration(localparam),
                    syntax_tree,
                    &mut local_parameters,
                    true,
                    base_const_env,
                    type_aliases,
                    parameter_overrides,
                )?;
            }
        }
        for local in local_parameters {
            if let Some(parameter) = parameters
                .iter_mut()
                .find(|parameter| parameter.name == local.name)
            {
                parameter.is_local = true;
            }
        }
    }

    for item in module_non_port_items(node.clone()) {
        if let Some(declaration) = package_or_generate_declaration_from_non_port_item(item) {
            match declaration {
                sv_parser::PackageOrGenerateItemDeclaration::LocalParameterDeclaration(
                    localparam,
                ) => parameters_from_ref_node(
                    RefNode::LocalParameterDeclaration(&localparam.0),
                    syntax_tree,
                    &mut parameters,
                    true,
                    base_const_env,
                    type_aliases,
                    parameter_overrides,
                )?,
                sv_parser::PackageOrGenerateItemDeclaration::ParameterDeclaration(parameter) => {
                    parameters_from_ref_node(
                        RefNode::ParameterDeclaration(&parameter.0),
                        syntax_tree,
                        &mut parameters,
                        false,
                        base_const_env,
                        type_aliases,
                        parameter_overrides,
                    )?
                }
                _ => {}
            }
        }
    }

    Ok(parameters)
}

fn signals_from_module_node(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Result<Vec<Signal>, AnalyzerError> {
    let mut signals = Vec::new();
    for item in module_non_port_items(node) {
        signals_from_non_port_module_item(
            item,
            syntax_tree,
            type_aliases,
            const_env,
            &mut signals,
        )?;
    }
    signals.sort_by(|a, b| a.name.cmp(&b.name));
    if let Some(name) = signals
        .windows(2)
        .find(|pair| pair[0].name == pair[1].name)
        .map(|pair| pair[0].name.clone())
    {
        return Err(AnalyzerError::Unsupported(format!(
            "duplicate internal signal `{name}`"
        )));
    }
    Ok(signals)
}

fn signals_from_non_port_module_item(
    item: &sv_parser::NonPortModuleItem,
    syntax_tree: &SyntaxTree,
    type_aliases: &HashMap<String, Type>,
    const_env: &HashMap<String, i128>,
    signals: &mut Vec<Signal>,
) -> Result<(), AnalyzerError> {
    match item {
        sv_parser::NonPortModuleItem::GenerateRegion(region) => {
            for item in &region.nodes.1 {
                signals_from_generate_item(item, syntax_tree, type_aliases, const_env, signals)?;
            }
        }
        sv_parser::NonPortModuleItem::ModuleOrGenerateItem(item) => {
            signals_from_module_or_generate_item(
                item,
                syntax_tree,
                type_aliases,
                const_env,
                signals,
            )?;
        }
        _ => {}
    }
    Ok(())
}

fn signals_from_generate_item(
    item: &sv_parser::GenerateItem,
    syntax_tree: &SyntaxTree,
    type_aliases: &HashMap<String, Type>,
    const_env: &HashMap<String, i128>,
    signals: &mut Vec<Signal>,
) -> Result<(), AnalyzerError> {
    if let sv_parser::GenerateItem::ModuleOrGenerateItem(item) = item {
        signals_from_module_or_generate_item(item, syntax_tree, type_aliases, const_env, signals)?;
    }
    Ok(())
}

fn signals_from_module_or_generate_item(
    item: &sv_parser::ModuleOrGenerateItem,
    syntax_tree: &SyntaxTree,
    type_aliases: &HashMap<String, Type>,
    const_env: &HashMap<String, i128>,
    signals: &mut Vec<Signal>,
) -> Result<(), AnalyzerError> {
    match item {
        sv_parser::ModuleOrGenerateItem::Module(module) => {
            let mut alias_signals = signals_from_type_alias_instantiation(
                &module.nodes.1,
                syntax_tree,
                type_aliases,
                const_env,
            )?;
            substitute_signal_local_constants(&mut alias_signals, const_env);
            signals.extend(alias_signals);
        }
        sv_parser::ModuleOrGenerateItem::ModuleItem(item) => {
            signals_from_module_common_item(
                &item.nodes.1,
                syntax_tree,
                type_aliases,
                const_env,
                signals,
            )?;
        }
        _ => {}
    }
    Ok(())
}

fn signals_from_module_common_item(
    item: &sv_parser::ModuleCommonItem,
    syntax_tree: &SyntaxTree,
    type_aliases: &HashMap<String, Type>,
    const_env: &HashMap<String, i128>,
    signals: &mut Vec<Signal>,
) -> Result<(), AnalyzerError> {
    match item {
        sv_parser::ModuleCommonItem::ModuleOrGenerateItemDeclaration(declaration) => {
            let sv_parser::ModuleOrGenerateItemDeclaration::PackageOrGenerateItemDeclaration(
                declaration,
            ) = &**declaration
            else {
                return Ok(());
            };
            let mut declared = match &**declaration {
                sv_parser::PackageOrGenerateItemDeclaration::DataDeclaration(data) => {
                    signals_from_data_declaration(data, syntax_tree, type_aliases, const_env)?
                }
                sv_parser::PackageOrGenerateItemDeclaration::NetDeclaration(net) => {
                    signals_from_net_declaration(net, syntax_tree, type_aliases, const_env)?
                }
                _ => Vec::new(),
            };
            substitute_signal_local_constants(&mut declared, const_env);
            signals.extend(declared);
        }
        sv_parser::ModuleCommonItem::ConditionalGenerateConstruct(generate) => {
            let sv_parser::ConditionalGenerateConstruct::If(generate) = &**generate else {
                return Ok(());
            };
            let condition = const_expr_from_ref_node(
                RefNode::ConstantExpression(&generate.nodes.1.nodes.1),
                syntax_tree,
            )
            .ok_or_else(|| {
                AnalyzerError::Unsupported("conditional-generate condition lowering".to_string())
            })?;
            let condition = eval_ast_const_expr(&condition, const_env).ok_or_else(|| {
                AnalyzerError::Unsupported("unknown conditional-generate condition".to_string())
            })?;
            let block = if condition != 0 {
                Some(&generate.nodes.2)
            } else {
                generate.nodes.3.as_ref().map(|(_, block)| block)
            };
            if let Some(block) = block {
                signals_from_generate_block(block, syntax_tree, type_aliases, const_env, signals)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn signals_from_generate_block(
    block: &sv_parser::GenerateBlock,
    syntax_tree: &SyntaxTree,
    type_aliases: &HashMap<String, Type>,
    const_env: &HashMap<String, i128>,
    signals: &mut Vec<Signal>,
) -> Result<(), AnalyzerError> {
    match block {
        sv_parser::GenerateBlock::GenerateItem(item) => {
            signals_from_generate_item(item, syntax_tree, type_aliases, const_env, signals)?;
        }
        sv_parser::GenerateBlock::Multiple(block) => {
            let mut block_env = const_env.clone();
            for item in &block.nodes.3 {
                if add_localparams_from_generate_item(item, syntax_tree, &mut block_env) {
                    continue;
                }
                signals_from_generate_item(item, syntax_tree, type_aliases, &block_env, signals)?;
            }
        }
    }
    Ok(())
}

fn substitute_signal_local_constants(signals: &mut [Signal], const_env: &HashMap<String, i128>) {
    for signal in signals {
        for range in &mut signal.r#type.packed_ranges {
            range.left = substitute_dimension_constants(range.left.clone(), const_env);
            range.right = substitute_dimension_constants(range.right.clone(), const_env);
        }
        for range in &mut signal.r#type.unpacked_ranges {
            range.left = substitute_dimension_constants(range.left.clone(), const_env);
            range.right = substitute_dimension_constants(range.right.clone(), const_env);
            range.size = range
                .size
                .take()
                .map(|size| substitute_dimension_constants(size, const_env));
        }
    }
}

fn signals_from_net_declaration(
    net: &sv_parser::NetDeclaration,
    syntax_tree: &SyntaxTree,
    type_aliases: &HashMap<String, Type>,
    const_env: &HashMap<String, i128>,
) -> Result<Vec<Signal>, AnalyzerError> {
    let (r#type, assignments, is_net) = match net {
        sv_parser::NetDeclaration::NetType(net) => {
            let r#type = type_from_ref_node_with_env(
                RefNode::DataTypeOrImplicit(&net.nodes.3),
                syntax_tree,
                const_env,
                type_aliases,
            )
            .or_else(|| {
                type_alias_from_ref_node(
                    RefNode::DataTypeOrImplicit(&net.nodes.3),
                    syntax_tree,
                    type_aliases,
                )
            });
            let r#type = match (&net.nodes.3, r#type) {
                (_, Some(r#type)) => r#type,
                (sv_parser::DataTypeOrImplicit::ImplicitDataType(_), None) => Type::implicit(),
                (sv_parser::DataTypeOrImplicit::DataType(_), None) => {
                    return Err(AnalyzerError::Unsupported(
                        "unsupported net data type".to_string(),
                    ));
                }
            };
            let r#type = type_with_fallback_ranges_with_env(
                r#type,
                RefNode::DataTypeOrImplicit(&net.nodes.3),
                syntax_tree,
                const_env,
                type_aliases,
            );
            (r#type, net.nodes.5.nodes.0.contents(), true)
        }
        sv_parser::NetDeclaration::NetTypeIdentifier(net) => {
            let Some(name) = identifier_text(RefNode::NetTypeIdentifier(&net.nodes.0), syntax_tree)
            else {
                return Ok(Vec::new());
            };
            let Some(r#type) = type_aliases.get(&name).cloned() else {
                return Ok(Vec::new());
            };
            (r#type, net.nodes.2.nodes.0.contents(), false)
        }
        sv_parser::NetDeclaration::Interconnect(_) => {
            return Err(AnalyzerError::Unsupported(
                "interconnect net declaration".to_string(),
            ));
        }
    };
    let mut signals = Vec::new();
    for assignment in assignments {
        let name = identifier_text(RefNode::NetIdentifier(&assignment.nodes.0), syntax_tree)
            .ok_or_else(|| {
                AnalyzerError::Unsupported("unsupported signal identifier".to_string())
            })?;
        let signal_type = type_with_unpacked_ranges(
            r#type.clone(),
            unpacked_ranges_from_dimensions_with_env(
                &assignment.nodes.1,
                syntax_tree,
                const_env,
                type_aliases,
            )?,
        );
        signals.push(if is_net {
            Signal::new_net(name, signal_type)
        } else {
            Signal::new(name, signal_type)
        });
    }
    Ok(signals)
}

fn signals_from_type_alias_instantiation(
    instantiation: &sv_parser::ModuleInstantiation,
    syntax_tree: &SyntaxTree,
    type_aliases: &HashMap<String, Type>,
    const_env: &HashMap<String, i128>,
) -> Result<Vec<Signal>, AnalyzerError> {
    let mut signals = Vec::new();
    let module_name = identifier_text(
        RefNode::ModuleIdentifier(&instantiation.nodes.0),
        syntax_tree,
    )
    .ok_or_else(|| {
        AnalyzerError::Unsupported("unsupported module instantiation identifier".to_string())
    })?;
    let Some(r#type) = type_aliases.get(&module_name) else {
        return Ok(signals);
    };
    for instance in instantiation.nodes.2.contents() {
        let name = identifier_text(
            RefNode::InstanceIdentifier(&instance.nodes.0.nodes.0),
            syntax_tree,
        )
        .ok_or_else(|| AnalyzerError::Unsupported("unsupported signal identifier".to_string()))?;
        let signal_type = type_with_unpacked_ranges(
            r#type.clone(),
            unpacked_ranges_from_dimensions_with_env(
                &instance.nodes.0.nodes.1,
                syntax_tree,
                const_env,
                type_aliases,
            )?,
        );
        signals.push(Signal::new(name, signal_type));
    }
    Ok(signals)
}

fn type_aliases_from_module_node(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
) -> Result<HashMap<String, Type>, AnalyzerError> {
    type_aliases_from_module_node_with_env(node, syntax_tree, &HashMap::default())
}

fn type_aliases_from_module_node_with_env(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
) -> Result<HashMap<String, Type>, AnalyzerError> {
    let mut aliases = HashMap::default();
    if let Some(parameter_port_list) = module_parameter_port_list(node.clone()) {
        if let RefNode::ParameterPortList(parameter_port_list) = parameter_port_list {
            add_type_aliases_from_parameter_port_list(
                parameter_port_list,
                syntax_tree,
                &mut aliases,
            );
        }
        for child in parameter_port_list {
            match child {
                RefNode::LocalParameterDeclaration(localparam) => {
                    add_type_aliases_from_localparam(localparam, syntax_tree, &mut aliases);
                }
                RefNode::ParameterDeclaration(parameter) => {
                    add_type_aliases_from_parameter(parameter, syntax_tree, &mut aliases);
                }
                _ => {}
            }
        }
    }
    for item in module_non_port_items(node.clone()) {
        let Some(declaration) = package_or_generate_declaration_from_non_port_item(item) else {
            continue;
        };
        match declaration {
            sv_parser::PackageOrGenerateItemDeclaration::DataDeclaration(declaration) => {
                add_type_alias_from_data_declaration(
                    declaration,
                    syntax_tree,
                    const_env,
                    &mut aliases,
                )?;
            }
            sv_parser::PackageOrGenerateItemDeclaration::LocalParameterDeclaration(localparam) => {
                add_type_aliases_from_localparam(&localparam.0, syntax_tree, &mut aliases);
            }
            sv_parser::PackageOrGenerateItemDeclaration::ParameterDeclaration(parameter) => {
                add_type_aliases_from_parameter(&parameter.0, syntax_tree, &mut aliases);
            }
            _ => {}
        }
    }
    for child in node {
        let RefNode::TypeAssignment(assignment) = child else {
            continue;
        };
        add_type_alias_from_type_assignment(assignment, syntax_tree, &mut aliases);
    }
    Ok(aliases)
}

fn add_type_alias_from_data_declaration(
    declaration: &sv_parser::DataDeclaration,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    aliases: &mut HashMap<String, Type>,
) -> Result<(), AnalyzerError> {
    let sv_parser::DataDeclaration::TypeDeclaration(declaration) = declaration else {
        return Ok(());
    };
    let sv_parser::TypeDeclaration::DataType(declaration) = &**declaration else {
        return Ok(());
    };
    let Some(name) = identifier_text(RefNode::TypeIdentifier(&declaration.nodes.2), syntax_tree)
    else {
        return Ok(());
    };
    let r#type = type_from_ref_node_with_env(
        RefNode::DataType(&declaration.nodes.1),
        syntax_tree,
        const_env,
        aliases,
    )
    .or_else(|| {
        let sv_parser::DataType::Enum(r#enum) = &declaration.nodes.1 else {
            return None;
        };
        if let Some(base) = &r#enum.nodes.1 {
            type_alias_from_ref_node(RefNode::EnumBaseType(base), syntax_tree, aliases)
        } else {
            let mut r#type = Type::new(TypeKind::Bit);
            r#type.is_signed = true;
            r#type.packed_ranges.push(PackedRange::new(
                ConstExpr::Literal("31".to_string()),
                ConstExpr::Literal("0".to_string()),
            ));
            Some(r#type)
        }
    });
    let Some(r#type) = r#type else {
        return Ok(());
    };
    let r#type = type_with_unpacked_ranges(
        r#type,
        unpacked_ranges_from_variable_dimensions_with_env(
            &declaration.nodes.3,
            syntax_tree,
            const_env,
            aliases,
        )?,
    );
    aliases.insert(name, r#type);
    Ok(())
}

fn add_type_aliases_from_parameter_port_list(
    list: &sv_parser::ParameterPortList,
    syntax_tree: &SyntaxTree,
    aliases: &mut HashMap<String, Type>,
) {
    match list {
        sv_parser::ParameterPortList::Assignment(list) => {
            for (_, declaration) in &list.nodes.1.nodes.1.1 {
                add_type_aliases_from_parameter_port_declaration(declaration, syntax_tree, aliases);
            }
        }
        sv_parser::ParameterPortList::Declaration(list) => {
            for declaration in list.nodes.1.nodes.1.contents() {
                add_type_aliases_from_parameter_port_declaration(declaration, syntax_tree, aliases);
            }
        }
        sv_parser::ParameterPortList::Empty(_) => {}
    }
}

fn add_type_aliases_from_parameter_port_declaration(
    declaration: &sv_parser::ParameterPortDeclaration,
    syntax_tree: &SyntaxTree,
    aliases: &mut HashMap<String, Type>,
) {
    match declaration {
        sv_parser::ParameterPortDeclaration::ParameterDeclaration(declaration) => {
            add_type_aliases_from_parameter(declaration, syntax_tree, aliases);
        }
        sv_parser::ParameterPortDeclaration::LocalParameterDeclaration(declaration) => {
            add_type_aliases_from_localparam(declaration, syntax_tree, aliases);
        }
        sv_parser::ParameterPortDeclaration::TypeList(list) => {
            for assignment in list.nodes.1.nodes.0.contents() {
                add_type_alias_from_type_assignment(assignment, syntax_tree, aliases);
            }
        }
        sv_parser::ParameterPortDeclaration::ParamList(_) => {}
    }
}

fn add_type_aliases_from_localparam(
    declaration: &sv_parser::LocalParameterDeclaration,
    syntax_tree: &SyntaxTree,
    aliases: &mut HashMap<String, Type>,
) {
    let sv_parser::LocalParameterDeclaration::Type(declaration) = declaration else {
        return;
    };
    for assignment in declaration.nodes.2.nodes.0.contents() {
        add_type_alias_from_type_assignment(assignment, syntax_tree, aliases);
    }
}

fn add_type_aliases_from_parameter(
    declaration: &sv_parser::ParameterDeclaration,
    syntax_tree: &SyntaxTree,
    aliases: &mut HashMap<String, Type>,
) {
    let sv_parser::ParameterDeclaration::Type(declaration) = declaration else {
        return;
    };
    for assignment in declaration.nodes.2.nodes.0.contents() {
        add_type_alias_from_type_assignment(assignment, syntax_tree, aliases);
    }
}

fn add_type_alias_from_type_assignment(
    assignment: &sv_parser::TypeAssignment,
    syntax_tree: &SyntaxTree,
    aliases: &mut HashMap<String, Type>,
) {
    let Some((_, data_type)) = &assignment.nodes.1 else {
        return;
    };
    let Some(name) = identifier_text(RefNode::TypeIdentifier(&assignment.nodes.0), syntax_tree)
    else {
        return;
    };
    let Some(r#type) = type_from_ref_node(RefNode::DataType(data_type), syntax_tree) else {
        return;
    };
    aliases.insert(name, r#type);
}

fn signals_from_data_declaration(
    data: &sv_parser::DataDeclaration,
    syntax_tree: &SyntaxTree,
    type_aliases: &HashMap<String, Type>,
    const_env: &HashMap<String, i128>,
) -> Result<Vec<Signal>, AnalyzerError> {
    let sv_parser::DataDeclaration::Variable(variable) = data else {
        return Ok(Vec::new());
    };
    let r#type = type_from_ref_node_with_env(
        RefNode::DataTypeOrImplicit(&variable.nodes.3),
        syntax_tree,
        const_env,
        type_aliases,
    )
    .or_else(|| {
        type_alias_from_ref_node(
            RefNode::DataTypeOrImplicit(&variable.nodes.3),
            syntax_tree,
            type_aliases,
        )
    });
    let r#type = match (&variable.nodes.3, r#type) {
        (_, Some(r#type)) => r#type,
        (sv_parser::DataTypeOrImplicit::ImplicitDataType(_), None) => Type::implicit(),
        (sv_parser::DataTypeOrImplicit::DataType(_), None) => {
            return Err(AnalyzerError::Unsupported(
                "unsupported internal data type".to_string(),
            ));
        }
    };
    let r#type = type_with_fallback_ranges_with_env(
        r#type,
        RefNode::DataTypeOrImplicit(&variable.nodes.3),
        syntax_tree,
        const_env,
        type_aliases,
    );
    let mut signals = Vec::new();
    for assignment in variable.nodes.4.nodes.0.contents() {
        let sv_parser::VariableDeclAssignment::Variable(assignment) = assignment else {
            continue;
        };
        let name = identifier_text(
            RefNode::VariableIdentifier(&assignment.nodes.0),
            syntax_tree,
        )
        .ok_or_else(|| AnalyzerError::Unsupported("unsupported signal identifier".to_string()))?;
        let signal_type = type_with_unpacked_ranges(
            r#type.clone(),
            unpacked_ranges_from_variable_dimensions_with_env(
                &assignment.nodes.1,
                syntax_tree,
                const_env,
                type_aliases,
            )?,
        );
        signals.push(Signal::new(name, signal_type));
    }
    Ok(signals)
}

fn type_alias_from_ref_node(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    type_aliases: &HashMap<String, Type>,
) -> Option<Type> {
    if let Some(RefNode::DataType(data_type)) = unwrap_node!(node.clone(), DataType)
        && let Some(r#type) = type_alias_from_data_type(data_type, syntax_tree, type_aliases)
    {
        return Some(r#type);
    }
    if let Some(RefNode::DataTypeOrImplicit(data_type)) =
        unwrap_node!(node.clone(), DataTypeOrImplicit)
        && let Some(r#type) =
            type_alias_from_data_type_or_implicit(data_type, syntax_tree, type_aliases)
    {
        return Some(r#type);
    }
    let name =
        if let Some(RefNode::DataTypeType(data_type)) = unwrap_node!(node.clone(), DataTypeType) {
            identifier_text(RefNode::TypeIdentifier(&data_type.nodes.1), syntax_tree)?
        } else {
            let RefNode::TypeIdentifier(identifier) = unwrap_node!(node, TypeIdentifier)? else {
                return None;
            };
            identifier_text(RefNode::TypeIdentifier(identifier), syntax_tree)?
        };
    type_aliases.get(&name).cloned()
}

fn parameters_from_ref_node(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    parameters: &mut Vec<Parameter>,
    is_local: bool,
    base_const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
    parameter_overrides: &HashMap<String, ConstExpr>,
) -> Result<(), AnalyzerError> {
    if node.clone().into_iter().any(|child| {
        matches!(
            child,
            RefNode::DataTypeOrImplicit(sv_parser::DataTypeOrImplicit::DataType(data_type))
                if !matches!(
                    &**data_type,
                    sv_parser::DataType::Vector(_)
                        | sv_parser::DataType::Atom(_)
                        | sv_parser::DataType::Type(_)
                        | sv_parser::DataType::ClassType(_)
                )
        )
    }) {
        return Err(AnalyzerError::Unsupported(
            "unsupported parameter data type".to_string(),
        ));
    }
    let declared_alias = type_alias_from_ref_node(node.clone(), syntax_tree, type_aliases);
    let parameter_width = parameter_declared_width(
        node.clone(),
        syntax_tree,
        base_const_env,
        parameters,
        type_aliases,
    );
    let has_declared_type = node.clone().into_iter().any(|child| {
        matches!(
            child,
            RefNode::DataTypeOrImplicit(sv_parser::DataTypeOrImplicit::DataType(_))
        )
    });
    let parameter_signed = parameter_width.map(|_| {
        declared_alias
            .as_ref()
            .map(Type::is_signed)
            .unwrap_or_else(|| {
                integer_atom_expr_type(node.clone())
                    .map(|r#type| r#type.signed)
                    .unwrap_or_else(|| is_signed_from_ref_node(node.clone()).unwrap_or(false))
            })
    });
    let parameter_is_2state = declared_alias
        .or_else(|| type_from_ref_node(node.clone(), syntax_tree))
        .is_some_and(|r#type| r#type.kind() == TypeKind::Bit);
    for child in node {
        if let RefNode::ParamAssignment(param) = child {
            let name = parameter_name(RefNode::ParameterIdentifier(&param.nodes.0), syntax_tree)?;
            let mut const_env = base_const_env.clone();
            const_env.extend(const_env_from_parameters(parameters));
            let mut value = param.nodes.2.as_ref().and_then(|(_, expr)| {
                const_expr_from_constant_param_with_env(expr, syntax_tree, &const_env, type_aliases)
            });
            value =
                normalize_unbased_unsized_parameter_value(value, parameter_width, parameter_signed);
            // Apply overrides as each declaration is collected so later
            // parameter initializers (including casts) are evaluated from the
            // specialized values rather than defaults that will be replaced
            // only after collection has finished.
            if !is_local && let Some(override_value) = parameter_overrides.get(&name) {
                value = Some(override_value.clone());
            }
            parameters.push(Parameter::new(
                name,
                value,
                parameter_width,
                parameter_signed,
                parameter_is_2state,
                has_declared_type,
                is_local,
            ));
        }
    }
    Ok(())
}

fn parameter_declared_width(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    base_const_env: &HashMap<String, i128>,
    parameters: &[Parameter],
    type_aliases: &HashMap<String, Type>,
) -> Option<usize> {
    let declared_alias = type_alias_from_ref_node(node.clone(), syntax_tree, type_aliases);
    let ranges = declared_alias
        .as_ref()
        .map(|r#type| r#type.packed_ranges.clone())
        .unwrap_or_else(|| packed_ranges_from_ref_node(node.clone(), syntax_tree));
    if ranges.is_empty() {
        if declared_alias.is_some() {
            return Some(1);
        }
        if let Some(r#type) = integer_atom_expr_type(node.clone()) {
            return Some(r#type.width);
        }
        return unwrap_node!(node, IntegerVectorType).is_some().then_some(1);
    }
    let mut env = base_const_env.clone();
    // A second lowering pass receives values from the first pass in the base
    // environment. Do not let stale values for parameters declared by this
    // same syntax node resolve its declaration ranges; only constants from
    // outside the declaration (such as enum members) belong here.
    for child in node.clone() {
        if let RefNode::ParamAssignment(parameter) = child
            && let Ok(name) = parameter_name(
                RefNode::ParameterIdentifier(&parameter.nodes.0),
                syntax_tree,
            )
        {
            env.remove(&name);
        }
    }
    env.extend(const_env_from_parameters(parameters));
    ranges.iter().try_fold(1usize, |acc, range| {
        let left = eval_ast_const_expr(range.left(), &env)?;
        let right = eval_ast_const_expr(range.right(), &env)?;
        acc.checked_mul(left.abs_diff(right) as usize + 1)
    })
}

fn normalize_unbased_unsized_parameter_value(
    value: Option<ConstExpr>,
    width: Option<usize>,
    signed: Option<bool>,
) -> Option<ConstExpr> {
    let signing = if signed.unwrap_or(false) { "s" } else { "" };
    match (value, width) {
        (Some(ConstExpr::Literal(value)), Some(width)) if value == "'1" => {
            let literal = if width <= 128 {
                let bits = if width == 128 {
                    u128::MAX
                } else {
                    (1u128 << width) - 1
                };
                format!("{width}'{signing}d{bits}")
            } else {
                format!("{width}'{signing}b{}", "1".repeat(width))
            };
            Some(ConstExpr::Literal(literal))
        }
        (Some(ConstExpr::Literal(value)), Some(width)) if value == "'0" => {
            Some(ConstExpr::Literal(format!("{width}'{signing}d0")))
        }
        (Some(ConstExpr::Literal(value)), Some(width))
            if matches!(value.as_str(), "'x" | "'X" | "'z" | "'Z" | "'?") =>
        {
            let fill = value.chars().nth(1)?;
            Some(ConstExpr::Literal(format!("{width}'{signing}b{fill}")))
        }
        (value, _) => value,
    }
}

fn const_env_from_parameters(parameters: &[Parameter]) -> HashMap<String, i128> {
    let mut env = HashMap::default();
    extend_const_env_with_parameters(&mut env, parameters);
    env
}

fn extend_const_env_with_parameters(env: &mut HashMap<String, i128>, parameters: &[Parameter]) {
    let mut parameter_types = parameter_types_from_const_env(env);
    for parameter in parameters {
        let Some(value) = parameter.resolved_value(env, &parameter_types) else {
            continue;
        };
        if let Some(r#type) = parameter.resolved_type(&parameter_types) {
            parameter_types.insert(parameter.name().to_string(), r#type);
            insert_parameter_type_markers(env, parameter.name(), r#type);
        }
        env.insert(parameter.name().to_string(), value);
        env.insert(parameter_marker(parameter.name()), value);
        if parameter.is_local {
            env.insert(local_parameter_marker(parameter.name()), value);
        }
    }
}

fn coerce_const_parameter_value(value: i128, width: usize, signed: bool) -> i128 {
    if width >= 128 {
        return value;
    }
    if width == 0 {
        return 0;
    }
    let mask = (1u128 << width) - 1;
    let bits = (value as u128) & mask;
    if signed && bits & (1u128 << (width - 1)) != 0 {
        (bits | !mask) as i128
    } else {
        bits as i128
    }
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

/// Collect enum member constants declared by module-level `typedef enum`
/// declarations. Members must carry explicit values, matching what Veryl
/// emits; each initializer may reference previously declared members of the
/// same module scope.
fn enum_member_constants_from_module_node(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    base_const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
    parameter_overrides: &HashMap<String, ConstExpr>,
) -> Result<EnumMemberConstants, AnalyzerError> {
    let mut constants = EnumMemberConstants::default();
    let mut eval_env = base_const_env.clone();
    for item in module_non_port_items(node) {
        let Some(declaration) = package_or_generate_declaration_from_non_port_item(item) else {
            continue;
        };
        let data = match declaration {
            sv_parser::PackageOrGenerateItemDeclaration::LocalParameterDeclaration(localparam) => {
                let mut parameters = Vec::new();
                parameters_from_ref_node(
                    RefNode::LocalParameterDeclaration(&localparam.0),
                    syntax_tree,
                    &mut parameters,
                    true,
                    &eval_env,
                    type_aliases,
                    &HashMap::default(),
                )?;
                extend_const_env_with_parameters(&mut eval_env, &parameters);
                continue;
            }
            sv_parser::PackageOrGenerateItemDeclaration::ParameterDeclaration(parameter) => {
                let mut parameters = Vec::new();
                parameters_from_ref_node(
                    RefNode::ParameterDeclaration(&parameter.0),
                    syntax_tree,
                    &mut parameters,
                    false,
                    &eval_env,
                    type_aliases,
                    parameter_overrides,
                )?;
                extend_const_env_with_parameters(&mut eval_env, &parameters);
                continue;
            }
            sv_parser::PackageOrGenerateItemDeclaration::DataDeclaration(data) => data,
            _ => continue,
        };
        let sv_parser::DataDeclaration::TypeDeclaration(type_declaration) = &**data else {
            continue;
        };
        let sv_parser::TypeDeclaration::DataType(type_declaration) = &**type_declaration else {
            continue;
        };
        let sv_parser::DataType::Enum(r#enum) = &type_declaration.nodes.1 else {
            continue;
        };
        let member_type = match &r#enum.nodes.1 {
            Some(base) => type_from_ref_node(RefNode::EnumBaseType(base), syntax_tree)
                .or_else(|| {
                    type_alias_from_ref_node(RefNode::EnumBaseType(base), syntax_tree, type_aliases)
                })
                .and_then(|r#type| expr_type_from_type(&r#type, &eval_env)),
            None => Some(ExprType {
                width: 32,
                signed: true,
            }),
        }
        .ok_or_else(|| AnalyzerError::Unsupported("enum base type".to_string()))?;
        for member in r#enum.nodes.2.nodes.1.contents() {
            let name = identifier_text(RefNode::Identifier(&member.nodes.0.nodes.0), syntax_tree)
                .ok_or_else(|| {
                AnalyzerError::Unsupported("enum member identifier".to_string())
            })?;
            if member.nodes.1.is_some() {
                return Err(AnalyzerError::Unsupported(format!(
                    "ranged enum member `{name}`"
                )));
            }
            let Some((_, value)) = &member.nodes.2 else {
                return Err(AnalyzerError::Unsupported(format!(
                    "enum member `{name}` without an explicit value"
                )));
            };
            let value = const_expr_from_ref_node_with_env(
                RefNode::ConstantExpression(value),
                syntax_tree,
                &eval_env,
                type_aliases,
            )
            .ok_or_else(|| AnalyzerError::Unsupported(format!("enum member `{name}` value")))?;
            let number = eval_ast_const_expr(&value, &eval_env).ok_or_else(|| {
                AnalyzerError::Unsupported(format!("unresolved enum member `{name}` value"))
            })?;
            let number =
                coerce_const_parameter_value(number, member_type.width, member_type.signed);
            constants.numbers.insert(name.clone(), number);
            eval_env.insert(name.clone(), number);
            insert_parameter_type_markers(&mut eval_env, &name, member_type);
            constants.types.insert(name.clone(), member_type);
            constants.exprs.insert(
                name,
                Expr::Literal(format_typed_parameter_literal(
                    number,
                    member_type.width,
                    member_type.signed,
                )),
            );
        }
    }
    Ok(constants)
}

fn parameter_value_env(
    parameters: &[Parameter],
    const_env: &HashMap<String, i128>,
) -> HashMap<String, Expr> {
    let mut values = HashMap::default();
    let mut parameter_types = HashMap::default();
    for parameter in parameters {
        let inferred_type = parameter.value().and_then(|value| {
            infer_parameter_value_type(value, parameter.has_declared_type, &parameter_types)
        });
        let width = parameter
            .declared_width
            .or(inferred_type.map(|r#type| r#type.width));
        let signed = parameter
            .declared_signed
            .or(inferred_type.map(|r#type| r#type.signed))
            .unwrap_or(false);
        if let Some(width) = width {
            parameter_types.insert(parameter.name().to_string(), ExprType { width, signed });
        }

        let mut value = if let Some(value) = const_env.get(parameter.name()).copied() {
            if let Some(width) = width {
                Expr::Literal(format_typed_parameter_literal(value, width, signed))
            } else if value.is_negative() {
                let width = (128 - (!value as u128).leading_zeros() as usize + 1).max(32);
                let mask = if width == 128 {
                    u128::MAX
                } else {
                    (1u128 << width) - 1
                };
                Expr::Literal(format!("{width}'sd{}", (value as u128) & mask))
            } else if let Some(ConstExpr::Literal(literal)) = parameter.value() {
                Expr::Literal(literal.clone())
            } else {
                Expr::Literal(value.to_string())
            }
        } else if let Some(value) = parameter.value().cloned() {
            let value = substitute_typed_parameter_literals(value, const_env, &parameter_types);
            let value = replace_oob_const_selects_with_unknown(value, const_env);
            let value = substitute_expr_constants_with_parameter_literals(
                const_expr_to_expr(value),
                const_env,
                &values,
            );
            if let Some(width) = width {
                match value {
                    Expr::Literal(literal) => {
                        let resized = resize_unbased_fill_literal_for_cast(&literal, width, signed)
                            .or_else(|| {
                                typecheck::parse_integral_literal(&literal).map(|literal| {
                                    resize_integral_literal_for_cast(literal, width, signed)
                                })
                            });
                        resized.map_or_else(
                            || Expr::Resize {
                                expr: Box::new(Expr::Literal(literal)),
                                width,
                                signed,
                            },
                            Expr::Literal,
                        )
                    }
                    value => Expr::Resize {
                        expr: Box::new(value),
                        width,
                        signed,
                    },
                }
            } else {
                value
            }
        } else {
            continue;
        };
        if parameter.declared_is_2state {
            value = Expr::Unary {
                op: UnaryOp::ToTwoState,
                expr: Box::new(value),
            };
        }
        values.insert(parameter.name().to_string(), value);
    }
    values
}

fn replace_oob_const_selects_with_unknown(
    expr: ConstExpr,
    const_env: &HashMap<String, i128>,
) -> ConstExpr {
    match expr {
        ConstExpr::Select { expr, bit } => {
            let expr = replace_oob_const_selects_with_unknown(*expr, const_env);
            let bit = replace_oob_const_selects_with_unknown(*bit, const_env);
            if let ConstExpr::Literal(literal) = &expr
                && let Some(width) =
                    typecheck::parse_integral_literal(literal).map(|literal| literal.width)
                && match typecheck::eval_const_expr(&bit.clone().into(), const_env) {
                    Some(bit_index) => {
                        usize::try_from(bit_index).map_or(true, |bit_index| bit_index >= width)
                    }
                    None => const_expr_contains_unknown_literal(&bit),
                }
            {
                ConstExpr::Literal("1'bx".to_string())
            } else {
                ConstExpr::Select {
                    expr: Box::new(expr),
                    bit: Box::new(bit),
                }
            }
        }
        ConstExpr::Function { name, args } => ConstExpr::Function {
            name,
            args: args
                .into_iter()
                .map(|arg| replace_oob_const_selects_with_unknown(arg, const_env))
                .collect(),
        },
        ConstExpr::Unary { op, expr } => ConstExpr::Unary {
            op,
            expr: Box::new(replace_oob_const_selects_with_unknown(*expr, const_env)),
        },
        ConstExpr::Binary { left, op, right } => ConstExpr::Binary {
            left: Box::new(replace_oob_const_selects_with_unknown(*left, const_env)),
            op,
            right: Box::new(replace_oob_const_selects_with_unknown(*right, const_env)),
        },
        ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => ConstExpr::Mux {
            condition: Box::new(replace_oob_const_selects_with_unknown(
                *condition, const_env,
            )),
            then_expr: Box::new(replace_oob_const_selects_with_unknown(
                *then_expr, const_env,
            )),
            else_expr: Box::new(replace_oob_const_selects_with_unknown(
                *else_expr, const_env,
            )),
        },
        ConstExpr::Ident(name) => ConstExpr::Ident(name),
        ConstExpr::Literal(value) => ConstExpr::Literal(value),
    }
}

fn const_expr_contains_unknown_literal(expr: &ConstExpr) -> bool {
    match expr {
        ConstExpr::Literal(literal) => typecheck::parse_integral_literal(literal)
            .is_some_and(|literal| literal.mask != Default::default()),
        ConstExpr::Ident(_) => false,
        ConstExpr::Select { expr, bit } => {
            const_expr_contains_unknown_literal(expr) || const_expr_contains_unknown_literal(bit)
        }
        ConstExpr::Function { args, .. } => args.iter().any(const_expr_contains_unknown_literal),
        ConstExpr::Unary { expr, .. } => const_expr_contains_unknown_literal(expr),
        ConstExpr::Binary { left, right, .. } => {
            const_expr_contains_unknown_literal(left) || const_expr_contains_unknown_literal(right)
        }
        ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            const_expr_contains_unknown_literal(condition)
                || const_expr_contains_unknown_literal(then_expr)
                || const_expr_contains_unknown_literal(else_expr)
        }
    }
}

fn const_expr_to_expr(expr: ConstExpr) -> Expr {
    match expr {
        ConstExpr::Literal(value) => Expr::Literal(value),
        ConstExpr::Ident(name) => Expr::Ident(name),
        ConstExpr::Select { expr, bit } => Expr::Select {
            expr: Box::new(const_expr_to_expr(*expr)),
            msb: (*bit).clone(),
            lsb: *bit,
            signed: false,
        },
        ConstExpr::Function { name, args } => Expr::Call {
            name,
            args: args.into_iter().map(const_expr_to_expr).collect(),
        },
        ConstExpr::Unary { op, expr } => Expr::Unary {
            op,
            expr: Box::new(const_expr_to_expr(*expr)),
        },
        ConstExpr::Binary { left, op, right } => Expr::Binary {
            left: Box::new(const_expr_to_expr(*left)),
            op,
            right: Box::new(const_expr_to_expr(*right)),
        },
        ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => Expr::Mux {
            condition: Box::new(const_expr_to_expr(*condition)),
            then_expr: Box::new(const_expr_to_expr(*then_expr)),
            else_expr: Box::new(const_expr_to_expr(*else_expr)),
        },
    }
}

fn format_typed_parameter_literal(value: i128, width: usize, signed: bool) -> String {
    let signing = if signed { "s" } else { "" };
    if width <= 128 {
        let mask = if width == 128 {
            u128::MAX
        } else {
            (1u128 << width) - 1
        };
        let bits = (value as u128) & mask;
        format!("{width}'{signing}d{bits}")
    } else {
        let extension = if value.is_negative() { '1' } else { '0' };
        let high_bits = extension.to_string().repeat(width - 128);
        let low_bits = value as u128;
        format!("{width}'{signing}b{high_bits}{low_bits:0128b}")
    }
}

#[derive(Clone, Copy)]
pub(crate) struct ExprType {
    pub(crate) width: usize,
    pub(crate) signed: bool,
}

fn substitute_typed_parameter_literals(
    expr: ConstExpr,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, ExprType>,
) -> ConstExpr {
    match expr {
        ConstExpr::Ident(name) => match (constants.get(&name), parameter_types.get(&name)) {
            (Some(value), Some(r#type)) => ConstExpr::Literal(format_typed_parameter_literal(
                *value,
                r#type.width,
                r#type.signed,
            )),
            _ => ConstExpr::Ident(name),
        },
        ConstExpr::Literal(value) => ConstExpr::Literal(value),
        ConstExpr::Select { expr, bit } => ConstExpr::Select {
            expr: Box::new(substitute_typed_parameter_literals(
                *expr,
                constants,
                parameter_types,
            )),
            bit: Box::new(substitute_typed_parameter_literals(
                *bit,
                constants,
                parameter_types,
            )),
        },
        ConstExpr::Function { name, args } => ConstExpr::Function {
            name,
            args: args
                .into_iter()
                .map(|arg| substitute_typed_parameter_literals(arg, constants, parameter_types))
                .collect(),
        },
        ConstExpr::Unary { op, expr } => ConstExpr::Unary {
            op,
            expr: Box::new(substitute_typed_parameter_literals(
                *expr,
                constants,
                parameter_types,
            )),
        },
        ConstExpr::Binary { left, op, right } => ConstExpr::Binary {
            left: Box::new(substitute_typed_parameter_literals(
                *left,
                constants,
                parameter_types,
            )),
            op,
            right: Box::new(substitute_typed_parameter_literals(
                *right,
                constants,
                parameter_types,
            )),
        },
        ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => ConstExpr::Mux {
            condition: Box::new(substitute_typed_parameter_literals(
                *condition,
                constants,
                parameter_types,
            )),
            then_expr: Box::new(substitute_typed_parameter_literals(
                *then_expr,
                constants,
                parameter_types,
            )),
            else_expr: Box::new(substitute_typed_parameter_literals(
                *else_expr,
                constants,
                parameter_types,
            )),
        },
    }
}

fn infer_const_expr_type(
    expr: &ConstExpr,
    parameter_types: &HashMap<String, ExprType>,
) -> Option<ExprType> {
    match expr {
        ConstExpr::Literal(literal) => {
            let literal = typecheck::parse_integral_literal(literal)?;
            Some(ExprType {
                width: literal.width,
                signed: literal.signed,
            })
        }
        ConstExpr::Ident(name) => parameter_types.get(name).copied(),
        ConstExpr::Select { .. } => Some(ExprType {
            width: 1,
            signed: false,
        }),
        ConstExpr::Function { name, .. } => match name.as_str() {
            "$clog2" => Some(ExprType {
                width: 32,
                signed: true,
            }),
            "$onehot" | "$onehot0" => Some(ExprType {
                width: 1,
                signed: false,
            }),
            _ => None,
        },
        ConstExpr::Unary { op, expr } => {
            let operand = infer_const_expr_type(expr, parameter_types)?;
            if matches!(
                op,
                UnaryOp::LogicNot | UnaryOp::RedAnd | UnaryOp::RedOr | UnaryOp::RedXor
            ) {
                Some(ExprType {
                    width: 1,
                    signed: false,
                })
            } else {
                Some(operand)
            }
        }
        ConstExpr::Binary { left, op, right } => {
            let left = infer_const_expr_type(left, parameter_types)?;
            let right = infer_const_expr_type(right, parameter_types)?;
            match op {
                BinaryOp::LogicAnd
                | BinaryOp::LogicOr
                | BinaryOp::Eq
                | BinaryOp::Ne
                | BinaryOp::EqCase
                | BinaryOp::NeCase
                | BinaryOp::EqWildcard
                | BinaryOp::NeWildcard
                | BinaryOp::Lt
                | BinaryOp::Le
                | BinaryOp::Gt
                | BinaryOp::Ge => Some(ExprType {
                    width: 1,
                    signed: false,
                }),
                BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar => Some(left),
                _ => Some(ExprType {
                    width: left.width.max(right.width),
                    signed: left.signed && right.signed,
                }),
            }
        }
        ConstExpr::Mux {
            then_expr,
            else_expr,
            ..
        } => {
            let then_type = infer_const_expr_type(then_expr, parameter_types)?;
            let else_type = infer_const_expr_type(else_expr, parameter_types)?;
            Some(ExprType {
                width: then_type.width.max(else_type.width),
                signed: then_type.signed && else_type.signed,
            })
        }
    }
}

fn infer_parameter_value_type(
    value: &ConstExpr,
    has_declared_type: bool,
    parameter_types: &HashMap<String, ExprType>,
) -> Option<ExprType> {
    if !has_declared_type
        && let ConstExpr::Literal(literal) = value
        && matches!(
            literal.trim(),
            "'0" | "'1" | "'x" | "'X" | "'z" | "'Z" | "'?"
        )
    {
        Some(ExprType {
            width: 1,
            signed: false,
        })
    } else {
        infer_const_expr_type(value, parameter_types)
    }
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
}

type VariablePackedDimensions = HashMap<String, VariableDimensions>;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct PackedDimensions {
    variables: VariablePackedDimensions,
    const_env: HashMap<String, i128>,
    type_aliases: HashMap<String, Type>,
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

fn packed_dimensions_from_ports_and_signals(
    ports: &[Port],
    signals: &[Signal],
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> PackedDimensions {
    let mut dimensions = HashMap::default();
    for port in ports {
        dimensions.insert(
            port.name().to_string(),
            VariableDimensions {
                packed: packed_dimension_widths(port.r#type().packed_ranges()),
                unpacked: unpacked_dimension_widths(port.r#type().unpacked_ranges()),
                signed: port.r#type().is_signed(),
                is_2state: port.r#type().kind() == TypeKind::Bit,
            },
        );
    }
    for signal in signals {
        dimensions.insert(
            signal.name().to_string(),
            VariableDimensions {
                packed: packed_dimension_widths(signal.r#type().packed_ranges()),
                unpacked: unpacked_dimension_widths(signal.r#type().unpacked_ranges()),
                signed: signal.r#type().is_signed(),
                is_2state: signal.r#type().kind() == TypeKind::Bit,
            },
        );
    }
    PackedDimensions::new(dimensions, const_env, type_aliases)
}

fn packed_dimension_widths(ranges: &[PackedRange]) -> Vec<PackedDimension> {
    ranges
        .iter()
        .map(|range| {
            let left = range.left().clone();
            let right = range.right().clone();
            let width = |high: ConstExpr, low: ConstExpr| ConstExpr::Binary {
                left: Box::new(ConstExpr::Binary {
                    left: Box::new(high),
                    op: BinaryOp::Sub,
                    right: Box::new(low),
                }),
                op: BinaryOp::Add,
                right: Box::new(ConstExpr::Literal("1".to_string())),
            };
            let width = ConstExpr::Mux {
                condition: Box::new(ConstExpr::Binary {
                    left: Box::new(left.clone()),
                    op: BinaryOp::Ge,
                    right: Box::new(right.clone()),
                }),
                then_expr: Box::new(width(left.clone(), right.clone())),
                else_expr: Box::new(width(right.clone(), left.clone())),
            };
            PackedDimension {
                left,
                right,
                width,
                normalize_single: false,
            }
        })
        .collect()
}

fn unpacked_dimension_widths(ranges: &[UnpackedRange]) -> Vec<UnpackedDimension> {
    ranges
        .iter()
        .map(|range| {
            let left = range.left().clone();
            let right = range.right().clone();
            let width = ConstExpr::Mux {
                condition: Box::new(ConstExpr::Binary {
                    left: Box::new(left.clone()),
                    op: BinaryOp::Ge,
                    right: Box::new(right.clone()),
                }),
                then_expr: Box::new(ConstExpr::Binary {
                    left: Box::new(left.clone()),
                    op: BinaryOp::Sub,
                    right: Box::new(right.clone()),
                }),
                else_expr: Box::new(ConstExpr::Binary {
                    left: Box::new(right.clone()),
                    op: BinaryOp::Sub,
                    right: Box::new(left.clone()),
                }),
            };
            UnpackedDimension {
                left,
                right,
                width: add_expr(width, ConstExpr::Literal("1".to_string())),
            }
        })
        .collect()
}

fn function_packed_dimension_widths(ranges: &[PackedRange]) -> Vec<PackedDimension> {
    packed_dimension_widths(ranges)
        .into_iter()
        .map(|mut dimension| {
            dimension.normalize_single = true;
            dimension
        })
        .collect()
}

fn packed_dimensions_from_ref_node(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    type_aliases: &HashMap<String, Type>,
) -> Vec<PackedDimension> {
    if let Some(alias) = type_alias_from_ref_node(node.clone(), syntax_tree, type_aliases) {
        function_packed_dimension_widths(alias.packed_ranges())
    } else {
        function_packed_dimension_widths(&packed_ranges_from_ref_node(node, syntax_tree))
    }
}

fn parameter_marker(name: &str) -> String {
    format!("__parameter::{name}")
}

fn local_parameter_marker(name: &str) -> String {
    format!("__parameter::local::{name}")
}

fn enum_marker(name: &str) -> String {
    format!("__enum::{name}")
}

fn parameter_width_marker(name: &str) -> String {
    format!("__parameter::width::{name}")
}

fn parameter_signed_marker(name: &str) -> String {
    format!("__parameter::signed::{name}")
}

fn insert_parameter_type_markers(
    const_env: &mut HashMap<String, i128>,
    name: &str,
    r#type: ExprType,
) {
    if let Ok(width) = i128::try_from(r#type.width) {
        const_env.insert(parameter_width_marker(name), width);
        const_env.insert(parameter_signed_marker(name), r#type.signed as i128);
    }
}

fn parameter_types_from_const_env(const_env: &HashMap<String, i128>) -> HashMap<String, ExprType> {
    const PREFIX: &str = "__parameter::width::";
    const_env
        .iter()
        .filter_map(|(marker, width)| {
            let name = marker.strip_prefix(PREFIX)?;
            let width = usize::try_from(*width).ok()?;
            let signed = const_env
                .get(&parameter_signed_marker(name))
                .is_some_and(|signed| *signed != 0);
            Some((name.to_string(), ExprType { width, signed }))
        })
        .collect()
}

fn module_parameter_port_list(node: RefNode<'_>) -> Option<RefNode<'_>> {
    match node {
        RefNode::ModuleDeclarationAnsi(module) => module
            .nodes
            .0
            .nodes
            .5
            .as_ref()
            .map(RefNode::ParameterPortList),
        RefNode::ModuleDeclarationNonansi(module) => module
            .nodes
            .0
            .nodes
            .5
            .as_ref()
            .map(RefNode::ParameterPortList),
        _ => None,
    }
}

fn module_non_port_items(node: RefNode<'_>) -> Vec<&sv_parser::NonPortModuleItem> {
    match node {
        RefNode::ModuleDeclarationAnsi(module) => module.nodes.2.iter().collect(),
        RefNode::ModuleDeclarationNonansi(_) => Vec::new(),
        _ => Vec::new(),
    }
}

fn package_or_generate_declaration_from_non_port_item(
    item: &sv_parser::NonPortModuleItem,
) -> Option<&sv_parser::PackageOrGenerateItemDeclaration> {
    let sv_parser::NonPortModuleItem::ModuleOrGenerateItem(item) = item else {
        return None;
    };
    let sv_parser::ModuleOrGenerateItem::ModuleItem(item) = &**item else {
        return None;
    };
    let sv_parser::ModuleCommonItem::ModuleOrGenerateItemDeclaration(declaration) = &item.nodes.1
    else {
        return None;
    };
    let sv_parser::ModuleOrGenerateItemDeclaration::PackageOrGenerateItemDeclaration(declaration) =
        &**declaration
    else {
        return None;
    };
    Some(declaration)
}

fn parameter_name(node: RefNode<'_>, syntax_tree: &SyntaxTree) -> Result<String, AnalyzerError> {
    let locate = identifier_locate(node).ok_or_else(|| {
        AnalyzerError::Unsupported("unsupported parameter identifier".to_string())
    })?;
    syntax_tree
        .get_str(&locate)
        .map(str::to_string)
        .ok_or_else(|| AnalyzerError::Unsupported("invalid parameter identifier span".to_string()))
}

fn port_name(node: RefNode<'_>, syntax_tree: &SyntaxTree) -> Result<String, AnalyzerError> {
    let locate = identifier_locate(node)
        .ok_or_else(|| AnalyzerError::Unsupported("unsupported port identifier".to_string()))?;
    syntax_tree
        .get_str(&locate)
        .map(str::to_string)
        .ok_or_else(|| AnalyzerError::Unsupported("invalid port identifier span".to_string()))
}

fn instances_from_module_node(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
) -> Result<Vec<Instance>, AnalyzerError> {
    let type_aliases = type_aliases_from_module_node(node.clone(), syntax_tree)?;
    for child in node.clone() {
        let RefNode::ModuleInstantiation(instantiation) = child else {
            continue;
        };
        let module_name = identifier_text(
            RefNode::ModuleIdentifier(&instantiation.nodes.0),
            syntax_tree,
        )
        .ok_or_else(|| {
            AnalyzerError::Unsupported("unsupported module instantiation identifier".to_string())
        })?;
        if !type_aliases.contains_key(&module_name)
            && instantiation
                .nodes
                .2
                .contents()
                .iter()
                .any(|instance| !instance.nodes.0.nodes.1.is_empty())
        {
            return Err(AnalyzerError::Unsupported(
                "module instance array".to_string(),
            ));
        }
    }
    let mut instances = Vec::new();
    for item in module_non_port_items(node) {
        instances_from_non_port_module_item(
            item,
            None,
            syntax_tree,
            const_env,
            packed_dimensions,
            &mut instances,
        )?;
    }
    instances.retain(|instance| !type_aliases.contains_key(instance.module_name()));
    Ok(instances)
}

fn instances_from_non_port_module_item(
    item: &sv_parser::NonPortModuleItem,
    condition: Option<ConstExpr>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
    instances: &mut Vec<Instance>,
) -> Result<(), AnalyzerError> {
    match item {
        sv_parser::NonPortModuleItem::GenerateRegion(region) => {
            for item in &region.nodes.1 {
                instances_from_generate_item(
                    item,
                    condition.clone(),
                    syntax_tree,
                    const_env,
                    packed_dimensions,
                    instances,
                )?;
            }
        }
        sv_parser::NonPortModuleItem::ModuleOrGenerateItem(item) => {
            instances_from_module_or_generate_item(
                item,
                condition,
                syntax_tree,
                const_env,
                packed_dimensions,
                instances,
            )?;
        }
        _ => {}
    }
    Ok(())
}

fn instances_from_generate_item(
    item: &sv_parser::GenerateItem,
    condition: Option<ConstExpr>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
    instances: &mut Vec<Instance>,
) -> Result<(), AnalyzerError> {
    if let sv_parser::GenerateItem::ModuleOrGenerateItem(item) = item {
        instances_from_module_or_generate_item(
            item,
            condition,
            syntax_tree,
            const_env,
            packed_dimensions,
            instances,
        )?;
    }
    Ok(())
}

fn instances_from_module_or_generate_item(
    item: &sv_parser::ModuleOrGenerateItem,
    condition: Option<ConstExpr>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
    instances: &mut Vec<Instance>,
) -> Result<(), AnalyzerError> {
    match item {
        sv_parser::ModuleOrGenerateItem::Module(module) => {
            instances_from_module_instantiation(
                &module.nodes.1,
                condition,
                syntax_tree,
                const_env,
                packed_dimensions,
                instances,
            )?;
        }
        sv_parser::ModuleOrGenerateItem::ModuleItem(item) => {
            instances_from_module_common_item(
                &item.nodes.1,
                condition,
                syntax_tree,
                const_env,
                packed_dimensions,
                instances,
            )?;
        }
        _ => {}
    }
    Ok(())
}

fn instances_from_module_common_item(
    item: &sv_parser::ModuleCommonItem,
    condition: Option<ConstExpr>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
    instances: &mut Vec<Instance>,
) -> Result<(), AnalyzerError> {
    match item {
        sv_parser::ModuleCommonItem::ConditionalGenerateConstruct(generate) => {
            instances_from_conditional_generate(
                generate,
                condition,
                syntax_tree,
                const_env,
                packed_dimensions,
                instances,
            )?;
        }
        sv_parser::ModuleCommonItem::ModuleOrGenerateItemDeclaration(_) => {}
        _ => {}
    }
    Ok(())
}

fn instances_from_conditional_generate(
    generate: &sv_parser::ConditionalGenerateConstruct,
    condition: Option<ConstExpr>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
    instances: &mut Vec<Instance>,
) -> Result<(), AnalyzerError> {
    let sv_parser::ConditionalGenerateConstruct::If(generate) = generate else {
        return Ok(());
    };
    let Some(generate_condition) = const_expr_from_ref_node(
        RefNode::ConstantExpression(&generate.nodes.1.nodes.1),
        syntax_tree,
    ) else {
        return Err(AnalyzerError::Unsupported(
            "conditional-generate condition lowering".to_string(),
        ));
    };
    let generate_condition = substitute_const_expr_constants(generate_condition, const_env);
    let then_condition = combine_conditions(condition.clone(), generate_condition.clone());
    instances_from_generate_block(
        &generate.nodes.2,
        then_condition,
        syntax_tree,
        const_env,
        packed_dimensions,
        instances,
    )?;
    if let Some((_, block)) = &generate.nodes.3 {
        let else_condition = combine_conditions(
            condition,
            ConstExpr::Unary {
                op: UnaryOp::LogicNot,
                expr: Box::new(generate_condition),
            },
        );
        instances_from_generate_block(
            block,
            else_condition,
            syntax_tree,
            const_env,
            packed_dimensions,
            instances,
        )?;
    }
    Ok(())
}

fn instances_from_generate_block(
    block: &sv_parser::GenerateBlock,
    condition: Option<ConstExpr>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
    instances: &mut Vec<Instance>,
) -> Result<(), AnalyzerError> {
    match block {
        sv_parser::GenerateBlock::GenerateItem(item) => {
            instances_from_generate_item(
                item,
                condition,
                syntax_tree,
                const_env,
                packed_dimensions,
                instances,
            )?;
        }
        sv_parser::GenerateBlock::Multiple(block) => {
            let mut block_env = const_env.clone();
            for item in &block.nodes.3 {
                if add_localparams_from_generate_item(item, syntax_tree, &mut block_env) {
                    continue;
                }
                instances_from_generate_item(
                    item,
                    condition.clone(),
                    syntax_tree,
                    &block_env,
                    packed_dimensions,
                    instances,
                )?;
            }
        }
    }
    Ok(())
}

fn combine_conditions(parent: Option<ConstExpr>, child: ConstExpr) -> Option<ConstExpr> {
    Some(match parent {
        Some(parent) => ConstExpr::Binary {
            left: Box::new(parent),
            op: BinaryOp::LogicAnd,
            right: Box::new(child),
        },
        None => child,
    })
}

fn instances_from_module_instantiation(
    instantiation: &sv_parser::ModuleInstantiation,
    condition: Option<ConstExpr>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
    instances: &mut Vec<Instance>,
) -> Result<(), AnalyzerError> {
    let module_name = identifier_text(
        RefNode::ModuleIdentifier(&instantiation.nodes.0),
        syntax_tree,
    )
    .ok_or_else(|| {
        AnalyzerError::Unsupported("unsupported module instantiation identifier".to_string())
    })?;
    let mut parameter_overrides =
        parameter_overrides_from_value_assignment(instantiation.nodes.1.as_ref(), syntax_tree)?;
    for override_ in &mut parameter_overrides {
        override_.value = override_
            .value
            .take()
            .map(|value| substitute_const_expr_constants_preserving_enum_types(value, const_env));
    }
    let condition =
        condition.map(|condition| substitute_const_expr_constants(condition, const_env));
    let parameter_names: Vec<String> = parameter_overrides
        .iter()
        .map(|parameter| parameter.name().to_string())
        .collect();
    for instance in instantiation.nodes.2.contents() {
        let name = identifier_text(
            RefNode::InstanceIdentifier(&instance.nodes.0.nodes.0),
            syntax_tree,
        )
        .ok_or_else(|| AnalyzerError::Unsupported("unsupported instance identifier".to_string()))?;
        let mut port_connections =
            port_connections_from_hierarchical_instance(instance, syntax_tree, packed_dimensions)?;
        for connection in &mut port_connections {
            connection.actual_expr = connection.actual_expr.take().map(|expr| {
                substitute_expr_constants_with_parameter_literals(
                    expr,
                    const_env,
                    &HashMap::default(),
                )
            });
        }
        let port_names = port_connections
            .iter()
            .map(|connection| connection.formal().to_string())
            .collect();
        instances.push(Instance::new(
            module_name.clone(),
            name,
            parameter_names.clone(),
            parameter_overrides.clone(),
            condition.clone(),
            port_names,
            port_connections,
        ));
    }
    Ok(())
}

fn parameter_overrides_from_value_assignment(
    assignment: Option<&sv_parser::ParameterValueAssignment>,
    syntax_tree: &SyntaxTree,
) -> Result<Vec<ParameterOverride>, AnalyzerError> {
    let Some(assignment) = assignment else {
        return Ok(Vec::new());
    };
    let Some(assignments) = assignment.nodes.1.nodes.1.as_ref() else {
        return Ok(Vec::new());
    };
    let sv_parser::ListOfParameterAssignments::Named(assignments) = assignments else {
        return Err(AnalyzerError::Unsupported(
            "ordered parameter assignment".to_string(),
        ));
    };
    let mut overrides = Vec::new();
    let mut names = HashSet::default();
    for assignment in assignments.nodes.0.contents() {
        let name = identifier_text(
            RefNode::ParameterIdentifier(&assignment.nodes.1),
            syntax_tree,
        )
        .ok_or_else(|| AnalyzerError::Unsupported("parameter override identifier".to_string()))?;
        if !names.insert(name.clone()) {
            return Err(AnalyzerError::Unsupported(format!(
                "duplicate parameter override `{name}`"
            )));
        }
        let value = match assignment.nodes.2.nodes.1.as_ref() {
            Some(expr) => Some(
                const_expr_from_param_expression(expr, syntax_tree).ok_or_else(|| {
                    AnalyzerError::Unsupported("parameter override expression".to_string())
                })?,
            ),
            None => {
                return Err(AnalyzerError::Unsupported(format!(
                    "empty parameter override `{name}`"
                )));
            }
        };
        overrides.push(ParameterOverride::new(name, value));
    }
    Ok(overrides)
}

fn port_connections_from_hierarchical_instance(
    instance: &sv_parser::HierarchicalInstance,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Result<Vec<PortConnection>, AnalyzerError> {
    let Some(connections) = instance.nodes.1.nodes.1.as_ref() else {
        return Ok(Vec::new());
    };
    let sv_parser::ListOfPortConnections::Named(connections) = connections else {
        if let sv_parser::ListOfPortConnections::Ordered(connections) = connections
            && connections
                .nodes
                .0
                .contents()
                .iter()
                .all(|connection| connection.nodes.1.is_none())
        {
            return Ok(Vec::new());
        }
        return Err(AnalyzerError::Unsupported(
            "ordered port connection".to_string(),
        ));
    };
    let mut lowered = Vec::new();
    for connection in connections.nodes.0.contents() {
        match connection {
            sv_parser::NamedPortConnection::Identifier(connection) => {
                let formal =
                    identifier_text(RefNode::PortIdentifier(&connection.nodes.2), syntax_tree)
                        .ok_or_else(|| {
                            AnalyzerError::Unsupported(
                                "named port connection identifier".to_string(),
                            )
                        })?;
                let actual_expr = match connection.nodes.3.as_ref() {
                    None => Some(Expr::Ident(formal.clone())),
                    Some(paren) => match paren.nodes.1.as_ref() {
                        None => None,
                        Some(expr) => Some(
                            expr_from_expression_with_types(expr, syntax_tree, packed_dimensions)
                                .ok_or_else(|| {
                                AnalyzerError::Unsupported(
                                    "named port connection expression".to_string(),
                                )
                            })?,
                        ),
                    },
                };
                let actual = actual_expr
                    .as_ref()
                    .and_then(expr_ident_name)
                    .unwrap_or_else(|| formal.clone());
                lowered.push(PortConnection::new(formal, actual, actual_expr));
            }
            sv_parser::NamedPortConnection::Asterisk(_) => {}
        }
    }
    Ok(lowered)
}

fn expr_ident_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Ident(name) => Some(name.clone()),
        _ => None,
    }
}

fn identifier_text(node: RefNode<'_>, syntax_tree: &SyntaxTree) -> Option<String> {
    let locate = identifier_locate(node)?;
    syntax_tree.get_str(&locate).map(str::to_string)
}

fn functions_from_module_node(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
) -> Result<HashMap<String, Function>, AnalyzerError> {
    let mut functions = HashMap::default();
    let type_aliases = type_aliases_from_module_node(node.clone(), syntax_tree)?;
    let inactive_nodes = inactive_conditional_generate_nodes(node.clone(), syntax_tree, const_env);
    for child in node {
        if inactive_nodes.iter().any(|inactive| inactive == &child) {
            continue;
        }
        let RefNode::FunctionDeclaration(declaration) = child else {
            continue;
        };
        validate_function_return_type(declaration, syntax_tree, const_env, &type_aliases)?;
        validate_function_formal_types(declaration, syntax_tree, const_env, &type_aliases)?;
        validate_function_local_names(declaration, syntax_tree, const_env, &type_aliases)?;
        validate_function_declaration_statements(
            declaration,
            syntax_tree,
            const_env,
            &type_aliases,
            packed_dimensions,
        )?;
        if let Some(function) = function_from_declaration(
            declaration,
            syntax_tree,
            const_env,
            &type_aliases,
            packed_dimensions,
        ) {
            let name = function.name.clone();
            let mut parameter_names = HashSet::default();
            if let Some(parameter) = function
                .params
                .iter()
                .find(|parameter| !parameter_names.insert(parameter.name.as_str()))
            {
                return Err(AnalyzerError::Unsupported(format!(
                    "duplicate function argument `{}`",
                    parameter.name
                )));
            }
            if functions.insert(name.clone(), function).is_some() {
                return Err(AnalyzerError::Unsupported(format!(
                    "duplicate function declaration `{name}`"
                )));
            }
        }
    }
    Ok(functions)
}

fn validate_function_local_names(
    declaration: &sv_parser::FunctionDeclaration,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Result<(), AnalyzerError> {
    let (params, local_types) = match &declaration.nodes.2 {
        sv_parser::FunctionBodyDeclaration::WithPort(body) => {
            let params = body
                .nodes
                .3
                .nodes
                .1
                .as_ref()
                .map(|ports| tf_params(ports, syntax_tree, const_env, type_aliases))
                .unwrap_or_default();
            let local_types = function_local_types_from_block_items(
                &body.nodes.5,
                syntax_tree,
                const_env,
                type_aliases,
            )
            .unwrap_or_default();
            (params, local_types)
        }
        sv_parser::FunctionBodyDeclaration::WithoutPort(body) => {
            let params = tf_item_params(&body.nodes.4, syntax_tree, const_env, type_aliases);
            let block_items = body.nodes.4.iter().filter_map(|item| match item {
                sv_parser::TfItemDeclaration::BlockItemDeclaration(item) => Some(&**item),
                sv_parser::TfItemDeclaration::TfPortDeclaration(_) => None,
            });
            let local_types = function_local_types_from_block_item_iter(
                block_items,
                syntax_tree,
                const_env,
                type_aliases,
            )
            .unwrap_or_default();
            (params, local_types)
        }
    };
    if let Some(param) = params
        .iter()
        .find(|param| local_types.contains_key(&param.name))
    {
        Err(AnalyzerError::Unsupported(format!(
            "function local shadows formal `{}`",
            param.name
        )))
    } else {
        Ok(())
    }
}

fn validate_function_return_type(
    declaration: &sv_parser::FunctionDeclaration,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Result<(), AnalyzerError> {
    let node = match &declaration.nodes.2 {
        sv_parser::FunctionBodyDeclaration::WithPort(body) => &body.nodes.0,
        sv_parser::FunctionBodyDeclaration::WithoutPort(body) => &body.nodes.0,
    };
    let unsupported = match node {
        sv_parser::FunctionDataTypeOrImplicit::DataTypeOrVoid(data_type) => {
            let sv_parser::DataTypeOrVoid::DataType(data_type) = &**data_type else {
                return Ok(());
            };
            let r#type = type_from_ref_node(RefNode::DataType(data_type), syntax_tree)
                .or_else(|| type_alias_from_data_type(data_type, syntax_tree, type_aliases));
            r#type
                .as_ref()
                .is_some_and(|r#type| !r#type.unpacked_ranges().is_empty())
                || value_type_from_data_type(data_type, syntax_tree, const_env, type_aliases)
                    .is_none()
        }
        sv_parser::FunctionDataTypeOrImplicit::ImplicitDataType(_) => false,
    };
    if unsupported {
        Err(AnalyzerError::Unsupported(
            "unsupported function return data type or unpacked dimension".to_string(),
        ))
    } else {
        Ok(())
    }
}

fn validate_function_formal_types(
    declaration: &sv_parser::FunctionDeclaration,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Result<(), AnalyzerError> {
    let unsupported = |node: &sv_parser::DataTypeOrImplicit| {
        matches!(node, sv_parser::DataTypeOrImplicit::DataType(_))
            && value_type_from_data_type_or_implicit(node, syntax_tree, const_env, type_aliases)
                .is_none()
    };
    let has_unpacked_dimensions =
        |node: &sv_parser::DataTypeOrImplicit, dimensions: &[sv_parser::VariableDimension]| {
            !dimensions.is_empty()
                || type_from_ref_node(RefNode::DataTypeOrImplicit(node), syntax_tree)
                    .or_else(|| {
                        type_alias_from_ref_node(
                            RefNode::DataTypeOrImplicit(node),
                            syntax_tree,
                            type_aliases,
                        )
                    })
                    .is_some_and(|r#type| !r#type.unpacked_ranges().is_empty())
        };
    let invalid = match &declaration.nodes.2 {
        sv_parser::FunctionBodyDeclaration::WithPort(body) => {
            body.nodes.3.nodes.1.as_ref().is_some_and(|ports| {
                ports
                    .nodes
                    .0
                    .contents()
                    .iter()
                    // A type-only item can be the parser's representation of
                    // the shorthand `input logic a, b`; only validate entries
                    // that actually carry a formal identifier here.
                    .any(|port| {
                        port.nodes.4.as_ref().is_some_and(|(_, dimensions, _)| {
                            unsupported(&port.nodes.3)
                                || has_unpacked_dimensions(&port.nodes.3, dimensions)
                        })
                    })
            })
        }
        sv_parser::FunctionBodyDeclaration::WithoutPort(body) => body.nodes.4.iter().any(|item| {
            let sv_parser::TfItemDeclaration::TfPortDeclaration(port) = item else {
                return false;
            };
            unsupported(&port.nodes.3)
                || port
                    .nodes
                    .4
                    .nodes
                    .0
                    .contents()
                    .iter()
                    .any(|(_, dimensions, _)| has_unpacked_dimensions(&port.nodes.3, dimensions))
        }),
    };
    if invalid {
        Err(AnalyzerError::Unsupported(
            "unsupported function formal data type or unpacked dimension".to_string(),
        ))
    } else {
        Ok(())
    }
}

fn validate_function_declaration_statements(
    declaration: &sv_parser::FunctionDeclaration,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
    packed_dimensions: &PackedDimensions,
) -> Result<(), AnalyzerError> {
    let (statements, params, local_types) = match &declaration.nodes.2 {
        sv_parser::FunctionBodyDeclaration::WithPort(body) => {
            let params = body
                .nodes
                .3
                .nodes
                .1
                .as_ref()
                .map(|ports| tf_params(ports, syntax_tree, const_env, type_aliases))
                .unwrap_or_default();
            let local_types = function_local_types_from_block_items(
                &body.nodes.5,
                syntax_tree,
                const_env,
                type_aliases,
            )
            .ok_or_else(|| {
                AnalyzerError::Unsupported("unsupported function local data type".to_string())
            })?;
            (&body.nodes.6, params, local_types)
        }
        sv_parser::FunctionBodyDeclaration::WithoutPort(body) => {
            let params = tf_item_params(&body.nodes.4, syntax_tree, const_env, type_aliases);
            let block_items = body.nodes.4.iter().filter_map(|item| match item {
                sv_parser::TfItemDeclaration::BlockItemDeclaration(item) => Some(&**item),
                sv_parser::TfItemDeclaration::TfPortDeclaration(_) => None,
            });
            let local_types = function_local_types_from_block_item_iter(
                block_items,
                syntax_tree,
                const_env,
                type_aliases,
            )
            .ok_or_else(|| {
                AnalyzerError::Unsupported("unsupported function local data type".to_string())
            })?;
            (&body.nodes.5, params, local_types)
        }
    };
    let mut assignment_targets = local_types.into_keys().collect::<HashSet<_>>();
    assignment_targets.extend(params.into_iter().map(|param| param.name));
    for node in RefNode::FunctionDeclaration(declaration) {
        let RefNode::BlockingAssignment(assignment) = node else {
            continue;
        };
        let lhs = match assignment {
            sv_parser::BlockingAssignment::Variable(assignment) => &assignment.nodes.0,
            sv_parser::BlockingAssignment::OperatorAssignment(assignment) => &assignment.nodes.0,
            _ => continue,
        };
        let Some(LValue::Ident(name)) =
            variable_lvalue_from_node(lhs, syntax_tree, packed_dimensions)
        else {
            continue;
        };
        if !assignment_targets.contains(&name) {
            return Err(AnalyzerError::Unsupported(format!(
                "function assignment target outside local scope `{name}`"
            )));
        }
    }
    for statement in statements {
        let sv_parser::FunctionStatementOrNull::Statement(statement) = statement else {
            continue;
        };
        validate_function_statement(&statement.nodes.0, syntax_tree, packed_dimensions)?;
    }
    Ok(())
}

fn validate_function_statement_or_null(
    statement: &sv_parser::StatementOrNull,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Result<(), AnalyzerError> {
    let sv_parser::StatementOrNull::Statement(statement) = statement else {
        return Ok(());
    };
    validate_function_statement(statement, syntax_tree, packed_dimensions)
}

fn validate_function_statement(
    statement: &sv_parser::Statement,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Result<(), AnalyzerError> {
    match &statement.nodes.2 {
        sv_parser::StatementItem::JumpStatement(statement)
            if matches!(&**statement, sv_parser::JumpStatement::Return(_)) =>
        {
            let sv_parser::JumpStatement::Return(statement) = &**statement else {
                unreachable!();
            };
            let expr = statement.nodes.1.as_ref().ok_or_else(|| {
                AnalyzerError::Unsupported("expressionless function return".to_string())
            })?;
            expr_from_expression_with_types(expr, syntax_tree, packed_dimensions).ok_or_else(
                || AnalyzerError::Unsupported("unsupported function return expression".to_string()),
            )?;
            Ok(())
        }
        sv_parser::StatementItem::BlockingAssignment(assignment) => {
            let rhs = match &assignment.0 {
                sv_parser::BlockingAssignment::Variable(assignment) => &assignment.nodes.3,
                sv_parser::BlockingAssignment::OperatorAssignment(assignment) => {
                    &assignment.nodes.2
                }
                _ => {
                    return Err(AnalyzerError::Unsupported(
                        "unsupported function assignment".to_string(),
                    ));
                }
            };
            expr_from_expression_with_types(rhs, syntax_tree, packed_dimensions).ok_or_else(
                || {
                    AnalyzerError::Unsupported(
                        "unsupported function assignment expression".to_string(),
                    )
                },
            )?;
            Ok(())
        }
        sv_parser::StatementItem::SeqBlock(block) => {
            for statement in &block.nodes.3 {
                validate_function_statement_or_null(statement, syntax_tree, packed_dimensions)?;
            }
            Ok(())
        }
        sv_parser::StatementItem::ConditionalStatement(statement) => {
            expr_from_cond_predicate(&statement.nodes.2.nodes.1, syntax_tree, packed_dimensions)
                .ok_or_else(|| {
                    AnalyzerError::Unsupported(
                        "unsupported function conditional predicate".to_string(),
                    )
                })?;
            validate_function_statement_or_null(
                &statement.nodes.3,
                syntax_tree,
                packed_dimensions,
            )?;
            for (_, _, predicate, branch) in &statement.nodes.4 {
                expr_from_cond_predicate(&predicate.nodes.1, syntax_tree, packed_dimensions)
                    .ok_or_else(|| {
                        AnalyzerError::Unsupported(
                            "unsupported function conditional predicate".to_string(),
                        )
                    })?;
                validate_function_statement_or_null(branch, syntax_tree, packed_dimensions)?;
            }
            if let Some((_, branch)) = &statement.nodes.5 {
                validate_function_statement_or_null(branch, syntax_tree, packed_dimensions)?;
            }
            Ok(())
        }
        sv_parser::StatementItem::CaseStatement(statement) => {
            let sv_parser::CaseStatement::Normal(statement) = &**statement else {
                return Err(AnalyzerError::Unsupported(
                    "unsupported statement inside function".to_string(),
                ));
            };
            expr_from_expression_with_types(
                &statement.nodes.2.nodes.1.nodes.0,
                syntax_tree,
                packed_dimensions,
            )
            .ok_or_else(|| {
                AnalyzerError::Unsupported("unsupported function case selector".to_string())
            })?;
            for item in std::iter::once(&statement.nodes.3).chain(statement.nodes.4.iter()) {
                let branch = match item {
                    sv_parser::CaseItem::NonDefault(item) => {
                        for expr in item.nodes.0.contents() {
                            expr_from_expression_with_types(
                                &expr.nodes.0,
                                syntax_tree,
                                packed_dimensions,
                            )
                            .ok_or_else(|| {
                                AnalyzerError::Unsupported(
                                    "unsupported function case item expression".to_string(),
                                )
                            })?;
                        }
                        &item.nodes.2
                    }
                    sv_parser::CaseItem::Default(item) => &item.nodes.2,
                };
                validate_function_statement_or_null(branch, syntax_tree, packed_dimensions)?;
            }
            Ok(())
        }
        _ => Err(AnalyzerError::Unsupported(
            "unsupported statement inside function".to_string(),
        )),
    }
}

fn function_from_declaration(
    declaration: &sv_parser::FunctionDeclaration,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
    packed_dimensions: &PackedDimensions,
) -> Option<Function> {
    match &declaration.nodes.2 {
        sv_parser::FunctionBodyDeclaration::WithPort(body) => {
            let name = identifier_text(RefNode::FunctionIdentifier(&body.nodes.2), syntax_tree)?;
            let params = body
                .nodes
                .3
                .nodes
                .1
                .as_ref()
                .map(|ports| tf_params(ports, syntax_tree, const_env, type_aliases))
                .unwrap_or_default();
            let mut local_types = function_local_types_from_block_items(
                &body.nodes.5,
                syntax_tree,
                const_env,
                type_aliases,
            )?;
            let mut function_packed_dimensions = packed_dimensions.clone();
            function_packed_dimensions.extend(params.iter().map(|param| {
                (
                    param.name.clone(),
                    VariableDimensions {
                        packed: param.packed_dimensions.clone(),
                        unpacked: Vec::new(),
                        signed: param.signed,
                        is_2state: param.is_2state,
                    },
                )
            }));
            function_packed_dimensions.extend(function_local_packed_dimensions_from_block_items(
                &body.nodes.5,
                syntax_tree,
                const_env,
                type_aliases,
            )?);
            let local_names = local_types.keys().cloned().collect::<HashSet<_>>();
            insert_function_param_types(&params, &mut local_types);
            let expr = function_body_expr(
                &body.nodes.6,
                syntax_tree,
                &function_packed_dimensions,
                &local_types,
                &local_names,
            )?;
            let return_type =
                function_return_type(&body.nodes.0, syntax_tree, const_env, type_aliases);
            let return_is_2state =
                function_return_is_2state(&body.nodes.0, syntax_tree, type_aliases);
            Some(Function {
                name,
                params,
                body: expr,
                return_width: return_type.map(|r#type| r#type.width),
                return_signed: return_type.is_some_and(|r#type| r#type.signed),
                return_is_2state,
            })
        }
        sv_parser::FunctionBodyDeclaration::WithoutPort(body) => {
            let name = identifier_text(RefNode::FunctionIdentifier(&body.nodes.2), syntax_tree)?;
            let params = tf_item_params(&body.nodes.4, syntax_tree, const_env, type_aliases);
            let block_items = body
                .nodes
                .4
                .iter()
                .filter_map(|item| match item {
                    sv_parser::TfItemDeclaration::BlockItemDeclaration(item) => Some(&**item),
                    sv_parser::TfItemDeclaration::TfPortDeclaration(_) => None,
                })
                .collect::<Vec<_>>();
            let mut local_types = function_local_types_from_block_item_iter(
                block_items.iter().copied(),
                syntax_tree,
                const_env,
                type_aliases,
            )?;
            let mut function_packed_dimensions = packed_dimensions.clone();
            function_packed_dimensions.extend(params.iter().map(|param| {
                (
                    param.name.clone(),
                    VariableDimensions {
                        packed: param.packed_dimensions.clone(),
                        unpacked: Vec::new(),
                        signed: param.signed,
                        is_2state: param.is_2state,
                    },
                )
            }));
            function_packed_dimensions.extend(
                function_local_packed_dimensions_from_block_item_iter(
                    block_items.iter().copied(),
                    syntax_tree,
                    const_env,
                    type_aliases,
                )?,
            );
            let local_names = local_types.keys().cloned().collect::<HashSet<_>>();
            insert_function_param_types(&params, &mut local_types);
            let expr = function_body_expr(
                &body.nodes.5,
                syntax_tree,
                &function_packed_dimensions,
                &local_types,
                &local_names,
            )?;
            let return_type =
                function_return_type(&body.nodes.0, syntax_tree, const_env, type_aliases);
            let return_is_2state =
                function_return_is_2state(&body.nodes.0, syntax_tree, type_aliases);
            Some(Function {
                name,
                params,
                body: expr,
                return_width: return_type.map(|r#type| r#type.width),
                return_signed: return_type.is_some_and(|r#type| r#type.signed),
                return_is_2state,
            })
        }
    }
}

fn insert_function_param_types(
    params: &[FunctionParam],
    local_types: &mut HashMap<String, FunctionLocalType>,
) {
    for param in params {
        if let Some(width) = param.width {
            local_types.insert(
                param.name.clone(),
                FunctionLocalType {
                    width,
                    signed: param.signed,
                    is_2state: param.is_2state,
                },
            );
        }
    }
}

fn function_local_types_from_block_items(
    items: &[sv_parser::BlockItemDeclaration],
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Option<HashMap<String, FunctionLocalType>> {
    function_local_types_from_block_item_iter(items.iter(), syntax_tree, const_env, type_aliases)
}

fn function_local_packed_dimensions_from_block_items(
    items: &[sv_parser::BlockItemDeclaration],
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Option<VariablePackedDimensions> {
    function_local_packed_dimensions_from_block_item_iter(
        items.iter(),
        syntax_tree,
        const_env,
        type_aliases,
    )
}

fn function_local_packed_dimensions_from_block_item_iter<'a>(
    items: impl IntoIterator<Item = &'a sv_parser::BlockItemDeclaration>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Option<VariablePackedDimensions> {
    let mut dimensions = HashMap::default();
    for item in items {
        let sv_parser::BlockItemDeclaration::Data(item) = item else {
            continue;
        };
        let signals =
            signals_from_data_declaration(&item.nodes.1, syntax_tree, type_aliases, const_env)
                .ok()?;
        dimensions.extend(signals.into_iter().map(|signal| {
            (
                signal.name().to_string(),
                VariableDimensions {
                    packed: function_packed_dimension_widths(signal.r#type().packed_ranges()),
                    unpacked: unpacked_dimension_widths(signal.r#type().unpacked_ranges()),
                    signed: signal.r#type().is_signed(),
                    is_2state: signal.r#type().kind() == TypeKind::Bit,
                },
            )
        }));
    }
    Some(dimensions)
}

fn function_local_types_from_block_item_iter<'a>(
    items: impl IntoIterator<Item = &'a sv_parser::BlockItemDeclaration>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Option<HashMap<String, FunctionLocalType>> {
    let mut local_types = HashMap::default();
    for item in items {
        let sv_parser::BlockItemDeclaration::Data(item) = item else {
            continue;
        };
        let signals =
            signals_from_data_declaration(&item.nodes.1, syntax_tree, type_aliases, const_env)
                .ok()?;
        for signal in signals {
            let r#type = signal.r#type();
            if !r#type.unpacked_ranges().is_empty() {
                return None;
            }
            let width = if r#type.packed_ranges().is_empty() {
                1
            } else {
                r#type
                    .packed_ranges()
                    .iter()
                    .try_fold(1usize, |acc, range| {
                        let left = eval_ast_const_expr(range.left(), const_env)?;
                        let right = eval_ast_const_expr(range.right(), const_env)?;
                        acc.checked_mul(left.abs_diff(right) as usize + 1)
                    })?
            };
            local_types.insert(
                signal.name().to_string(),
                FunctionLocalType {
                    width,
                    signed: r#type.is_signed(),
                    is_2state: r#type.kind() == TypeKind::Bit,
                },
            );
        }
    }
    Some(local_types)
}

fn function_return_is_2state(
    node: &sv_parser::FunctionDataTypeOrImplicit,
    syntax_tree: &SyntaxTree,
    type_aliases: &HashMap<String, Type>,
) -> bool {
    let r#type = match node {
        sv_parser::FunctionDataTypeOrImplicit::DataTypeOrVoid(data_type) => match &**data_type {
            sv_parser::DataTypeOrVoid::DataType(data_type) => {
                type_from_ref_node(RefNode::DataType(data_type), syntax_tree)
                    .or_else(|| type_alias_from_data_type(data_type, syntax_tree, type_aliases))
            }
            sv_parser::DataTypeOrVoid::Void(_) => None,
        },
        sv_parser::FunctionDataTypeOrImplicit::ImplicitDataType(data_type) => {
            type_from_ref_node(RefNode::ImplicitDataType(data_type), syntax_tree).or_else(|| {
                type_alias_from_ref_node(
                    RefNode::ImplicitDataType(data_type),
                    syntax_tree,
                    type_aliases,
                )
            })
        }
    };
    r#type.is_some_and(|r#type| r#type.kind() == TypeKind::Bit)
}

fn function_return_type(
    node: &sv_parser::FunctionDataTypeOrImplicit,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Option<ExprType> {
    match node {
        sv_parser::FunctionDataTypeOrImplicit::DataTypeOrVoid(data_type) => match &**data_type {
            sv_parser::DataTypeOrVoid::DataType(data_type) => {
                value_type_from_data_type(data_type, syntax_tree, const_env, type_aliases)
            }
            sv_parser::DataTypeOrVoid::Void(_) => None,
        },
        sv_parser::FunctionDataTypeOrImplicit::ImplicitDataType(data_type) => {
            value_type_from_ref_node(
                RefNode::ImplicitDataType(data_type),
                syntax_tree,
                const_env,
                type_aliases,
            )
        }
    }
}

fn tf_params(
    list: &sv_parser::TfPortList,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Vec<FunctionParam> {
    let mut params = Vec::new();
    let mut previous_type = None;
    let mut previous_is_2state = false;
    let mut previous_packed_dimensions = Vec::new();
    for port in list.nodes.0.contents() {
        let type_node = RefNode::DataTypeOrImplicit(&port.nodes.3);
        let inferred_type = value_type_from_data_type_or_implicit(
            &port.nodes.3,
            syntax_tree,
            const_env,
            type_aliases,
        );
        let inferred_is_2state = type_from_ref_node(type_node.clone(), syntax_tree)
            .or_else(|| type_alias_from_ref_node(type_node.clone(), syntax_tree, type_aliases))
            .is_some_and(|r#type| r#type.kind() == TypeKind::Bit);
        let inferred_packed_dimensions =
            packed_dimensions_from_ref_node(type_node.clone(), syntax_tree, type_aliases);
        let omitted_type = matches!(
            port.nodes.3,
            sv_parser::DataTypeOrImplicit::ImplicitDataType(_)
        ) && is_signed_from_ref_node(type_node.clone()).is_none()
            && packed_ranges_from_ref_node(type_node.clone(), syntax_tree).is_empty();
        let (name, r#type, is_2state, packed_dimensions) =
            if let Some((identifier, _, _)) = port.nodes.4.as_ref() {
                let Some(name) = identifier_text(RefNode::PortIdentifier(identifier), syntax_tree)
                else {
                    continue;
                };
                let r#type = if port.nodes.1.is_none() && omitted_type {
                    previous_type.or(inferred_type)
                } else {
                    inferred_type
                };
                let is_2state = if port.nodes.1.is_none() && omitted_type {
                    previous_is_2state
                } else {
                    inferred_is_2state
                };
                let packed_dimensions = if port.nodes.1.is_none() && omitted_type {
                    previous_packed_dimensions.clone()
                } else {
                    inferred_packed_dimensions
                };
                (name, r#type, is_2state, packed_dimensions)
            } else {
                // An identifier following a comma is syntactically ambiguous with a
                // user-defined type. sv-parser represents the shorthand `a, b` as a
                // type-only item, so reinterpret an unknown type name as the next
                // parameter and inherit the preceding item's type.
                if type_alias_from_data_type_or_implicit(&port.nodes.3, syntax_tree, type_aliases)
                    .is_some()
                {
                    continue;
                }
                let Some(name) = identifier_text(type_node, syntax_tree) else {
                    continue;
                };
                let r#type = if port.nodes.1.is_none() {
                    previous_type
                } else {
                    Some(ExprType {
                        width: 1,
                        signed: false,
                    })
                };
                let is_2state = port.nodes.1.is_none() && previous_is_2state;
                let packed_dimensions = if port.nodes.1.is_none() {
                    previous_packed_dimensions.clone()
                } else {
                    inferred_packed_dimensions
                };
                (name, r#type, is_2state, packed_dimensions)
            };
        previous_type = r#type;
        previous_is_2state = is_2state;
        previous_packed_dimensions = packed_dimensions.clone();
        params.push(FunctionParam {
            name,
            width: r#type.map(|r#type| r#type.width),
            signed: r#type.is_some_and(|r#type| r#type.signed),
            is_2state,
            packed_dimensions,
        });
    }
    params
}

fn tf_item_params(
    items: &[sv_parser::TfItemDeclaration],
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Vec<FunctionParam> {
    let mut params = Vec::new();
    for item in items {
        let sv_parser::TfItemDeclaration::TfPortDeclaration(declaration) = item else {
            continue;
        };
        let r#type = value_type_from_data_type_or_implicit(
            &declaration.nodes.3,
            syntax_tree,
            const_env,
            type_aliases,
        );
        let is_2state = type_from_ref_node(
            RefNode::DataTypeOrImplicit(&declaration.nodes.3),
            syntax_tree,
        )
        .or_else(|| {
            type_alias_from_ref_node(
                RefNode::DataTypeOrImplicit(&declaration.nodes.3),
                syntax_tree,
                type_aliases,
            )
        })
        .is_some_and(|r#type| r#type.kind() == TypeKind::Bit);
        let packed_dimensions = packed_dimensions_from_ref_node(
            RefNode::DataTypeOrImplicit(&declaration.nodes.3),
            syntax_tree,
            type_aliases,
        );
        for (identifier, _, _) in declaration.nodes.4.nodes.0.contents() {
            let Some(name) = identifier_text(RefNode::PortIdentifier(identifier), syntax_tree)
            else {
                continue;
            };
            params.push(FunctionParam {
                name,
                width: r#type.map(|r#type| r#type.width),
                signed: r#type.is_some_and(|r#type| r#type.signed),
                is_2state,
                packed_dimensions: packed_dimensions.clone(),
            });
        }
    }
    params
}

fn value_type_from_data_type_or_implicit(
    node: &sv_parser::DataTypeOrImplicit,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Option<ExprType> {
    match node {
        sv_parser::DataTypeOrImplicit::DataType(data_type) => {
            value_type_from_data_type(data_type, syntax_tree, const_env, type_aliases)
        }
        sv_parser::DataTypeOrImplicit::ImplicitDataType(data_type) => value_type_from_ref_node(
            RefNode::ImplicitDataType(data_type),
            syntax_tree,
            const_env,
            type_aliases,
        ),
    }
}

fn value_type_from_data_type(
    node: &sv_parser::DataType,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Option<ExprType> {
    let r#type = type_from_ref_node(RefNode::DataType(node), syntax_tree)
        .or_else(|| type_alias_from_data_type(node, syntax_tree, type_aliases))?;
    let width = if r#type.packed_ranges().is_empty() {
        1
    } else {
        r#type
            .packed_ranges()
            .iter()
            .try_fold(1usize, |acc, range| {
                let left = eval_ast_const_expr(range.left(), const_env)?;
                let right = eval_ast_const_expr(range.right(), const_env)?;
                acc.checked_mul(left.abs_diff(right) as usize + 1)
            })?
    };
    Some(ExprType {
        width,
        signed: r#type.is_signed(),
    })
}

fn value_type_from_ref_node(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Option<ExprType> {
    let alias = type_alias_from_ref_node(node.clone(), syntax_tree, type_aliases);
    if alias.is_none()
        && let Some(r#type) = integer_atom_expr_type(node.clone())
    {
        return Some(r#type);
    }
    let direct_ranges;
    let ranges = if let Some(alias) = &alias {
        alias.packed_ranges()
    } else {
        direct_ranges = packed_ranges_from_ref_node(node.clone(), syntax_tree);
        &direct_ranges
    };
    let width = if ranges.is_empty() {
        1
    } else {
        ranges.iter().try_fold(1usize, |acc, range| {
            let left = eval_ast_const_expr(range.left(), const_env)?;
            let right = eval_ast_const_expr(range.right(), const_env)?;
            acc.checked_mul(left.abs_diff(right) as usize + 1)
        })?
    };
    Some(ExprType {
        width,
        signed: alias
            .as_ref()
            .map(|r#type| r#type.is_signed())
            .unwrap_or_else(|| is_signed_from_ref_node(node).unwrap_or(false)),
    })
}

fn integer_atom_expr_type(node: RefNode<'_>) -> Option<ExprType> {
    let atom = unwrap_node!(node.clone(), IntegerAtomType)?;
    let RefNode::IntegerAtomType(atom) = atom else {
        return None;
    };
    let (width, default_signed) = match atom {
        sv_parser::IntegerAtomType::Byte(_) => (8, true),
        sv_parser::IntegerAtomType::Shortint(_) => (16, true),
        sv_parser::IntegerAtomType::Int(_) | sv_parser::IntegerAtomType::Integer(_) => (32, true),
        sv_parser::IntegerAtomType::Longint(_) => (64, true),
        sv_parser::IntegerAtomType::Time(_) => (64, false),
    };
    Some(ExprType {
        width,
        signed: is_signed_from_ref_node(node).unwrap_or(default_signed),
    })
}

fn function_body_expr(
    statements: &[sv_parser::FunctionStatementOrNull],
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
    local_types: &HashMap<String, FunctionLocalType>,
    local_names: &HashSet<String>,
) -> Option<Expr> {
    let mut locals = local_types
        .iter()
        .map(|(name, r#type)| {
            let initial = if local_names.contains(name) {
                coerce_function_local_assignment(Expr::Literal("'x".to_string()), *r#type)
            } else {
                Expr::Ident(name.clone())
            };
            (name.clone(), initial)
        })
        .collect::<HashMap<_, _>>();
    for statement in statements {
        if let Some(expr) = function_expr_from_statement_or_null(
            statement,
            &mut locals,
            syntax_tree,
            packed_dimensions,
            local_types,
        ) {
            return Some(expr);
        }
    }
    None
}

fn function_expr_from_statement_or_null(
    statement: &sv_parser::FunctionStatementOrNull,
    locals: &mut HashMap<String, Expr>,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
    local_types: &HashMap<String, FunctionLocalType>,
) -> Option<Expr> {
    let sv_parser::FunctionStatementOrNull::Statement(statement) = statement else {
        return None;
    };
    function_expr_from_statement(
        &statement.nodes.0,
        locals,
        syntax_tree,
        packed_dimensions,
        local_types,
    )
}

fn function_expr_from_statement_or_null_stmt(
    statement: &sv_parser::StatementOrNull,
    locals: &mut HashMap<String, Expr>,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
    local_types: &HashMap<String, FunctionLocalType>,
) -> Option<Expr> {
    let sv_parser::StatementOrNull::Statement(statement) = statement else {
        return None;
    };
    function_expr_from_statement(
        statement,
        locals,
        syntax_tree,
        packed_dimensions,
        local_types,
    )
}

fn function_expr_from_statement(
    statement: &sv_parser::Statement,
    locals: &mut HashMap<String, Expr>,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
    local_types: &HashMap<String, FunctionLocalType>,
) -> Option<Expr> {
    match &statement.nodes.2 {
        sv_parser::StatementItem::JumpStatement(statement) => {
            let sv_parser::JumpStatement::Return(statement) = &**statement else {
                return None;
            };
            let expr = statement.nodes.1.as_ref()?;
            expr_from_expression_with_types(expr, syntax_tree, packed_dimensions)
                .map(|expr| substitute_expr_idents(expr, locals))
        }
        sv_parser::StatementItem::BlockingAssignment(assignment) => {
            let (lhs, rhs) = match &assignment.0 {
                sv_parser::BlockingAssignment::Variable(assignment) => (
                    variable_lvalue_from_node(&assignment.nodes.0, syntax_tree, packed_dimensions),
                    expr_from_expression_with_types(
                        &assignment.nodes.3,
                        syntax_tree,
                        packed_dimensions,
                    ),
                ),
                sv_parser::BlockingAssignment::OperatorAssignment(assignment) => {
                    let lhs = variable_lvalue_from_node(
                        &assignment.nodes.0,
                        syntax_tree,
                        packed_dimensions,
                    );
                    let rhs = expr_from_expression_with_types(
                        &assignment.nodes.2,
                        syntax_tree,
                        packed_dimensions,
                    );
                    let op = syntax_tree.get_str(&assignment.nodes.1.nodes.0.nodes.0);
                    let rhs = match (&lhs, rhs, op) {
                        (_, Some(rhs), Some("=")) => Some(rhs),
                        (Some(lhs), Some(rhs), Some(op)) => {
                            assignment_op_expr(lhs, op, rhs, packed_dimensions)
                        }
                        _ => None,
                    };
                    (lhs, rhs)
                }
                _ => (None, None),
            };
            let Some(LValue::Ident(name)) = lhs else {
                return None;
            };
            if let Some(rhs) = rhs {
                let rhs = substitute_expr_idents(rhs, locals);
                let rhs = local_types
                    .get(&name)
                    .copied()
                    .map(|r#type| coerce_function_local_assignment(rhs.clone(), r#type))
                    .unwrap_or(rhs);
                locals.insert(name, rhs);
            }
            None
        }
        sv_parser::StatementItem::SeqBlock(block) => {
            let mut block_locals = locals.clone();
            for statement in &block.nodes.3 {
                if let Some(expr) = function_expr_from_statement_or_null_stmt(
                    statement,
                    &mut block_locals,
                    syntax_tree,
                    packed_dimensions,
                    local_types,
                ) {
                    return Some(expr);
                }
            }
            *locals = block_locals;
            None
        }
        sv_parser::StatementItem::ConditionalStatement(statement) => {
            function_expr_from_conditional_statement(
                statement,
                locals,
                syntax_tree,
                packed_dimensions,
                local_types,
            )
        }
        sv_parser::StatementItem::CaseStatement(statement) => function_expr_from_case_statement(
            statement,
            locals,
            syntax_tree,
            packed_dimensions,
            local_types,
        ),
        _ => None,
    }
}

fn function_expr_from_conditional_statement(
    statement: &sv_parser::ConditionalStatement,
    locals: &mut HashMap<String, Expr>,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
    local_types: &HashMap<String, FunctionLocalType>,
) -> Option<Expr> {
    let mut branches = Vec::new();
    let if_condition =
        expr_from_cond_predicate(&statement.nodes.2.nodes.1, syntax_tree, packed_dimensions)?;
    let mut then_locals = locals.clone();
    let then_expr = function_expr_from_statement_or_null_stmt(
        &statement.nodes.3,
        &mut then_locals,
        syntax_tree,
        packed_dimensions,
        local_types,
    );
    branches.push((
        procedural_truth_condition(substitute_expr_idents(if_condition, locals)),
        then_expr,
        then_locals,
    ));

    for (_, _, predicate, branch) in &statement.nodes.4 {
        let condition =
            expr_from_cond_predicate(&predicate.nodes.1, syntax_tree, packed_dimensions)?;
        let mut branch_locals = locals.clone();
        let branch_expr = function_expr_from_statement_or_null_stmt(
            branch,
            &mut branch_locals,
            syntax_tree,
            packed_dimensions,
            local_types,
        );
        branches.push((
            procedural_truth_condition(substitute_expr_idents(condition, locals)),
            branch_expr,
            branch_locals,
        ));
    }

    let (else_expr, else_locals) = if let Some((_, branch)) = &statement.nodes.5 {
        let mut branch_locals = locals.clone();
        (
            function_expr_from_statement_or_null_stmt(
                branch,
                &mut branch_locals,
                syntax_tree,
                packed_dimensions,
                local_types,
            ),
            branch_locals,
        )
    } else {
        (None, locals.clone())
    };

    if branches.iter().all(|(_, expr, _)| expr.is_some()) && else_expr.is_some() {
        let mut result = else_expr?;
        for (condition, branch_expr, _) in branches.into_iter().rev() {
            result = Expr::Mux {
                condition: Box::new(condition),
                then_expr: Box::new(branch_expr?),
                else_expr: Box::new(result),
            };
        }
        return Some(result);
    }

    if branches.iter().all(|(_, expr, _)| expr.is_none()) && else_expr.is_none() {
        let mut merged = locals.clone();
        let mut names = locals.keys().cloned().collect::<HashSet<_>>();
        names.extend(else_locals.keys().cloned());
        names.extend(
            branches
                .iter()
                .flat_map(|(_, _, branch_locals)| branch_locals.keys().cloned()),
        );
        for name in names {
            let mut value = else_locals
                .get(&name)
                .or_else(|| locals.get(&name))
                .cloned()
                .unwrap_or_else(|| Expr::Ident(name.clone()));
            for (condition, _, branch_locals) in branches.iter().rev() {
                let branch_value = branch_locals
                    .get(&name)
                    .cloned()
                    .unwrap_or_else(|| value.clone());
                if branch_value != value {
                    value = Expr::Mux {
                        condition: Box::new(condition.clone()),
                        then_expr: Box::new(branch_value),
                        else_expr: Box::new(value),
                    };
                }
            }
            merged.insert(name, value);
        }
        *locals = merged;
        return None;
    }

    Some(Expr::Call {
        name: "$unsupported_mixed_function_conditional".to_string(),
        args: Vec::new(),
    })
}

fn coerce_function_local_assignment(expr: Expr, r#type: FunctionLocalType) -> Expr {
    let expr = Expr::Resize {
        expr: Box::new(expr),
        width: r#type.width,
        signed: r#type.signed,
    };
    if r#type.is_2state {
        Expr::Unary {
            op: UnaryOp::ToTwoState,
            expr: Box::new(expr),
        }
    } else {
        expr
    }
}

fn procedural_truth_condition(condition: Expr) -> Expr {
    Expr::Unary {
        op: UnaryOp::RedOr,
        expr: Box::new(Expr::Unary {
            op: UnaryOp::ToTwoState,
            expr: Box::new(condition),
        }),
    }
}

fn function_expr_from_case_statement(
    statement: &sv_parser::CaseStatement,
    locals: &mut HashMap<String, Expr>,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
    local_types: &HashMap<String, FunctionLocalType>,
) -> Option<Expr> {
    let sv_parser::CaseStatement::Normal(statement) = statement else {
        return None;
    };
    let case_expr = expr_from_expression_with_types(
        &statement.nodes.2.nodes.1.nodes.0,
        syntax_tree,
        packed_dimensions,
    )?;
    let case_expr = substitute_expr_idents(case_expr, locals);
    let mut default_branch = (None, locals.clone());
    let mut branches = Vec::new();
    for item in std::iter::once(&statement.nodes.3).chain(statement.nodes.4.iter()) {
        match item {
            sv_parser::CaseItem::NonDefault(item) => {
                let mut branch_locals = locals.clone();
                let branch_expr = function_expr_from_statement_or_null_stmt(
                    &item.nodes.2,
                    &mut branch_locals,
                    syntax_tree,
                    packed_dimensions,
                    local_types,
                );
                let conditions = item
                    .nodes
                    .0
                    .contents()
                    .into_iter()
                    .filter_map(|expr| {
                        expr_from_expression_with_types(
                            &expr.nodes.0,
                            syntax_tree,
                            packed_dimensions,
                        )
                    })
                    .map(|expr| substitute_expr_idents(expr, locals))
                    .map(|expr| case_item_condition(case_expr.clone(), expr))
                    .collect::<Vec<_>>();
                let condition = conditions.into_iter().reduce(|left, right| Expr::Binary {
                    left: Box::new(left),
                    op: BinaryOp::LogicOr,
                    right: Box::new(right),
                })?;
                branches.push((condition, branch_expr, branch_locals));
            }
            sv_parser::CaseItem::Default(item) => {
                let mut branch_locals = locals.clone();
                default_branch = (
                    function_expr_from_statement_or_null_stmt(
                        &item.nodes.2,
                        &mut branch_locals,
                        syntax_tree,
                        packed_dimensions,
                        local_types,
                    ),
                    branch_locals,
                );
            }
        }
    }

    if branches.iter().all(|(_, expr, _)| expr.is_some()) && default_branch.0.is_some() {
        let mut result = default_branch.0?;
        for (condition, branch_expr, _) in branches.into_iter().rev() {
            result = Expr::Mux {
                condition: Box::new(condition),
                then_expr: Box::new(branch_expr?),
                else_expr: Box::new(result),
            };
        }
        return Some(result);
    }

    if branches.iter().all(|(_, expr, _)| expr.is_none()) && default_branch.0.is_none() {
        let mut merged = locals.clone();
        let mut names = locals.keys().cloned().collect::<HashSet<_>>();
        names.extend(default_branch.1.keys().cloned());
        names.extend(
            branches
                .iter()
                .flat_map(|(_, _, branch_locals)| branch_locals.keys().cloned()),
        );
        for name in names {
            let mut value = default_branch
                .1
                .get(&name)
                .or_else(|| locals.get(&name))
                .cloned()
                .unwrap_or_else(|| Expr::Ident(name.clone()));
            for (condition, _, branch_locals) in branches.iter().rev() {
                let branch_value = branch_locals
                    .get(&name)
                    .cloned()
                    .unwrap_or_else(|| value.clone());
                if branch_value != value {
                    value = Expr::Mux {
                        condition: Box::new(condition.clone()),
                        then_expr: Box::new(branch_value),
                        else_expr: Box::new(value),
                    };
                }
            }
            merged.insert(name, value);
        }
        *locals = merged;
        return None;
    }

    Some(Expr::Call {
        name: "$unsupported_mixed_function_case".to_string(),
        args: Vec::new(),
    })
}

fn case_item_condition(case_expr: Expr, item_expr: Expr) -> Expr {
    Expr::Binary {
        left: Box::new(case_expr),
        op: BinaryOp::EqCase,
        right: Box::new(item_expr),
    }
}

fn comb_processes_from_module_node(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
    functions: &HashMap<String, Function>,
    expression_signedness: &HashMap<String, bool>,
    parameter_literals: &HashMap<String, Expr>,
) -> Result<Vec<CombProcess>, AnalyzerError> {
    let mut processes = Vec::new();
    for item in module_non_port_items(node) {
        comb_processes_from_non_port_module_item(
            item,
            None,
            syntax_tree,
            const_env,
            packed_dimensions,
            functions,
            expression_signedness,
            parameter_literals,
            &mut processes,
        )?;
    }
    Ok(processes)
}

fn comb_processes_from_non_port_module_item(
    item: &sv_parser::NonPortModuleItem,
    condition: Option<ConstExpr>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
    functions: &HashMap<String, Function>,
    expression_signedness: &HashMap<String, bool>,
    parameter_literals: &HashMap<String, Expr>,
    processes: &mut Vec<CombProcess>,
) -> Result<(), AnalyzerError> {
    match item {
        sv_parser::NonPortModuleItem::GenerateRegion(region) => {
            for item in &region.nodes.1 {
                comb_processes_from_generate_item(
                    item,
                    condition.clone(),
                    syntax_tree,
                    const_env,
                    packed_dimensions,
                    functions,
                    expression_signedness,
                    parameter_literals,
                    processes,
                )?;
            }
        }
        sv_parser::NonPortModuleItem::ModuleOrGenerateItem(item) => {
            comb_processes_from_module_or_generate_item(
                item,
                condition,
                syntax_tree,
                const_env,
                packed_dimensions,
                functions,
                expression_signedness,
                parameter_literals,
                processes,
            )?;
        }
        _ => {}
    }
    Ok(())
}

fn comb_processes_from_generate_item(
    item: &sv_parser::GenerateItem,
    condition: Option<ConstExpr>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
    functions: &HashMap<String, Function>,
    expression_signedness: &HashMap<String, bool>,
    parameter_literals: &HashMap<String, Expr>,
    processes: &mut Vec<CombProcess>,
) -> Result<(), AnalyzerError> {
    if let sv_parser::GenerateItem::ModuleOrGenerateItem(item) = item {
        comb_processes_from_module_or_generate_item(
            item,
            condition,
            syntax_tree,
            const_env,
            packed_dimensions,
            functions,
            expression_signedness,
            parameter_literals,
            processes,
        )?;
    }
    Ok(())
}

fn comb_processes_from_module_or_generate_item(
    item: &sv_parser::ModuleOrGenerateItem,
    condition: Option<ConstExpr>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
    functions: &HashMap<String, Function>,
    expression_signedness: &HashMap<String, bool>,
    parameter_literals: &HashMap<String, Expr>,
    processes: &mut Vec<CombProcess>,
) -> Result<(), AnalyzerError> {
    if let sv_parser::ModuleOrGenerateItem::ModuleItem(item) = item {
        comb_processes_from_module_common_item(
            &item.nodes.1,
            condition,
            syntax_tree,
            const_env,
            packed_dimensions,
            functions,
            expression_signedness,
            parameter_literals,
            processes,
        )?;
    }
    Ok(())
}

fn comb_processes_from_module_common_item(
    item: &sv_parser::ModuleCommonItem,
    condition: Option<ConstExpr>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
    functions: &HashMap<String, Function>,
    expression_signedness: &HashMap<String, bool>,
    parameter_literals: &HashMap<String, Expr>,
    processes: &mut Vec<CombProcess>,
) -> Result<(), AnalyzerError> {
    match item {
        sv_parser::ModuleCommonItem::ContinuousAssign(assign) => {
            processes.extend(
                assignments_from_continuous_assign(assign, syntax_tree, packed_dimensions)?
                    .into_iter()
                    .map(|assignment| substitute_assignment_constants(assignment, const_env))
                    .map(|assignment| {
                        CombProcess::new(
                            CombProcessKind::ContinuousAssign,
                            condition.clone().map(|condition| {
                                substitute_const_expr_constants(condition, const_env)
                            }),
                            vec![assignment],
                        )
                    }),
            );
        }
        sv_parser::ModuleCommonItem::AlwaysConstruct(always) => {
            if let Some(process) = comb_process_from_always_construct(
                always,
                condition,
                syntax_tree,
                packed_dimensions,
                functions,
                expression_signedness,
                parameter_literals,
            )? {
                processes.push(substitute_process_constants(process, const_env));
            }
        }
        sv_parser::ModuleCommonItem::ConditionalGenerateConstruct(generate) => {
            comb_processes_from_conditional_generate(
                generate,
                condition,
                syntax_tree,
                const_env,
                packed_dimensions,
                functions,
                expression_signedness,
                parameter_literals,
                processes,
            )?;
        }
        sv_parser::ModuleCommonItem::LoopGenerateConstruct(generate) => {
            comb_processes_from_loop_generate(
                generate,
                condition,
                syntax_tree,
                const_env,
                packed_dimensions,
                functions,
                expression_signedness,
                parameter_literals,
                processes,
            )?;
        }
        sv_parser::ModuleCommonItem::NetAlias(_) => {
            return Err(AnalyzerError::Unsupported(
                "module-level net alias".to_string(),
            ));
        }
        _ => {}
    }
    Ok(())
}

fn comb_processes_from_conditional_generate(
    generate: &sv_parser::ConditionalGenerateConstruct,
    condition: Option<ConstExpr>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
    functions: &HashMap<String, Function>,
    expression_signedness: &HashMap<String, bool>,
    parameter_literals: &HashMap<String, Expr>,
    processes: &mut Vec<CombProcess>,
) -> Result<(), AnalyzerError> {
    let sv_parser::ConditionalGenerateConstruct::If(generate) = generate else {
        return Ok(());
    };
    let Some(generate_condition) = const_expr_from_ref_node(
        RefNode::ConstantExpression(&generate.nodes.1.nodes.1),
        syntax_tree,
    ) else {
        return Err(AnalyzerError::Unsupported(
            "conditional-generate condition lowering".to_string(),
        ));
    };
    if let Some(value) = eval_ast_const_expr(&generate_condition, const_env) {
        let block = if value != 0 {
            Some(&generate.nodes.2)
        } else {
            generate.nodes.3.as_ref().map(|(_, block)| block)
        };
        if let Some(block) = block {
            comb_processes_from_generate_block(
                block,
                condition,
                syntax_tree,
                const_env,
                packed_dimensions,
                functions,
                expression_signedness,
                parameter_literals,
                processes,
            )?;
        }
        return Ok(());
    }
    if has_local_constants(const_env)
        && eval_ast_const_expr(&generate_condition, const_env).is_none()
    {
        return Err(AnalyzerError::Unsupported(
            "unknown conditional-generate condition".to_string(),
        ));
    }
    let generate_condition = substitute_const_expr_constants(generate_condition, const_env);
    let then_condition = combine_conditions(condition.clone(), generate_condition.clone());
    comb_processes_from_generate_block(
        &generate.nodes.2,
        then_condition,
        syntax_tree,
        const_env,
        packed_dimensions,
        functions,
        expression_signedness,
        parameter_literals,
        processes,
    )?;
    if let Some((_, block)) = &generate.nodes.3 {
        let else_condition = combine_conditions(
            condition,
            ConstExpr::Unary {
                op: UnaryOp::LogicNot,
                expr: Box::new(generate_condition),
            },
        );
        comb_processes_from_generate_block(
            block,
            else_condition,
            syntax_tree,
            const_env,
            packed_dimensions,
            functions,
            expression_signedness,
            parameter_literals,
            processes,
        )?;
    }
    Ok(())
}

fn comb_processes_from_loop_generate(
    generate: &sv_parser::LoopGenerateConstruct,
    condition: Option<ConstExpr>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
    functions: &HashMap<String, Function>,
    expression_signedness: &HashMap<String, bool>,
    parameter_literals: &HashMap<String, Expr>,
    processes: &mut Vec<CombProcess>,
) -> Result<(), AnalyzerError> {
    if generate_block_has_data_declaration(&generate.nodes.2) {
        return Ok(());
    }
    let name = identifier_text(
        RefNode::GenvarIdentifier(&generate.nodes.1.nodes.1.0.nodes.1),
        syntax_tree,
    )
    .ok_or_else(|| AnalyzerError::Unsupported("loop-generate variable".to_string()))?;
    let init_expr = const_expr_from_ref_node(
        RefNode::ConstantExpression(&generate.nodes.1.nodes.1.0.nodes.3),
        syntax_tree,
    )
    .ok_or_else(|| AnalyzerError::Unsupported("loop-generate initializer".to_string()))?;
    let init = eval_ast_const_expr(&init_expr, const_env)
        .ok_or_else(|| AnalyzerError::Unsupported("loop-generate initializer".to_string()))?;
    let condition_expr = const_expr_from_ref_node(
        RefNode::ConstantExpression(&generate.nodes.1.nodes.1.2.nodes.0),
        syntax_tree,
    )
    .ok_or_else(|| AnalyzerError::Unsupported("loop-generate condition".to_string()))?;

    let mut value = init;
    let mut iterations = 0;
    for _ in 0..10_000 {
        let mut loop_env = const_env.clone();
        loop_env.insert(name.clone(), value);
        let condition_value = eval_ast_const_expr(&condition_expr, &loop_env)
            .ok_or_else(|| AnalyzerError::Unsupported("loop-generate condition".to_string()))?;
        if condition_value == 0 {
            break;
        }
        comb_processes_from_generate_block(
            &generate.nodes.2,
            condition.clone(),
            syntax_tree,
            &loop_env,
            packed_dimensions,
            functions,
            expression_signedness,
            parameter_literals,
            processes,
        )?;
        iterations += 1;
        let next = next_genvar_value(value, &generate.nodes.1.nodes.1.4, syntax_tree, &loop_env)
            .ok_or_else(|| AnalyzerError::Unsupported("genvar update operator".to_string()))?;
        value = next;
    }
    let mut loop_env = const_env.clone();
    loop_env.insert(name, value);
    if iterations == 10_000
        && eval_ast_const_expr(&condition_expr, &loop_env).is_some_and(|value| value != 0)
    {
        return Err(AnalyzerError::Unsupported(
            "loop-generate unroll limit exceeded".to_string(),
        ));
    }
    Ok(())
}

fn generate_block_has_data_declaration(block: &sv_parser::GenerateBlock) -> bool {
    match block {
        sv_parser::GenerateBlock::GenerateItem(item) => generate_item_has_data_declaration(item),
        sv_parser::GenerateBlock::Multiple(block) => {
            block.nodes.3.iter().any(generate_item_has_data_declaration)
        }
    }
}

fn generate_block_has_type_declaration(block: &sv_parser::GenerateBlock) -> bool {
    RefNode::GenerateBlock(block).into_iter().any(|node| {
        matches!(
            node,
            RefNode::DataDeclaration(sv_parser::DataDeclaration::TypeDeclaration(_))
                | RefNode::TypeAssignment(_)
        )
    })
}

fn has_local_constants(const_env: &HashMap<String, i128>) -> bool {
    const_env.keys().any(|name| {
        !name.starts_with("__parameter::") && !const_env.contains_key(&parameter_marker(name))
    })
}

fn reject_duplicate_conditional_generate_locals(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
) -> Result<(), AnalyzerError> {
    let mut signal_names = HashSet::default();
    let mut parameter_names = HashSet::default();
    for child in node {
        let RefNode::ConditionalGenerateConstruct(sv_parser::ConditionalGenerateConstruct::If(
            generate,
        )) = child
        else {
            continue;
        };
        let blocks = std::iter::once(&generate.nodes.2).chain(
            generate
                .nodes
                .3
                .as_ref()
                .into_iter()
                .map(|(_, block)| block),
        );
        for block in blocks {
            for name in generate_block_direct_data_declaration_names(block, syntax_tree) {
                if signal_names.insert(name) {
                    continue;
                }
                return Err(AnalyzerError::Unsupported(
                    "local data declaration inside conditional-generate".to_string(),
                ));
            }
            for name in generate_block_direct_local_parameter_names(block, syntax_tree) {
                if parameter_names.insert(name) {
                    continue;
                }
                return Err(AnalyzerError::Unsupported(
                    "local data declaration inside conditional-generate".to_string(),
                ));
            }
        }
    }
    Ok(())
}

fn conditional_generate_has_leaking_local(module: RefNode<'_>, syntax_tree: &SyntaxTree) -> bool {
    let module_identifiers = module
        .clone()
        .into_iter()
        .filter_map(identifier_locate)
        .collect::<Vec<_>>();
    module.into_iter().any(|node| {
        let RefNode::ConditionalGenerateConstruct(sv_parser::ConditionalGenerateConstruct::If(
            generate,
        )) = node
        else {
            return false;
        };
        std::iter::once(&generate.nodes.2)
            .chain(
                generate
                    .nodes
                    .3
                    .as_ref()
                    .into_iter()
                    .map(|(_, block)| block),
            )
            .any(|block| {
                let block_identifiers = RefNode::GenerateBlock(block)
                    .into_iter()
                    .filter_map(identifier_locate)
                    .collect::<Vec<_>>();
                let Some(block_start) = block_identifiers.iter().map(|locate| locate.offset).min()
                else {
                    return false;
                };
                let block_end = block_identifiers
                    .iter()
                    .map(|locate| locate.offset + locate.len)
                    .max()
                    .unwrap_or(block_start);
                generate_block_direct_data_declaration_names(block, syntax_tree)
                    .into_iter()
                    .any(|name| {
                        module_identifiers.iter().any(|locate| {
                            (locate.offset < block_start || locate.offset >= block_end)
                                && syntax_tree.get_str(locate) == Some(name.as_str())
                        })
                    })
            })
    })
}

fn generate_block_direct_data_declaration_names(
    block: &sv_parser::GenerateBlock,
    syntax_tree: &SyntaxTree,
) -> Vec<String> {
    let aliases = HashMap::default();
    let mut names = Vec::new();
    visit_direct_generate_items(block, |item| {
        let Some(sv_parser::PackageOrGenerateItemDeclaration::DataDeclaration(data)) =
            package_declaration_from_generate_item(item)
        else {
            return;
        };
        names.extend(
            signals_from_data_declaration(data, syntax_tree, &aliases, &HashMap::default())
                .unwrap_or_default()
                .into_iter()
                .map(|signal| signal.name),
        );
    });
    names
}

fn generate_block_direct_local_parameter_names(
    block: &sv_parser::GenerateBlock,
    syntax_tree: &SyntaxTree,
) -> Vec<String> {
    let mut names = Vec::new();
    visit_direct_generate_items(block, |item| {
        let Some(sv_parser::PackageOrGenerateItemDeclaration::LocalParameterDeclaration(
            localparam,
        )) = package_declaration_from_generate_item(item)
        else {
            return;
        };
        let mut parameters = Vec::new();
        if parameters_from_ref_node(
            RefNode::LocalParameterDeclaration(&localparam.0),
            syntax_tree,
            &mut parameters,
            true,
            &HashMap::default(),
            &HashMap::default(),
            &HashMap::default(),
        )
        .is_ok()
        {
            names.extend(parameters.into_iter().map(|parameter| parameter.name));
        }
    });
    names
}

fn visit_direct_generate_items(
    block: &sv_parser::GenerateBlock,
    mut visit: impl FnMut(&sv_parser::GenerateItem),
) {
    match block {
        sv_parser::GenerateBlock::GenerateItem(item) => visit(item),
        sv_parser::GenerateBlock::Multiple(block) => {
            for item in &block.nodes.3 {
                visit(item);
            }
        }
    }
}

fn package_declaration_from_generate_item(
    item: &sv_parser::GenerateItem,
) -> Option<&sv_parser::PackageOrGenerateItemDeclaration> {
    let sv_parser::GenerateItem::ModuleOrGenerateItem(item) = item else {
        return None;
    };
    let sv_parser::ModuleOrGenerateItem::ModuleItem(item) = &**item else {
        return None;
    };
    let sv_parser::ModuleCommonItem::ModuleOrGenerateItemDeclaration(declaration) = &item.nodes.1
    else {
        return None;
    };
    let sv_parser::ModuleOrGenerateItemDeclaration::PackageOrGenerateItemDeclaration(declaration) =
        &**declaration
    else {
        return None;
    };
    Some(declaration)
}

fn generate_item_has_data_declaration(item: &sv_parser::GenerateItem) -> bool {
    let sv_parser::GenerateItem::ModuleOrGenerateItem(item) = item else {
        return false;
    };
    let sv_parser::ModuleOrGenerateItem::ModuleItem(item) = &**item else {
        return false;
    };
    let sv_parser::ModuleCommonItem::ModuleOrGenerateItemDeclaration(declaration) = &item.nodes.1
    else {
        return false;
    };
    let sv_parser::ModuleOrGenerateItemDeclaration::PackageOrGenerateItemDeclaration(declaration) =
        &**declaration
    else {
        return false;
    };
    matches!(
        &**declaration,
        sv_parser::PackageOrGenerateItemDeclaration::DataDeclaration(_)
    )
}

fn next_genvar_value(
    value: i128,
    iteration: &sv_parser::GenvarIteration,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
) -> Option<i128> {
    match iteration {
        sv_parser::GenvarIteration::Prefix(iteration) => {
            let op = syntax_tree.get_str(&iteration.nodes.0.nodes.0)?;
            match op {
                "++" => value.checked_add(1),
                "--" => value.checked_sub(1),
                _ => None,
            }
        }
        sv_parser::GenvarIteration::Suffix(iteration) => {
            let op = syntax_tree.get_str(&iteration.nodes.1.nodes.0)?;
            match op {
                "++" => value.checked_add(1),
                "--" => value.checked_sub(1),
                _ => None,
            }
        }
        sv_parser::GenvarIteration::Assignment(iteration) => {
            let op = syntax_tree.get_str(&iteration.nodes.1.nodes.0)?;
            let rhs = const_expr_from_ref_node(
                RefNode::ConstantExpression(&iteration.nodes.2.nodes.0),
                syntax_tree,
            )?;
            let rhs = eval_ast_const_expr(&rhs, const_env)?;
            match op {
                "=" => Some(rhs),
                "+=" => value.checked_add(rhs),
                "-=" => value.checked_sub(rhs),
                "*=" => value.checked_mul(rhs),
                "/=" => (rhs != 0).then(|| value / rhs),
                "%=" => (rhs != 0).then(|| value % rhs),
                "<<=" => u32::try_from(rhs)
                    .ok()
                    .and_then(|rhs| value.checked_shl(rhs)),
                ">>=" => u32::try_from(rhs)
                    .ok()
                    .and_then(|rhs| value.checked_shr(rhs)),
                _ => None,
            }
        }
    }
}

fn comb_processes_from_generate_block(
    block: &sv_parser::GenerateBlock,
    condition: Option<ConstExpr>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
    functions: &HashMap<String, Function>,
    expression_signedness: &HashMap<String, bool>,
    parameter_literals: &HashMap<String, Expr>,
    processes: &mut Vec<CombProcess>,
) -> Result<(), AnalyzerError> {
    match block {
        sv_parser::GenerateBlock::GenerateItem(item) => {
            comb_processes_from_generate_item(
                item,
                condition,
                syntax_tree,
                const_env,
                packed_dimensions,
                functions,
                expression_signedness,
                parameter_literals,
                processes,
            )?;
        }
        sv_parser::GenerateBlock::Multiple(block) => {
            let mut block_env = const_env.clone();
            for item in &block.nodes.3 {
                if add_localparams_from_generate_item(item, syntax_tree, &mut block_env) {
                    continue;
                }
                comb_processes_from_generate_item(
                    item,
                    condition.clone(),
                    syntax_tree,
                    &block_env,
                    packed_dimensions,
                    functions,
                    expression_signedness,
                    parameter_literals,
                    processes,
                )?;
            }
        }
    }
    Ok(())
}

fn add_localparams_from_generate_item(
    item: &sv_parser::GenerateItem,
    syntax_tree: &SyntaxTree,
    const_env: &mut HashMap<String, i128>,
) -> bool {
    add_localparams_from_generate_item_with_literals(item, syntax_tree, const_env, None)
}

fn add_localparams_from_generate_item_with_literals(
    item: &sv_parser::GenerateItem,
    syntax_tree: &SyntaxTree,
    const_env: &mut HashMap<String, i128>,
    mut parameter_literals: Option<&mut HashMap<String, Expr>>,
) -> bool {
    let sv_parser::GenerateItem::ModuleOrGenerateItem(item) = item else {
        return false;
    };
    let sv_parser::ModuleOrGenerateItem::ModuleItem(item) = &**item else {
        return false;
    };
    let sv_parser::ModuleCommonItem::ModuleOrGenerateItemDeclaration(declaration) = &item.nodes.1
    else {
        return false;
    };
    let sv_parser::ModuleOrGenerateItemDeclaration::PackageOrGenerateItemDeclaration(declaration) =
        &**declaration
    else {
        return false;
    };
    let sv_parser::PackageOrGenerateItemDeclaration::LocalParameterDeclaration(localparam) =
        &**declaration
    else {
        return false;
    };
    let mut parameters = Vec::new();
    if parameters_from_ref_node(
        RefNode::LocalParameterDeclaration(&localparam.0),
        syntax_tree,
        &mut parameters,
        true,
        const_env,
        &HashMap::default(),
        &HashMap::default(),
    )
    .is_err()
    {
        return true;
    }
    let mut parameter_types = parameter_types_from_const_env(const_env);
    for parameter in parameters {
        let Some(value) = parameter.resolved_value(const_env, &parameter_types) else {
            continue;
        };
        let resolved_type = parameter.resolved_type(&parameter_types);
        if let Some(r#type) = resolved_type {
            parameter_types.insert(parameter.name().to_string(), r#type);
            insert_parameter_type_markers(const_env, parameter.name(), r#type);
        }
        if let Some(parameter_literals) = parameter_literals.as_deref_mut() {
            let literal = if let Some(r#type) = resolved_type {
                format_typed_parameter_literal(value, r#type.width, r#type.signed)
            } else {
                value.to_string()
            };
            parameter_literals.insert(parameter.name().to_string(), Expr::Literal(literal));
        }
        const_env.insert(parameter.name().to_string(), value);
    }
    true
}

fn eval_ast_const_expr(expr: &ConstExpr, const_env: &HashMap<String, i128>) -> Option<i128> {
    let parameter_types = parameter_types_from_const_env(const_env);
    let expr = substitute_typed_parameter_literals(expr.clone(), const_env, &parameter_types);
    typecheck::eval_const_expr(&expr.into(), const_env)
}

fn substitute_process_constants(
    process: CombProcess,
    const_env: &HashMap<String, i128>,
) -> CombProcess {
    substitute_process_constants_with_parameter_literals(process, const_env, &HashMap::default())
}

fn substitute_process_constants_with_parameter_literals(
    process: CombProcess,
    const_env: &HashMap<String, i128>,
    parameter_literals: &HashMap<String, Expr>,
) -> CombProcess {
    CombProcess::new(
        process.kind,
        process
            .condition
            .map(|condition| substitute_const_expr_constants(condition, const_env)),
        process
            .assignments
            .into_iter()
            .map(|assignment| {
                substitute_assignment_constants_with_parameter_literals(
                    assignment,
                    const_env,
                    parameter_literals,
                )
            })
            .collect(),
    )
}

fn substitute_assignment_constants(
    assignment: Assignment,
    const_env: &HashMap<String, i128>,
) -> Assignment {
    substitute_assignment_constants_with_parameter_literals(
        assignment,
        const_env,
        &HashMap::default(),
    )
}

fn substitute_assignment_constants_with_parameter_literals(
    assignment: Assignment,
    const_env: &HashMap<String, i128>,
    parameter_literals: &HashMap<String, Expr>,
) -> Assignment {
    Assignment::new(
        substitute_lvalue_constants(assignment.lhs, const_env),
        substitute_expr_constants_with_parameter_literals(
            assignment.rhs,
            const_env,
            parameter_literals,
        ),
    )
}

fn substitute_lvalue_constants(lvalue: LValue, const_env: &HashMap<String, i128>) -> LValue {
    match lvalue {
        LValue::Ident(name) => LValue::Ident(name),
        LValue::Select {
            name,
            msb,
            lsb,
            signed,
            array_slice_width,
            array_slice_reversed,
        } => LValue::Select {
            name,
            msb: substitute_const_expr_constants(msb, const_env),
            lsb: substitute_const_expr_constants(lsb, const_env),
            signed,
            array_slice_width: array_slice_width
                .map(|width| substitute_const_expr_constants(width, const_env)),
            array_slice_reversed,
        },
    }
}

fn substitute_expr_constants_with_parameter_literals(
    expr: Expr,
    const_env: &HashMap<String, i128>,
    parameter_literals: &HashMap<String, Expr>,
) -> Expr {
    match expr {
        Expr::Ident(name) => parameter_literals
            .get(&name)
            .cloned()
            .or_else(|| {
                let value = *const_env.get(&name)?;
                if const_env.contains_key(&enum_marker(&name)) {
                    let width = const_env
                        .get(&parameter_width_marker(&name))
                        .and_then(|width| usize::try_from(*width).ok())?;
                    let signed = const_env
                        .get(&parameter_signed_marker(&name))
                        .is_some_and(|signed| *signed != 0);
                    Some(Expr::Literal(format_typed_parameter_literal(
                        value, width, signed,
                    )))
                } else if !const_env.contains_key(&parameter_marker(&name)) {
                    Some(Expr::Literal(value.to_string()))
                } else {
                    None
                }
            })
            .unwrap_or(Expr::Ident(name)),
        Expr::Literal(value) => Expr::Literal(value),
        Expr::Select {
            expr,
            msb,
            lsb,
            signed,
        } => Expr::Select {
            expr: Box::new(substitute_expr_constants_with_parameter_literals(
                *expr,
                const_env,
                parameter_literals,
            )),
            msb: substitute_const_expr_constants(msb, const_env),
            lsb: substitute_const_expr_constants(lsb, const_env),
            signed,
        },
        Expr::Concat(parts) => Expr::Concat(
            parts
                .into_iter()
                .map(|part| {
                    substitute_expr_constants_with_parameter_literals(
                        part,
                        const_env,
                        parameter_literals,
                    )
                })
                .collect(),
        ),
        Expr::RepeatConcat { count, parts } => Expr::RepeatConcat {
            count: substitute_const_expr_constants(count, const_env),
            parts: parts
                .into_iter()
                .map(|part| {
                    substitute_expr_constants_with_parameter_literals(
                        part,
                        const_env,
                        parameter_literals,
                    )
                })
                .collect(),
        },
        Expr::Resize {
            expr,
            width,
            signed,
        } => Expr::Resize {
            expr: Box::new(substitute_expr_constants_with_parameter_literals(
                *expr,
                const_env,
                parameter_literals,
            )),
            width,
            signed,
        },
        Expr::Unary { op, expr } => Expr::Unary {
            op,
            expr: Box::new(substitute_expr_constants_with_parameter_literals(
                *expr,
                const_env,
                parameter_literals,
            )),
        },
        Expr::Binary { left, op, right } => Expr::Binary {
            left: Box::new(substitute_expr_constants_with_parameter_literals(
                *left,
                const_env,
                parameter_literals,
            )),
            op,
            right: Box::new(substitute_expr_constants_with_parameter_literals(
                *right,
                const_env,
                parameter_literals,
            )),
        },
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => Expr::Mux {
            condition: Box::new(substitute_expr_constants_with_parameter_literals(
                *condition,
                const_env,
                parameter_literals,
            )),
            then_expr: Box::new(substitute_expr_constants_with_parameter_literals(
                *then_expr,
                const_env,
                parameter_literals,
            )),
            else_expr: Box::new(substitute_expr_constants_with_parameter_literals(
                *else_expr,
                const_env,
                parameter_literals,
            )),
        },
        Expr::Call { name, args } => Expr::Call {
            name,
            args: args
                .into_iter()
                .map(|arg| {
                    substitute_expr_constants_with_parameter_literals(
                        arg,
                        const_env,
                        parameter_literals,
                    )
                })
                .collect(),
        },
    }
}

fn expand_process_calls(
    process: CombProcess,
    functions: &HashMap<String, Function>,
    expression_signedness: &HashMap<String, bool>,
) -> CombProcess {
    CombProcess::new(
        process.kind,
        process.condition,
        process
            .assignments
            .into_iter()
            .map(|assignment| {
                expand_assignment_calls(assignment, functions, expression_signedness, true)
            })
            .collect(),
    )
}

fn expand_ff_process_calls(
    process: FfProcess,
    functions: &HashMap<String, Function>,
    expression_signedness: &HashMap<String, bool>,
    const_env: &HashMap<String, i128>,
    parameter_literals: &HashMap<String, Expr>,
) -> FfProcess {
    FfProcess::new(
        process.events,
        process
            .assignments
            .into_iter()
            .map(|assignment| {
                let condition = assignment.condition.map(|condition| {
                    substitute_expr_constants_with_parameter_literals(
                        expand_expr_calls(condition, functions, expression_signedness, 0, true),
                        const_env,
                        parameter_literals,
                    )
                });
                let assignment = substitute_assignment_constants_with_parameter_literals(
                    expand_assignment_calls(
                        assignment.assignment,
                        functions,
                        expression_signedness,
                        true,
                    ),
                    const_env,
                    parameter_literals,
                );
                ConditionalAssignment::new(condition, assignment)
            })
            .collect(),
    )
}

fn expand_assignment_calls(
    assignment: Assignment,
    functions: &HashMap<String, Function>,
    expression_signedness: &HashMap<String, bool>,
    apply_return_type: bool,
) -> Assignment {
    Assignment::new(
        assignment.lhs,
        expand_expr_calls(
            assignment.rhs,
            functions,
            expression_signedness,
            0,
            apply_return_type,
        ),
    )
}

fn expr_signedness(
    expr: &Expr,
    identifiers: &HashMap<String, bool>,
    functions: &HashMap<String, Function>,
) -> Option<bool> {
    match expr {
        Expr::Ident(name) => identifiers.get(name).copied(),
        Expr::Literal(literal) => {
            typecheck::parse_integral_literal(literal).map(|literal| literal.signed)
        }
        Expr::Select { signed, .. } => Some(*signed),
        Expr::Concat(_) | Expr::RepeatConcat { .. } => Some(false),
        Expr::Resize { signed, .. } => Some(*signed),
        Expr::Unary { op, expr } => {
            if matches!(
                op,
                UnaryOp::LogicNot | UnaryOp::RedAnd | UnaryOp::RedOr | UnaryOp::RedXor
            ) {
                Some(false)
            } else {
                expr_signedness(expr, identifiers, functions)
            }
        }
        Expr::Binary { left, op, right } => {
            if matches!(
                op,
                BinaryOp::LogicAnd
                    | BinaryOp::LogicOr
                    | BinaryOp::Eq
                    | BinaryOp::Ne
                    | BinaryOp::EqCase
                    | BinaryOp::NeCase
                    | BinaryOp::EqWildcard
                    | BinaryOp::NeWildcard
                    | BinaryOp::Lt
                    | BinaryOp::Le
                    | BinaryOp::Gt
                    | BinaryOp::Ge
            ) {
                Some(false)
            } else if matches!(op, BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar) {
                expr_signedness(left, identifiers, functions)
            } else {
                Some(
                    expr_signedness(left, identifiers, functions)?
                        && expr_signedness(right, identifiers, functions)?,
                )
            }
        }
        Expr::Mux {
            then_expr,
            else_expr,
            ..
        } => Some(
            expr_signedness(then_expr, identifiers, functions)?
                && expr_signedness(else_expr, identifiers, functions)?,
        ),
        Expr::Call { name, .. } => functions.get(name).map(|function| function.return_signed),
    }
}

fn expand_expr_calls(
    expr: Expr,
    functions: &HashMap<String, Function>,
    expression_signedness: &HashMap<String, bool>,
    depth: usize,
    apply_return_type: bool,
) -> Expr {
    if depth > 32 {
        return expr;
    }
    match expr {
        Expr::Ident(name) => Expr::Ident(name),
        Expr::Literal(value) => Expr::Literal(value),
        Expr::Select {
            expr,
            msb,
            lsb,
            signed,
        } => Expr::Select {
            expr: Box::new(expand_expr_calls(
                *expr,
                functions,
                expression_signedness,
                depth,
                apply_return_type,
            )),
            msb,
            lsb,
            signed,
        },
        Expr::Concat(parts) => Expr::Concat(
            parts
                .into_iter()
                .map(|part| {
                    expand_expr_calls(
                        part,
                        functions,
                        expression_signedness,
                        depth,
                        apply_return_type,
                    )
                })
                .collect(),
        ),
        Expr::RepeatConcat { count, parts } => Expr::RepeatConcat {
            count,
            parts: parts
                .into_iter()
                .map(|part| {
                    expand_expr_calls(
                        part,
                        functions,
                        expression_signedness,
                        depth,
                        apply_return_type,
                    )
                })
                .collect(),
        },
        Expr::Resize {
            expr,
            width,
            signed,
        } => Expr::Resize {
            expr: Box::new(expand_expr_calls(
                *expr,
                functions,
                expression_signedness,
                depth,
                apply_return_type,
            )),
            width,
            signed,
        },
        Expr::Unary { op, expr } => Expr::Unary {
            op,
            expr: Box::new(expand_expr_calls(
                *expr,
                functions,
                expression_signedness,
                depth,
                apply_return_type,
            )),
        },
        Expr::Binary { left, op, right } => Expr::Binary {
            left: Box::new(expand_expr_calls(
                *left,
                functions,
                expression_signedness,
                depth,
                apply_return_type,
            )),
            op,
            right: Box::new(expand_expr_calls(
                *right,
                functions,
                expression_signedness,
                depth,
                apply_return_type,
            )),
        },
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => Expr::Mux {
            condition: Box::new(expand_expr_calls(
                *condition,
                functions,
                expression_signedness,
                depth,
                apply_return_type,
            )),
            then_expr: Box::new(expand_expr_calls(
                *then_expr,
                functions,
                expression_signedness,
                depth,
                apply_return_type,
            )),
            else_expr: Box::new(expand_expr_calls(
                *else_expr,
                functions,
                expression_signedness,
                depth,
                apply_return_type,
            )),
        },
        Expr::Call { name, args } => {
            let args = args
                .into_iter()
                .map(|arg| {
                    expand_expr_calls(
                        arg,
                        functions,
                        expression_signedness,
                        depth,
                        apply_return_type,
                    )
                })
                .collect::<Vec<_>>();
            let Some(function) = functions.get(&name) else {
                return Expr::Call { name, args };
            };
            if function.params.len() != args.len() {
                return Expr::Call { name, args };
            }
            let env = function
                .params
                .iter()
                .zip(args)
                .map(|(param, arg)| {
                    let mut arg = if apply_return_type && let Some(width) = param.width {
                        let assigned = Expr::Resize {
                            signed: expr_signedness(&arg, expression_signedness, functions)
                                .unwrap_or(false),
                            expr: Box::new(arg),
                            width,
                        };
                        Expr::Resize {
                            expr: Box::new(assigned),
                            width,
                            signed: param.signed,
                        }
                    } else {
                        arg
                    };
                    if param.is_2state {
                        arg = Expr::Unary {
                            op: UnaryOp::ToTwoState,
                            expr: Box::new(arg),
                        };
                    }
                    (param.name.clone(), arg)
                })
                .collect::<HashMap<_, _>>();
            let body = substitute_expr_idents(function.body.clone(), &env);
            let expanded = expand_expr_calls(
                body,
                functions,
                expression_signedness,
                depth + 1,
                apply_return_type,
            );
            let mut expanded = if apply_return_type && let Some(width) = function.return_width {
                let expression_signed =
                    expr_signedness(&expanded, expression_signedness, functions).unwrap_or(false);
                let assigned = if expression_signed == function.return_signed {
                    expanded
                } else {
                    Expr::Resize {
                        signed: expression_signed,
                        expr: Box::new(expanded),
                        width,
                    }
                };
                Expr::Resize {
                    expr: Box::new(assigned),
                    width,
                    signed: function.return_signed,
                }
            } else {
                expanded
            };
            if function.return_is_2state {
                expanded = Expr::Unary {
                    op: UnaryOp::ToTwoState,
                    expr: Box::new(expanded),
                };
            }
            expanded
        }
    }
}

fn substitute_expr_idents(expr: Expr, env: &HashMap<String, Expr>) -> Expr {
    match expr {
        Expr::Ident(name) => env.get(&name).cloned().unwrap_or(Expr::Ident(name)),
        Expr::Literal(value) => Expr::Literal(value),
        Expr::Select {
            expr,
            msb,
            lsb,
            signed,
        } => Expr::Select {
            expr: Box::new(substitute_expr_idents(*expr, env)),
            msb,
            lsb,
            signed,
        },
        Expr::Concat(parts) => Expr::Concat(
            parts
                .into_iter()
                .map(|part| substitute_expr_idents(part, env))
                .collect(),
        ),
        Expr::RepeatConcat { count, parts } => Expr::RepeatConcat {
            count,
            parts: parts
                .into_iter()
                .map(|part| substitute_expr_idents(part, env))
                .collect(),
        },
        Expr::Resize {
            expr,
            width,
            signed,
        } => Expr::Resize {
            expr: Box::new(substitute_expr_idents(*expr, env)),
            width,
            signed,
        },
        Expr::Unary { op, expr } => Expr::Unary {
            op,
            expr: Box::new(substitute_expr_idents(*expr, env)),
        },
        Expr::Binary { left, op, right } => Expr::Binary {
            left: Box::new(substitute_expr_idents(*left, env)),
            op,
            right: Box::new(substitute_expr_idents(*right, env)),
        },
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => Expr::Mux {
            condition: Box::new(substitute_expr_idents(*condition, env)),
            then_expr: Box::new(substitute_expr_idents(*then_expr, env)),
            else_expr: Box::new(substitute_expr_idents(*else_expr, env)),
        },
        Expr::Call { name, args } => Expr::Call {
            name,
            args: args
                .into_iter()
                .map(|arg| substitute_expr_idents(arg, env))
                .collect(),
        },
    }
}

fn substitute_const_expr_constants(
    expr: ConstExpr,
    const_env: &HashMap<String, i128>,
) -> ConstExpr {
    substitute_const_expr_constants_impl(expr, const_env, false, false)
}

fn substitute_dimension_constants(expr: ConstExpr, const_env: &HashMap<String, i128>) -> ConstExpr {
    substitute_const_expr_constants_impl(expr, const_env, true, false)
}

fn substitute_const_expr_constants_preserving_enum_types(
    expr: ConstExpr,
    const_env: &HashMap<String, i128>,
) -> ConstExpr {
    substitute_const_expr_constants_impl(expr, const_env, false, true)
}

fn substitute_const_expr_constants_impl(
    expr: ConstExpr,
    const_env: &HashMap<String, i128>,
    include_local_parameters: bool,
    preserve_enum_types: bool,
) -> ConstExpr {
    match expr {
        ConstExpr::Ident(name) => {
            let Some(value) = const_env.get(&name).filter(|_| {
                !const_env.contains_key(&parameter_marker(&name))
                    || include_local_parameters
                        && const_env.contains_key(&local_parameter_marker(&name))
            }) else {
                return ConstExpr::Ident(name);
            };
            if preserve_enum_types && const_env.contains_key(&enum_marker(&name)) {
                let width = const_env
                    .get(&parameter_width_marker(&name))
                    .and_then(|width| usize::try_from(*width).ok());
                let signed = const_env
                    .get(&parameter_signed_marker(&name))
                    .is_some_and(|signed| *signed != 0);
                if let Some(width) = width {
                    return ConstExpr::Literal(format_typed_parameter_literal(
                        *value, width, signed,
                    ));
                }
            }
            ConstExpr::Literal(value.to_string())
        }
        ConstExpr::Literal(value) => ConstExpr::Literal(value),
        ConstExpr::Select { expr, bit } => ConstExpr::Select {
            expr: Box::new(substitute_const_expr_constants_impl(
                *expr,
                const_env,
                include_local_parameters,
                preserve_enum_types,
            )),
            bit: Box::new(substitute_const_expr_constants_impl(
                *bit,
                const_env,
                include_local_parameters,
                preserve_enum_types,
            )),
        },
        ConstExpr::Function { name, args } => ConstExpr::Function {
            name,
            args: args
                .into_iter()
                .map(|arg| {
                    substitute_const_expr_constants_impl(
                        arg,
                        const_env,
                        include_local_parameters,
                        preserve_enum_types,
                    )
                })
                .collect(),
        },
        ConstExpr::Unary { op, expr } => ConstExpr::Unary {
            op,
            expr: Box::new(substitute_const_expr_constants_impl(
                *expr,
                const_env,
                include_local_parameters,
                preserve_enum_types,
            )),
        },
        ConstExpr::Binary { left, op, right } => ConstExpr::Binary {
            left: Box::new(substitute_const_expr_constants_impl(
                *left,
                const_env,
                include_local_parameters,
                preserve_enum_types,
            )),
            op,
            right: Box::new(substitute_const_expr_constants_impl(
                *right,
                const_env,
                include_local_parameters,
                preserve_enum_types,
            )),
        },
        ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => ConstExpr::Mux {
            condition: Box::new(substitute_const_expr_constants_impl(
                *condition,
                const_env,
                include_local_parameters,
                preserve_enum_types,
            )),
            then_expr: Box::new(substitute_const_expr_constants_impl(
                *then_expr,
                const_env,
                include_local_parameters,
                preserve_enum_types,
            )),
            else_expr: Box::new(substitute_const_expr_constants_impl(
                *else_expr,
                const_env,
                include_local_parameters,
                preserve_enum_types,
            )),
        },
    }
}

fn assignments_from_continuous_assign(
    assign: &sv_parser::ContinuousAssign,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Result<Vec<Assignment>, AnalyzerError> {
    match assign {
        sv_parser::ContinuousAssign::Net(assign) => assign
            .nodes
            .3
            .nodes
            .0
            .contents()
            .into_iter()
            .map(|assignment| {
                let lhs = net_lvalue_from_node(&assignment.nodes.0, syntax_tree, packed_dimensions)
                    .ok_or_else(|| {
                        AnalyzerError::Unsupported("continuous assignment lvalue".to_string())
                    })?;
                let rhs = expr_from_expression_with_types(
                    &assignment.nodes.2,
                    syntax_tree,
                    packed_dimensions,
                )
                .ok_or_else(|| {
                    AnalyzerError::Unsupported("continuous assignment expression".to_string())
                })?;
                Ok(Assignment::new(lhs, rhs))
            })
            .collect(),
        sv_parser::ContinuousAssign::Variable(assign) => assign
            .nodes
            .2
            .nodes
            .0
            .contents()
            .into_iter()
            .map(|assignment| {
                let lhs =
                    variable_lvalue_from_node(&assignment.nodes.0, syntax_tree, packed_dimensions)
                        .ok_or_else(|| {
                            AnalyzerError::Unsupported("continuous assignment lvalue".to_string())
                        })?;
                let rhs = expr_from_expression_with_types(
                    &assignment.nodes.2,
                    syntax_tree,
                    packed_dimensions,
                )
                .ok_or_else(|| {
                    AnalyzerError::Unsupported("continuous assignment expression".to_string())
                })?;
                Ok(Assignment::new(lhs, rhs))
            })
            .collect(),
    }
}

fn comb_process_from_always_construct(
    always: &sv_parser::AlwaysConstruct,
    condition: Option<ConstExpr>,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
    functions: &HashMap<String, Function>,
    expression_signedness: &HashMap<String, bool>,
    parameter_literals: &HashMap<String, Expr>,
) -> Result<Option<CombProcess>, AnalyzerError> {
    if !matches!(always.nodes.0, sv_parser::AlwaysKeyword::AlwaysComb(_)) {
        return Ok(None);
    }
    validate_always_comb_statement(&always.nodes.1)?;
    let mut guarded_assignments = Vec::new();
    conditional_assignments_from_statement(
        &always.nodes.1,
        None,
        true,
        syntax_tree,
        &packed_dimensions.const_env,
        packed_dimensions,
        &mut guarded_assignments,
    )?;
    for guarded in &mut guarded_assignments {
        guarded.condition = guarded.condition.take().map(|condition| {
            substitute_expr_constants_with_parameter_literals(
                expand_expr_calls(condition, functions, expression_signedness, 0, true),
                &HashMap::default(),
                parameter_literals,
            )
        });
        guarded.assignment = substitute_assignment_constants_with_parameter_literals(
            expand_assignment_calls(
                guarded.assignment.clone(),
                functions,
                expression_signedness,
                true,
            ),
            &HashMap::default(),
            parameter_literals,
        );
    }
    let assignments = comb_assignments_from_guarded(guarded_assignments, packed_dimensions)?;
    Ok((!assignments.is_empty())
        .then(|| CombProcess::new(CombProcessKind::AlwaysComb, condition, assignments)))
}

/// Merge guarded always_comb assignments into per-target multiplexer chains.
///
/// Groups made up solely of unconditional writes stay untouched so the
/// downstream ordered-assignment semantics (and its dependency checks) keep
/// applying. Once a conditional write participates, the group folds
/// back-to-front into nested muxes so earlier branches take priority. A
/// conditional write with no later unconditional fallback would keep the
/// previous value, which infers a latch, and is rejected.
fn comb_assignments_from_guarded(
    mut guarded: Vec<ConditionalAssignment>,
    packed_dimensions: &PackedDimensions,
) -> Result<Vec<Assignment>, AnalyzerError> {
    normalize_mixed_whole_selected_comb_writes(&mut guarded, packed_dimensions);
    let const_env = &packed_dimensions.const_env;
    let mut targets: Vec<LValue> = Vec::new();
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for (index, conditional) in guarded.iter().enumerate() {
        let target = conditional.assignment().lhs_value();
        let reusable_group = targets
            .iter()
            .enumerate()
            .rev()
            .find_map(|(group, existing)| {
                if existing != target {
                    return None;
                }
                let previous = *groups[group].last()?;
                let separated_by_overlap = guarded[previous + 1..index].iter().any(|assignment| {
                    lvalues_overlap(assignment.assignment().lhs_value(), target, const_env)
                        && assignment.assignment().lhs_value() != target
                });
                (!separated_by_overlap).then_some(group)
            });
        match reusable_group {
            Some(group) => groups[group].push(index),
            None => {
                targets.push(target.clone());
                groups.push(vec![index]);
            }
        }
    }
    // Every arm that will participate in a mux must first undergo its own
    // procedural assignment conversion. This includes an unconditional
    // initializer that becomes the fallback of a later guarded write.
    for indices in &groups {
        let has_conditional = indices
            .iter()
            .any(|index| guarded[*index].condition().is_some());
        if !has_conditional {
            continue;
        }
        for index in indices {
            let assignment = guarded[*index].assignment().clone();
            let lhs = assignment.lhs_value().clone();
            let rhs =
                coerce_procedural_assignment_rhs(assignment.rhs().clone(), &lhs, packed_dimensions);
            guarded[*index].assignment = Assignment::new(lhs, rhs);
        }
    }
    // Apply every cross-target blocking-assignment substitution before any
    // group is materialized. A later group may rewrite an assignment that
    // belongs to an earlier group.
    for (target, indices) in targets.iter().zip(&groups) {
        if indices
            .iter()
            .all(|index| guarded[*index].condition().is_none())
        {
            continue;
        }
        let initial = overlapping_value_before(&guarded, indices[0], target, packed_dimensions);
        substitute_intermediate_comb_value_reads(
            &mut guarded,
            indices,
            target,
            initial,
            packed_dimensions,
        )?;
    }

    // Merged groups land on the slot of their last write so relative
    // statement ordering across different, non-overlapping targets is
    // preserved.
    let mut slots: Vec<Option<Assignment>> = Vec::with_capacity(guarded.len());
    slots.extend((0..guarded.len()).map(|_| None));
    for (target, indices) in targets.into_iter().zip(groups) {
        if indices
            .iter()
            .all(|index| guarded[*index].condition().is_none())
        {
            for index in indices {
                slots[index] = Some(guarded[index].assignment().clone());
            }
            continue;
        }
        let last = *indices.last().expect("group is non-empty");
        // Fold in source order so writes after an if/else retain procedural
        // priority. A target-specific exhaustive fallback fills the previous
        // value in the branch chain instead of becoming globally
        // unconditional.
        let initial = overlapping_value_before(&guarded, indices[0], &target, packed_dimensions);
        let has_established_initial = initial.is_some();
        let mut current = initial.unwrap_or_else(comb_previous_value_placeholder);
        for (position, index) in indices.iter().enumerate() {
            let write = &guarded[*index];
            let value = write.assignment().rhs().clone();
            current = if let Some(chain_start) = write.exhaustive_fallback_start {
                let mut branch_value = value;
                for prior in indices[..position].iter().copied().filter(|prior| {
                    *prior >= chain_start && guarded[*prior].path_epochs != write.path_epochs
                }) {
                    branch_value = fold_conditional_assignment_over(branch_value, &guarded[prior]);
                }
                branch_value
            } else {
                fold_conditional_assignment_over(current, write)
            };
        }
        let rhs = simplify_constant_mux_conditions(current, const_env);
        if expr_contains_comb_previous_value(&rhs)
            || (!has_established_initial
                && expr_references_overlapping_lvalue(&rhs, &target, const_env))
        {
            return Err(AnalyzerError::Unsupported(
                "latch inference inside always_comb".to_string(),
            ));
        }
        slots[last] = Some(Assignment::new(target, rhs));
    }
    Ok(slots.into_iter().flatten().collect())
}

fn normalize_mixed_whole_selected_comb_writes(
    guarded: &mut [ConditionalAssignment],
    packed_dimensions: &PackedDimensions,
) {
    let mut whole_names = HashSet::default();
    let mut selected_targets: HashMap<String, Vec<LValue>> = HashMap::default();
    let mut conditional_selected_names = HashSet::default();
    let mut earlier_selected_names = HashSet::default();
    for write in guarded.iter() {
        match write.assignment().lhs_value() {
            LValue::Ident(name)
                if write.condition().is_some() || earlier_selected_names.contains(name) =>
            {
                whole_names.insert(name.clone());
            }
            LValue::Ident(_) => {}
            target @ LValue::Select { name, .. } => {
                selected_targets
                    .entry(name.clone())
                    .or_default()
                    .push(target.clone());
                if write.condition().is_some() {
                    conditional_selected_names.insert(name.clone());
                }
                earlier_selected_names.insert(name.clone());
            }
        }
    }
    whole_names.retain(|name| selected_targets.contains_key(name));
    for name in conditional_selected_names {
        let Some(whole_target) = whole_packed_lvalue(&name, packed_dimensions) else {
            continue;
        };
        if selected_targets
            .get(&name)
            .is_some_and(|targets| lvalue_is_covered_by(&whole_target, targets, packed_dimensions))
        {
            whole_names.insert(name);
        }
    }
    for write in guarded.iter_mut() {
        let target = write.assignment().lhs_value().clone();
        let LValue::Select { name, .. } = &target else {
            continue;
        };
        if !whole_names.contains(name) {
            continue;
        }
        let Some(whole_target) = whole_packed_lvalue(name, packed_dimensions) else {
            continue;
        };
        let write_value = coerce_procedural_assignment_rhs(
            write.assignment().rhs().clone(),
            &target,
            packed_dimensions,
        );
        let Some((rhs, _)) = selected_value_after_write(
            &Expr::Ident(name.clone()),
            &whole_target,
            &target,
            &write_value,
            packed_dimensions,
        ) else {
            continue;
        };
        write.assignment = Assignment::new(LValue::Ident(name.clone()), rhs);
    }

    // Several selected writes in one fallback branch become writes to the
    // same normalized whole value. Only the final write completes that
    // fallback; earlier writes must remain ordinary guarded updates.
    let mut last_fallback = HashMap::default();
    for (index, write) in guarded.iter().enumerate() {
        if let Some(chain_start) = write.exhaustive_fallback_start {
            last_fallback.insert((chain_start, write.assignment().lhs().to_string()), index);
        }
    }
    for (index, write) in guarded.iter_mut().enumerate() {
        if let Some(chain_start) = write.exhaustive_fallback_start
            && last_fallback.get(&(chain_start, write.assignment().lhs().to_string()))
                != Some(&index)
        {
            write.exhaustive_fallback_start = None;
        }
    }
}

fn fold_conditional_assignment_over(current: Expr, write: &ConditionalAssignment) -> Expr {
    let value = write.assignment().rhs().clone();
    match write.condition() {
        None => value,
        Some(condition) => Expr::Mux {
            condition: Box::new(condition.clone()),
            then_expr: Box::new(value),
            else_expr: Box::new(current),
        },
    }
}

fn simplify_constant_mux_conditions(expr: Expr, const_env: &HashMap<String, i128>) -> Expr {
    match expr {
        Expr::Select {
            expr,
            msb,
            lsb,
            signed,
        } => Expr::Select {
            expr: Box::new(simplify_constant_mux_conditions(*expr, const_env)),
            msb,
            lsb,
            signed,
        },
        Expr::Concat(parts) => Expr::Concat(
            parts
                .into_iter()
                .map(|part| simplify_constant_mux_conditions(part, const_env))
                .collect(),
        ),
        Expr::RepeatConcat { count, parts } => Expr::RepeatConcat {
            count,
            parts: parts
                .into_iter()
                .map(|part| simplify_constant_mux_conditions(part, const_env))
                .collect(),
        },
        Expr::Resize {
            expr,
            width,
            signed,
        } => Expr::Resize {
            expr: Box::new(simplify_constant_mux_conditions(*expr, const_env)),
            width,
            signed,
        },
        Expr::Unary { op, expr } => Expr::Unary {
            op,
            expr: Box::new(simplify_constant_mux_conditions(*expr, const_env)),
        },
        Expr::Binary { left, op, right } => Expr::Binary {
            left: Box::new(simplify_constant_mux_conditions(*left, const_env)),
            op,
            right: Box::new(simplify_constant_mux_conditions(*right, const_env)),
        },
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            let condition = simplify_constant_mux_conditions(*condition, const_env);
            let then_expr = simplify_constant_mux_conditions(*then_expr, const_env);
            let else_expr = simplify_constant_mux_conditions(*else_expr, const_env);
            match expr_to_const(condition.clone())
                .and_then(|condition| eval_ast_const_expr(&condition, const_env))
            {
                Some(0) => else_expr,
                Some(_) => then_expr,
                None => Expr::Mux {
                    condition: Box::new(condition),
                    then_expr: Box::new(then_expr),
                    else_expr: Box::new(else_expr),
                },
            }
        }
        Expr::Call { name, args } => Expr::Call {
            name,
            args: args
                .into_iter()
                .map(|arg| simplify_constant_mux_conditions(arg, const_env))
                .collect(),
        },
        Expr::Ident(_) | Expr::Literal(_) => expr,
    }
}

/// Substitute the value established by earlier writes to `target` into reads
/// that occur before the merged write is emitted. This handles procedural
/// sequences such as `x = 0; y = x; if (c) x = 1;` without making `y` observe
/// the value of `x` from the preceding process activation.
fn substitute_intermediate_comb_value_reads(
    guarded: &mut [ConditionalAssignment],
    indices: &[usize],
    target: &LValue,
    initial: Option<Expr>,
    packed_dimensions: &PackedDimensions,
) -> Result<(), AnalyzerError> {
    let first = *indices.first().expect("group is non-empty");
    let mut initialized = initial.is_some();
    let mut established = initial.unwrap_or_else(comb_previous_value_placeholder);
    let mut write_index = 0;
    let mut prior_target_writes: Vec<(usize, ConditionalAssignment)> = Vec::new();
    let mut guard_values: HashMap<usize, Expr> = HashMap::default();
    let mut path_values: HashMap<usize, Expr> = HashMap::default();
    let whole_target = match target {
        LValue::Select { name, .. } => whole_packed_lvalue(name, packed_dimensions),
        LValue::Ident(_) => None,
    };
    let mut whole_established = whole_target.as_ref().and_then(|whole_target| {
        overlapping_value_before(guarded, first, whole_target, packed_dimensions)
    });

    for (index, guarded_assignment) in guarded.iter_mut().enumerate().skip(first) {
        let is_target_write = indices.get(write_index) == Some(&index);
        if let Some(condition) = guarded_assignment.condition.take() {
            let condition = if let Some(boundary) = guarded_assignment.guard_boundary {
                let value = guard_values
                    .entry(boundary)
                    .or_insert_with(|| established.clone());
                if initialized {
                    substitute_comb_value_reads(
                        condition,
                        target,
                        value,
                        whole_established.as_ref(),
                        packed_dimensions,
                    )
                } else {
                    condition
                }
            } else if initialized {
                substitute_comb_value_reads(
                    condition,
                    target,
                    &established,
                    whole_established.as_ref(),
                    packed_dimensions,
                )
            } else {
                condition
            };
            guarded_assignment.condition = Some(condition);
        }

        let path_value = guarded_assignment
            .path_epochs
            .iter()
            .find_map(|epoch| path_values.get(epoch));
        if initialized || path_value.is_some() {
            let value = path_value.unwrap_or(&established);
            let assignment = guarded_assignment.assignment.clone();
            let lhs = substitute_comb_lvalue_reads(
                assignment.lhs_value().clone(),
                target,
                value,
                whole_established.as_ref(),
                packed_dimensions,
            );
            guarded_assignment.assignment = Assignment::new(
                lhs,
                substitute_comb_value_reads(
                    assignment.rhs,
                    target,
                    value,
                    whole_established.as_ref(),
                    packed_dimensions,
                ),
            );
        } else if !is_target_write
            && (guarded_assignment
                .condition()
                .is_some_and(|condition| expr_references_lvalue(condition, target))
                || expr_references_lvalue(guarded_assignment.assignment().rhs(), target))
        {
            return Err(AnalyzerError::Unsupported(
                "read-before-write dependency inside always_comb".to_string(),
            ));
        }

        if !is_target_write {
            continue;
        }
        write_index += 1;
        let write = &*guarded_assignment;
        let value = write.assignment().rhs().clone();
        if let (Some(whole_target), Some(current_whole)) =
            (whole_target.as_ref(), whole_established.clone())
            && let Some((updated, _)) = selected_value_after_write(
                &current_whole,
                whole_target,
                target,
                &value,
                packed_dimensions,
            )
        {
            whole_established = Some(match write.condition() {
                None => updated,
                Some(condition) => Expr::Mux {
                    condition: Box::new(condition.clone()),
                    then_expr: Box::new(updated),
                    else_expr: Box::new(current_whole),
                },
            });
        }
        if let Some(chain_start) = write.exhaustive_fallback_start {
            established = value;
            for (prior_index, prior_write) in &prior_target_writes {
                if *prior_index >= chain_start && prior_write.path_epochs != write.path_epochs {
                    established = fold_conditional_assignment_over(established, prior_write);
                }
            }
            initialized = true;
        } else {
            if write.condition().is_none() {
                initialized = true;
            }
            established = fold_conditional_assignment_over(established, write);
        }
        for (depth, epoch) in write.path_epochs.iter().enumerate() {
            if depth == 0 {
                path_values.insert(*epoch, write.assignment().rhs().clone());
            } else {
                let current = path_values
                    .get(epoch)
                    .cloned()
                    .unwrap_or_else(|| established.clone());
                path_values.insert(*epoch, fold_conditional_assignment_over(current, write));
            }
        }
        prior_target_writes.push((index, write.clone()));
    }
    Ok(())
}

fn substitute_comb_value_reads(
    expr: Expr,
    target: &LValue,
    value: &Expr,
    whole_value: Option<&Expr>,
    packed_dimensions: &PackedDimensions,
) -> Expr {
    let substituted =
        if let (LValue::Select { name, .. }, Some(whole_value)) = (target, whole_value) {
            let mut env = HashMap::default();
            env.insert(name.clone(), whole_value.clone());
            substitute_expr_idents(expr, &env)
        } else {
            substitute_expr_lvalue(expr, target, value, packed_dimensions)
        };
    simplify_single_bit_concat_selects(substituted, packed_dimensions)
}

fn substitute_comb_lvalue_reads(
    lvalue: LValue,
    target: &LValue,
    value: &Expr,
    whole_value: Option<&Expr>,
    packed_dimensions: &PackedDimensions,
) -> LValue {
    let LValue::Select {
        name,
        msb,
        lsb,
        signed,
        array_slice_width,
        array_slice_reversed,
    } = lvalue
    else {
        return lvalue;
    };
    let substitute_bound = |bound: ConstExpr| {
        let original = bound.clone();
        expr_to_const(substitute_comb_value_reads(
            const_expr_to_expr(bound),
            target,
            value,
            whole_value,
            packed_dimensions,
        ))
        .unwrap_or(original)
    };
    LValue::Select {
        name,
        msb: substitute_bound(msb),
        lsb: substitute_bound(lsb),
        signed,
        array_slice_width,
        array_slice_reversed,
    }
}

fn simplify_single_bit_concat_selects(expr: Expr, packed_dimensions: &PackedDimensions) -> Expr {
    match expr {
        Expr::Select {
            expr,
            msb,
            lsb,
            signed,
        } => {
            let expr = simplify_single_bit_concat_selects(*expr, packed_dimensions);
            let bounds = match (
                eval_ast_const_expr(&msb, &packed_dimensions.const_env),
                eval_ast_const_expr(&lsb, &packed_dimensions.const_env),
            ) {
                (Some(msb), Some(lsb)) => Some((msb, lsb)),
                _ => None,
            };
            if let (Some((msb, lsb)), Expr::Concat(parts)) = (bounds, &expr)
                && let (Ok(msb), Ok(lsb)) = (usize::try_from(msb), usize::try_from(lsb))
                && msb >= lsb
            {
                let mut offset = 0usize;
                for part in parts.iter().rev() {
                    let Some(width) = expr_static_width(part, packed_dimensions) else {
                        break;
                    };
                    let end = offset.saturating_add(width);
                    if lsb == offset && msb.checked_add(1) == Some(end) {
                        return Expr::Resize {
                            expr: Box::new(part.clone()),
                            width,
                            signed: false,
                        };
                    }
                    if msb == lsb && msb < end {
                        let selected = msb - offset;
                        return if width == 1 {
                            part.clone()
                        } else {
                            Expr::Select {
                                expr: Box::new(part.clone()),
                                msb: ConstExpr::Literal(selected.to_string()),
                                lsb: ConstExpr::Literal(selected.to_string()),
                                signed: false,
                            }
                        };
                    }
                    offset = end;
                }
            }
            Expr::Select {
                expr: Box::new(expr),
                msb,
                lsb,
                signed,
            }
        }
        Expr::Concat(parts) => Expr::Concat(
            parts
                .into_iter()
                .map(|part| simplify_single_bit_concat_selects(part, packed_dimensions))
                .collect(),
        ),
        Expr::RepeatConcat { count, parts } => Expr::RepeatConcat {
            count,
            parts: parts
                .into_iter()
                .map(|part| simplify_single_bit_concat_selects(part, packed_dimensions))
                .collect(),
        },
        Expr::Resize {
            expr,
            width,
            signed,
        } => Expr::Resize {
            expr: Box::new(simplify_single_bit_concat_selects(*expr, packed_dimensions)),
            width,
            signed,
        },
        Expr::Unary { op, expr } => Expr::Unary {
            op,
            expr: Box::new(simplify_single_bit_concat_selects(*expr, packed_dimensions)),
        },
        Expr::Binary { left, op, right } => Expr::Binary {
            left: Box::new(simplify_single_bit_concat_selects(*left, packed_dimensions)),
            op,
            right: Box::new(simplify_single_bit_concat_selects(
                *right,
                packed_dimensions,
            )),
        },
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => Expr::Mux {
            condition: Box::new(simplify_single_bit_concat_selects(
                *condition,
                packed_dimensions,
            )),
            then_expr: Box::new(simplify_single_bit_concat_selects(
                *then_expr,
                packed_dimensions,
            )),
            else_expr: Box::new(simplify_single_bit_concat_selects(
                *else_expr,
                packed_dimensions,
            )),
        },
        Expr::Call { name, args } => Expr::Call {
            name,
            args: args
                .into_iter()
                .map(|arg| simplify_single_bit_concat_selects(arg, packed_dimensions))
                .collect(),
        },
        Expr::Ident(_) | Expr::Literal(_) => expr,
    }
}

fn expr_static_width(expr: &Expr, packed_dimensions: &PackedDimensions) -> Option<usize> {
    match expr {
        Expr::Ident(name) => lvalue_expr_type(&LValue::Ident(name.clone()), packed_dimensions)
            .map(|r#type| r#type.width),
        Expr::Literal(literal) => {
            typecheck::parse_integral_literal(literal).map(|literal| literal.width)
        }
        Expr::Select { msb, lsb, .. } => {
            let msb = eval_ast_const_expr(msb, &packed_dimensions.const_env)?;
            let lsb = eval_ast_const_expr(lsb, &packed_dimensions.const_env)?;
            usize::try_from(msb.abs_diff(lsb)).ok()?.checked_add(1)
        }
        Expr::Concat(parts) => parts.iter().try_fold(0usize, |width, part| {
            width.checked_add(expr_static_width(part, packed_dimensions)?)
        }),
        Expr::RepeatConcat { count, parts } => {
            let count = eval_ast_const_expr(count, &packed_dimensions.const_env)?;
            let count = usize::try_from(count).ok()?;
            let width = parts.iter().try_fold(0usize, |width, part| {
                width.checked_add(expr_static_width(part, packed_dimensions)?)
            })?;
            width.checked_mul(count)
        }
        Expr::Resize { width, .. } => Some(*width),
        Expr::Unary { op, expr } => {
            if matches!(
                op,
                UnaryOp::LogicNot | UnaryOp::RedAnd | UnaryOp::RedOr | UnaryOp::RedXor
            ) {
                Some(1)
            } else {
                expr_static_width(expr, packed_dimensions)
            }
        }
        Expr::Binary { left, op, right } => {
            if matches!(
                op,
                BinaryOp::LogicAnd
                    | BinaryOp::LogicOr
                    | BinaryOp::Eq
                    | BinaryOp::Ne
                    | BinaryOp::EqCase
                    | BinaryOp::NeCase
                    | BinaryOp::EqWildcard
                    | BinaryOp::NeWildcard
                    | BinaryOp::Lt
                    | BinaryOp::Le
                    | BinaryOp::Gt
                    | BinaryOp::Ge
            ) {
                Some(1)
            } else if matches!(op, BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar) {
                expr_static_width(left, packed_dimensions)
            } else {
                Some(
                    expr_static_width(left, packed_dimensions)?
                        .max(expr_static_width(right, packed_dimensions)?),
                )
            }
        }
        Expr::Mux {
            then_expr,
            else_expr,
            ..
        } => Some(
            expr_static_width(then_expr, packed_dimensions)?
                .max(expr_static_width(else_expr, packed_dimensions)?),
        ),
        Expr::Call { .. } => None,
    }
}

fn lvalues_overlap(left: &LValue, right: &LValue, const_env: &HashMap<String, i128>) -> bool {
    match (left, right) {
        (LValue::Ident(left), LValue::Ident(right)) => left == right,
        (LValue::Ident(left), LValue::Select { name: right, .. })
        | (LValue::Select { name: left, .. }, LValue::Ident(right)) => left == right,
        (
            LValue::Select {
                name: left_name,
                msb: left_msb,
                lsb: left_lsb,
                ..
            },
            LValue::Select {
                name: right_name,
                msb: right_msb,
                lsb: right_lsb,
                ..
            },
        ) => {
            if left_name != right_name {
                return false;
            }
            match (
                eval_ast_const_expr(left_msb, const_env),
                eval_ast_const_expr(left_lsb, const_env),
                eval_ast_const_expr(right_msb, const_env),
                eval_ast_const_expr(right_lsb, const_env),
            ) {
                (Some(left_msb), Some(left_lsb), Some(right_msb), Some(right_lsb)) => {
                    left_msb.min(left_lsb) <= right_msb.max(right_lsb)
                        && right_msb.min(right_lsb) <= left_msb.max(left_lsb)
                }
                _ => true,
            }
        }
    }
}

fn overlapping_value_before(
    guarded: &[ConditionalAssignment],
    before: usize,
    target: &LValue,
    packed_dimensions: &PackedDimensions,
) -> Option<Expr> {
    if let LValue::Ident(target_name) = target {
        let selected_target = whole_packed_lvalue(target_name, packed_dimensions)?;
        return overlapping_value_before(guarded, before, &selected_target, packed_dimensions);
    }
    let mut current = comb_previous_value_placeholder();
    let mut initialized = false;
    let mut states_before = vec![(current.clone(), initialized)];
    for (index, write) in guarded[..before].iter().enumerate() {
        let Some((updated, covers_target)) = selected_value_after_write(
            &current,
            target,
            write.assignment().lhs_value(),
            write.assignment().rhs(),
            packed_dimensions,
        ) else {
            states_before.push((current.clone(), initialized));
            continue;
        };
        if let Some(chain_start) = write.exhaustive_fallback_start {
            let (base, base_initialized) = states_before
                .get(chain_start)
                .cloned()
                .unwrap_or_else(|| (comb_previous_value_placeholder(), false));
            let Some((mut branch_value, fallback_covers_target)) = selected_value_after_write(
                &base,
                target,
                write.assignment().lhs_value(),
                write.assignment().rhs(),
                packed_dimensions,
            ) else {
                states_before.push((current.clone(), initialized));
                continue;
            };
            for prior in &guarded[chain_start..index] {
                let Some((prior_value, _)) = selected_value_after_write(
                    &branch_value,
                    target,
                    prior.assignment().lhs_value(),
                    prior.assignment().rhs(),
                    packed_dimensions,
                ) else {
                    continue;
                };
                branch_value = match prior.condition() {
                    None => prior_value,
                    Some(condition) => Expr::Mux {
                        condition: Box::new(condition.clone()),
                        then_expr: Box::new(prior_value),
                        else_expr: Box::new(branch_value),
                    },
                };
            }
            current = branch_value;
            initialized = base_initialized || fallback_covers_target;
        } else {
            current = match write.condition() {
                None => {
                    initialized |= covers_target;
                    updated
                }
                Some(condition) => Expr::Mux {
                    condition: Box::new(condition.clone()),
                    then_expr: Box::new(updated),
                    else_expr: Box::new(current),
                },
            };
        }
        states_before.push((current.clone(), initialized));
    }
    initialized.then_some(current)
}

fn whole_packed_lvalue(name: &str, packed_dimensions: &PackedDimensions) -> Option<LValue> {
    let dimensions = packed_dimensions.get(name)?;
    let (msb, lsb) = if dimensions.unpacked.is_empty()
        && dimensions.packed.len() == 1
        && !dimensions.packed[0].normalize_single
    {
        (
            dimensions.packed[0].left.clone(),
            dimensions.packed[0].right.clone(),
        )
    } else {
        let width = product_expr(
            &dimensions
                .packed
                .iter()
                .map(|dimension| dimension.width.clone())
                .chain(
                    dimensions
                        .unpacked
                        .iter()
                        .map(|dimension| dimension.width.clone()),
                )
                .collect::<Vec<_>>(),
        );
        (
            ConstExpr::Binary {
                left: Box::new(width),
                op: BinaryOp::Sub,
                right: Box::new(ConstExpr::Literal("1".to_string())),
            },
            ConstExpr::Literal("0".to_string()),
        )
    };
    Some(LValue::Select {
        name: name.to_string(),
        msb,
        lsb,
        signed: dimensions.signed,
        array_slice_width: None,
        array_slice_reversed: false,
    })
}

fn selected_value_after_write(
    current: &Expr,
    target: &LValue,
    write_target: &LValue,
    write_value: &Expr,
    packed_dimensions: &PackedDimensions,
) -> Option<(Expr, bool)> {
    let const_env = &packed_dimensions.const_env;
    let LValue::Select {
        name: target_name,
        msb: target_msb_expr,
        lsb: target_lsb_expr,
        signed,
        ..
    } = target
    else {
        return None;
    };
    match write_target {
        LValue::Ident(write_name) => (write_name == target_name).then(|| {
            let (msb, lsb) = whole_write_select_offsets(
                target_name,
                target_msb_expr,
                target_lsb_expr,
                packed_dimensions,
            );
            (
                Expr::Select {
                    expr: Box::new(write_value.clone()),
                    msb,
                    lsb,
                    signed: *signed,
                },
                true,
            )
        }),
        LValue::Select {
            name: write_name,
            msb: write_msb_expr,
            lsb: write_lsb_expr,
            ..
        } => {
            if write_name != target_name {
                return None;
            }
            let target_msb = eval_ast_const_expr(target_msb_expr, const_env)?;
            let target_lsb = eval_ast_const_expr(target_lsb_expr, const_env)?;
            let (write_msb, write_lsb) = match (
                eval_ast_const_expr(write_msb_expr, const_env),
                eval_ast_const_expr(write_lsb_expr, const_env),
            ) {
                (Some(msb), Some(lsb)) => (msb, lsb),
                _ => {
                    return dynamic_selected_value_after_write(
                        current,
                        target,
                        write_target,
                        write_value,
                        packed_dimensions,
                    );
                }
            };
            let target_low = target_msb.min(target_lsb);
            let target_high = target_msb.max(target_lsb);
            let overlap_low = target_low.max(write_msb.min(write_lsb));
            let overlap_high = target_high.min(write_msb.max(write_lsb));
            if overlap_low > overlap_high {
                return None;
            }

            let target_step = if target_msb >= target_lsb { -1 } else { 1 };
            let overlap_first = if target_step < 0 {
                overlap_high
            } else {
                overlap_low
            };
            let overlap_last = if target_step < 0 {
                overlap_low
            } else {
                overlap_high
            };
            let mut parts = Vec::new();
            if target_msb != overlap_first {
                parts.push(selected_value_read(
                    current,
                    target_msb,
                    target_lsb,
                    target_msb,
                    overlap_first.checked_sub(target_step)?,
                )?);
            }
            parts.push(selected_value_read(
                write_value,
                write_msb,
                write_lsb,
                overlap_first,
                overlap_last,
            )?);
            if overlap_last != target_lsb {
                parts.push(selected_value_read(
                    current,
                    target_msb,
                    target_lsb,
                    overlap_last.checked_add(target_step)?,
                    target_lsb,
                )?);
            }
            let value = if parts.len() == 1 {
                parts.pop()?
            } else {
                Expr::Concat(parts)
            };
            Some((
                value,
                overlap_low == target_low && overlap_high == target_high,
            ))
        }
    }
}

fn dynamic_selected_value_after_write(
    current: &Expr,
    target: &LValue,
    write_target: &LValue,
    write_value: &Expr,
    packed_dimensions: &PackedDimensions,
) -> Option<(Expr, bool)> {
    let LValue::Select {
        name,
        msb: write_msb,
        lsb: write_lsb,
        signed,
        ..
    } = write_target
    else {
        return None;
    };

    // Runtime indices still describe a fixed-width selection. Evaluate one
    // representative position to recover its direction and width, then use
    // case-equality guards so unknown and out-of-range positions are no-ops.
    let mut sample_env = packed_dimensions.const_env.clone();
    for variable in packed_dimensions.keys() {
        sample_env.entry(variable.clone()).or_insert(0);
    }
    let sample_msb = eval_ast_const_expr(write_msb, &sample_env)?;
    let sample_lsb = eval_ast_const_expr(write_lsb, &sample_env)?;
    let delta = sample_msb.checked_sub(sample_lsb)?;
    let whole = whole_packed_lvalue(name, packed_dimensions)?;
    let LValue::Select {
        msb: whole_msb,
        lsb: whole_lsb,
        ..
    } = whole
    else {
        unreachable!("whole packed lvalue is always a selection");
    };
    let whole_msb = eval_ast_const_expr(&whole_msb, &packed_dimensions.const_env)?;
    let whole_lsb = eval_ast_const_expr(&whole_lsb, &packed_dimensions.const_env)?;
    let whole_low = whole_msb.min(whole_lsb);
    let whole_high = whole_msb.max(whole_lsb);
    let mut result = current.clone();
    let mut matched = false;
    for candidate_lsb in whole_low..=whole_high {
        let Some(candidate_msb) = candidate_lsb.checked_add(delta) else {
            continue;
        };
        if candidate_msb < whole_low || candidate_msb > whole_high {
            continue;
        }
        let candidate = LValue::Select {
            name: name.clone(),
            msb: const_expr_from_i128(candidate_msb),
            lsb: const_expr_from_i128(candidate_lsb),
            signed: *signed,
            array_slice_width: None,
            array_slice_reversed: false,
        };
        let Some((updated, _)) =
            selected_value_after_write(current, target, &candidate, write_value, packed_dimensions)
        else {
            continue;
        };
        let matches_msb = Expr::Binary {
            left: Box::new(const_expr_to_expr(write_msb.clone())),
            op: BinaryOp::EqCase,
            right: Box::new(const_expr_to_expr(const_expr_from_i128(candidate_msb))),
        };
        let matches_lsb = Expr::Binary {
            left: Box::new(const_expr_to_expr(write_lsb.clone())),
            op: BinaryOp::EqCase,
            right: Box::new(const_expr_to_expr(const_expr_from_i128(candidate_lsb))),
        };
        result = Expr::Mux {
            condition: Box::new(Expr::Binary {
                left: Box::new(matches_msb),
                op: BinaryOp::LogicAnd,
                right: Box::new(matches_lsb),
            }),
            then_expr: Box::new(updated),
            else_expr: Box::new(result),
        };
        matched = true;
    }
    matched.then_some((result, false))
}

fn whole_write_select_offsets(
    name: &str,
    msb: &ConstExpr,
    lsb: &ConstExpr,
    packed_dimensions: &PackedDimensions,
) -> (ConstExpr, ConstExpr) {
    let Some(dimensions) = packed_dimensions.get(name) else {
        return (msb.clone(), lsb.clone());
    };
    if dimensions.unpacked.is_empty()
        && dimensions.packed.len() == 1
        && !dimensions.packed[0].normalize_single
    {
        let dimension = &dimensions.packed[0];
        return (
            packed_index_offset(dimension, msb.clone()),
            packed_index_offset(dimension, lsb.clone()),
        );
    }
    (msb.clone(), lsb.clone())
}

fn selected_value_read(
    value: &Expr,
    value_msb: i128,
    value_lsb: i128,
    first_coordinate: i128,
    last_coordinate: i128,
) -> Option<Expr> {
    Some(Expr::Select {
        expr: Box::new(value.clone()),
        msb: const_expr_from_i128(selected_target_bit(value_msb, value_lsb, first_coordinate)?),
        lsb: const_expr_from_i128(selected_target_bit(value_msb, value_lsb, last_coordinate)?),
        signed: false,
    })
}

const COMB_PREVIOUS_VALUE: &str = "\0celox_comb_previous_value";

fn comb_previous_value_placeholder() -> Expr {
    Expr::Ident(COMB_PREVIOUS_VALUE.to_string())
}

fn expr_contains_comb_previous_value(expr: &Expr) -> bool {
    match expr {
        Expr::Ident(name) => name == COMB_PREVIOUS_VALUE,
        Expr::Literal(_) => false,
        Expr::Select { expr, .. } | Expr::Resize { expr, .. } | Expr::Unary { expr, .. } => {
            expr_contains_comb_previous_value(expr)
        }
        Expr::Concat(parts) | Expr::RepeatConcat { parts, .. } => {
            parts.iter().any(expr_contains_comb_previous_value)
        }
        Expr::Binary { left, right, .. } => {
            expr_contains_comb_previous_value(left) || expr_contains_comb_previous_value(right)
        }
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            expr_contains_comb_previous_value(condition)
                || expr_contains_comb_previous_value(then_expr)
                || expr_contains_comb_previous_value(else_expr)
        }
        Expr::Call { args, .. } => args.iter().any(expr_contains_comb_previous_value),
    }
}

fn substitute_expr_lvalue(
    expr: Expr,
    target: &LValue,
    value: &Expr,
    packed_dimensions: &PackedDimensions,
) -> Expr {
    if expr_matches_lvalue(&expr, target) {
        return value.clone();
    }
    if let Some(replacement) =
        substitute_overlapping_selected_read(&expr, target, value, packed_dimensions)
    {
        return replacement;
    }
    match expr {
        Expr::Ident(_) | Expr::Literal(_) => expr,
        Expr::Select {
            expr,
            msb,
            lsb,
            signed,
        } => Expr::Select {
            expr: Box::new(substitute_expr_lvalue(
                *expr,
                target,
                value,
                packed_dimensions,
            )),
            msb: substitute_const_expr_lvalue(msb, target, value, packed_dimensions),
            lsb: substitute_const_expr_lvalue(lsb, target, value, packed_dimensions),
            signed,
        },
        Expr::Concat(parts) => Expr::Concat(
            parts
                .into_iter()
                .map(|part| substitute_expr_lvalue(part, target, value, packed_dimensions))
                .collect(),
        ),
        Expr::RepeatConcat { count, parts } => Expr::RepeatConcat {
            count,
            parts: parts
                .into_iter()
                .map(|part| substitute_expr_lvalue(part, target, value, packed_dimensions))
                .collect(),
        },
        Expr::Resize {
            expr,
            width,
            signed,
        } => Expr::Resize {
            expr: Box::new(substitute_expr_lvalue(
                *expr,
                target,
                value,
                packed_dimensions,
            )),
            width,
            signed,
        },
        Expr::Unary { op, expr } => Expr::Unary {
            op,
            expr: Box::new(substitute_expr_lvalue(
                *expr,
                target,
                value,
                packed_dimensions,
            )),
        },
        Expr::Binary { left, op, right } => Expr::Binary {
            left: Box::new(substitute_expr_lvalue(
                *left,
                target,
                value,
                packed_dimensions,
            )),
            op,
            right: Box::new(substitute_expr_lvalue(
                *right,
                target,
                value,
                packed_dimensions,
            )),
        },
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => Expr::Mux {
            condition: Box::new(substitute_expr_lvalue(
                *condition,
                target,
                value,
                packed_dimensions,
            )),
            then_expr: Box::new(substitute_expr_lvalue(
                *then_expr,
                target,
                value,
                packed_dimensions,
            )),
            else_expr: Box::new(substitute_expr_lvalue(
                *else_expr,
                target,
                value,
                packed_dimensions,
            )),
        },
        Expr::Call { name, args } => Expr::Call {
            name,
            args: args
                .into_iter()
                .map(|arg| substitute_expr_lvalue(arg, target, value, packed_dimensions))
                .collect(),
        },
    }
}

fn substitute_const_expr_lvalue(
    expr: ConstExpr,
    target: &LValue,
    value: &Expr,
    packed_dimensions: &PackedDimensions,
) -> ConstExpr {
    let original = expr.clone();
    expr_to_const(substitute_expr_lvalue(
        const_expr_to_expr(expr),
        target,
        value,
        packed_dimensions,
    ))
    .unwrap_or(original)
}

fn substitute_overlapping_selected_read(
    expr: &Expr,
    target: &LValue,
    value: &Expr,
    packed_dimensions: &PackedDimensions,
) -> Option<Expr> {
    let const_env = &packed_dimensions.const_env;
    let LValue::Select {
        name: target_name,
        msb: target_msb,
        lsb: target_lsb,
        ..
    } = target
    else {
        return None;
    };
    if let Expr::Ident(read_name) = expr {
        if read_name != target_name {
            return None;
        }
        let whole = whole_packed_lvalue(read_name, packed_dimensions)?;
        return selected_value_after_write(
            &Expr::Ident(read_name.clone()),
            &whole,
            target,
            value,
            packed_dimensions,
        )
        .map(|(value, _)| value);
    }
    let Expr::Select {
        expr: read_base,
        msb: read_msb,
        lsb: read_lsb,
        ..
    } = expr
    else {
        return None;
    };
    let Expr::Ident(read_name) = &**read_base else {
        return None;
    };
    if read_name != target_name {
        return None;
    }

    let read_msb = eval_ast_const_expr(read_msb, const_env)?;
    let read_lsb = eval_ast_const_expr(read_lsb, const_env)?;
    let target_msb = eval_ast_const_expr(target_msb, const_env)?;
    let target_lsb = eval_ast_const_expr(target_lsb, const_env)?;
    let overlap_low = read_msb.min(read_lsb).max(target_msb.min(target_lsb));
    let overlap_high = read_msb.max(read_lsb).min(target_msb.max(target_lsb));
    if overlap_low > overlap_high {
        return None;
    }

    let read_step = if read_msb >= read_lsb { -1 } else { 1 };
    let overlap_first = if read_step < 0 {
        overlap_high
    } else {
        overlap_low
    };
    let overlap_last = if read_step < 0 {
        overlap_low
    } else {
        overlap_high
    };
    let mut parts = Vec::new();
    if read_msb != overlap_first {
        parts.push(selected_ident_read(
            read_name,
            read_msb,
            overlap_first.checked_sub(read_step)?,
        ));
    }

    let value_msb = selected_target_bit(target_msb, target_lsb, overlap_first)?;
    let value_lsb = selected_target_bit(target_msb, target_lsb, overlap_last)?;
    parts.push(Expr::Select {
        expr: Box::new(value.clone()),
        msb: const_expr_from_i128(value_msb),
        lsb: const_expr_from_i128(value_lsb),
        signed: false,
    });

    if overlap_last != read_lsb {
        parts.push(selected_ident_read(
            read_name,
            overlap_last.checked_add(read_step)?,
            read_lsb,
        ));
    }
    if parts.len() == 1 {
        parts.pop()
    } else {
        Some(Expr::Concat(parts))
    }
}

fn selected_target_bit(target_msb: i128, target_lsb: i128, coordinate: i128) -> Option<i128> {
    if target_msb >= target_lsb {
        coordinate.checked_sub(target_lsb)
    } else {
        target_lsb.checked_sub(coordinate)
    }
}

fn selected_ident_read(name: &str, msb: i128, lsb: i128) -> Expr {
    Expr::Select {
        expr: Box::new(Expr::Ident(name.to_string())),
        msb: const_expr_from_i128(msb),
        lsb: const_expr_from_i128(lsb),
        signed: false,
    }
}

fn expr_matches_lvalue(expr: &Expr, target: &LValue) -> bool {
    match (expr, target) {
        (Expr::Ident(expr_name), LValue::Ident(target_name)) => expr_name == target_name,
        (
            Expr::Select { expr, msb, lsb, .. },
            LValue::Select {
                name,
                msb: target_msb,
                lsb: target_lsb,
                ..
            },
        ) => {
            matches!(&**expr, Expr::Ident(expr_name) if expr_name == name)
                && msb == target_msb
                && lsb == target_lsb
        }
        _ => false,
    }
}

fn expr_references_lvalue(expr: &Expr, target: &LValue) -> bool {
    if expr_matches_lvalue(expr, target) {
        return true;
    }
    match expr {
        Expr::Ident(_) | Expr::Literal(_) => false,
        Expr::Select { expr, msb, lsb, .. } => {
            expr_references_lvalue(expr, target)
                || expr_references_lvalue(&const_expr_to_expr(msb.clone()), target)
                || expr_references_lvalue(&const_expr_to_expr(lsb.clone()), target)
        }
        Expr::Resize { expr, .. } | Expr::Unary { expr, .. } => {
            expr_references_lvalue(expr, target)
        }
        Expr::Concat(parts) | Expr::RepeatConcat { parts, .. } => parts
            .iter()
            .any(|part| expr_references_lvalue(part, target)),
        Expr::Binary { left, right, .. } => {
            expr_references_lvalue(left, target) || expr_references_lvalue(right, target)
        }
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            expr_references_lvalue(condition, target)
                || expr_references_lvalue(then_expr, target)
                || expr_references_lvalue(else_expr, target)
        }
        Expr::Call { args, .. } => args.iter().any(|arg| expr_references_lvalue(arg, target)),
    }
}

fn expr_references_overlapping_lvalue(
    expr: &Expr,
    target: &LValue,
    const_env: &HashMap<String, i128>,
) -> bool {
    if expr_matches_lvalue(expr, target) {
        return true;
    }
    match expr {
        Expr::Ident(name) => match target {
            LValue::Ident(target_name)
            | LValue::Select {
                name: target_name, ..
            } => name == target_name,
        },
        Expr::Literal(_) => false,
        Expr::Select {
            expr: selected,
            msb,
            lsb,
            ..
        } => {
            let selected_reference = if let Expr::Ident(read_name) = &**selected {
                match target {
                    LValue::Ident(target_name) => read_name == target_name,
                    LValue::Select {
                        name: target_name,
                        msb: target_msb,
                        lsb: target_lsb,
                        ..
                    } if read_name == target_name => match (
                        eval_ast_const_expr(msb, const_env),
                        eval_ast_const_expr(lsb, const_env),
                        eval_ast_const_expr(target_msb, const_env),
                        eval_ast_const_expr(target_lsb, const_env),
                    ) {
                        (Some(msb), Some(lsb), Some(target_msb), Some(target_lsb)) => {
                            msb.min(lsb) <= target_msb.max(target_lsb)
                                && target_msb.min(target_lsb) <= msb.max(lsb)
                        }
                        _ => true,
                    },
                    _ => false,
                }
            } else {
                expr_references_overlapping_lvalue(selected, target, const_env)
            };
            selected_reference
                || expr_references_overlapping_lvalue(
                    &const_expr_to_expr(msb.clone()),
                    target,
                    const_env,
                )
                || expr_references_overlapping_lvalue(
                    &const_expr_to_expr(lsb.clone()),
                    target,
                    const_env,
                )
        }
        Expr::Resize { expr, .. } | Expr::Unary { expr, .. } => {
            expr_references_overlapping_lvalue(expr, target, const_env)
        }
        Expr::Concat(parts) | Expr::RepeatConcat { parts, .. } => parts
            .iter()
            .any(|part| expr_references_overlapping_lvalue(part, target, const_env)),
        Expr::Binary { left, right, .. } => {
            expr_references_overlapping_lvalue(left, target, const_env)
                || expr_references_overlapping_lvalue(right, target, const_env)
        }
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            expr_references_overlapping_lvalue(condition, target, const_env)
                || expr_references_overlapping_lvalue(then_expr, target, const_env)
                || expr_references_overlapping_lvalue(else_expr, target, const_env)
        }
        Expr::Call { args, .. } => args
            .iter()
            .any(|arg| expr_references_overlapping_lvalue(arg, target, const_env)),
    }
}

fn validate_always_comb_statement(stmt: &sv_parser::Statement) -> Result<(), AnalyzerError> {
    match &stmt.nodes.2 {
        sv_parser::StatementItem::BlockingAssignment(_) => Ok(()),
        sv_parser::StatementItem::ConditionalStatement(conditional) => {
            validate_always_comb_statement_or_null(&conditional.nodes.3)?;
            for (_, _, _, branch) in &conditional.nodes.4 {
                validate_always_comb_statement_or_null(branch)?;
            }
            if let Some((_, branch)) = &conditional.nodes.5 {
                validate_always_comb_statement_or_null(branch)?;
            }
            Ok(())
        }
        sv_parser::StatementItem::CaseStatement(case) => {
            let sv_parser::CaseStatement::Normal(case) = &**case else {
                return Err(AnalyzerError::Unsupported(
                    "casez, casex, or pattern case inside always_comb".to_string(),
                ));
            };
            if !matches!(case.nodes.1, sv_parser::CaseKeyword::Case(_)) {
                return Err(AnalyzerError::Unsupported(
                    "casez or casex inside always_comb".to_string(),
                ));
            }
            for item in std::iter::once(&case.nodes.3).chain(case.nodes.4.iter()) {
                match item {
                    sv_parser::CaseItem::NonDefault(item) => {
                        validate_always_comb_statement_or_null(&item.nodes.2)?;
                    }
                    sv_parser::CaseItem::Default(item) => {
                        validate_always_comb_statement_or_null(&item.nodes.2)?;
                    }
                }
            }
            Ok(())
        }
        sv_parser::StatementItem::SeqBlock(block) => {
            if !block.nodes.2.is_empty() {
                return Err(AnalyzerError::Unsupported(
                    "block-local declaration inside always_comb".to_string(),
                ));
            }
            for stmt in &block.nodes.3 {
                if let sv_parser::StatementOrNull::Statement(stmt) = stmt {
                    validate_always_comb_statement(stmt)?;
                }
            }
            Ok(())
        }
        _ => Err(AnalyzerError::Unsupported(
            "unsupported statement inside always_comb".to_string(),
        )),
    }
}

fn validate_always_comb_statement_or_null(
    stmt: &sv_parser::StatementOrNull,
) -> Result<(), AnalyzerError> {
    if let sv_parser::StatementOrNull::Statement(stmt) = stmt {
        validate_always_comb_statement(stmt)?;
    }
    Ok(())
}

fn ff_processes_from_module_node(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    parameter_literals: &HashMap<String, Expr>,
    packed_dimensions: &PackedDimensions,
) -> Result<Vec<FfProcess>, AnalyzerError> {
    let mut processes = Vec::new();
    for item in module_non_port_items(node) {
        ff_processes_from_non_port_module_item(
            item,
            syntax_tree,
            const_env,
            parameter_literals,
            packed_dimensions,
            &mut processes,
        )?;
    }
    Ok(processes)
}

fn ff_processes_from_non_port_module_item(
    item: &sv_parser::NonPortModuleItem,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    parameter_literals: &HashMap<String, Expr>,
    packed_dimensions: &PackedDimensions,
    processes: &mut Vec<FfProcess>,
) -> Result<(), AnalyzerError> {
    match item {
        sv_parser::NonPortModuleItem::GenerateRegion(region) => {
            for item in &region.nodes.1 {
                ff_processes_from_generate_item(
                    item,
                    syntax_tree,
                    const_env,
                    parameter_literals,
                    packed_dimensions,
                    processes,
                )?;
            }
        }
        sv_parser::NonPortModuleItem::ModuleOrGenerateItem(item) => {
            ff_processes_from_module_or_generate_item(
                item,
                syntax_tree,
                const_env,
                parameter_literals,
                packed_dimensions,
                processes,
            )?;
        }
        _ => {}
    }
    Ok(())
}

fn ff_processes_from_generate_item(
    item: &sv_parser::GenerateItem,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    parameter_literals: &HashMap<String, Expr>,
    packed_dimensions: &PackedDimensions,
    processes: &mut Vec<FfProcess>,
) -> Result<(), AnalyzerError> {
    if let sv_parser::GenerateItem::ModuleOrGenerateItem(item) = item {
        ff_processes_from_module_or_generate_item(
            item,
            syntax_tree,
            const_env,
            parameter_literals,
            packed_dimensions,
            processes,
        )?;
    }
    Ok(())
}

fn ff_processes_from_module_or_generate_item(
    item: &sv_parser::ModuleOrGenerateItem,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    parameter_literals: &HashMap<String, Expr>,
    packed_dimensions: &PackedDimensions,
    processes: &mut Vec<FfProcess>,
) -> Result<(), AnalyzerError> {
    if let sv_parser::ModuleOrGenerateItem::ModuleItem(item) = item {
        ff_processes_from_module_common_item(
            &item.nodes.1,
            syntax_tree,
            const_env,
            parameter_literals,
            packed_dimensions,
            processes,
        )?;
    }
    Ok(())
}

fn ff_processes_from_module_common_item(
    item: &sv_parser::ModuleCommonItem,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    parameter_literals: &HashMap<String, Expr>,
    packed_dimensions: &PackedDimensions,
    processes: &mut Vec<FfProcess>,
) -> Result<(), AnalyzerError> {
    match item {
        sv_parser::ModuleCommonItem::AlwaysConstruct(always) => {
            if let Some(process) = ff_process_from_always_construct(
                always,
                syntax_tree,
                const_env,
                parameter_literals,
                packed_dimensions,
            )? {
                processes.push(process);
            }
        }
        sv_parser::ModuleCommonItem::ConditionalGenerateConstruct(generate) => {
            let sv_parser::ConditionalGenerateConstruct::If(generate) = &**generate else {
                return Ok(());
            };
            let Some(condition) = const_expr_from_ref_node(
                RefNode::ConstantExpression(&generate.nodes.1.nodes.1),
                syntax_tree,
            ) else {
                return Err(AnalyzerError::Unsupported(
                    "unknown conditional-generate condition".to_string(),
                ));
            };
            let Some(condition_value) = eval_ast_const_expr(&condition, const_env) else {
                return Err(AnalyzerError::Unsupported(
                    "unknown conditional-generate condition".to_string(),
                ));
            };
            let block = if condition_value != 0 {
                Some(&generate.nodes.2)
            } else {
                generate.nodes.3.as_ref().map(|(_, block)| block)
            };
            if let Some(block) = block {
                ff_processes_from_generate_block(
                    block,
                    syntax_tree,
                    const_env,
                    parameter_literals,
                    packed_dimensions,
                    processes,
                )?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn ff_processes_from_generate_block(
    block: &sv_parser::GenerateBlock,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    parameter_literals: &HashMap<String, Expr>,
    packed_dimensions: &PackedDimensions,
    processes: &mut Vec<FfProcess>,
) -> Result<(), AnalyzerError> {
    match block {
        sv_parser::GenerateBlock::GenerateItem(item) => {
            ff_processes_from_generate_item(
                item,
                syntax_tree,
                const_env,
                parameter_literals,
                packed_dimensions,
                processes,
            )?;
        }
        sv_parser::GenerateBlock::Multiple(block) => {
            let mut block_env = const_env.clone();
            let mut block_parameter_literals = parameter_literals.clone();
            for item in &block.nodes.3 {
                if add_localparams_from_generate_item_with_literals(
                    item,
                    syntax_tree,
                    &mut block_env,
                    Some(&mut block_parameter_literals),
                ) {
                    continue;
                }
                ff_processes_from_generate_item(
                    item,
                    syntax_tree,
                    &block_env,
                    &block_parameter_literals,
                    packed_dimensions,
                    processes,
                )?;
            }
        }
    }
    Ok(())
}

fn ff_process_from_always_construct(
    always: &sv_parser::AlwaysConstruct,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    parameter_literals: &HashMap<String, Expr>,
    packed_dimensions: &PackedDimensions,
) -> Result<Option<FfProcess>, AnalyzerError> {
    if !matches!(always.nodes.0, sv_parser::AlwaysKeyword::AlwaysFf(_)) {
        return Ok(None);
    }
    let Some((events, body)) = ff_event_control_and_body(&always.nodes.1, syntax_tree) else {
        return Err(AnalyzerError::Unsupported(
            "always_ff event expression".to_string(),
        ));
    };
    let mut assignments = Vec::new();
    conditional_assignments_from_statement_or_null(
        body,
        None,
        false,
        syntax_tree,
        const_env,
        packed_dimensions,
        &mut assignments,
    )?;
    let assignments = assignments
        .into_iter()
        .map(|assignment| {
            ConditionalAssignment::new(
                assignment.condition.map(|condition| {
                    substitute_expr_constants_with_parameter_literals(
                        condition,
                        const_env,
                        parameter_literals,
                    )
                }),
                substitute_assignment_constants_with_parameter_literals(
                    assignment.assignment,
                    const_env,
                    parameter_literals,
                ),
            )
        })
        .collect::<Vec<_>>();
    Ok(
        (!events.is_empty() && !assignments.is_empty())
            .then(|| FfProcess::new(events, assignments)),
    )
}

fn ff_event_control_and_body<'a>(
    stmt: &'a sv_parser::Statement,
    syntax_tree: &SyntaxTree,
) -> Option<(Vec<FfEvent>, &'a sv_parser::StatementOrNull)> {
    let sv_parser::StatementItem::ProceduralTimingControlStatement(timing) = &stmt.nodes.2 else {
        return None;
    };
    let sv_parser::ProceduralTimingControl::EventControl(event_control) = &timing.nodes.0 else {
        return None;
    };
    Some((
        ff_events_from_event_control(event_control, syntax_tree)?,
        &timing.nodes.1,
    ))
}

fn ff_events_from_event_control(
    control: &sv_parser::EventControl,
    syntax_tree: &SyntaxTree,
) -> Option<Vec<FfEvent>> {
    let sv_parser::EventControl::EventExpression(control) = control else {
        return None;
    };
    ff_events_from_event_expression(&control.nodes.1.nodes.1, syntax_tree)
}

fn ff_events_from_event_expression(
    expr: &sv_parser::EventExpression,
    syntax_tree: &SyntaxTree,
) -> Option<Vec<FfEvent>> {
    match expr {
        sv_parser::EventExpression::Expression(expr) => {
            let edge = match expr.nodes.0.as_ref()? {
                sv_parser::EdgeIdentifier::Posedge(_) => FfEdge::Pos,
                sv_parser::EdgeIdentifier::Negedge(_) => FfEdge::Neg,
                sv_parser::EdgeIdentifier::Edge(_) => return None,
            };
            let signal = expr_ident_name(&expr_from_expression(&expr.nodes.1, syntax_tree)?);
            signal.map(|signal| vec![FfEvent::new(edge, signal)])
        }
        sv_parser::EventExpression::Or(expr) => {
            let mut left = ff_events_from_event_expression(&expr.nodes.0, syntax_tree)?;
            left.extend(ff_events_from_event_expression(&expr.nodes.2, syntax_tree)?);
            Some(left)
        }
        sv_parser::EventExpression::Comma(expr) => {
            let mut left = ff_events_from_event_expression(&expr.nodes.0, syntax_tree)?;
            left.extend(ff_events_from_event_expression(&expr.nodes.2, syntax_tree)?);
            Some(left)
        }
        sv_parser::EventExpression::Paren(expr) => {
            ff_events_from_event_expression(&expr.nodes.0.nodes.1, syntax_tree)
        }
        _ => None,
    }
}

fn conditional_assignments_from_statement_or_null(
    stmt: &sv_parser::StatementOrNull,
    condition: Option<Expr>,
    exhaustive_fallback: bool,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
    assignments: &mut Vec<ConditionalAssignment>,
) -> Result<(), AnalyzerError> {
    if let sv_parser::StatementOrNull::Statement(stmt) = stmt {
        conditional_assignments_from_statement(
            stmt,
            condition,
            exhaustive_fallback,
            syntax_tree,
            const_env,
            packed_dimensions,
            assignments,
        )?;
    }
    Ok(())
}

fn conditional_assignments_from_statement(
    stmt: &sv_parser::Statement,
    condition: Option<Expr>,
    exhaustive_fallback: bool,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
    assignments: &mut Vec<ConditionalAssignment>,
) -> Result<(), AnalyzerError> {
    match &stmt.nodes.2 {
        sv_parser::StatementItem::BlockingAssignment(assignment) => {
            let lowered = match &assignment.0 {
                sv_parser::BlockingAssignment::Variable(assignment) => {
                    variable_lvalue_from_node(&assignment.nodes.0, syntax_tree, packed_dimensions)
                        .zip(expr_from_expression_with_types(
                            &assignment.nodes.3,
                            syntax_tree,
                            packed_dimensions,
                        ))
                }
                sv_parser::BlockingAssignment::OperatorAssignment(assignment) => {
                    let op = syntax_tree.get_str(&assignment.nodes.1.nodes.0.nodes.0);
                    let lhs = variable_lvalue_from_node(
                        &assignment.nodes.0,
                        syntax_tree,
                        packed_dimensions,
                    );
                    let rhs = expr_from_expression_with_types(
                        &assignment.nodes.2,
                        syntax_tree,
                        packed_dimensions,
                    );
                    match (lhs, rhs, op) {
                        (Some(lhs), Some(rhs), Some("=")) => Some((lhs, rhs)),
                        (Some(lhs), Some(rhs), Some(op)) => {
                            assignment_op_expr(&lhs, op, rhs.clone(), packed_dimensions)
                                .map(|rhs| (lhs, rhs))
                        }
                        _ => None,
                    }
                }
                _ => None,
            };
            let Some((lhs, rhs)) = lowered else {
                return Err(AnalyzerError::Unsupported(
                    "always_comb assignment expression".to_string(),
                ));
            };
            let rhs = if condition.is_some() {
                coerce_procedural_assignment_rhs(rhs, &lhs, packed_dimensions)
            } else {
                rhs
            };
            assignments.push(ConditionalAssignment::new(
                condition,
                Assignment::new(lhs, rhs),
            ));
        }
        sv_parser::StatementItem::NonblockingAssignment(assignment) => {
            let lhs =
                variable_lvalue_from_node(&assignment.0.nodes.0, syntax_tree, packed_dimensions)
                    .ok_or_else(|| {
                        AnalyzerError::Unsupported("always_ff assignment lowering".to_string())
                    })?;
            let rhs = expr_from_expression_with_types(
                &assignment.0.nodes.3,
                syntax_tree,
                packed_dimensions,
            )
            .ok_or_else(|| {
                AnalyzerError::Unsupported("always_ff assignment lowering".to_string())
            })?;
            let rhs = if condition.is_some() {
                coerce_procedural_assignment_rhs(rhs, &lhs, packed_dimensions)
            } else {
                rhs
            };
            assignments.push(ConditionalAssignment::new(
                condition,
                Assignment::new(lhs, rhs),
            ));
        }
        sv_parser::StatementItem::SeqBlock(block) => {
            for stmt in &block.nodes.3 {
                conditional_assignments_from_statement_or_null(
                    stmt,
                    condition.clone(),
                    exhaustive_fallback,
                    syntax_tree,
                    const_env,
                    packed_dimensions,
                    assignments,
                )?;
            }
        }
        sv_parser::StatementItem::ConditionalStatement(stmt) => {
            conditional_assignments_from_conditional_statement(
                stmt,
                condition,
                exhaustive_fallback,
                syntax_tree,
                const_env,
                packed_dimensions,
                assignments,
            )?;
        }
        sv_parser::StatementItem::CaseStatement(stmt) => {
            conditional_assignments_from_case_statement(
                stmt,
                condition,
                exhaustive_fallback,
                syntax_tree,
                const_env,
                packed_dimensions,
                assignments,
            )?;
        }
        sv_parser::StatementItem::LoopStatement(loop_statement) => {
            let (name, values) = static_for_loop_iterations(loop_statement, syntax_tree, const_env)
                .ok_or_else(|| {
                    AnalyzerError::Unsupported("unsupported procedural for loop".to_string())
                })?;
            let body = match &**loop_statement {
                sv_parser::LoopStatement::For(loop_statement) => &loop_statement.nodes.2,
                _ => unreachable!(),
            };
            for value in values {
                let mut loop_env = const_env.clone();
                loop_env.insert(name.clone(), value);
                let mut loop_packed_dimensions = packed_dimensions.clone();
                let mut loop_const_env = loop_env.clone();
                insert_parameter_type_markers(
                    &mut loop_const_env,
                    &name,
                    ExprType {
                        width: 32,
                        signed: true,
                    },
                );
                loop_packed_dimensions.const_env = loop_const_env.clone();
                let start = assignments.len();
                conditional_assignments_from_statement_or_null(
                    body,
                    condition.clone(),
                    exhaustive_fallback,
                    syntax_tree,
                    &loop_const_env,
                    &loop_packed_dimensions,
                    assignments,
                )?;
                for assignment in &mut assignments[start..] {
                    assignment.condition = assignment.condition.take().map(|condition| {
                        substitute_expr_constants_with_parameter_literals(
                            condition,
                            &loop_env,
                            &HashMap::default(),
                        )
                    });
                    assignment.assignment =
                        substitute_assignment_constants(assignment.assignment.clone(), &loop_env);
                }
            }
        }
        _ => {
            return Err(AnalyzerError::Unsupported(
                "unsupported statement inside always_ff".to_string(),
            ));
        }
    }
    Ok(())
}

fn coerce_procedural_assignment_rhs(
    rhs: Expr,
    lhs: &LValue,
    packed_dimensions: &PackedDimensions,
) -> Expr {
    let Some(target_type) = lvalue_expr_type(lhs, packed_dimensions) else {
        return rhs;
    };
    let identifier_signedness = packed_dimensions
        .iter()
        .map(|(name, dimensions)| (name.clone(), dimensions.signed))
        .collect();
    let Some(source_signed) = expr_signedness(&rhs, &identifier_signedness, &HashMap::default())
    else {
        return rhs;
    };
    let assigned = if source_signed == target_type.signed
        && expr_static_width(&rhs, packed_dimensions) == Some(target_type.width)
    {
        rhs
    } else {
        let assigned = Expr::Resize {
            expr: Box::new(rhs),
            width: target_type.width,
            signed: source_signed,
        };
        Expr::Resize {
            expr: Box::new(assigned),
            width: target_type.width,
            signed: target_type.signed,
        }
    };
    let name = match lhs {
        LValue::Ident(name) | LValue::Select { name, .. } => name,
    };
    if packed_dimensions
        .get(name)
        .is_some_and(|dimensions| dimensions.is_2state)
    {
        Expr::Unary {
            op: UnaryOp::ToTwoState,
            expr: Box::new(assigned),
        }
    } else {
        assigned
    }
}

fn lvalue_expr_type(value: &LValue, packed_dimensions: &PackedDimensions) -> Option<ExprType> {
    match value {
        LValue::Ident(name) => {
            let dimensions = packed_dimensions.get(name)?;
            let width = dimensions
                .packed
                .iter()
                .map(|dimension| &dimension.width)
                .chain(dimensions.unpacked.iter().map(|dimension| &dimension.width))
                .try_fold(1usize, |width, dimension| {
                    let dimension_width =
                        eval_ast_const_expr(dimension, &packed_dimensions.const_env)?;
                    width.checked_mul(usize::try_from(dimension_width).ok()?)
                })?;
            Some(ExprType {
                width: width.max(1),
                signed: dimensions.signed,
            })
        }
        LValue::Select {
            msb, lsb, signed, ..
        } => {
            let msb = eval_ast_const_expr(msb, &packed_dimensions.const_env)?;
            let lsb = eval_ast_const_expr(lsb, &packed_dimensions.const_env)?;
            Some(ExprType {
                width: usize::try_from(msb.abs_diff(lsb)).ok()?.checked_add(1)?,
                signed: *signed,
            })
        }
    }
}

fn conditional_assignments_from_conditional_statement(
    stmt: &sv_parser::ConditionalStatement,
    parent_condition: Option<Expr>,
    exhaustive_fallback: bool,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
    assignments: &mut Vec<ConditionalAssignment>,
) -> Result<(), AnalyzerError> {
    let chain_start = assignments.len();
    let if_condition =
        expr_from_cond_predicate(&stmt.nodes.2.nodes.1, syntax_tree, packed_dimensions)
            .ok_or_else(|| {
                AnalyzerError::Unsupported("always_ff predicate lowering".to_string())
            })?;
    let mut prior_false = Vec::new();
    let mut definitely_assigned_branches = Vec::new();
    let then_condition = combine_expr_conditions(
        parent_condition.clone(),
        procedural_truth_condition(if_condition.clone()),
    );
    let then_start = assignments.len();
    conditional_assignments_from_statement_or_null(
        &stmt.nodes.3,
        then_condition,
        false,
        syntax_tree,
        const_env,
        packed_dimensions,
        assignments,
    )?;
    mark_condition_context(assignments, then_start, chain_start);
    definitely_assigned_branches.push(definitely_assigned_comb_targets_statement_or_null(
        &stmt.nodes.3,
        syntax_tree,
        packed_dimensions,
    ));
    prior_false.push(procedural_false_condition(if_condition));

    for (_, _, predicate, branch) in &stmt.nodes.4 {
        let branch_condition =
            expr_from_cond_predicate(&predicate.nodes.1, syntax_tree, packed_dimensions)
                .ok_or_else(|| {
                    AnalyzerError::Unsupported("always_ff predicate lowering".to_string())
                })?;
        let mut terms = prior_false.clone();
        terms.push(procedural_truth_condition(branch_condition.clone()));
        let condition = combine_expr_condition_terms(parent_condition.clone(), terms);
        let branch_start = assignments.len();
        conditional_assignments_from_statement_or_null(
            branch,
            condition,
            false,
            syntax_tree,
            const_env,
            packed_dimensions,
            assignments,
        )?;
        mark_condition_context(assignments, branch_start, chain_start);
        definitely_assigned_branches.push(definitely_assigned_comb_targets_statement_or_null(
            branch,
            syntax_tree,
            packed_dimensions,
        ));
        prior_false.push(procedural_false_condition(branch_condition));
    }

    if let Some((_, branch)) = &stmt.nodes.5 {
        // Keep the false-path guard while lowering. It can only be removed
        // for targets that are written in every branch of this chain.
        let exhaustive = exhaustive_fallback && parent_condition.is_none();
        let condition = combine_expr_condition_terms(parent_condition, prior_false);
        let branch_start = assignments.len();
        conditional_assignments_from_statement_or_null(
            branch,
            condition.clone(),
            false,
            syntax_tree,
            const_env,
            packed_dimensions,
            assignments,
        )?;
        mark_condition_context(assignments, branch_start, chain_start);
        definitely_assigned_branches.push(definitely_assigned_comb_targets_statement_or_null(
            branch,
            syntax_tree,
            packed_dimensions,
        ));
        if exhaustive {
            mark_exhaustive_fallback(
                &mut assignments[branch_start..],
                &definitely_assigned_branches,
                chain_start,
                packed_dimensions,
            );
        }
    }
    Ok(())
}

fn procedural_false_condition(condition: Expr) -> Expr {
    Expr::Unary {
        op: UnaryOp::LogicNot,
        expr: Box::new(Expr::Unary {
            op: UnaryOp::ToTwoState,
            expr: Box::new(condition),
        }),
    }
}

fn conditional_assignments_from_case_statement(
    stmt: &sv_parser::CaseStatement,
    parent_condition: Option<Expr>,
    exhaustive_fallback: bool,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
    assignments: &mut Vec<ConditionalAssignment>,
) -> Result<(), AnalyzerError> {
    let chain_start = assignments.len();
    let sv_parser::CaseStatement::Normal(stmt) = stmt else {
        return Err(AnalyzerError::Unsupported(
            "casez, casex, or pattern case inside always_comb".to_string(),
        ));
    };
    if !matches!(&stmt.nodes.1, sv_parser::CaseKeyword::Case(_)) {
        return Err(AnalyzerError::Unsupported(
            "casez or casex inside always_comb".to_string(),
        ));
    }
    let case_expr = expr_from_expression_with_types(
        &stmt.nodes.2.nodes.1.nodes.0,
        syntax_tree,
        packed_dimensions,
    )
    .ok_or_else(|| AnalyzerError::Unsupported("always_ff case selector lowering".to_string()))?;
    let complete_two_state_case =
        two_state_case_items_cover_selector(stmt, syntax_tree, const_env, packed_dimensions);

    let mut branches = Vec::new();
    let mut default_branch = None;
    for item in std::iter::once(&stmt.nodes.3).chain(stmt.nodes.4.iter()) {
        match item {
            sv_parser::CaseItem::NonDefault(item) => {
                let mut conditions = Vec::new();
                for expr in item.nodes.0.contents() {
                    let expr = expr_from_expression_with_types(
                        &expr.nodes.0,
                        syntax_tree,
                        packed_dimensions,
                    )
                    .ok_or_else(|| {
                        AnalyzerError::Unsupported(
                            "always_ff case item expression lowering".to_string(),
                        )
                    })?;
                    conditions.push(case_item_condition(case_expr.clone(), expr));
                }
                if let Some(condition) = conditions.into_iter().reduce(|left, right| Expr::Binary {
                    left: Box::new(left),
                    op: BinaryOp::LogicOr,
                    right: Box::new(right),
                }) {
                    branches.push((condition, &item.nodes.2));
                }
            }
            sv_parser::CaseItem::Default(item) => {
                default_branch = Some(&item.nodes.2);
            }
        }
    }

    let mut prior_false = Vec::new();
    let mut definitely_assigned_branches = Vec::new();
    let branch_count = branches.len();
    for (branch_index, (branch_condition, branch)) in branches.into_iter().enumerate() {
        let mut terms = prior_false.clone();
        terms.push(branch_condition.clone());
        let condition = combine_expr_condition_terms(parent_condition.clone(), terms);
        // Case-item guards are never tautological, so statements nested in
        // a branch must keep the item condition.
        let branch_start = assignments.len();
        conditional_assignments_from_statement_or_null(
            branch,
            condition,
            false,
            syntax_tree,
            const_env,
            packed_dimensions,
            assignments,
        )?;
        mark_condition_context(assignments, branch_start, chain_start);
        definitely_assigned_branches.push(definitely_assigned_comb_targets_statement_or_null(
            branch,
            syntax_tree,
            packed_dimensions,
        ));
        if complete_two_state_case
            && exhaustive_fallback
            && parent_condition.is_none()
            && branch_index + 1 == branch_count
        {
            mark_exhaustive_fallback(
                &mut assignments[branch_start..],
                &definitely_assigned_branches,
                chain_start,
                packed_dimensions,
            );
        }
        prior_false.push(Expr::Unary {
            op: UnaryOp::LogicNot,
            expr: Box::new(branch_condition),
        });
    }

    if let Some(branch) = default_branch {
        // As with an else branch, retain the selector guard until all case
        // arms are known to assign the same target.
        let exhaustive = exhaustive_fallback && parent_condition.is_none();
        let condition = combine_expr_condition_terms(parent_condition, prior_false);
        let branch_start = assignments.len();
        conditional_assignments_from_statement_or_null(
            branch,
            condition.clone(),
            false,
            syntax_tree,
            const_env,
            packed_dimensions,
            assignments,
        )?;
        mark_condition_context(assignments, branch_start, chain_start);
        definitely_assigned_branches.push(definitely_assigned_comb_targets_statement_or_null(
            branch,
            syntax_tree,
            packed_dimensions,
        ));
        if exhaustive {
            mark_exhaustive_fallback(
                &mut assignments[branch_start..],
                &definitely_assigned_branches,
                chain_start,
                packed_dimensions,
            );
        }
    }
    Ok(())
}

fn two_state_case_selector_width(
    selector: &Expr,
    packed_dimensions: &PackedDimensions,
) -> Option<usize> {
    expr_is_two_state(selector, packed_dimensions)
        .then(|| expr_static_width(selector, packed_dimensions))?
}

fn expr_is_two_state(expr: &Expr, packed_dimensions: &PackedDimensions) -> bool {
    match expr {
        Expr::Ident(name) => packed_dimensions
            .get(name)
            .is_some_and(|dimensions| dimensions.is_2state),
        Expr::Literal(value) => typecheck::parse_integral_literal(value)
            .is_some_and(|literal| literal.mask == num_bigint::BigUint::default()),
        Expr::Select { expr, msb, lsb, .. } => {
            expr_is_two_state(expr, packed_dimensions)
                && select_bounds_are_statically_valid(expr, msb, lsb, packed_dimensions)
        }
        Expr::Resize { expr, .. } => expr_is_two_state(expr, packed_dimensions),
        Expr::Concat(parts) | Expr::RepeatConcat { parts, .. } => parts
            .iter()
            .all(|part| expr_is_two_state(part, packed_dimensions)),
        Expr::Unary { op, expr } => {
            *op == UnaryOp::ToTwoState || expr_is_two_state(expr, packed_dimensions)
        }
        Expr::Binary { left, op, right } => {
            matches!(op, BinaryOp::EqCase | BinaryOp::NeCase)
                || (expr_is_two_state(left, packed_dimensions)
                    && expr_is_two_state(right, packed_dimensions)
                    && (!matches!(op, BinaryOp::Div | BinaryOp::Mod)
                        || expr_to_const((**right).clone())
                            .and_then(|right| {
                                eval_ast_const_expr(&right, &packed_dimensions.const_env)
                            })
                            .is_some_and(|right| right != 0)))
        }
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            expr_is_two_state(condition, packed_dimensions)
                && expr_is_two_state(then_expr, packed_dimensions)
                && expr_is_two_state(else_expr, packed_dimensions)
        }
        Expr::Call { .. } => false,
    }
}

fn select_bounds_are_statically_valid(
    expr: &Expr,
    msb: &ConstExpr,
    lsb: &ConstExpr,
    packed_dimensions: &PackedDimensions,
) -> bool {
    let (Some(msb), Some(lsb)) = (
        eval_ast_const_expr(msb, &packed_dimensions.const_env),
        eval_ast_const_expr(lsb, &packed_dimensions.const_env),
    ) else {
        return false;
    };
    let (valid_low, valid_high) = if let Expr::Ident(name) = expr {
        let Some(LValue::Select { msb, lsb, .. }) = whole_packed_lvalue(name, packed_dimensions)
        else {
            return false;
        };
        let (Some(msb), Some(lsb)) = (
            eval_ast_const_expr(&msb, &packed_dimensions.const_env),
            eval_ast_const_expr(&lsb, &packed_dimensions.const_env),
        ) else {
            return false;
        };
        (msb.min(lsb), msb.max(lsb))
    } else {
        let Some(width) = expr_static_width(expr, packed_dimensions)
            .and_then(|width| width.checked_sub(1))
            .and_then(|high| i128::try_from(high).ok())
        else {
            return false;
        };
        (0, width)
    };
    msb.min(lsb) >= valid_low && msb.max(lsb) <= valid_high
}

fn two_state_case_items_cover_selector(
    stmt: &sv_parser::CaseStatementNormal,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    packed_dimensions: &PackedDimensions,
) -> bool {
    if !matches!(&stmt.nodes.1, sv_parser::CaseKeyword::Case(_)) {
        return false;
    }
    let Some(selector) = expr_from_expression_with_types(
        &stmt.nodes.2.nodes.1.nodes.0,
        syntax_tree,
        packed_dimensions,
    ) else {
        return false;
    };
    let Some(width) = two_state_case_selector_width(&selector, packed_dimensions) else {
        return false;
    };
    let Some(value_count) = u32::try_from(width)
        .ok()
        .and_then(|width| 1usize.checked_shl(width))
    else {
        return false;
    };
    if i128::try_from(value_count).is_err() {
        return false;
    }
    let identifiers = packed_dimensions
        .iter()
        .map(|(name, dimensions)| (name.clone(), dimensions.signed))
        .collect::<HashMap<_, _>>();
    let Some(selector_signed) = expr_signedness(&selector, &identifiers, &HashMap::default())
    else {
        return false;
    };
    let mut labels = Vec::new();
    for item in std::iter::once(&stmt.nodes.3).chain(stmt.nodes.4.iter()) {
        let sv_parser::CaseItem::NonDefault(item) = item else {
            continue;
        };
        for label in item.nodes.0.contents() {
            let Some(label) =
                expr_from_expression_with_types(&label.nodes.0, syntax_tree, packed_dimensions)
            else {
                return false;
            };
            if let Some(label) = expr_to_const(label) {
                labels.push(label);
            }
        }
    }
    if labels.len() < value_count {
        return false;
    }
    (0..value_count).all(|value| {
        let selector = ConstExpr::Literal(format_typed_parameter_literal(
            value as i128,
            width,
            selector_signed,
        ));
        labels.iter().any(|label| {
            eval_ast_const_expr(
                &ConstExpr::Binary {
                    left: Box::new(selector.clone()),
                    op: BinaryOp::EqCase,
                    right: Box::new(label.clone()),
                },
                const_env,
            ) == Some(1)
        })
    })
}

fn mark_condition_context(
    assignments: &mut [ConditionalAssignment],
    start: usize,
    guard_boundary: usize,
) {
    let epoch = assignments
        .iter()
        .flat_map(|assignment| assignment.path_epochs.iter().copied())
        .max()
        .map_or(0, |epoch| epoch + 1);
    for assignment in &mut assignments[start..] {
        if assignment.condition.is_some() {
            assignment.guard_boundary.get_or_insert(guard_boundary);
            assignment.path_epochs.push(epoch);
        }
    }
}

fn mark_exhaustive_fallback(
    fallback_assignments: &mut [ConditionalAssignment],
    branch_targets: &[Vec<LValue>],
    chain_start: usize,
    packed_dimensions: &PackedDimensions,
) {
    let mut definite_targets = Vec::new();
    for assignment in fallback_assignments.iter() {
        let target = assignment.assignment().lhs_value().clone();
        if branch_targets
            .iter()
            .all(|targets| lvalue_is_covered_by(&target, targets, packed_dimensions))
            && !definite_targets.contains(&target)
        {
            definite_targets.push(target);
        }
    }
    for target in definite_targets {
        let candidates = fallback_assignments
            .iter()
            .enumerate()
            .filter(|(_, assignment)| assignment.assignment().lhs_value() == &target)
            .collect::<Vec<_>>();
        let marked = candidates
            .iter()
            .find(|(_, assignment)| assignment.exhaustive_fallback_start.is_some())
            .map(|(index, _)| *index);
        let shallowest = candidates
            .iter()
            .min_by_key(|(index, assignment)| (assignment.path_epochs.len(), *index))
            .map(|(index, _)| *index);
        if let Some(index) = marked.or(shallowest) {
            fallback_assignments[index].exhaustive_fallback_start = Some(chain_start);
        }
    }
}

fn definitely_assigned_comb_targets_statement_or_null(
    stmt: &sv_parser::StatementOrNull,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Vec<LValue> {
    match stmt {
        sv_parser::StatementOrNull::Statement(stmt) => {
            definitely_assigned_comb_targets(stmt, syntax_tree, packed_dimensions)
        }
        sv_parser::StatementOrNull::Attribute(_) => Vec::new(),
    }
}

fn definitely_assigned_comb_targets(
    stmt: &sv_parser::Statement,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Vec<LValue> {
    match &stmt.nodes.2 {
        sv_parser::StatementItem::BlockingAssignment(assignment) => {
            let target = match &assignment.0 {
                sv_parser::BlockingAssignment::Variable(assignment) => {
                    variable_lvalue_from_node(&assignment.nodes.0, syntax_tree, packed_dimensions)
                }
                sv_parser::BlockingAssignment::OperatorAssignment(assignment) => {
                    variable_lvalue_from_node(&assignment.nodes.0, syntax_tree, packed_dimensions)
                }
                _ => None,
            };
            target.into_iter().collect()
        }
        sv_parser::StatementItem::SeqBlock(block) => {
            let mut targets = Vec::new();
            let mut guarded_targets: Vec<(Expr, Vec<LValue>)> = Vec::new();
            for stmt in &block.nodes.3 {
                let statement_targets = definitely_assigned_comb_targets_statement_or_null(
                    stmt,
                    syntax_tree,
                    packed_dimensions,
                );
                guarded_targets.retain(|(condition, _)| {
                    !statement_targets
                        .iter()
                        .any(|target| expr_references_lvalue(condition, target))
                });
                for target in statement_targets {
                    if !targets.contains(&target) {
                        targets.push(target);
                    }
                }
                let Some((condition, branch_targets)) =
                    guarded_comb_targets(stmt, syntax_tree, packed_dimensions)
                else {
                    if !statement_or_null_is_blocking_assignment(stmt) {
                        guarded_targets.clear();
                    }
                    continue;
                };
                guarded_targets.retain(|(prior_condition, _)| {
                    !branch_targets
                        .iter()
                        .any(|target| expr_references_lvalue(prior_condition, target))
                });
                for (prior_condition, prior_targets) in &guarded_targets {
                    if !two_state_conditions_are_complements(
                        prior_condition,
                        &condition,
                        packed_dimensions,
                    ) {
                        continue;
                    }
                    for target in intersect_lvalue_sets(
                        vec![prior_targets.clone(), branch_targets.clone()],
                        packed_dimensions,
                    ) {
                        if !targets.contains(&target) {
                            targets.push(target);
                        }
                    }
                }
                guarded_targets.push((condition, branch_targets));
            }
            targets
        }
        sv_parser::StatementItem::ConditionalStatement(conditional) => {
            let mut branches = Vec::new();
            let mut terminal = false;
            for (predicate, branch) in
                std::iter::once((&conditional.nodes.2.nodes.1, &conditional.nodes.3)).chain(
                    conditional
                        .nodes
                        .4
                        .iter()
                        .map(|(_, _, predicate, branch)| (&predicate.nodes.1, branch)),
                )
            {
                let condition = expr_from_cond_predicate(predicate, syntax_tree, packed_dimensions)
                    .map(procedural_truth_condition)
                    .and_then(expr_to_const)
                    .and_then(|condition| {
                        eval_ast_const_expr(&condition, &packed_dimensions.const_env)
                    });
                if condition == Some(0) {
                    continue;
                }
                branches.push(definitely_assigned_comb_targets_statement_or_null(
                    branch,
                    syntax_tree,
                    packed_dimensions,
                ));
                if condition.is_some() {
                    terminal = true;
                    break;
                }
            }
            if !terminal {
                let Some((_, else_branch)) = &conditional.nodes.5 else {
                    return Vec::new();
                };
                branches.push(definitely_assigned_comb_targets_statement_or_null(
                    else_branch,
                    syntax_tree,
                    packed_dimensions,
                ));
            }
            intersect_lvalue_sets(branches, packed_dimensions)
        }
        sv_parser::StatementItem::CaseStatement(case) => {
            let sv_parser::CaseStatement::Normal(case) = &**case else {
                return Vec::new();
            };
            let mut has_default = false;
            let branches = std::iter::once(&case.nodes.3)
                .chain(case.nodes.4.iter())
                .map(|item| {
                    let branch = match item {
                        sv_parser::CaseItem::NonDefault(item) => &item.nodes.2,
                        sv_parser::CaseItem::Default(item) => {
                            has_default = true;
                            &item.nodes.2
                        }
                    };
                    definitely_assigned_comb_targets_statement_or_null(
                        branch,
                        syntax_tree,
                        packed_dimensions,
                    )
                })
                .collect::<Vec<_>>();
            if has_default
                || two_state_case_items_cover_selector(
                    case,
                    syntax_tree,
                    &packed_dimensions.const_env,
                    packed_dimensions,
                )
            {
                intersect_lvalue_sets(branches, packed_dimensions)
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    }
}

fn guarded_comb_targets(
    stmt: &sv_parser::StatementOrNull,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Option<(Expr, Vec<LValue>)> {
    let sv_parser::StatementOrNull::Statement(stmt) = stmt else {
        return None;
    };
    let sv_parser::StatementItem::ConditionalStatement(conditional) = &stmt.nodes.2 else {
        return None;
    };
    if !conditional.nodes.4.is_empty() || conditional.nodes.5.is_some() {
        return None;
    }
    let condition =
        expr_from_cond_predicate(&conditional.nodes.2.nodes.1, syntax_tree, packed_dimensions)?;
    if !statement_or_null_is_blocking_assignment(&conditional.nodes.3) {
        return None;
    }
    let targets = definitely_assigned_comb_targets_statement_or_null(
        &conditional.nodes.3,
        syntax_tree,
        packed_dimensions,
    );
    if targets
        .iter()
        .any(|target| expr_references_lvalue(&condition, target))
    {
        return None;
    }
    Some((condition, targets))
}

fn statement_or_null_is_blocking_assignment(stmt: &sv_parser::StatementOrNull) -> bool {
    matches!(
        stmt,
        sv_parser::StatementOrNull::Statement(stmt)
            if matches!(stmt.nodes.2, sv_parser::StatementItem::BlockingAssignment(_))
    )
}

fn two_state_conditions_are_complements(
    left: &Expr,
    right: &Expr,
    packed_dimensions: &PackedDimensions,
) -> bool {
    let is_complement = |candidate: &Expr, other: &Expr| {
        matches!(
            candidate,
            Expr::Unary {
                op: UnaryOp::LogicNot,
                expr,
            } if &**expr == other && expr_is_two_state(other, packed_dimensions)
        )
    };
    is_complement(left, right) || is_complement(right, left)
}

fn intersect_lvalue_sets(
    sets: Vec<Vec<LValue>>,
    packed_dimensions: &PackedDimensions,
) -> Vec<LValue> {
    if sets.is_empty() {
        return Vec::new();
    }
    let mut candidates = Vec::new();
    for target in sets.iter().flatten() {
        if !candidates.contains(target) {
            candidates.push(target.clone());
        }
    }
    candidates.retain(|target| {
        sets.iter()
            .all(|set| lvalue_is_covered_by(target, set, packed_dimensions))
    });
    candidates
}

fn lvalue_is_covered_by(
    target: &LValue,
    writes: &[LValue],
    packed_dimensions: &PackedDimensions,
) -> bool {
    if writes.contains(target) {
        return true;
    }
    let Some((target_name, target_low, target_high)) = lvalue_bit_range(target, packed_dimensions)
    else {
        return false;
    };
    let mut ranges = writes
        .iter()
        .filter_map(|write| lvalue_bit_range(write, packed_dimensions))
        .filter_map(|(name, low, high)| (name == target_name).then_some((low, high)))
        .collect::<Vec<_>>();
    ranges.sort_unstable_by_key(|(low, _)| *low);
    let mut covered_through = target_low.checked_sub(1);
    for (low, high) in ranges {
        let next = covered_through.and_then(|covered| covered.checked_add(1));
        if next.is_some_and(|next| low > next) {
            continue;
        }
        if low <= target_low || next.is_some_and(|next| low <= next) {
            covered_through = Some(covered_through.map_or(high, |covered| covered.max(high)));
        }
        if covered_through.is_some_and(|covered| covered >= target_high) {
            return true;
        }
    }
    false
}

fn lvalue_bit_range(
    value: &LValue,
    packed_dimensions: &PackedDimensions,
) -> Option<(String, i128, i128)> {
    let selected;
    let value = match value {
        LValue::Ident(name) => {
            selected = whole_packed_lvalue(name, packed_dimensions)?;
            &selected
        }
        value => value,
    };
    let LValue::Select { name, msb, lsb, .. } = value else {
        unreachable!();
    };
    let msb = eval_ast_const_expr(msb, &packed_dimensions.const_env)?;
    let lsb = eval_ast_const_expr(lsb, &packed_dimensions.const_env)?;
    Some((name.clone(), msb.min(lsb), msb.max(lsb)))
}

fn expr_from_cond_predicate(
    predicate: &sv_parser::CondPredicate,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Option<Expr> {
    if cond_predicate_has_conjunction_operator(predicate, syntax_tree) {
        return None;
    }
    let entries = predicate.nodes.0.contents();
    let [sv_parser::ExpressionOrCondPattern::Expression(expr)] = entries.as_slice() else {
        return None;
    };
    expr_from_expression_with_types(expr, syntax_tree, packed_dimensions)
}

fn cond_predicate_has_conjunction_operator(
    predicate: &sv_parser::CondPredicate,
    syntax_tree: &SyntaxTree,
) -> bool {
    let mut previous = None;
    for node in RefNode::CondPredicate(predicate) {
        let RefNode::Symbol(symbol) = node else {
            continue;
        };
        let locate = symbol.nodes.0;
        if previous.is_some_and(|previous: sv_parser::Locate| {
            previous.offset + previous.len == locate.offset
                && syntax_tree.get_str(&previous) == Some("&&")
                && syntax_tree.get_str(&locate) == Some("&")
        }) {
            return true;
        }
        previous = Some(locate);
    }
    false
}

fn combine_expr_conditions(parent: Option<Expr>, child: Expr) -> Option<Expr> {
    combine_expr_condition_terms(parent, vec![child])
}

fn combine_expr_condition_terms(parent: Option<Expr>, terms: Vec<Expr>) -> Option<Expr> {
    let Some(condition) = terms.into_iter().reduce(|left, right| Expr::Binary {
        left: Box::new(left),
        op: BinaryOp::LogicAnd,
        right: Box::new(right),
    }) else {
        return parent;
    };
    Some(match parent {
        Some(parent) => Expr::Binary {
            left: Box::new(parent),
            op: BinaryOp::LogicAnd,
            right: Box::new(condition),
        },
        None => condition,
    })
}

fn assignment_op_expr(
    lhs: &LValue,
    op: &str,
    rhs: Expr,
    packed_dimensions: &PackedDimensions,
) -> Option<Expr> {
    let op = match op.strip_suffix('=')? {
        "+" => BinaryOp::Add,
        "-" => BinaryOp::Sub,
        "*" => BinaryOp::Mul,
        "/" => BinaryOp::Div,
        "%" => BinaryOp::Mod,
        "<<" => BinaryOp::Shl,
        "<<<" => BinaryOp::Shl,
        ">>" => BinaryOp::Shr,
        ">>>" => BinaryOp::Sar,
        "&" => BinaryOp::BitAnd,
        "|" => BinaryOp::BitOr,
        "^" => BinaryOp::BitXor,
        _ => return None,
    };
    Some(guard_zero_divisions(Expr::Binary {
        left: Box::new(expr_from_lvalue(lhs, packed_dimensions)),
        op,
        right: Box::new(rhs),
    }))
}

fn expr_from_lvalue(lhs: &LValue, packed_dimensions: &PackedDimensions) -> Expr {
    let (base, array_slice_width, array_slice_reversed, signed) = match lhs {
        LValue::Ident(name) => return Expr::Ident(name.clone()),
        LValue::Select {
            name,
            msb,
            lsb,
            signed,
            array_slice_width,
            array_slice_reversed,
        } => (
            Expr::Select {
                expr: Box::new(Expr::Ident(name.clone())),
                msb: msb.clone(),
                lsb: lsb.clone(),
                signed: *signed,
            },
            array_slice_width,
            *array_slice_reversed,
            *signed,
        ),
    };
    if !array_slice_reversed {
        return base;
    }
    let Some(array_slice_width) = array_slice_width else {
        return base;
    };
    let Some(element_width) = eval_ast_const_expr(array_slice_width, &packed_dimensions.const_env)
        .and_then(|width| usize::try_from(width).ok())
        .filter(|width| *width != 0)
    else {
        return base;
    };
    let Expr::Select { msb, lsb, .. } = &base else {
        return base;
    };
    let Some(total_width) = eval_ast_const_expr(msb, &packed_dimensions.const_env)
        .and_then(|msb| {
            eval_ast_const_expr(lsb, &packed_dimensions.const_env)
                .and_then(|lsb| usize::try_from(msb.abs_diff(lsb)).ok())
        })
        .and_then(|width| width.checked_add(1))
    else {
        return base;
    };
    if total_width % element_width != 0 || total_width == element_width {
        return base;
    }
    let mut parts = Vec::new();
    for offset in (0..total_width).step_by(element_width) {
        let Some(part_msb) = offset
            .checked_add(element_width)
            .and_then(|value| value.checked_sub(1))
        else {
            return base;
        };
        parts.push(Expr::Select {
            expr: Box::new(base.clone()),
            msb: ConstExpr::Literal(part_msb.to_string()),
            lsb: ConstExpr::Literal(offset.to_string()),
            signed,
        });
    }
    Expr::Concat(parts)
}

fn net_lvalue_from_node(
    node: &sv_parser::NetLvalue,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Option<LValue> {
    match node {
        sv_parser::NetLvalue::Identifier(identifier) => {
            let name = identifier_text(
                RefNode::PsOrHierarchicalNetIdentifier(&identifier.nodes.0),
                syntax_tree,
            )?;
            lvalue_from_constant_select(name, &identifier.nodes.1, syntax_tree, packed_dimensions)
        }
        _ => None,
    }
}

fn variable_lvalue_from_node(
    node: &sv_parser::VariableLvalue,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Option<LValue> {
    match node {
        sv_parser::VariableLvalue::Identifier(identifier) => {
            let name = identifier_text(
                RefNode::HierarchicalVariableIdentifier(&identifier.nodes.1),
                syntax_tree,
            )?;
            lvalue_from_select(name, &identifier.nodes.2, syntax_tree, packed_dimensions)
        }
        _ => None,
    }
}

fn lvalue_from_select(
    name: String,
    select: &sv_parser::Select,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Option<LValue> {
    let bit_selects = select.nodes.1.nodes.0.as_slice();
    let indices = bit_selects
        .iter()
        .map(|bit_select| const_expr_from_expr(&bit_select.nodes.1, syntax_tree))
        .collect::<Option<Vec<_>>>()?;
    if let Some(range) = &select.nodes.2 {
        let sv_parser::PartSelectRange::ConstantRange(range) = &range.nodes.1 else {
            return None;
        };
        let mut msb =
            const_expr_from_ref_node(RefNode::ConstantExpression(&range.nodes.0), syntax_tree)?;
        let mut lsb =
            const_expr_from_ref_node(RefNode::ConstantExpression(&range.nodes.2), syntax_tree)?;
        let (array_slice_width, array_slice_reversed) =
            unpacked_select_range_metadata(&name, &indices, &msb, &lsb, packed_dimensions)
                .map_or((None, false), |(width, reversed)| (Some(width), reversed));
        (msb, lsb) = flatten_select_range(&name, &indices, msb, lsb, packed_dimensions)?;
        return Some(LValue::Select {
            name,
            msb,
            lsb,
            signed: false,
            array_slice_width,
            array_slice_reversed,
        });
    }

    if let Some((array_offset, packed_indices)) =
        flatten_variable_select(&name, &indices, packed_dimensions)
    {
        if !packed_indices.is_empty() {
            if let Some((msb, lsb)) =
                flatten_packed_select(&name, &packed_indices, packed_dimensions)
            {
                return Some(LValue::Select {
                    name,
                    msb: add_expr(array_offset.clone(), msb),
                    lsb: add_expr(array_offset, lsb),
                    signed: false,
                    array_slice_width: None,
                    array_slice_reversed: false,
                });
            }
        } else if let Some(dimensions) = packed_dimensions.get(&name)
            && !dimensions.unpacked.is_empty()
            && indices.len() == dimensions.unpacked.len()
        {
            let width = product_expr(
                &dimensions
                    .packed
                    .iter()
                    .map(|dimension| dimension.width.clone())
                    .collect::<Vec<_>>(),
            );
            return Some(LValue::Select {
                name,
                msb: add_expr(
                    array_offset.clone(),
                    ConstExpr::Binary {
                        left: Box::new(width),
                        op: BinaryOp::Sub,
                        right: Box::new(ConstExpr::Literal("1".to_string())),
                    },
                ),
                lsb: array_offset,
                signed: dimensions.signed,
                array_slice_width: None,
                array_slice_reversed: false,
            });
        } else if let Some(dimensions) = packed_dimensions.get(&name)
            && !dimensions.unpacked.is_empty()
            && !indices.is_empty()
            && indices.len() < dimensions.unpacked.len()
        {
            let width = remaining_unpacked_selection_width(dimensions, indices.len());
            return Some(LValue::Select {
                name,
                msb: add_expr(
                    array_offset.clone(),
                    ConstExpr::Binary {
                        left: Box::new(width),
                        op: BinaryOp::Sub,
                        right: Box::new(ConstExpr::Literal("1".to_string())),
                    },
                ),
                lsb: array_offset,
                signed: dimensions.signed,
                array_slice_width: None,
                array_slice_reversed: false,
            });
        }
    }
    if !indices.is_empty()
        && packed_dimensions
            .get(&name)
            .is_some_and(|dimensions| !dimensions.unpacked.is_empty())
    {
        return None;
    }
    if indices.len() == 1 {
        let bit = indices[0].clone();
        return Some(LValue::Select {
            name,
            msb: bit.clone(),
            lsb: bit,
            signed: false,
            array_slice_width: None,
            array_slice_reversed: false,
        });
    }

    Some(LValue::Ident(name))
}

fn lvalue_from_constant_select(
    name: String,
    select: &sv_parser::ConstantSelect,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Option<LValue> {
    let bit_selects = select.nodes.1.nodes.0.as_slice();
    let indices = bit_selects
        .iter()
        .map(|bit_select| {
            const_expr_from_ref_node(
                RefNode::ConstantExpression(&bit_select.nodes.1),
                syntax_tree,
            )
        })
        .collect::<Option<Vec<_>>>()?;
    if let Some(range) = &select.nodes.2 {
        let sv_parser::ConstantPartSelectRange::ConstantRange(range) = &range.nodes.1 else {
            return None;
        };
        let mut msb =
            const_expr_from_ref_node(RefNode::ConstantExpression(&range.nodes.0), syntax_tree)?;
        let mut lsb =
            const_expr_from_ref_node(RefNode::ConstantExpression(&range.nodes.2), syntax_tree)?;
        let (array_slice_width, array_slice_reversed) =
            unpacked_select_range_metadata(&name, &indices, &msb, &lsb, packed_dimensions)
                .map_or((None, false), |(width, reversed)| (Some(width), reversed));
        (msb, lsb) = flatten_select_range(&name, &indices, msb, lsb, packed_dimensions)?;
        return Some(LValue::Select {
            name,
            msb,
            lsb,
            signed: false,
            array_slice_width,
            array_slice_reversed,
        });
    }

    if let Some((array_offset, packed_indices)) =
        flatten_variable_select(&name, &indices, packed_dimensions)
    {
        if !packed_indices.is_empty() {
            if let Some((msb, lsb)) =
                flatten_packed_select(&name, &packed_indices, packed_dimensions)
            {
                return Some(LValue::Select {
                    name,
                    msb: add_expr(array_offset.clone(), msb),
                    lsb: add_expr(array_offset, lsb),
                    signed: false,
                    array_slice_width: None,
                    array_slice_reversed: false,
                });
            }
        } else if let Some(dimensions) = packed_dimensions.get(&name)
            && !dimensions.unpacked.is_empty()
            && indices.len() == dimensions.unpacked.len()
        {
            let width = product_expr(
                &dimensions
                    .packed
                    .iter()
                    .map(|dimension| dimension.width.clone())
                    .collect::<Vec<_>>(),
            );
            return Some(LValue::Select {
                name,
                msb: add_expr(
                    array_offset.clone(),
                    ConstExpr::Binary {
                        left: Box::new(width),
                        op: BinaryOp::Sub,
                        right: Box::new(ConstExpr::Literal("1".to_string())),
                    },
                ),
                lsb: array_offset,
                signed: dimensions.signed,
                array_slice_width: None,
                array_slice_reversed: false,
            });
        } else if let Some(dimensions) = packed_dimensions.get(&name)
            && !dimensions.unpacked.is_empty()
            && !indices.is_empty()
            && indices.len() < dimensions.unpacked.len()
        {
            let width = remaining_unpacked_selection_width(dimensions, indices.len());
            return Some(LValue::Select {
                name,
                msb: add_expr(
                    array_offset.clone(),
                    ConstExpr::Binary {
                        left: Box::new(width),
                        op: BinaryOp::Sub,
                        right: Box::new(ConstExpr::Literal("1".to_string())),
                    },
                ),
                lsb: array_offset,
                signed: dimensions.signed,
                array_slice_width: None,
                array_slice_reversed: false,
            });
        }
    }
    if !indices.is_empty()
        && packed_dimensions
            .get(&name)
            .is_some_and(|dimensions| !dimensions.unpacked.is_empty())
    {
        return None;
    }
    if indices.len() == 1 {
        let bit = indices[0].clone();
        return Some(LValue::Select {
            name,
            msb: bit.clone(),
            lsb: bit,
            signed: false,
            array_slice_width: None,
            array_slice_reversed: false,
        });
    }

    Some(LValue::Ident(name))
}

fn expr_from_expression(expr: &sv_parser::Expression, syntax_tree: &SyntaxTree) -> Option<Expr> {
    expr_from_expression_with_types(expr, syntax_tree, &PackedDimensions::default())
}

fn expr_from_expression_with_types(
    expr: &sv_parser::Expression,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Option<Expr> {
    expr_from_expression_with_types_raw(expr, syntax_tree, packed_dimensions)
        .map(guard_zero_divisions)
}

fn expr_from_expression_with_types_raw(
    expr: &sv_parser::Expression,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Option<Expr> {
    match expr {
        sv_parser::Expression::Primary(primary) => {
            expr_from_primary_with_types(primary, syntax_tree, packed_dimensions)
        }
        sv_parser::Expression::Unary(unary) => {
            let expr =
                expr_from_primary_with_types(&unary.nodes.2, syntax_tree, packed_dimensions)?;
            unary_expr_from_symbol(&unary.nodes.0.nodes.0.nodes.0, expr, syntax_tree)
        }
        sv_parser::Expression::Binary(binary) => {
            let right_is_grouped = expression_is_grouped(&binary.nodes.3);
            let left = expr_from_expression_with_types_raw(
                &binary.nodes.0,
                syntax_tree,
                packed_dimensions,
            )?;
            let op = binary_op_from_symbol(&binary.nodes.1.nodes.0.nodes.0, syntax_tree)?;
            let right = expr_from_expression_with_types_raw(
                &binary.nodes.3,
                syntax_tree,
                packed_dimensions,
            )?;
            let expr = Expr::Binary {
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
            Some(if right_is_grouped {
                expr
            } else {
                left_associate_expr_binary(expr)
            })
        }
        sv_parser::Expression::ConditionalExpression(expr) => {
            expr_from_conditional_expression(expr, syntax_tree, packed_dimensions)
        }
        _ => None,
    }
}

fn guard_zero_divisions(expr: Expr) -> Expr {
    match expr {
        Expr::Ident(_) | Expr::Literal(_) => expr,
        Expr::Select {
            expr,
            msb,
            lsb,
            signed,
        } => Expr::Select {
            expr: Box::new(guard_zero_divisions(*expr)),
            msb,
            lsb,
            signed,
        },
        Expr::Concat(parts) => Expr::Concat(parts.into_iter().map(guard_zero_divisions).collect()),
        Expr::RepeatConcat { count, parts } => Expr::RepeatConcat {
            count,
            parts: parts.into_iter().map(guard_zero_divisions).collect(),
        },
        Expr::Resize {
            expr,
            width,
            signed,
        } => Expr::Resize {
            expr: Box::new(guard_zero_divisions(*expr)),
            width,
            signed,
        },
        Expr::Unary { op, expr } => Expr::Unary {
            op,
            expr: Box::new(guard_zero_divisions(*expr)),
        },
        Expr::Binary { left, op, right } => {
            let left = Box::new(guard_zero_divisions(*left));
            let right = Box::new(guard_zero_divisions(*right));
            let operation = Expr::Binary {
                left,
                op,
                right: right.clone(),
            };
            let divisor_is_nonzero = expr_to_const((*right).clone())
                .and_then(|right| eval_ast_const_expr(&right, &HashMap::default()))
                .is_some_and(|right| right != 0);
            if matches!(op, BinaryOp::Div | BinaryOp::Mod) && !divisor_is_nonzero {
                Expr::Mux {
                    condition: Box::new(Expr::Binary {
                        left: right,
                        op: BinaryOp::EqCase,
                        right: Box::new(Expr::Literal("0".to_string())),
                    }),
                    then_expr: Box::new(Expr::Literal(crate::DIV_ZERO_UNKNOWN_LITERAL.to_string())),
                    else_expr: Box::new(operation),
                }
            } else {
                operation
            }
        }
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => Expr::Mux {
            condition: Box::new(guard_zero_divisions(*condition)),
            then_expr: Box::new(guard_zero_divisions(*then_expr)),
            else_expr: Box::new(guard_zero_divisions(*else_expr)),
        },
        Expr::Call { name, args } => Expr::Call {
            name,
            args: args.into_iter().map(guard_zero_divisions).collect(),
        },
    }
}

fn expr_from_primary(primary: &sv_parser::Primary, syntax_tree: &SyntaxTree) -> Option<Expr> {
    expr_from_primary_with_types(primary, syntax_tree, &PackedDimensions::default())
}

fn expr_from_primary_with_types(
    primary: &sv_parser::Primary,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Option<Expr> {
    match primary {
        sv_parser::Primary::PrimaryLiteral(_) => {
            primary_literal_text(RefNode::Primary(primary), syntax_tree).map(Expr::Literal)
        }
        sv_parser::Primary::Hierarchical(hierarchical) => {
            let name = identifier_text(
                RefNode::HierarchicalIdentifier(&hierarchical.nodes.1),
                syntax_tree,
            )?;
            let base = Expr::Ident(name);
            let select = &hierarchical.nodes.2;
            if select.nodes.1.nodes.0.is_empty() && select.nodes.2.is_none() {
                Some(base)
            } else {
                expr_select_from_select(base, select, syntax_tree, packed_dimensions)
            }
        }
        sv_parser::Primary::Concatenation(concat) => {
            let parts = concat
                .nodes
                .0
                .nodes
                .0
                .nodes
                .1
                .contents()
                .into_iter()
                .map(|expr| expr_from_expression_with_types(expr, syntax_tree, packed_dimensions))
                .collect::<Option<Vec<_>>>()?;
            let base = (!parts.is_empty()).then_some(Expr::Concat(parts))?;
            match concat.nodes.1.as_ref().map(|range| &range.nodes.1) {
                None => Some(base),
                Some(sv_parser::RangeExpression::PartSelectRange(range)) => {
                    let sv_parser::PartSelectRange::ConstantRange(range) = &**range else {
                        return None;
                    };
                    let msb = const_expr_from_ref_node(
                        RefNode::ConstantExpression(&range.nodes.0),
                        syntax_tree,
                    )?;
                    let lsb = const_expr_from_ref_node(
                        RefNode::ConstantExpression(&range.nodes.2),
                        syntax_tree,
                    )?;
                    Some(Expr::Select {
                        expr: Box::new(base),
                        msb,
                        lsb,
                        signed: false,
                    })
                }
                Some(sv_parser::RangeExpression::Expression(bit)) => {
                    let bit = const_expr_from_expr(bit, syntax_tree)?;
                    Some(Expr::Select {
                        expr: Box::new(base),
                        msb: bit.clone(),
                        lsb: bit,
                        signed: false,
                    })
                }
            }
        }
        sv_parser::Primary::MultipleConcatenation(concat) => {
            let (count, repeated) = &concat.nodes.0.nodes.0.nodes.1;
            let count = const_expr_from_expr(count, syntax_tree)?;
            let parts = repeated
                .nodes
                .0
                .nodes
                .1
                .contents()
                .into_iter()
                .map(|expr| expr_from_expression_with_types(expr, syntax_tree, packed_dimensions))
                .collect::<Option<Vec<_>>>()?;
            (!parts.is_empty()).then_some(Expr::RepeatConcat { count, parts })
        }
        sv_parser::Primary::FunctionSubroutineCall(call) => {
            expr_from_function_subroutine_call(call, syntax_tree, packed_dimensions)
        }
        sv_parser::Primary::Cast(cast) => {
            let expr = expr_from_expression_with_types(
                &cast.nodes.2.nodes.1,
                syntax_tree,
                packed_dimensions,
            )?;
            cast_zero_type(
                cast,
                syntax_tree,
                &packed_dimensions.const_env,
                &packed_dimensions.type_aliases,
            )
            .map(|r#type| Expr::Resize {
                expr: Box::new(expr),
                width: r#type.width,
                signed: r#type.signed,
            })
        }
        sv_parser::Primary::MintypmaxExpression(expr) => match &expr.nodes.0.nodes.1 {
            sv_parser::MintypmaxExpression::Expression(expr) => {
                expr_from_expression_with_types(expr, syntax_tree, packed_dimensions)
            }
            sv_parser::MintypmaxExpression::Ternary(_) => None,
        },
        _ => None,
    }
}

fn expression_is_grouped(expr: &sv_parser::Expression) -> bool {
    matches!(
        expr,
        sv_parser::Expression::Primary(primary)
            if matches!(&**primary, sv_parser::Primary::MintypmaxExpression(_))
    )
}

fn expr_from_conditional_expression(
    expr: &sv_parser::ConditionalExpression,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Option<Expr> {
    Some(Expr::Mux {
        condition: Box::new(expr_from_cond_predicate(
            &expr.nodes.0,
            syntax_tree,
            packed_dimensions,
        )?),
        then_expr: Box::new(expr_from_expression_with_types(
            &expr.nodes.3,
            syntax_tree,
            packed_dimensions,
        )?),
        else_expr: Box::new(expr_from_expression_with_types(
            &expr.nodes.5,
            syntax_tree,
            packed_dimensions,
        )?),
    })
}

fn expr_from_function_subroutine_call(
    call: &sv_parser::FunctionSubroutineCall,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Option<Expr> {
    let sv_parser::SubroutineCall::TfCall(call) = &call.nodes.0 else {
        return None;
    };
    let name = identifier_text(
        RefNode::PsOrHierarchicalTfIdentifier(&call.nodes.0),
        syntax_tree,
    )?;
    let args = match call.nodes.2.as_ref().map(|paren| &paren.nodes.1) {
        None => Vec::new(),
        Some(sv_parser::ListOfArguments::Ordered(args)) => {
            let contents = args.nodes.0.contents();
            if contents.len() == 1 && contents[0].is_none() {
                Vec::new()
            } else {
                let mut lowered = Vec::new();
                for expr in contents {
                    let Some(expr) = expr.as_ref() else {
                        return Some(Expr::Call {
                            name: "$unsupported_function_call".to_string(),
                            args: Vec::new(),
                        });
                    };
                    let Some(expr) =
                        expr_from_expression_with_types(expr, syntax_tree, packed_dimensions)
                    else {
                        return Some(Expr::Call {
                            name: "$unsupported_function_call".to_string(),
                            args: Vec::new(),
                        });
                    };
                    lowered.push(expr);
                }
                lowered
            }
        }
        Some(sv_parser::ListOfArguments::Named(_)) => {
            return Some(Expr::Call {
                name: "$unsupported_function_call".to_string(),
                args: Vec::new(),
            });
        }
    };
    Some(Expr::Call { name, args })
}

fn expr_select_from_select(
    base: Expr,
    select: &sv_parser::Select,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Option<Expr> {
    let bit_selects = select.nodes.1.nodes.0.as_slice();
    let indices = bit_selects
        .iter()
        .map(|bit_select| const_expr_from_expr(&bit_select.nodes.1, syntax_tree))
        .collect::<Option<Vec<_>>>()?;
    if let Some(range) = &select.nodes.2 {
        let sv_parser::PartSelectRange::ConstantRange(range) = &range.nodes.1 else {
            return None;
        };
        let mut msb =
            const_expr_from_ref_node(RefNode::ConstantExpression(&range.nodes.0), syntax_tree)?;
        let mut lsb =
            const_expr_from_ref_node(RefNode::ConstantExpression(&range.nodes.2), syntax_tree)?;
        let Expr::Ident(name) = &base else {
            return indices.is_empty().then_some(Expr::Select {
                expr: Box::new(base),
                msb,
                lsb,
                signed: false,
            });
        };
        if let Some(expr) = expr_from_unpacked_select_range(
            name,
            &indices,
            msb.clone(),
            lsb.clone(),
            packed_dimensions,
        ) {
            return Some(expr);
        }
        (msb, lsb) = flatten_select_range(name, &indices, msb, lsb, packed_dimensions)?;
        return Some(Expr::Select {
            expr: Box::new(base),
            msb,
            lsb,
            signed: false,
        });
    }

    if let Expr::Ident(name) = &base {
        if let Some((array_offset, packed_indices)) =
            flatten_variable_select(name, &indices, packed_dimensions)
        {
            if !packed_indices.is_empty()
                && let Some((msb, lsb)) =
                    flatten_packed_select(name, &packed_indices, packed_dimensions)
            {
                return Some(Expr::Select {
                    expr: Box::new(base),
                    msb: add_expr(array_offset.clone(), msb),
                    lsb: add_expr(array_offset, lsb),
                    signed: false,
                });
            }
            if let Some(dimensions) = packed_dimensions.get(name)
                && !dimensions.unpacked.is_empty()
                && indices.len() == dimensions.unpacked.len()
            {
                let width = product_expr(
                    &dimensions
                        .packed
                        .iter()
                        .map(|dimension| dimension.width.clone())
                        .collect::<Vec<_>>(),
                );
                return Some(Expr::Select {
                    expr: Box::new(base),
                    msb: add_expr(
                        array_offset.clone(),
                        ConstExpr::Binary {
                            left: Box::new(width),
                            op: BinaryOp::Sub,
                            right: Box::new(ConstExpr::Literal("1".to_string())),
                        },
                    ),
                    lsb: array_offset,
                    signed: dimensions.signed,
                });
            }
            if let Some(dimensions) = packed_dimensions.get(name)
                && !dimensions.unpacked.is_empty()
                && packed_indices.is_empty()
                && indices.len() < dimensions.unpacked.len()
            {
                let width = remaining_unpacked_selection_width(dimensions, indices.len());
                return Some(Expr::Select {
                    expr: Box::new(base),
                    msb: add_expr(
                        array_offset.clone(),
                        ConstExpr::Binary {
                            left: Box::new(width),
                            op: BinaryOp::Sub,
                            right: Box::new(ConstExpr::Literal("1".to_string())),
                        },
                    ),
                    lsb: array_offset,
                    signed: dimensions.signed,
                });
            }
        }
    }
    if indices.len() == 1 {
        if let Expr::Ident(name) = &base
            && packed_dimensions
                .get(name)
                .is_some_and(|dimensions| !dimensions.unpacked.is_empty())
        {
            return None;
        }
        let bit = indices[0].clone();
        return Some(Expr::Select {
            expr: Box::new(base),
            msb: bit.clone(),
            lsb: bit,
            signed: false,
        });
    }

    None
}

fn flatten_variable_select(
    name: &str,
    indices: &[ConstExpr],
    packed_dimensions: &PackedDimensions,
) -> Option<(ConstExpr, Vec<ConstExpr>)> {
    let dimensions = packed_dimensions.get(name)?;
    let unpacked_count = dimensions.unpacked.len();
    let selected_unpacked_count = indices.len().min(unpacked_count);

    let mut offset = ConstExpr::Literal("0".to_string());
    let mut valid = None;
    for (index, value) in indices[..selected_unpacked_count].iter().enumerate() {
        if const_expr_is_out_of_range(
            value,
            &dimensions.unpacked[index].left,
            &dimensions.unpacked[index].right,
            &packed_dimensions.const_env,
        ) {
            return None;
        }
        let dimension = &dimensions.unpacked[index];
        let descending = ConstExpr::Binary {
            left: Box::new(dimension.left.clone()),
            op: BinaryOp::Ge,
            right: Box::new(dimension.right.clone()),
        };
        let descending_valid = ConstExpr::Binary {
            left: Box::new(ConstExpr::Binary {
                left: Box::new(value.clone()),
                op: BinaryOp::Ge,
                right: Box::new(dimension.right.clone()),
            }),
            op: BinaryOp::LogicAnd,
            right: Box::new(ConstExpr::Binary {
                left: Box::new(value.clone()),
                op: BinaryOp::Le,
                right: Box::new(dimension.left.clone()),
            }),
        };
        let ascending_valid = ConstExpr::Binary {
            left: Box::new(ConstExpr::Binary {
                left: Box::new(value.clone()),
                op: BinaryOp::Ge,
                right: Box::new(dimension.left.clone()),
            }),
            op: BinaryOp::LogicAnd,
            right: Box::new(ConstExpr::Binary {
                left: Box::new(value.clone()),
                op: BinaryOp::Le,
                right: Box::new(dimension.right.clone()),
            }),
        };
        let dimension_valid = ConstExpr::Mux {
            condition: Box::new(descending),
            then_expr: Box::new(descending_valid),
            else_expr: Box::new(ascending_valid),
        };
        valid = Some(match valid {
            Some(previous) => ConstExpr::Binary {
                left: Box::new(previous),
                op: BinaryOp::LogicAnd,
                right: Box::new(dimension_valid),
            },
            None => dimension_valid,
        });
        let mut stride_parts = dimensions.unpacked[index + 1..]
            .iter()
            .map(|dimension| dimension.width.clone())
            .collect::<Vec<_>>();
        stride_parts.extend(
            dimensions
                .packed
                .iter()
                .map(|dimension| dimension.width.clone()),
        );
        let stride = product_expr(&stride_parts);
        let index = unpacked_index_offset(&dimensions.unpacked[index], value.clone());
        let term = if is_one(&stride) {
            index
        } else {
            ConstExpr::Binary {
                left: Box::new(index),
                op: BinaryOp::Mul,
                right: Box::new(stride),
            }
        };
        offset = add_expr(offset, term);
    }
    if let Some(valid) = valid {
        let mut total_width_parts = dimensions
            .unpacked
            .iter()
            .map(|dimension| dimension.width.clone())
            .collect::<Vec<_>>();
        total_width_parts.extend(
            dimensions
                .packed
                .iter()
                .map(|dimension| dimension.width.clone()),
        );
        offset = ConstExpr::Mux {
            condition: Box::new(valid),
            then_expr: Box::new(offset),
            else_expr: Box::new(product_expr(&total_width_parts)),
        };
    }
    Some((offset, indices[selected_unpacked_count..].to_vec()))
}

fn expr_from_unpacked_select_range(
    name: &str,
    indices: &[ConstExpr],
    msb: ConstExpr,
    lsb: ConstExpr,
    packed_dimensions: &PackedDimensions,
) -> Option<Expr> {
    let dimensions = packed_dimensions.get(name)?;
    let dimension = dimensions.unpacked.get(indices.len())?;
    if const_expr_is_out_of_range(
        &msb,
        &dimension.left,
        &dimension.right,
        &packed_dimensions.const_env,
    ) || const_expr_is_out_of_range(
        &lsb,
        &dimension.left,
        &dimension.right,
        &packed_dimensions.const_env,
    ) {
        return None;
    }
    let (array_offset, packed_indices) = flatten_variable_select(name, indices, packed_dimensions)?;
    if !packed_indices.is_empty() {
        return None;
    }
    let msb_value = eval_ast_const_expr(&msb, &packed_dimensions.const_env)?;
    let lsb_value = eval_ast_const_expr(&lsb, &packed_dimensions.const_env)?;
    let stride = product_expr(
        &dimensions.unpacked[indices.len() + 1..]
            .iter()
            .map(|dimension| dimension.width.clone())
            .chain(
                dimensions
                    .packed
                    .iter()
                    .map(|dimension| dimension.width.clone()),
            )
            .collect::<Vec<_>>(),
    );
    let stride_minus_one = ConstExpr::Binary {
        left: Box::new(stride.clone()),
        op: BinaryOp::Sub,
        right: Box::new(ConstExpr::Literal("1".to_string())),
    };
    // Concat parts are ordered from most-significant to least-significant.
    // Walking from the slice lsb toward its msb preserves the array element
    // order when the slice direction is reversed.
    let mut parts = Vec::new();
    let mut value = lsb_value;
    let step = if value <= msb_value { 1 } else { -1 };
    loop {
        let offset = unpacked_index_offset(dimension, ConstExpr::Literal(value.to_string()));
        let offset = if is_one(&stride) {
            offset
        } else {
            ConstExpr::Binary {
                left: Box::new(offset),
                op: BinaryOp::Mul,
                right: Box::new(stride.clone()),
            }
        };
        let msb = add_expr(
            array_offset.clone(),
            add_expr(offset.clone(), stride_minus_one.clone()),
        );
        let lsb = add_expr(array_offset.clone(), offset);
        parts.push(Expr::Select {
            expr: Box::new(Expr::Ident(name.to_string())),
            msb,
            lsb,
            signed: dimensions.signed,
        });
        if value == msb_value {
            break;
        }
        value = value.checked_add(step)?;
    }
    match parts.len() {
        0 => None,
        1 => parts.pop(),
        _ => Some(Expr::Concat(parts)),
    }
}

fn unpacked_select_range_metadata(
    name: &str,
    indices: &[ConstExpr],
    msb: &ConstExpr,
    lsb: &ConstExpr,
    packed_dimensions: &PackedDimensions,
) -> Option<(ConstExpr, bool)> {
    let dimensions = packed_dimensions.get(name)?;
    let dimension = dimensions.unpacked.get(indices.len())?;
    if const_expr_is_out_of_range(
        msb,
        &dimension.left,
        &dimension.right,
        &packed_dimensions.const_env,
    ) || const_expr_is_out_of_range(
        lsb,
        &dimension.left,
        &dimension.right,
        &packed_dimensions.const_env,
    ) {
        return None;
    }
    let (_, packed_indices) = flatten_variable_select(name, indices, packed_dimensions)?;
    if !packed_indices.is_empty() {
        return None;
    }
    let msb_offset = eval_ast_const_expr(
        &unpacked_index_offset(dimension, msb.clone()),
        &packed_dimensions.const_env,
    )?;
    let lsb_offset = eval_ast_const_expr(
        &unpacked_index_offset(dimension, lsb.clone()),
        &packed_dimensions.const_env,
    )?;
    let stride = product_expr(
        &dimensions.unpacked[indices.len() + 1..]
            .iter()
            .map(|dimension| dimension.width.clone())
            .chain(
                dimensions
                    .packed
                    .iter()
                    .map(|dimension| dimension.width.clone()),
            )
            .collect::<Vec<_>>(),
    );
    Some((stride, msb_offset > lsb_offset))
}

fn remaining_unpacked_selection_width(
    dimensions: &VariableDimensions,
    selected_unpacked_count: usize,
) -> ConstExpr {
    let mut parts = dimensions.unpacked[selected_unpacked_count..]
        .iter()
        .map(|dimension| dimension.width.clone())
        .collect::<Vec<_>>();
    parts.extend(
        dimensions
            .packed
            .iter()
            .map(|dimension| dimension.width.clone()),
    );
    product_expr(&parts)
}

fn flatten_unpacked_select_range(
    name: &str,
    indices: &[ConstExpr],
    msb: ConstExpr,
    lsb: ConstExpr,
    dimensions: &VariableDimensions,
    packed_dimensions: &PackedDimensions,
) -> Option<(ConstExpr, ConstExpr)> {
    let dimension = dimensions.unpacked.get(indices.len())?;
    if const_expr_is_out_of_range(
        &msb,
        &dimension.left,
        &dimension.right,
        &packed_dimensions.const_env,
    ) || const_expr_is_out_of_range(
        &lsb,
        &dimension.left,
        &dimension.right,
        &packed_dimensions.const_env,
    ) {
        return None;
    }
    let (array_offset, packed_indices) = flatten_variable_select(name, indices, packed_dimensions)?;
    if !packed_indices.is_empty() {
        return None;
    }
    let msb_offset = unpacked_index_offset(dimension, msb);
    let lsb_offset = unpacked_index_offset(dimension, lsb);
    let stride = product_expr(
        &dimensions.unpacked[indices.len() + 1..]
            .iter()
            .map(|dimension| dimension.width.clone())
            .chain(
                dimensions
                    .packed
                    .iter()
                    .map(|dimension| dimension.width.clone()),
            )
            .collect::<Vec<_>>(),
    );
    let scale = |offset: ConstExpr| {
        if is_one(&stride) {
            offset
        } else {
            ConstExpr::Binary {
                left: Box::new(offset),
                op: BinaryOp::Mul,
                right: Box::new(stride.clone()),
            }
        }
    };
    let stride_minus_one = ConstExpr::Binary {
        left: Box::new(stride.clone()),
        op: BinaryOp::Sub,
        right: Box::new(ConstExpr::Literal("1".to_string())),
    };
    let descending = ConstExpr::Binary {
        left: Box::new(msb_offset.clone()),
        op: BinaryOp::Ge,
        right: Box::new(lsb_offset.clone()),
    };
    let high = ConstExpr::Mux {
        condition: Box::new(descending.clone()),
        then_expr: Box::new(add_expr(
            scale(msb_offset.clone()),
            stride_minus_one.clone(),
        )),
        else_expr: Box::new(add_expr(scale(lsb_offset.clone()), stride_minus_one)),
    };
    let low = ConstExpr::Mux {
        condition: Box::new(descending),
        then_expr: Box::new(scale(lsb_offset)),
        else_expr: Box::new(scale(msb_offset)),
    };
    Some((
        add_expr(array_offset.clone(), high),
        add_expr(array_offset, low),
    ))
}

fn flatten_select_range(
    name: &str,
    indices: &[ConstExpr],
    mut msb: ConstExpr,
    mut lsb: ConstExpr,
    packed_dimensions: &PackedDimensions,
) -> Option<(ConstExpr, ConstExpr)> {
    let Some(dimensions) = packed_dimensions.get(name) else {
        return indices.is_empty().then_some((msb, lsb));
    };
    if indices.is_empty()
        && dimensions.unpacked.is_empty()
        && dimensions.packed.len() == 1
        && !dimensions.packed[0].normalize_single
    {
        return Some((msb, lsb));
    }
    if indices.len() < dimensions.unpacked.len() {
        return flatten_unpacked_select_range(
            name,
            indices,
            msb,
            lsb,
            dimensions,
            packed_dimensions,
        );
    }
    let (array_offset, packed_indices) = flatten_variable_select(name, indices, packed_dimensions)?;
    let dimension = dimensions.packed.get(packed_indices.len())?;
    if const_expr_is_out_of_range(
        &msb,
        &dimension.left,
        &dimension.right,
        &packed_dimensions.const_env,
    ) || const_expr_is_out_of_range(
        &lsb,
        &dimension.left,
        &dimension.right,
        &packed_dimensions.const_env,
    ) {
        return None;
    }
    msb = packed_index_offset(dimension, msb);
    lsb = packed_index_offset(dimension, lsb);

    let stride = product_expr(
        &dimensions.packed[packed_indices.len() + 1..]
            .iter()
            .map(|dimension| dimension.width.clone())
            .collect::<Vec<_>>(),
    );
    if !is_one(&stride) {
        msb = add_expr(
            ConstExpr::Binary {
                left: Box::new(msb),
                op: BinaryOp::Mul,
                right: Box::new(stride.clone()),
            },
            ConstExpr::Binary {
                left: Box::new(stride.clone()),
                op: BinaryOp::Sub,
                right: Box::new(ConstExpr::Literal("1".to_string())),
            },
        );
        lsb = ConstExpr::Binary {
            left: Box::new(lsb),
            op: BinaryOp::Mul,
            right: Box::new(stride),
        };
    }

    let prefix_offset = if packed_indices.is_empty() {
        ConstExpr::Literal("0".to_string())
    } else {
        let (_, offset) = flatten_packed_select(name, &packed_indices, packed_dimensions)?;
        offset
    };
    let offset = add_expr(array_offset, prefix_offset);
    Some((add_expr(offset.clone(), msb), add_expr(offset, lsb)))
}

fn flatten_packed_select(
    name: &str,
    indices: &[ConstExpr],
    packed_dimensions: &PackedDimensions,
) -> Option<(ConstExpr, ConstExpr)> {
    let variable_dimensions = packed_dimensions.get(name)?;
    let dimensions = &variable_dimensions.packed;
    if dimensions.is_empty() {
        if variable_dimensions.unpacked.is_empty() || indices.len() != 1 {
            return None;
        }
        let zero = ConstExpr::Literal("0".to_string());
        if eval_ast_const_expr(&indices[0], &packed_dimensions.const_env) != Some(0) {
            return None;
        }
        return Some((zero.clone(), zero));
    }
    if indices.is_empty()
        || indices.len() > dimensions.len()
        || (variable_dimensions.unpacked.is_empty()
            && dimensions.len() == 1
            && !dimensions[0].normalize_single)
    {
        return None;
    }

    let mut offset = ConstExpr::Literal("0".to_string());
    for (idx, index) in indices.iter().enumerate() {
        let dimension = &dimensions[idx];
        if const_expr_is_out_of_range(
            index,
            &dimension.left,
            &dimension.right,
            &packed_dimensions.const_env,
        ) {
            return None;
        }
        let stride = product_expr(
            &dimensions[idx + 1..]
                .iter()
                .map(|dimension| dimension.width.clone())
                .collect::<Vec<_>>(),
        );
        let index = packed_index_offset(dimension, index.clone());
        let term = if is_one(&stride) {
            index
        } else {
            ConstExpr::Binary {
                left: Box::new(index),
                op: BinaryOp::Mul,
                right: Box::new(stride),
            }
        };
        offset = add_expr(offset, term);
    }

    let remaining_width = product_expr(
        &dimensions[indices.len()..]
            .iter()
            .map(|dimension| dimension.width.clone())
            .collect::<Vec<_>>(),
    );
    if is_one(&remaining_width) {
        return Some((offset.clone(), offset));
    }
    let msb = ConstExpr::Binary {
        left: Box::new(offset.clone()),
        op: BinaryOp::Add,
        right: Box::new(ConstExpr::Binary {
            left: Box::new(remaining_width),
            op: BinaryOp::Sub,
            right: Box::new(ConstExpr::Literal("1".to_string())),
        }),
    };
    Some((msb, offset))
}

fn packed_index_offset(dimension: &PackedDimension, index: ConstExpr) -> ConstExpr {
    ConstExpr::Mux {
        condition: Box::new(ConstExpr::Binary {
            left: Box::new(dimension.left.clone()),
            op: BinaryOp::Ge,
            right: Box::new(dimension.right.clone()),
        }),
        then_expr: Box::new(ConstExpr::Binary {
            left: Box::new(index.clone()),
            op: BinaryOp::Sub,
            right: Box::new(dimension.right.clone()),
        }),
        else_expr: Box::new(ConstExpr::Binary {
            left: Box::new(dimension.right.clone()),
            op: BinaryOp::Sub,
            right: Box::new(index),
        }),
    }
}

fn unpacked_index_offset(dimension: &UnpackedDimension, index: ConstExpr) -> ConstExpr {
    ConstExpr::Mux {
        condition: Box::new(ConstExpr::Binary {
            left: Box::new(dimension.left.clone()),
            op: BinaryOp::Ge,
            right: Box::new(dimension.right.clone()),
        }),
        then_expr: Box::new(ConstExpr::Binary {
            left: Box::new(dimension.left.clone()),
            op: BinaryOp::Sub,
            right: Box::new(index.clone()),
        }),
        else_expr: Box::new(ConstExpr::Binary {
            left: Box::new(index),
            op: BinaryOp::Sub,
            right: Box::new(dimension.left.clone()),
        }),
    }
}

fn const_expr_is_out_of_range(
    index: &ConstExpr,
    left: &ConstExpr,
    right: &ConstExpr,
    const_env: &HashMap<String, i128>,
) -> bool {
    let (Some(index), Some(left), Some(right)) = (
        eval_ast_const_expr(index, const_env),
        eval_ast_const_expr(left, const_env),
        eval_ast_const_expr(right, const_env),
    ) else {
        return false;
    };
    index < left.min(right) || index > left.max(right)
}

fn product_expr(parts: &[ConstExpr]) -> ConstExpr {
    parts
        .iter()
        .cloned()
        .reduce(|left, right| ConstExpr::Binary {
            left: Box::new(left),
            op: BinaryOp::Mul,
            right: Box::new(right),
        })
        .unwrap_or_else(|| ConstExpr::Literal("1".to_string()))
}

fn add_expr(left: ConstExpr, right: ConstExpr) -> ConstExpr {
    if is_zero(&left) {
        right
    } else if is_zero(&right) {
        left
    } else {
        ConstExpr::Binary {
            left: Box::new(left),
            op: BinaryOp::Add,
            right: Box::new(right),
        }
    }
}

fn is_zero(expr: &ConstExpr) -> bool {
    matches!(expr, ConstExpr::Literal(value) if value == "0")
}

fn is_one(expr: &ConstExpr) -> bool {
    matches!(expr, ConstExpr::Literal(value) if value == "1")
}

fn const_expr_from_expr(
    expr: &sv_parser::Expression,
    syntax_tree: &SyntaxTree,
) -> Option<ConstExpr> {
    match expr {
        sv_parser::Expression::Primary(primary) => const_expr_from_primary(primary, syntax_tree),
        sv_parser::Expression::Unary(unary) => {
            let op = unary_op_from_symbol(&unary.nodes.0.nodes.0.nodes.0, syntax_tree)?;
            let expr = const_expr_from_primary(&unary.nodes.2, syntax_tree)?;
            Some(ConstExpr::Unary {
                op,
                expr: Box::new(expr),
            })
        }
        sv_parser::Expression::Binary(binary) => {
            let right_is_grouped = expression_is_grouped(&binary.nodes.3);
            let left = const_expr_from_expr(&binary.nodes.0, syntax_tree)?;
            let op = binary_op_from_symbol(&binary.nodes.1.nodes.0.nodes.0, syntax_tree)?;
            let right = const_expr_from_expr(&binary.nodes.3, syntax_tree)?;
            let expr = ConstExpr::Binary {
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
            Some(if right_is_grouped {
                expr
            } else {
                left_associate_const_binary(expr)
            })
        }
        _ => None,
    }
}

fn const_expr_from_primary(
    primary: &sv_parser::Primary,
    syntax_tree: &SyntaxTree,
) -> Option<ConstExpr> {
    match primary {
        sv_parser::Primary::PrimaryLiteral(_) => {
            primary_literal_text(RefNode::Primary(primary), syntax_tree).map(ConstExpr::Literal)
        }
        sv_parser::Primary::Hierarchical(hierarchical) => identifier_text(
            RefNode::HierarchicalIdentifier(&hierarchical.nodes.1),
            syntax_tree,
        )
        .map(ConstExpr::Ident),
        sv_parser::Primary::FunctionSubroutineCall(call) => {
            const_expr_from_function_subroutine_call(call, syntax_tree)
        }
        sv_parser::Primary::MintypmaxExpression(expr) => match &expr.nodes.0.nodes.1 {
            sv_parser::MintypmaxExpression::Expression(expr) => {
                const_expr_from_expr(expr, syntax_tree)
            }
            sv_parser::MintypmaxExpression::Ternary(_) => None,
        },
        _ => expr_from_primary(primary, syntax_tree).and_then(expr_to_const),
    }
}

fn left_associate_expr_binary(expr: Expr) -> Expr {
    let Expr::Binary { left, op, right } = expr else {
        return expr;
    };
    match *right {
        Expr::Binary {
            left: right_left,
            op: right_op,
            right: right_right,
        } if binary_precedence(op) >= binary_precedence(right_op) => {
            left_associate_expr_binary(Expr::Binary {
                left: Box::new(left_associate_expr_binary(Expr::Binary {
                    left,
                    op,
                    right: right_left,
                })),
                op: right_op,
                right: right_right,
            })
        }
        right => Expr::Binary {
            left,
            op,
            right: Box::new(right),
        },
    }
}

fn left_associate_const_binary(expr: ConstExpr) -> ConstExpr {
    let ConstExpr::Binary { left, op, right } = expr else {
        return expr;
    };
    match *right {
        ConstExpr::Binary {
            left: right_left,
            op: right_op,
            right: right_right,
        } if binary_precedence(op) >= binary_precedence(right_op) => {
            left_associate_const_binary(ConstExpr::Binary {
                left: Box::new(left_associate_const_binary(ConstExpr::Binary {
                    left,
                    op,
                    right: right_left,
                })),
                op: right_op,
                right: right_right,
            })
        }
        right => ConstExpr::Binary {
            left,
            op,
            right: Box::new(right),
        },
    }
}

fn binary_precedence(op: BinaryOp) -> u8 {
    match op {
        BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => 11,
        BinaryOp::Add | BinaryOp::Sub => 10,
        BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar => 9,
        BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => 8,
        BinaryOp::Eq
        | BinaryOp::Ne
        | BinaryOp::EqCase
        | BinaryOp::NeCase
        | BinaryOp::EqWildcard
        | BinaryOp::NeWildcard => 7,
        BinaryOp::BitAnd => 6,
        BinaryOp::BitXor => 5,
        BinaryOp::BitOr => 4,
        BinaryOp::LogicAnd => 3,
        BinaryOp::LogicOr => 2,
    }
}

fn expr_to_const(expr: Expr) -> Option<ConstExpr> {
    match expr {
        Expr::Ident(name) => Some(ConstExpr::Ident(name)),
        Expr::Literal(value) => Some(ConstExpr::Literal(value)),
        Expr::Unary { op, expr } => Some(ConstExpr::Unary {
            op,
            expr: Box::new(expr_to_const(*expr)?),
        }),
        Expr::Binary { left, op, right } => Some(ConstExpr::Binary {
            left: Box::new(expr_to_const(*left)?),
            op,
            right: Box::new(expr_to_const(*right)?),
        }),
        Expr::Select { expr, msb, lsb, .. } if msb == lsb => Some(ConstExpr::Select {
            expr: Box::new(expr_to_const(*expr)?),
            bit: Box::new(msb),
        }),
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => Some(ConstExpr::Mux {
            condition: Box::new(expr_to_const(*condition)?),
            then_expr: Box::new(expr_to_const(*then_expr)?),
            else_expr: Box::new(expr_to_const(*else_expr)?),
        }),
        Expr::Call { name, args } => Some(ConstExpr::Function {
            name,
            args: args.into_iter().map(expr_to_const).collect::<Option<_>>()?,
        }),
        Expr::Select { .. } | Expr::Concat(_) | Expr::RepeatConcat { .. } | Expr::Resize { .. } => {
            None
        }
    }
}

fn const_expr_from_constant_param_with_env(
    expr: &sv_parser::ConstantParamExpression,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Option<ConstExpr> {
    match expr {
        sv_parser::ConstantParamExpression::ConstantMintypmaxExpression(expr) => match &**expr {
            sv_parser::ConstantMintypmaxExpression::Unary(expr) => {
                const_expr_from_ref_node_with_env(
                    RefNode::ConstantExpression(expr),
                    syntax_tree,
                    const_env,
                    type_aliases,
                )
            }
            sv_parser::ConstantMintypmaxExpression::Ternary(_) => None,
        },
        _ => None,
    }
}

fn const_expr_from_param_expression(
    expr: &sv_parser::ParamExpression,
    syntax_tree: &SyntaxTree,
) -> Option<ConstExpr> {
    match expr {
        sv_parser::ParamExpression::MintypmaxExpression(expr) => match &**expr {
            sv_parser::MintypmaxExpression::Expression(expr) => {
                const_expr_from_expr(expr.as_ref(), syntax_tree)
            }
            sv_parser::MintypmaxExpression::Ternary(_) => None,
        },
        sv_parser::ParamExpression::DataType(_) | sv_parser::ParamExpression::Dollar(_) => None,
    }
}

fn direction_from_ref_node(node: RefNode<'_>) -> Option<PortDirection> {
    let direction = unwrap_node!(node, PortDirection)?;
    match direction {
        RefNode::PortDirection(direction) => Some(direction_from_port_direction(direction)),
        _ => None,
    }
}

fn direction_from_port_direction(direction: &sv_parser::PortDirection) -> PortDirection {
    match direction {
        sv_parser::PortDirection::Input(_) => PortDirection::Input,
        sv_parser::PortDirection::Output(_) => PortDirection::Output,
        sv_parser::PortDirection::Inout(_) => PortDirection::Inout,
        sv_parser::PortDirection::Ref(_) => PortDirection::Ref,
    }
}

fn type_from_ref_node(node: RefNode<'_>, syntax_tree: &SyntaxTree) -> Option<Type> {
    type_from_ref_node_with_env(node, syntax_tree, &HashMap::default(), &HashMap::default())
}

fn type_from_ref_node_with_env(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Option<Type> {
    if let Some(atom) = integer_atom_expr_type(node.clone()) {
        let kind = if integer_atom_is_2state(node.clone()) {
            TypeKind::Bit
        } else {
            TypeKind::Logic
        };
        let mut r#type = Type::new(kind);
        r#type.is_signed = atom.signed;
        r#type.packed_ranges = vec![PackedRange::new(
            ConstExpr::Literal((atom.width - 1).to_string()),
            ConstExpr::Literal("0".to_string()),
        )];
        return Some(r#type);
    }
    let integer_vector = unwrap_node!(node.clone(), IntegerVectorType)?;
    let kind = match integer_vector {
        RefNode::IntegerVectorType(integer_vector) => match integer_vector {
            sv_parser::IntegerVectorType::Bit(_) => TypeKind::Bit,
            sv_parser::IntegerVectorType::Logic(_) => TypeKind::Logic,
            sv_parser::IntegerVectorType::Reg(_) => TypeKind::Reg,
        },
        _ => return None,
    };
    let mut r#type = Type::new(kind);
    r#type.is_signed = is_signed_from_ref_node(node.clone()).unwrap_or(false);
    r#type.packed_ranges =
        packed_ranges_from_ref_node_with_env(node, syntax_tree, const_env, type_aliases);
    Some(r#type)
}

fn integer_atom_is_2state(node: RefNode<'_>) -> bool {
    matches!(
        unwrap_node!(node, IntegerAtomType),
        Some(RefNode::IntegerAtomType(
            sv_parser::IntegerAtomType::Byte(_)
                | sv_parser::IntegerAtomType::Shortint(_)
                | sv_parser::IntegerAtomType::Int(_)
                | sv_parser::IntegerAtomType::Longint(_)
        ))
    )
}

fn type_from_net_port_header(
    header: &sv_parser::NetPortHeaderOrInterfacePortHeader,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Option<Type> {
    let sv_parser::NetPortHeaderOrInterfacePortHeader::NetPortHeader(header) = header else {
        return None;
    };
    match &header.nodes.1 {
        sv_parser::NetPortType::DataType(data_type) => match &data_type.nodes.1 {
            sv_parser::DataTypeOrImplicit::ImplicitDataType(_) => Some(Type::implicit()),
            sv_parser::DataTypeOrImplicit::DataType(_) => type_from_ref_node_with_env(
                RefNode::DataTypeOrImplicit(&data_type.nodes.1),
                syntax_tree,
                const_env,
                type_aliases,
            )
            .or_else(|| {
                type_alias_from_ref_node(
                    RefNode::DataTypeOrImplicit(&data_type.nodes.1),
                    syntax_tree,
                    type_aliases,
                )
            }),
        },
        sv_parser::NetPortType::NetTypeIdentifier(identifier) => {
            let name = identifier_text(RefNode::NetTypeIdentifier(identifier), syntax_tree)?;
            type_aliases.get(&name).cloned()
        }
        sv_parser::NetPortType::Interconnect(_) => None,
    }
}

fn type_from_variable_port_header(
    header: &sv_parser::VariablePortHeader,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Option<Type> {
    match &header.nodes.1.nodes.0 {
        sv_parser::VarDataType::DataType(data_type) => type_from_ref_node_with_env(
            RefNode::DataType(data_type),
            syntax_tree,
            const_env,
            type_aliases,
        )
        .or_else(|| type_alias_from_data_type(data_type, syntax_tree, type_aliases)),
        sv_parser::VarDataType::Var(data_type) => match &data_type.nodes.1 {
            sv_parser::DataTypeOrImplicit::ImplicitDataType(_) => Some(Type::implicit()),
            sv_parser::DataTypeOrImplicit::DataType(_) => type_from_ref_node_with_env(
                RefNode::DataTypeOrImplicit(&data_type.nodes.1),
                syntax_tree,
                const_env,
                type_aliases,
            )
            .or_else(|| {
                type_alias_from_data_type_or_implicit(&data_type.nodes.1, syntax_tree, type_aliases)
            }),
        },
    }
}

fn type_alias_from_data_type_or_implicit(
    data_type: &sv_parser::DataTypeOrImplicit,
    syntax_tree: &SyntaxTree,
    type_aliases: &HashMap<String, Type>,
) -> Option<Type> {
    let sv_parser::DataTypeOrImplicit::DataType(data_type) = data_type else {
        return None;
    };
    type_alias_from_data_type(data_type, syntax_tree, type_aliases)
}

fn type_alias_from_data_type(
    data_type: &sv_parser::DataType,
    syntax_tree: &SyntaxTree,
    type_aliases: &HashMap<String, Type>,
) -> Option<Type> {
    let name = match data_type {
        sv_parser::DataType::Type(data_type) => {
            identifier_text(RefNode::TypeIdentifier(&data_type.nodes.1), syntax_tree)?
        }
        sv_parser::DataType::ClassType(data_type) => {
            identifier_text(RefNode::PsClassIdentifier(&data_type.nodes.0), syntax_tree)?
        }
        _ => return None,
    };
    type_aliases.get(&name).cloned()
}

fn type_with_fallback_ranges_with_env(
    mut r#type: Type,
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Type {
    let direct_ranges =
        packed_ranges_from_ref_node_with_env(node.clone(), syntax_tree, const_env, type_aliases);
    if type_alias_from_ref_node(node.clone(), syntax_tree, type_aliases).is_some() {
        r#type.packed_ranges.extend(direct_ranges);
    } else if r#type.packed_ranges.is_empty() {
        r#type.packed_ranges = direct_ranges;
    }
    if !r#type.is_signed {
        r#type.is_signed = is_signed_from_ref_node(node).unwrap_or(false);
    }
    r#type
}

fn type_with_unpacked_ranges(mut r#type: Type, ranges: Vec<UnpackedRange>) -> Type {
    let mut unpacked_ranges = ranges;
    unpacked_ranges.extend(r#type.unpacked_ranges);
    r#type.unpacked_ranges = unpacked_ranges;
    r#type
}

fn is_signed_from_ref_node(node: RefNode<'_>) -> Option<bool> {
    match unwrap_node!(node, Signing)? {
        RefNode::Signing(signing) => match signing {
            sv_parser::Signing::Signed(_) => Some(true),
            sv_parser::Signing::Unsigned(_) => Some(false),
        },
        _ => None,
    }
}

fn packed_ranges_from_ref_node(node: RefNode<'_>, syntax_tree: &SyntaxTree) -> Vec<PackedRange> {
    packed_ranges_from_ref_node_with_env(
        node,
        syntax_tree,
        &HashMap::default(),
        &HashMap::default(),
    )
}

fn packed_ranges_from_ref_node_with_env(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Vec<PackedRange> {
    let mut ranges = Vec::new();
    for child in node {
        if let RefNode::PackedDimensionRange(range) = child {
            let constant_range = &range.nodes.0.nodes.1;
            let left = const_expr_from_ref_node_with_env(
                RefNode::ConstantExpression(&constant_range.nodes.0),
                syntax_tree,
                const_env,
                type_aliases,
            );
            let right = const_expr_from_ref_node_with_env(
                RefNode::ConstantExpression(&constant_range.nodes.2),
                syntax_tree,
                const_env,
                type_aliases,
            );
            if let (Some(left), Some(right)) = (left, right) {
                ranges.push(PackedRange::new(left, right));
            }
        }
    }
    ranges
}

fn unpacked_ranges_from_dimensions_with_env(
    dimensions: &[sv_parser::UnpackedDimension],
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Result<Vec<UnpackedRange>, AnalyzerError> {
    dimensions
        .iter()
        .map(|dimension| match dimension {
            sv_parser::UnpackedDimension::Range(range) => {
                let constant_range = &range.nodes.0.nodes.1;
                let left = const_expr_from_ref_node_with_env(
                    RefNode::ConstantExpression(&constant_range.nodes.0),
                    syntax_tree,
                    const_env,
                    type_aliases,
                );
                let right = const_expr_from_ref_node_with_env(
                    RefNode::ConstantExpression(&constant_range.nodes.2),
                    syntax_tree,
                    const_env,
                    type_aliases,
                );
                match (left, right) {
                    (Some(left), Some(right)) => Ok(UnpackedRange::new(left, right)),
                    _ => Err(AnalyzerError::Unsupported(
                        "unresolved unpacked array dimension".to_string(),
                    )),
                }
            }
            sv_parser::UnpackedDimension::Expression(expression) => {
                let size = const_expr_from_ref_node_with_env(
                    RefNode::ConstantExpression(&expression.nodes.0.nodes.1),
                    syntax_tree,
                    const_env,
                    type_aliases,
                );
                size.map(UnpackedRange::sized).ok_or_else(|| {
                    AnalyzerError::Unsupported("unresolved unpacked array dimension".to_string())
                })
            }
        })
        .collect()
}

fn validate_unpacked_dimension_sizes(
    ranges: &[UnpackedRange],
    const_env: &HashMap<String, i128>,
) -> Result<(), AnalyzerError> {
    if ranges.iter().any(|range| {
        range
            .size()
            .and_then(|size| eval_ast_const_expr(size, const_env))
            .is_some_and(|size| size <= 0)
    }) {
        return Err(AnalyzerError::Unsupported(
            "nonpositive unpacked array dimension".to_string(),
        ));
    }
    Ok(())
}

fn unpacked_ranges_from_variable_dimensions_with_env(
    dimensions: &[sv_parser::VariableDimension],
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Result<Vec<UnpackedRange>, AnalyzerError> {
    let mut ranges = Vec::new();
    for dimension in dimensions {
        match dimension {
            sv_parser::VariableDimension::UnpackedDimension(dimension) => {
                ranges.extend(unpacked_ranges_from_dimensions_with_env(
                    std::slice::from_ref(&**dimension),
                    syntax_tree,
                    const_env,
                    type_aliases,
                )?);
            }
            sv_parser::VariableDimension::UnsizedDimension(_) => {
                return Err(AnalyzerError::Unsupported(
                    "unsized unpacked array dimension".to_string(),
                ));
            }
            sv_parser::VariableDimension::AssociativeDimension(_) => {
                return Err(AnalyzerError::Unsupported(
                    "associative array dimension".to_string(),
                ));
            }
            sv_parser::VariableDimension::QueueDimension(_) => {
                return Err(AnalyzerError::Unsupported(
                    "queue array dimension".to_string(),
                ));
            }
        }
    }
    Ok(ranges)
}

fn const_expr_from_ref_node(node: RefNode<'_>, syntax_tree: &SyntaxTree) -> Option<ConstExpr> {
    const_expr_from_ref_node_with_env(node, syntax_tree, &HashMap::default(), &HashMap::default())
}

fn const_expr_from_ref_node_with_env(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Option<ConstExpr> {
    match node {
        RefNode::ConstantExpression(expr) => match expr {
            sv_parser::ConstantExpression::ConstantPrimary(primary) => {
                const_expr_from_ref_node_with_env(
                    RefNode::ConstantPrimary(primary),
                    syntax_tree,
                    const_env,
                    type_aliases,
                )
            }
            sv_parser::ConstantExpression::Unary(unary) => {
                let op = unary_op_from_symbol(&unary.nodes.0.nodes.0.nodes.0, syntax_tree)?;
                let expr = const_expr_from_ref_node_with_env(
                    RefNode::ConstantPrimary(&unary.nodes.2),
                    syntax_tree,
                    const_env,
                    type_aliases,
                )?;
                Some(ConstExpr::Unary {
                    op,
                    expr: Box::new(expr),
                })
            }
            sv_parser::ConstantExpression::Binary(binary) => {
                let right_is_grouped = constant_expression_is_grouped(&binary.nodes.3);
                let left = const_expr_from_ref_node_with_env(
                    RefNode::ConstantExpression(&binary.nodes.0),
                    syntax_tree,
                    const_env,
                    type_aliases,
                )?;
                let op = binary_op_from_symbol(&binary.nodes.1.nodes.0.nodes.0, syntax_tree)?;
                let right = const_expr_from_ref_node_with_env(
                    RefNode::ConstantExpression(&binary.nodes.3),
                    syntax_tree,
                    const_env,
                    type_aliases,
                )?;
                let expr = ConstExpr::Binary {
                    left: Box::new(left),
                    op,
                    right: Box::new(right),
                };
                Some(if right_is_grouped {
                    expr
                } else {
                    left_associate_const_binary(expr)
                })
            }
            sv_parser::ConstantExpression::Ternary(expr) => {
                const_expr_from_constant_expression_ternary_with_env(
                    expr,
                    syntax_tree,
                    const_env,
                    type_aliases,
                )
            }
            sv_parser::ConstantExpression::Inside(_) => None,
        },
        RefNode::ConstantPrimary(primary) => match primary {
            sv_parser::ConstantPrimary::PrimaryLiteral(_) => {
                primary_literal_text(node, syntax_tree).map(ConstExpr::Literal)
            }
            sv_parser::ConstantPrimary::PsParameter(parameter) => {
                let identifier = unwrap_node!(
                    RefNode::ConstantPrimaryPsParameter(parameter),
                    SimpleIdentifier,
                    EscapedIdentifier
                )?;
                let base = identifier_locate(identifier)
                    .and_then(|locate| syntax_tree.get_str(&locate).map(str::to_string))
                    .map(ConstExpr::Ident)?;
                const_select_expr(
                    base.clone(),
                    &parameter.nodes.1,
                    syntax_tree,
                    const_env,
                    type_aliases,
                )
                .or(Some(base))
            }
            sv_parser::ConstantPrimary::ConstantFunctionCall(call) => {
                let lowered = const_expr_from_function_subroutine_call(&call.nodes.0, syntax_tree);
                if let Some(ConstExpr::Function { name, args }) = &lowered
                    && name == "$bits"
                    && let [arg] = args.as_slice()
                    && let Some(r#type) =
                        infer_const_expr_type(arg, &parameter_types_from_const_env(const_env))
                {
                    return Some(ConstExpr::Literal(r#type.width.to_string()));
                }
                lowered.or_else(|| {
                    let sv_parser::SubroutineCall::TfCall(tf_call) = &call.nodes.0.nodes.0 else {
                        return None;
                    };
                    if tf_call.nodes.2.is_some() {
                        return None;
                    }
                    let identifier = unwrap_node!(
                        RefNode::ConstantFunctionCall(call),
                        SimpleIdentifier,
                        EscapedIdentifier
                    )?;
                    identifier_locate(identifier)
                        .and_then(|locate| syntax_tree.get_str(&locate).map(str::to_string))
                        .map(ConstExpr::Ident)
                })
            }
            sv_parser::ConstantPrimary::ConstantCast(cast) => {
                constant_cast_const_expr(cast, syntax_tree, const_env, type_aliases)
            }
            sv_parser::ConstantPrimary::MintypmaxExpression(expr) => match &expr.nodes.0.nodes.1 {
                sv_parser::ConstantMintypmaxExpression::Unary(expr) => {
                    const_expr_from_ref_node_with_env(
                        RefNode::ConstantExpression(expr),
                        syntax_tree,
                        const_env,
                        type_aliases,
                    )
                }
                sv_parser::ConstantMintypmaxExpression::Ternary(_) => None,
            },
            _ => None,
        },
        _ => {
            if let Some(integral_number) = unwrap_node!(node.clone(), IntegralNumber) {
                return integral_number_literal(integral_number, syntax_tree)
                    .map(ConstExpr::Literal);
            }
            if let Some(identifier) = unwrap_node!(node, SimpleIdentifier, EscapedIdentifier) {
                return identifier_locate(identifier)
                    .and_then(|locate| syntax_tree.get_str(&locate).map(str::to_string))
                    .map(ConstExpr::Ident);
            }
            None
        }
    }
}

fn constant_expression_is_grouped(expr: &sv_parser::ConstantExpression) -> bool {
    matches!(
        expr,
        sv_parser::ConstantExpression::ConstantPrimary(primary)
            if matches!(
                &**primary,
                sv_parser::ConstantPrimary::MintypmaxExpression(_)
            )
    )
}

fn const_expr_from_constant_expression_ternary_with_env(
    expr: &sv_parser::ConstantExpressionTernary,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Option<ConstExpr> {
    Some(ConstExpr::Mux {
        condition: Box::new(const_expr_from_ref_node_with_env(
            RefNode::ConstantExpression(&expr.nodes.0),
            syntax_tree,
            const_env,
            type_aliases,
        )?),
        then_expr: Box::new(const_expr_from_ref_node_with_env(
            RefNode::ConstantExpression(&expr.nodes.3),
            syntax_tree,
            const_env,
            type_aliases,
        )?),
        else_expr: Box::new(const_expr_from_ref_node_with_env(
            RefNode::ConstantExpression(&expr.nodes.5),
            syntax_tree,
            const_env,
            type_aliases,
        )?),
    })
}

fn const_select_expr(
    base: ConstExpr,
    select: &sv_parser::ConstantSelect,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Option<ConstExpr> {
    let bit_selects = select.nodes.1.nodes.0.as_slice();
    if bit_selects.len() != 1 || select.nodes.2.is_some() {
        return None;
    }
    let bit = const_expr_from_ref_node_with_env(
        RefNode::ConstantExpression(&bit_selects[0].nodes.1),
        syntax_tree,
        const_env,
        type_aliases,
    )?;
    Some(ConstExpr::Select {
        expr: Box::new(base),
        bit: Box::new(bit),
    })
}

fn const_expr_from_function_subroutine_call(
    call: &sv_parser::FunctionSubroutineCall,
    syntax_tree: &SyntaxTree,
) -> Option<ConstExpr> {
    let sv_parser::SubroutineCall::SystemTfCall(call) = &call.nodes.0 else {
        return None;
    };
    let (identifier, arguments) = match &**call {
        sv_parser::SystemTfCall::ArgExpression(call) => {
            (&call.nodes.0, call.nodes.1.nodes.1.0.contents())
        }
        _ => return None,
    };
    let name = syntax_tree.get_str(&identifier.nodes.0)?.to_string();
    let args = arguments
        .into_iter()
        .filter_map(|argument| argument.as_ref())
        .map(|argument| const_expr_from_expr(argument, syntax_tree))
        .collect::<Option<Vec<_>>>()?;
    Some(ConstExpr::Function { name, args })
}

fn integral_number_literal(node: RefNode<'_>, syntax_tree: &SyntaxTree) -> Option<String> {
    let RefNode::IntegralNumber(number) = node else {
        return None;
    };
    match number {
        sv_parser::IntegralNumber::DecimalNumber(decimal) => match &**decimal {
            sv_parser::DecimalNumber::UnsignedNumber(number) => {
                locate_text(&number.nodes.0, syntax_tree)
            }
            sv_parser::DecimalNumber::BaseUnsigned(number) => based_literal(
                number.nodes.0.as_ref().map(|size| &size.nodes.0.nodes.0),
                &number.nodes.1.nodes.0,
                &number.nodes.2.nodes.0,
                syntax_tree,
            ),
            sv_parser::DecimalNumber::BaseXNumber(number) => based_literal(
                number.nodes.0.as_ref().map(|size| &size.nodes.0.nodes.0),
                &number.nodes.1.nodes.0,
                &number.nodes.2.nodes.0,
                syntax_tree,
            ),
            sv_parser::DecimalNumber::BaseZNumber(number) => based_literal(
                number.nodes.0.as_ref().map(|size| &size.nodes.0.nodes.0),
                &number.nodes.1.nodes.0,
                &number.nodes.2.nodes.0,
                syntax_tree,
            ),
        },
        sv_parser::IntegralNumber::BinaryNumber(number) => based_literal(
            number.nodes.0.as_ref().map(|size| &size.nodes.0.nodes.0),
            &number.nodes.1.nodes.0,
            &number.nodes.2.nodes.0,
            syntax_tree,
        ),
        sv_parser::IntegralNumber::OctalNumber(number) => based_literal(
            number.nodes.0.as_ref().map(|size| &size.nodes.0.nodes.0),
            &number.nodes.1.nodes.0,
            &number.nodes.2.nodes.0,
            syntax_tree,
        ),
        sv_parser::IntegralNumber::HexNumber(number) => based_literal(
            number.nodes.0.as_ref().map(|size| &size.nodes.0.nodes.0),
            &number.nodes.1.nodes.0,
            &number.nodes.2.nodes.0,
            syntax_tree,
        ),
    }
}

fn based_literal(
    size: Option<&Locate>,
    base: &Locate,
    digits: &Locate,
    syntax_tree: &SyntaxTree,
) -> Option<String> {
    let size = size
        .and_then(|size| locate_text(size, syntax_tree))
        .unwrap_or_default();
    let base = locate_text(base, syntax_tree)?;
    let digits = locate_text(digits, syntax_tree)?;
    Some(format!("{size}{base}{digits}"))
}

fn locate_text(locate: &Locate, syntax_tree: &SyntaxTree) -> Option<String> {
    syntax_tree.get_str(locate).map(str::to_string)
}

fn primary_literal_text(node: RefNode<'_>, syntax_tree: &SyntaxTree) -> Option<String> {
    if let Some(integral_number) = unwrap_node!(node.clone(), IntegralNumber) {
        return integral_number_literal(integral_number, syntax_tree);
    }
    let unbased = unwrap_node!(node, UnbasedUnsizedLiteral)?;
    let RefNode::UnbasedUnsizedLiteral(unbased) = unbased else {
        return None;
    };
    syntax_tree.get_str(&unbased.nodes.0).map(str::to_string)
}

fn unary_op_from_symbol(symbol: &Locate, syntax_tree: &SyntaxTree) -> Option<UnaryOp> {
    match syntax_tree.get_str(symbol)? {
        "+" => Some(UnaryOp::Plus),
        "-" => Some(UnaryOp::Minus),
        "~" => Some(UnaryOp::BitNot),
        "!" => Some(UnaryOp::LogicNot),
        "&" => Some(UnaryOp::RedAnd),
        "|" => Some(UnaryOp::RedOr),
        "^" => Some(UnaryOp::RedXor),
        _ => None,
    }
}

fn unary_expr_from_symbol(symbol: &Locate, expr: Expr, syntax_tree: &SyntaxTree) -> Option<Expr> {
    let reduction = match syntax_tree.get_str(symbol)? {
        "~&" => Some(UnaryOp::RedAnd),
        "~|" => Some(UnaryOp::RedOr),
        "~^" | "^~" => Some(UnaryOp::RedXor),
        _ => None,
    };
    if let Some(op) = reduction {
        return Some(Expr::Unary {
            op: UnaryOp::BitNot,
            expr: Box::new(Expr::Unary {
                op,
                expr: Box::new(expr),
            }),
        });
    }
    Some(Expr::Unary {
        op: unary_op_from_symbol(symbol, syntax_tree)?,
        expr: Box::new(expr),
    })
}

fn binary_op_from_symbol(symbol: &Locate, syntax_tree: &SyntaxTree) -> Option<BinaryOp> {
    match syntax_tree.get_str(symbol)? {
        "+" => Some(BinaryOp::Add),
        "-" => Some(BinaryOp::Sub),
        "*" => Some(BinaryOp::Mul),
        "/" => Some(BinaryOp::Div),
        "%" => Some(BinaryOp::Mod),
        "<<" => Some(BinaryOp::Shl),
        "<<<" => Some(BinaryOp::Shl),
        ">>" => Some(BinaryOp::Shr),
        ">>>" => Some(BinaryOp::Sar),
        "&" => Some(BinaryOp::BitAnd),
        "|" => Some(BinaryOp::BitOr),
        "^" => Some(BinaryOp::BitXor),
        "&&" => Some(BinaryOp::LogicAnd),
        "||" => Some(BinaryOp::LogicOr),
        "==" => Some(BinaryOp::Eq),
        "!=" => Some(BinaryOp::Ne),
        "===" => Some(BinaryOp::EqCase),
        "!==" => Some(BinaryOp::NeCase),
        "==?" => Some(BinaryOp::EqWildcard),
        "!=?" => Some(BinaryOp::NeWildcard),
        "<" => Some(BinaryOp::Lt),
        "<=" => Some(BinaryOp::Le),
        ">" => Some(BinaryOp::Gt),
        ">=" => Some(BinaryOp::Ge),
        _ => None,
    }
}
