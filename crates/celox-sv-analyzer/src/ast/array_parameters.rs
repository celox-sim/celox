//! Parameters of unpacked array type (IEEE 1800-2023 6.20.2), such as the
//! table `localparam logic [2:0] T [4] = '{...};`, and parameters whose value
//! is an assignment pattern, such as a packed structure `S'{m: 1, k: 2}`.
//! Such a parameter becomes a module variable whose initial value is the
//! pattern; nothing else writes it, so it holds that value throughout the
//! simulation.

use super::*;

/// A parameter of unpacked array type, as the variable that holds it.
pub(super) struct ArrayParameter<'a> {
    pub signal: Signal,
    pattern: &'a sv_parser::AssignmentPatternExpression,
}

/// Whether a parameter assignment declares an unpacked array, or a value
/// given by an assignment pattern (a packed structure or array).
pub(super) fn is_array_parameter(param: &sv_parser::ParamAssignment) -> bool {
    !param.nodes.1.is_empty() || value_pattern(param).is_some()
}

fn declared_type(
    node: RefNode<'_>,
    tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Option<Type> {
    let type_node = match node {
        RefNode::ParameterDeclaration(sv_parser::ParameterDeclaration::Param(declaration)) => {
            RefNode::DataTypeOrImplicit(&declaration.nodes.1)
        }
        RefNode::LocalParameterDeclaration(sv_parser::LocalParameterDeclaration::Param(
            declaration,
        )) => RefNode::DataTypeOrImplicit(&declaration.nodes.1),
        RefNode::ParameterPortDeclaration(sv_parser::ParameterPortDeclaration::ParamList(
            declaration,
        )) => RefNode::DataType(&declaration.nodes.0),
        _ => return None,
    };
    let r#type = type_from_ref_node_with_env(type_node.clone(), tree, const_env, type_aliases)
        .or_else(|| type_alias_from_ref_node(type_node.clone(), tree, type_aliases))?;
    Some(type_with_fallback_ranges_with_env(
        r#type,
        type_node,
        tree,
        const_env,
        type_aliases,
    ))
}

/// The value pattern of an array parameter.
fn value_pattern(
    param: &sv_parser::ParamAssignment,
) -> Option<&sv_parser::AssignmentPatternExpression> {
    let (_, value) = param.nodes.2.as_ref()?;
    let sv_parser::ConstantParamExpression::ConstantMintypmaxExpression(value) = value else {
        return None;
    };
    let sv_parser::ConstantMintypmaxExpression::Unary(value) = &**value else {
        return None;
    };
    let sv_parser::ConstantExpression::ConstantPrimary(primary) = &**value else {
        return None;
    };
    let sv_parser::ConstantPrimary::ConstantAssignmentPatternExpression(pattern) = &**primary
    else {
        return None;
    };
    Some(&pattern.nodes.0)
}

/// The array parameters declared in the module scope.
pub(super) fn array_parameters_from_module_node<'a>(
    node: RefNode<'a>,
    tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Result<Vec<ArrayParameter<'a>>, AnalyzerError> {
    let mut declarations = Vec::new();
    // The `#(...)` list, then the module items.
    if let Some(list) = module_parameter_port_list(node.clone()) {
        for child in list {
            match child {
                RefNode::ParameterPortDeclaration(
                    declaration @ sv_parser::ParameterPortDeclaration::ParamList(_),
                ) => declarations.push(RefNode::ParameterPortDeclaration(declaration)),
                RefNode::ParameterDeclaration(_) | RefNode::LocalParameterDeclaration(_) => {
                    declarations.push(child)
                }
                _ => {}
            }
        }
    }
    for item in module_non_port_items(node.clone()) {
        let Some(declaration) = package_or_generate_declaration_from_non_port_item(item) else {
            continue;
        };
        declarations.push(match declaration {
            sv_parser::PackageOrGenerateItemDeclaration::LocalParameterDeclaration(declaration) => {
                RefNode::LocalParameterDeclaration(&declaration.0)
            }
            sv_parser::PackageOrGenerateItemDeclaration::ParameterDeclaration(declaration) => {
                RefNode::ParameterDeclaration(&declaration.0)
            }
            _ => continue,
        });
    }
    let mut parameters = Vec::new();
    for declaration in declarations {
        for child in declaration.clone() {
            let RefNode::ParamAssignment(param) = child else {
                continue;
            };
            if !is_array_parameter(param) {
                continue;
            }
            let name = parameter_name(RefNode::ParameterIdentifier(&param.nodes.0), tree)?;
            let element = declared_type(declaration.clone(), tree, const_env, type_aliases)
                .ok_or_else(|| {
                    AnalyzerError::Unsupported(format!("type of array parameter `{name}`"))
                })?;
            let ranges = unpacked_ranges_from_dimensions_with_env(
                &param.nodes.1,
                tree,
                const_env,
                type_aliases,
            )?;
            let pattern = value_pattern(param).ok_or_else(|| {
                AnalyzerError::Unsupported(format!("value of array parameter `{name}`"))
            })?;
            parameters.push(ArrayParameter {
                signal: Signal::new(name, type_with_unpacked_ranges(element, ranges)),
                pattern,
            });
        }
    }
    Ok(parameters)
}

impl ArrayParameter<'_> {
    /// The `initial` block that gives the variable its value.
    pub(super) fn initial_process(
        &self,
        tree: &SyntaxTree,
        dims: &PackedDimensions,
    ) -> Result<InitialProcess, AnalyzerError> {
        let name = self.signal.name().to_string();
        let shape = procedural::dimensions_from_type(self.signal.r#type());
        let value = if self.pattern.nodes.0.is_some() {
            patterns::typed_pattern(self.pattern, tree, dims)
        } else {
            patterns::expr_from_pattern(&self.pattern.nodes.1, &shape, tree, dims)
        }
        .ok_or_else(|| AnalyzerError::Unsupported(format!("value of array parameter `{name}`")))?;
        Ok(InitialProcess {
            condition: None,
            body: vec![Stmt::Assign {
                lhs: LValue::Ident(name),
                rhs: value,
                nonblocking: false,
            }],
        })
    }
}
