//! Packed/unpacked dimension metadata and size-query type discovery.

use super::*;

/// IEEE 1800-2023 20.7: count the declared dimensions without evaluating
/// the operand. Preserve array shape before expression lowering flattens it.
pub(super) fn dimensions_system_function_call_value(
    call: &sv_parser::SystemTfCall,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
    dimensions: Option<&PackedDimensions>,
) -> Option<usize> {
    match call {
        sv_parser::SystemTfCall::ArgDataType(call) => {
            if syntax_tree.get_str(&call.nodes.0.nodes.0)? != "$dimensions"
                || call.nodes.1.nodes.1.1.is_some()
            {
                return None;
            }
            match &call.nodes.1.nodes.1.0 {
                sv_parser::DataType::String(_) => return Some(1),
                sv_parser::DataType::NonIntegerType(_)
                | sv_parser::DataType::Chandle(_)
                | sv_parser::DataType::Event(_) => return Some(0),
                _ => {}
            }
            let ty = function_type_from_ref_node(
                RefNode::DataType(&call.nodes.1.nodes.1.0),
                syntax_tree,
                const_env,
                type_aliases,
            )?;
            Some(ty.unpacked_ranges().len() + ty.packed_ranges().len())
        }
        sv_parser::SystemTfCall::ArgExpression(call) => {
            if syntax_tree.get_str(&call.nodes.0.nodes.0)? != "$dimensions" {
                return None;
            }
            let arguments = call.nodes.1.nodes.1.0.contents();
            let [Some(argument)] = arguments.as_slice() else {
                return None;
            };
            let mut context = if let Some(dimensions) = dimensions
                && dimensions.scope_types_complete
            {
                dimensions.clone()
            } else {
                let mut context = containing_packed_dimensions(
                    RefNode::Expression(argument),
                    syntax_tree,
                    const_env,
                    type_aliases,
                )
                .unwrap_or_else(|| {
                    PackedDimensions::new(HashMap::default(), const_env, type_aliases)
                });
                context.function_return_types = containing_function_return_types(
                    RefNode::Expression(argument),
                    syntax_tree,
                    const_env,
                    type_aliases,
                );
                if let Some(dimensions) = dimensions {
                    context.extend(
                        dimensions
                            .iter()
                            .map(|(name, metadata)| (name.clone(), metadata.clone())),
                    );
                    context
                        .function_return_types
                        .extend(dimensions.function_return_types.clone());
                    context.parameter_values = dimensions.parameter_values.clone();
                }
                if let Some(locals) = containing_function_dimensions(
                    RefNode::Expression(argument),
                    syntax_tree,
                    const_env,
                    type_aliases,
                ) {
                    for (name, ty) in &locals {
                        context.const_env.insert(
                            variable_dimensions_marker(name),
                            (ty.unpacked.len() + ty.packed.len()) as i128,
                        );
                    }
                    context.extend(locals);
                }
                context
            };
            // Cast lowering needs signedness even in declaration-time queries.
            let signedness = context
                .iter()
                .map(|(name, ty)| (name.clone(), ty.signed))
                .collect::<Vec<_>>();
            context.expression_signedness.extend(signedness);
            // Query declared return types, including functions with side effects.
            context.functions = Arc::default();
            if !context.scope_types_complete {
                let mut shapes = containing_subroutine_param_shapes(
                    RefNode::Expression(argument),
                    syntax_tree,
                    const_env,
                    type_aliases,
                );
                if let Some(dimensions) = dimensions {
                    shapes.extend(
                        dimensions
                            .subroutine_param_shapes
                            .iter()
                            .map(|(name, shapes)| (name.clone(), shapes.clone())),
                    );
                }
                context.subroutine_param_shapes = Arc::new(shapes);
            }
            expression_dimensions(
                argument,
                syntax_tree,
                &context.const_env,
                type_aliases,
                &context,
            )
        }
        sv_parser::SystemTfCall::ArgOptionl(_) => None,
    }
}

fn expression_dimensions(
    argument: &sv_parser::Expression,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
    dimensions: &PackedDimensions,
) -> Option<usize> {
    match argument {
        sv_parser::Expression::Unary(unary)
            if matches!(
                syntax_tree.get_str(&unary.nodes.0.nodes.0.nodes.0)?,
                "+" | "-" | "~"
            ) =>
        {
            let operand = sv_parser::Expression::Primary(Box::new(unary.nodes.2.clone()));
            return expression_dimensions(
                &operand,
                syntax_tree,
                const_env,
                type_aliases,
                dimensions,
            );
        }
        sv_parser::Expression::Binary(binary)
            if matches!(
                syntax_tree.get_str(&binary.nodes.1.nodes.0.nodes.0)?,
                "<<" | ">>" | "<<<" | ">>>"
            ) =>
        {
            return expression_dimensions(
                &binary.nodes.0,
                syntax_tree,
                const_env,
                type_aliases,
                dimensions,
            );
        }
        _ => {}
    }
    if let sv_parser::Expression::Primary(primary) = argument {
        match &**primary {
            sv_parser::Primary::MintypmaxExpression(grouped) => {
                if let sv_parser::MintypmaxExpression::Expression(argument) =
                    &grouped.nodes.0.nodes.1
                {
                    return expression_dimensions(
                        argument,
                        syntax_tree,
                        const_env,
                        type_aliases,
                        dimensions,
                    );
                }
            }
            sv_parser::Primary::PrimaryLiteral(literal) => {
                if unwrap_node!(RefNode::PrimaryLiteral(literal), StringLiteral).is_some() {
                    return Some(1);
                }
                if unwrap_node!(RefNode::PrimaryLiteral(literal), RealNumber).is_some() {
                    return Some(0);
                }
                if matches!(
                    &**literal,
                    sv_parser::PrimaryLiteral::Number(_)
                        | sv_parser::PrimaryLiteral::UnbasedUnsizedLiteral(_)
                ) {
                    return Some(1);
                }
            }
            sv_parser::Primary::Hierarchical(hierarchical) => {
                let select = &hierarchical.nodes.2;
                if packed_structs::has_member_access(
                    RefNode::HierarchicalIdentifier(&hierarchical.nodes.1),
                    RefNode::Select(select),
                ) && let Some(count) = packed_structs::member_dimension_count(
                    RefNode::HierarchicalIdentifier(&hierarchical.nodes.1),
                    select,
                    syntax_tree,
                    dimensions,
                ) {
                    return Some(count);
                }
                let name = reference_name(RefNode::PrimaryHierarchical(hierarchical), syntax_tree)?;
                let declared_count = dimensions
                    .get(&name)
                    .map(|ty| ty.unpacked.len() + ty.packed.len());
                let scoped_count = const_env
                    .get(&variable_dimensions_marker(&name))
                    .and_then(|n| usize::try_from(*n).ok());
                // Preliminary discovery sees module declarations; generated
                // locals are already represented by the current scope's markers.
                let count = if dimensions.scope_types_complete {
                    declared_count.or(scoped_count)
                } else {
                    scoped_count.or(declared_count)
                }
                .or_else(|| {
                    const_env
                        .get(&parameter_rank_marker(&name))
                        .and_then(|n| usize::try_from(*n).ok())
                })
                .or_else(|| {
                    type_aliases
                        .get(&name)
                        .map(|ty| ty.unpacked_ranges().len() + ty.packed_ranges().len())
                });
                if let Some(count) = count {
                    let remaining = count.checked_sub(select.nodes.1.nodes.0.len())?;
                    if select.nodes.2.is_some() && remaining == 0 {
                        return None;
                    }
                    return Some(remaining);
                }
            }
            sv_parser::Primary::Cast(cast) => {
                if let Some(ty) = type_from_ref_node_with_env(
                    RefNode::CastingType(&cast.nodes.0),
                    syntax_tree,
                    const_env,
                    type_aliases,
                )
                .or_else(|| {
                    type_alias_from_ref_node(
                        RefNode::CastingType(&cast.nodes.0),
                        syntax_tree,
                        type_aliases,
                    )
                }) {
                    return Some(ty.unpacked_ranges().len() + ty.packed_ranges().len());
                }
            }
            _ => {}
        }
    }
    let expression = expr_from_expression_with_types(argument, syntax_tree, dimensions).ok()?;
    if let Expr::Call { name, .. } = &expression
        && let Some(metadata) = dimensions.function_return_types.get(name)
    {
        return metadata.dimensions;
    }
    // Ordinary integral expression results are scalar or simple bit vectors.
    expr_static_width(&expression, dimensions).map(|width| {
        usize::from(
            width > 1
                || matches!(
                    expression,
                    Expr::Literal(_)
                        | Expr::Concat(_)
                        | Expr::RepeatConcat { .. }
                        | Expr::Resize { .. }
                ),
        )
    })
}

mod queries;
pub(super) use queries::array_query_call;

pub(super) fn size_system_function_expr_type(
    primary: &sv_parser::ConstantPrimary,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Option<ExprType> {
    let sv_parser::ConstantPrimary::ConstantFunctionCall(call) = primary else {
        return None;
    };
    let sv_parser::SubroutineCall::SystemTfCall(call) = &call.nodes.0.nodes.0 else {
        return None;
    };
    if let Some(count) =
        dimensions_system_function_call_value(call, syntax_tree, const_env, type_aliases, None)
    {
        return Some(ExprType {
            width: count,
            signed: false,
        });
    }
    size_system_function_call_type(call, syntax_tree, const_env, type_aliases, None)
}

pub(super) fn size_system_function_call_type(
    call: &sv_parser::SystemTfCall,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
    dimensions: Option<&PackedDimensions>,
) -> Option<ExprType> {
    // The dimension argument of `$size(x, n)`, a constant expression.
    let dimension_argument = |dimension: &sv_parser::Expression| {
        const_expr_from_expr(dimension, syntax_tree)
            .ok()
            .flatten()
            .and_then(|dimension| eval_ast_const_expr(&dimension, const_env))
    };
    let (name, r#type, dimension) = match call {
        sv_parser::SystemTfCall::ArgDataType(call) => {
            let name = syntax_tree.get_str(&call.nodes.0.nodes.0)?;
            let (data_type, dimension) = &call.nodes.1.nodes.1;
            let dimension = match dimension {
                Some((_, dimension)) => Some(dimension_argument(dimension)?),
                None => None,
            };
            let r#type = match data_type {
                sv_parser::DataType::Type(data_type) => {
                    let name = reference_name(RefNode::DataTypeType(data_type), syntax_tree)?;
                    type_aliases.get(&name).cloned()
                }
                _ => type_from_ref_node_with_env(
                    RefNode::DataType(data_type),
                    syntax_tree,
                    const_env,
                    type_aliases,
                ),
            }?;
            (name, r#type, dimension)
        }
        // sv-parser classifies an unqualified typedef argument as an
        // expression because its grammar cannot know whether the identifier
        // names a type. Resolve that ambiguity from the module alias table.
        sv_parser::SystemTfCall::ArgExpression(call) => {
            let name = syntax_tree.get_str(&call.nodes.0.nodes.0)?;
            let arguments = call.nodes.1.nodes.1.0.contents();
            // `$size(x, n)`: the number of elements of dimension `n`, counted
            // from the outermost unpacked dimension (IEEE 1800-2023 20.7).
            if name == "$size"
                && let [Some(argument), Some(dimension)] = arguments.as_slice()
            {
                let identifier = match const_expr_from_expr(argument, syntax_tree).ok().flatten()? {
                    ConstExpr::Ident(identifier) => identifier,
                    _ => return None,
                };
                let dimension = dimension_argument(dimension)?;
                let Some(variable) = dimensions.and_then(|dimensions| dimensions.get(&identifier))
                else {
                    // A type name that sv-parser parsed as an expression.
                    let r#type = type_aliases.get(&identifier)?.clone();
                    return type_dimension_size(&r#type, Some(dimension), const_env);
                };
                let widths: Vec<&ConstExpr> = variable
                    .unpacked
                    .iter()
                    .map(|dimension| &dimension.width)
                    .chain(variable.packed.iter().map(|dimension| &dimension.width))
                    .collect();
                let width = widths.get(usize::try_from(dimension).ok()?.checked_sub(1)?)?;
                return Some(ExprType {
                    width: usize::try_from(eval_ast_const_expr(width, const_env)?).ok()?,
                    signed: false,
                });
            }
            if arguments.len() != 1 {
                return None;
            }
            let argument = arguments[0].as_ref()?;
            if name != "$bits" && name != "$size" {
                return None;
            }
            // An unqualified typedef is parsed as an expression. Once the
            // lexical scope is complete, distinguish it from a visible value
            // before falling back to enclosing-declaration discovery.
            if let Some(dimensions) = dimensions
                && dimensions.scope_types_complete
                && !type_aliases.is_empty()
                && let Ok(Some(ConstExpr::Ident(identifier))) =
                    const_expr_from_expr(argument, syntax_tree)
                && dimensions.get(&identifier).is_none()
                && !const_env.contains_key(&identifier)
                && !const_env.contains_key(&parameter_width_marker(&identifier))
                && !dimensions.parameter_values.contains_key(&identifier)
                && let Some(r#type) = type_aliases.get(&identifier)
            {
                return size_query_for_type(r#type, const_env, name == "$size");
            }
            if let Some(dimensions) = dimensions
                && let Some(ty) = size_function_expression_type(
                    argument,
                    syntax_tree,
                    const_env,
                    type_aliases,
                    name == "$size",
                    Some(dimensions),
                )
            {
                return Some(ty);
            }
            // Function formals and locals shadow generated signal type markers.
            if let Some(dimensions) = containing_function_dimensions(
                RefNode::Expression(argument),
                syntax_tree,
                const_env,
                type_aliases,
            ) && let Ok(Some(ConstExpr::Ident(identifier))) =
                const_expr_from_expr(argument, syntax_tree)
                && dimensions.contains_key(&identifier)
            {
                let dimensions = PackedDimensions::new(dimensions, const_env, type_aliases);
                let expression = Expr::Ident(identifier.clone());
                let width = if name == "$size" {
                    selected_expression_first_dimension_width(argument, syntax_tree, &dimensions)
                } else {
                    expr_static_width(&expression, &dimensions)
                }?;
                return Some(ExprType {
                    width,
                    signed: dimensions.get(&identifier)?.signed,
                });
            }
            // Scoped type markers take precedence over a same-named declaration
            // found by the enclosing-module scan used for complex expressions.
            if let Ok(Some(ConstExpr::Ident(identifier))) =
                const_expr_from_expr(argument, syntax_tree)
                && let Some(width) =
                    variable_size_function_width(const_env, &identifier, name == "$size")
            {
                return Some(ExprType {
                    width,
                    signed: variable_type_is_signed(const_env, &identifier),
                });
            }
            // A size-function argument only needs a statically known type;
            // it need not itself be a constant expression. Lower the typed
            // expression first so selects and other runtime-valued forms can
            // still determine the cast width.
            if let Some(r#type) = size_function_expression_type(
                argument,
                syntax_tree,
                const_env,
                type_aliases,
                name == "$size",
                None,
            ) {
                return Some(r#type);
            }
            let argument = const_expr_from_expr(argument, syntax_tree).ok().flatten()?;
            if let ConstExpr::Ident(alias) = &argument
                && let Some(r#type) = type_aliases.get(alias)
            {
                (name, r#type.clone(), None)
            } else {
                if let ConstExpr::Ident(identifier) = &argument
                    && let Some(width) =
                        variable_size_function_width(const_env, identifier, name == "$size")
                {
                    return Some(ExprType {
                        width,
                        signed: variable_type_is_signed(const_env, identifier),
                    });
                }
                let r#type =
                    infer_const_expr_type(&argument, &parameter_types_from_const_env(const_env))?;
                return Some(ExprType {
                    width: r#type.width.max(1),
                    signed: r#type.signed,
                });
            }
        }
        sv_parser::SystemTfCall::ArgOptionl(_) => return None,
    };
    // $bits covers every unpacked and packed dimension; $size covers one
    // dimension, by default the outermost one.
    if name == "$size" {
        return type_dimension_size(&r#type, dimension, const_env);
    }
    if name != "$bits" || dimension.is_some() {
        return None;
    }
    size_query_for_type(&r#type, const_env, false)
}

fn size_query_for_type(
    r#type: &Type,
    const_env: &HashMap<String, i128>,
    first_dimension_only: bool,
) -> Option<ExprType> {
    if first_dimension_only {
        return type_dimension_size(r#type, None, const_env);
    }
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
    Some(ExprType {
        width: unpacked_width.checked_mul(packed_width)?.max(1),
        signed: r#type.is_signed(),
    })
}

/// `$size` of a type: the number of elements of dimension `dimension`
/// (default 1), numbered from the slowest varying dimension, unpacked
/// dimensions first (IEEE 1800-2023 20.7).
fn type_dimension_size(
    r#type: &Type,
    dimension: Option<i128>,
    const_env: &HashMap<String, i128>,
) -> Option<ExprType> {
    let dimension = usize::try_from(dimension.unwrap_or(1))
        .ok()?
        .checked_sub(1)?;
    let mut ranges = r#type
        .unpacked_ranges()
        .iter()
        .map(|range| (range.left(), range.right()))
        .chain(
            r#type
                .packed_ranges()
                .iter()
                .map(|range| (range.left(), range.right())),
        )
        .peekable();
    if ranges.peek().is_none() && dimension == 0 {
        return Some(ExprType {
            width: 1,
            signed: r#type.is_signed(),
        });
    }
    let (left, right) = ranges.nth(dimension)?;
    let left = eval_ast_const_expr(left, const_env)?;
    let right = eval_ast_const_expr(right, const_env)?;
    let width = usize::try_from(left.abs_diff(right)).ok()?.checked_add(1)?;
    Some(ExprType {
        width: width.max(1),
        signed: r#type.is_signed(),
    })
}

fn size_function_expression_type(
    argument: &sv_parser::Expression,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
    first_dimension_only: bool,
    dimensions: Option<&PackedDimensions>,
) -> Option<ExprType> {
    if let Some(dimensions) = dimensions
        && dimensions.scope_types_complete
    {
        // IEEE 1800-2023 20.6.2: query the operand's type without evaluating
        // function bodies. Keep the legacy query context's unexpanded calls
        // (including their first return dimension for $size).
        let mut query_dimensions = dimensions.clone();
        query_dimensions.functions = Arc::default();
        if let Some(r#type) = size_function_expression_type_from_dimensions(
            argument,
            syntax_tree,
            const_env,
            first_dimension_only,
            &query_dimensions,
        ) {
            return Some(r#type);
        }
    }
    let mut packed_dimensions = containing_packed_dimensions(
        RefNode::Expression(argument),
        syntax_tree,
        const_env,
        type_aliases,
    )
    .unwrap_or_else(|| PackedDimensions {
        const_env: const_env.clone().into(),
        type_aliases: type_aliases.clone(),
        ..PackedDimensions::default()
    });
    packed_dimensions.function_return_types = containing_function_return_types(
        RefNode::Expression(argument),
        syntax_tree,
        const_env,
        type_aliases,
    );
    if let Some(dimensions) = dimensions {
        packed_dimensions.extend(
            dimensions
                .iter()
                .map(|(name, metadata)| (name.clone(), metadata.clone())),
        );
        packed_dimensions.parameter_values = dimensions.parameter_values.clone();
        packed_dimensions.constant_indexed_base = dimensions.constant_indexed_base;
        packed_dimensions
            .function_return_types
            .extend(dimensions.function_return_types.clone());
    }
    let mut shapes = containing_subroutine_param_shapes(
        RefNode::Expression(argument),
        syntax_tree,
        const_env,
        type_aliases,
    );
    if let Some(dimensions) = dimensions {
        shapes.extend(
            dimensions
                .subroutine_param_shapes
                .iter()
                .map(|(name, shapes)| (name.clone(), shapes.clone())),
        );
    }
    packed_dimensions.subroutine_param_shapes = Arc::new(shapes);
    size_function_expression_type_from_dimensions(
        argument,
        syntax_tree,
        const_env,
        first_dimension_only,
        &packed_dimensions,
    )
}

fn size_function_expression_type_from_dimensions(
    argument: &sv_parser::Expression,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    first_dimension_only: bool,
    packed_dimensions: &PackedDimensions,
) -> Option<ExprType> {
    let expression =
        expr_from_expression_with_types(argument, syntax_tree, packed_dimensions).ok()?;
    let width = if first_dimension_only {
        selected_expression_first_dimension_width(argument, syntax_tree, packed_dimensions).or_else(
            || match &expression {
                // An untyped integral parameter has an implied packed range
                // (IEEE 1800-2023 6.20.2), even without explicit dimensions.
                Expr::Ident(name) => variable_size_function_width(const_env, name, true)
                    .or_else(|| parameter_type_from_const_env(const_env, name).map(|ty| ty.width)),
                Expr::Call { name, args } => {
                    typecheck::bit_vector_function_return_type(name, args.len())
                        .map(|(width, _)| width)
                        .or_else(|| {
                            packed_dimensions
                                .function_return_types
                                .get(name)
                                .and_then(|metadata| metadata.first_packed_dimension_width)
                        })
                }
                _ => expr_static_width(&expression, packed_dimensions),
            },
        )
    } else {
        expr_static_width(&expression, packed_dimensions)
    }?;
    let signed = expr_signedness_with_return_types(
        &expression,
        &SizeQuerySignedness {
            dimensions: packed_dimensions,
            const_env,
        },
        &HashMap::default(),
        &packed_dimensions.function_return_types,
    )?;
    Some(ExprType {
        width: width.max(1),
        signed,
    })
}

/// The same precedence as the former eagerly collected signedness table,
/// looking up only the identifiers used by the query operand.
struct SizeQuerySignedness<'a> {
    dimensions: &'a PackedDimensions,
    const_env: &'a HashMap<String, i128>,
}

impl Signedness for SizeQuerySignedness<'_> {
    fn signedness(&self, name: &str) -> Option<bool> {
        if let Some(variable) = self.dimensions.get(name) {
            return Some(variable.signed);
        }
        if let Some(signed) = self.const_env.get(&variable_signed_marker(name)) {
            return Some(*signed != 0);
        }
        parameter_type_from_const_env(self.const_env, name).map(|ty| ty.signed)
    }
}

fn selected_expression_first_dimension_width(
    argument: &sv_parser::Expression,
    syntax_tree: &SyntaxTree,
    packed_dimensions: &PackedDimensions,
) -> Option<usize> {
    let sv_parser::Expression::Primary(primary) = argument else {
        return None;
    };
    if let sv_parser::Primary::MintypmaxExpression(grouped) = &**primary
        && let sv_parser::MintypmaxExpression::Expression(argument) = &grouped.nodes.0.nodes.1
    {
        return selected_expression_first_dimension_width(argument, syntax_tree, packed_dimensions);
    }
    let sv_parser::Primary::Hierarchical(hierarchical) = &**primary else {
        return None;
    };
    if packed_structs::has_member_access(
        RefNode::HierarchicalIdentifier(&hierarchical.nodes.1),
        RefNode::Select(&hierarchical.nodes.2),
    ) {
        return packed_structs::member_first_dimension_width(
            RefNode::HierarchicalIdentifier(&hierarchical.nodes.1),
            &hierarchical.nodes.2,
            syntax_tree,
            packed_dimensions,
        );
    }
    let name = reference_name(RefNode::PrimaryHierarchical(hierarchical), syntax_tree)?;
    let dimensions = packed_dimensions.get(&name)?;
    let select = &hierarchical.nodes.2;
    if let Some(range) = &select.nodes.2 {
        let (msb, lsb) = part_select_bounds(
            &range.nodes.1,
            syntax_tree,
            Some(&name),
            select.nodes.1.nodes.0.len(),
            packed_dimensions,
        )
        .ok()?;
        return usize::try_from(
            eval_ast_const_expr(&msb, &packed_dimensions.const_env)?
                .abs_diff(eval_ast_const_expr(&lsb, &packed_dimensions.const_env)?),
        )
        .ok()?
        .checked_add(1);
    }
    // Each index removes one declared dimension. Inspect the syntax before
    // flattening, which otherwise loses the remaining array shape.
    let index_count = select.nodes.1.nodes.0.len();
    let width = dimensions
        .unpacked
        .iter()
        .map(|dimension| &dimension.width)
        .chain(dimensions.packed.iter().map(|dimension| &dimension.width))
        .nth(index_count);
    match width {
        Some(width) => {
            usize::try_from(eval_ast_const_expr(width, &packed_dimensions.const_env)?).ok()
        }
        None if index_count == dimensions.unpacked.len() + dimensions.packed.len() => Some(1),
        None => None,
    }
}

fn containing_packed_dimensions(
    target: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Option<PackedDimensions> {
    #[cfg(test)]
    SYNTAX_TYPE_DISCOVERIES.with(|count| count.set(count.get() + 1));
    let (target_start, target_end) = ref_node_source_span(target.clone())?;
    for node in syntax_tree {
        let module = match node {
            RefNode::ModuleDeclarationAnsi(module) => RefNode::ModuleDeclarationAnsi(module),
            RefNode::ModuleDeclarationNonansi(module) => RefNode::ModuleDeclarationNonansi(module),
            RefNode::PackageDeclaration(package) => RefNode::PackageDeclaration(package),
            _ => continue,
        };
        let Some((module_start, module_end)) = ref_node_source_span(module.clone()) else {
            continue;
        };
        if target_start < module_start || target_end > module_end {
            continue;
        }
        let module_span = (module_start, module_end);
        if !ACTIVE_PACKED_DIMENSIONS.with(|active| active.borrow_mut().insert(module_span)) {
            return None;
        }
        // A declaration range can query a size that requires this same
        // module's metadata. Recursive discovery uses the caller's constant
        // and function type environments instead of rebuilding declarations.
        let _guard = ActivePackedDimensionsGuard { module_span };
        let ports =
            ports_from_module_node(module.clone(), syntax_tree, const_env, type_aliases).ok()?;
        let mut signals =
            signals_from_module_node(module.clone(), syntax_tree, const_env, type_aliases).ok()?;
        signals.extend(
            array_parameters::array_parameters_from_module_node(
                module.clone(),
                syntax_tree,
                const_env,
                type_aliases,
            )
            .ok()?
            .into_iter()
            .map(|parameter| parameter.signal),
        );
        let imported = scope::imported();
        let mut dimensions = packed_dimensions_from_ports_and_signals(
            &[],
            &imported.signals,
            const_env,
            type_aliases,
        );
        let declared =
            packed_dimensions_from_ports_and_signals(&ports, &signals, const_env, type_aliases);
        dimensions.extend(declared.iter().map(|(name, ty)| (name.clone(), ty.clone())));
        if let Some(locals) =
            containing_function_dimensions(target.clone(), syntax_tree, const_env, type_aliases)
        {
            dimensions.extend(locals);
        }
        return Some(dimensions);
    }
    None
}

fn containing_function_dimensions(
    target: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Option<HashMap<String, VariableDimensions>> {
    let (target_start, target_end) = ref_node_source_span(target)?;
    let mut scopes = syntax_tree
        .into_iter()
        .filter_map(|node| {
            if !matches!(
                node,
                RefNode::FunctionDeclaration(_)
                    | RefNode::TaskDeclaration(_)
                    | RefNode::SeqBlock(_)
            ) {
                return None;
            }
            let (start, end) = ref_node_source_span(node.clone())?;
            (start <= target_start && target_end <= end).then_some(((start, end), node))
        })
        .collect::<Vec<_>>();
    // Only ancestors contribute names; inner declarations shadow outer ones.
    scopes.sort_by_key(|((start, end), _)| std::cmp::Reverse(end - start));
    let function_span = scopes.first()?.0;
    if !ACTIVE_FUNCTION_DIMENSIONS.with(|active| active.borrow_mut().insert(function_span)) {
        return None;
    }
    let _guard = ActiveFunctionDimensionsGuard { function_span };
    let mut dimensions = HashMap::default();
    for (_, scope) in scopes {
        let locals = match scope {
            RefNode::SeqBlock(block) => function_local_packed_dimensions_from_block_items(
                &block.nodes.2,
                syntax_tree,
                const_env,
                type_aliases,
            ),
            node => procedural::subroutine_declared_dimensions(
                node,
                syntax_tree,
                const_env,
                type_aliases,
            ),
        }?;
        dimensions.extend(locals);
    }
    Some(dimensions)
}

fn containing_function_return_types(
    target: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> HashMap<String, FunctionReturnMetadata> {
    #[cfg(test)]
    SYNTAX_TYPE_DISCOVERIES.with(|count| count.set(count.get() + 1));
    let Some((target_start, target_end)) = ref_node_source_span(target) else {
        return HashMap::default();
    };
    for node in syntax_tree {
        let module = match node {
            RefNode::ModuleDeclarationAnsi(module) => RefNode::ModuleDeclarationAnsi(module),
            RefNode::ModuleDeclarationNonansi(module) => RefNode::ModuleDeclarationNonansi(module),
            RefNode::PackageDeclaration(package) => RefNode::PackageDeclaration(package),
            _ => continue,
        };
        let Some((module_start, module_end)) = ref_node_source_span(module.clone()) else {
            continue;
        };
        if target_start < module_start || target_end > module_end {
            continue;
        }
        // Only declarations in the target's lexical ancestors are visible.
        // A syntax-wide scan would let inactive or sibling generate functions
        // replace a module function before generate elaboration has even run.
        let generate_scopes: Vec<_> = module
            .clone()
            .into_iter()
            .filter_map(|node| {
                let RefNode::GenerateBlock(block) = node else {
                    return None;
                };
                ref_node_source_span(RefNode::GenerateBlock(block))
            })
            .collect();
        let contains = |(start, end): (usize, usize), (inner_start, inner_end): (usize, usize)| {
            start <= inner_start && inner_end <= end
        };
        let target_span = (target_start, target_end);
        let scope_span = generate_scopes
            .iter()
            .copied()
            .filter(|span| contains(*span, target_span))
            .min_by_key(|(start, end)| end - start)
            .unwrap_or((module_start, module_end));
        if let Some(active) = ACTIVE_FUNCTION_RETURN_METADATA
            .with(|metadata| metadata.borrow().get(&scope_span).cloned())
        {
            return active;
        }
        ACTIVE_FUNCTION_RETURN_METADATA.with(|metadata| {
            metadata.borrow_mut().insert(scope_span, HashMap::default());
        });
        let _guard = ActiveFunctionReturnMetadataGuard { scope_span };
        let mut declarations = module
            .into_iter()
            .filter_map(|child| {
                let RefNode::FunctionDeclaration(declaration) = child else {
                    return None;
                };
                let declaration_span =
                    ref_node_source_span(RefNode::FunctionDeclaration(declaration))?;
                let ancestors: Vec<_> = generate_scopes
                    .iter()
                    .filter(|span| contains(**span, declaration_span))
                    .collect();
                if ancestors.iter().any(|span| !contains(**span, target_span)) {
                    return None;
                }
                Some((ancestors.len(), declaration))
            })
            .collect::<Vec<_>>();
        // Populate outer declarations first so inner declarations shadow them
        // regardless of their relative order in the source text.
        declarations.sort_by_key(|(depth, _)| *depth);
        let mut result = scope::imported().function_return_types.clone();
        // A return range may depend on a function declared later in the
        // module. Revisit declarations after publishing each partial pass;
        // recursive discovery reads that partial map instead of recursing.
        for _ in 0..=declarations.len() {
            for (_, declaration) in &declarations {
                if let Some((name, metadata)) = function_declaration_return_metadata(
                    declaration,
                    syntax_tree,
                    const_env,
                    type_aliases,
                ) {
                    result.insert(name, metadata);
                }
            }
            ACTIVE_FUNCTION_RETURN_METADATA.with(|active| {
                active.borrow_mut().insert(scope_span, result.clone());
            });
        }
        return result;
    }
    scope::imported().function_return_types.clone()
}

/// Return types alone do not provide the context for an untyped argument pattern.
fn containing_subroutine_param_shapes(
    target: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> HashMap<String, Vec<VariableDimensions>> {
    let mut shapes = scope::imported().subroutine_shapes.clone();
    let Some(target_span) = ref_node_source_span(target) else {
        return shapes;
    };
    let contains = |(start, end): (usize, usize), (inner_start, inner_end): (usize, usize)| {
        start <= inner_start && inner_end <= end
    };
    for node in syntax_tree {
        if !matches!(
            node,
            RefNode::ModuleDeclarationAnsi(_)
                | RefNode::ModuleDeclarationNonansi(_)
                | RefNode::PackageDeclaration(_)
        ) {
            continue;
        }
        let Some(module_span) = ref_node_source_span(node.clone()) else {
            continue;
        };
        if !contains(module_span, target_span) {
            continue;
        }
        // Formal bounds can contain another type query. Do not recursively
        // reconstruct this same declaration scope while resolving those bounds.
        if !ACTIVE_PACKED_DIMENSIONS.with(|active| active.borrow_mut().insert(module_span)) {
            return shapes;
        }
        let _guard = ActivePackedDimensionsGuard { module_span };
        let generates = node
            .clone()
            .into_iter()
            .filter_map(|child| match child {
                RefNode::GenerateBlock(_) => ref_node_source_span(child),
                _ => None,
            })
            .collect::<Vec<_>>();
        let mut declarations = node
            .into_iter()
            .filter_map(|child| {
                if !matches!(
                    child,
                    RefNode::FunctionDeclaration(_) | RefNode::TaskDeclaration(_)
                ) {
                    return None;
                }
                let span = ref_node_source_span(child.clone())?;
                let ancestors = generates
                    .iter()
                    .filter(|scope| contains(**scope, span))
                    .collect::<Vec<_>>();
                if ancestors
                    .iter()
                    .any(|scope| !contains(**scope, target_span))
                {
                    return None;
                }
                Some((ancestors.len(), child))
            })
            .collect::<Vec<_>>();
        declarations.sort_by_key(|(depth, _)| *depth);
        for (_, declaration) in declarations {
            if let Some((name, params)) = procedural::subroutine_declared_parameter_shapes(
                declaration,
                syntax_tree,
                const_env,
                type_aliases,
            ) {
                shapes.insert(name, params);
            }
        }
        break;
    }
    shapes
}

fn ref_node_source_span(node: RefNode<'_>) -> Option<(usize, usize)> {
    let mut start = None;
    let mut end = None;
    for child in node {
        let RefNode::Locate(locate) = child else {
            continue;
        };
        start.get_or_insert(locate.offset);
        end = Some(locate.offset.checked_add(locate.len)?);
    }
    start.zip(end)
}

fn function_declaration_return_metadata(
    declaration: &sv_parser::FunctionDeclaration,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Option<(String, FunctionReturnMetadata)> {
    let (return_node, identifier) = match &declaration.nodes.2 {
        sv_parser::FunctionBodyDeclaration::WithPort(body) => (&body.nodes.0, &body.nodes.2),
        sv_parser::FunctionBodyDeclaration::WithoutPort(body) => (&body.nodes.0, &body.nodes.2),
    };
    let name = identifier_text(RefNode::FunctionIdentifier(identifier), syntax_tree)?;
    let return_type = function_return_type(return_node, syntax_tree, const_env, type_aliases);
    Some((
        name,
        FunctionReturnMetadata {
            width: return_type.map(|r#type| r#type.width),
            dimensions: function_return_dimensions(
                return_node,
                syntax_tree,
                const_env,
                type_aliases,
            ),
            first_packed_dimension_width: function_return_first_packed_dimension_width(
                return_node,
                syntax_tree,
                const_env,
                type_aliases,
                return_type,
            ),
            signed: return_type.is_some_and(|r#type| r#type.signed),
            is_2state: function_return_is_2state(return_node, syntax_tree, type_aliases),
        },
    ))
}

pub(super) fn packed_dimensions_from_ports_and_signals(
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
                packed: signal_packed_dimension_widths(port.r#type().packed_ranges()),
                unpacked: unpacked_dimension_widths(port.r#type().unpacked_ranges()),
                signed: port.r#type().is_signed(),
                is_2state: port.r#type().kind() == TypeKind::Bit,
                members: port.r#type().members.clone(),
                signed_element_depth: port.r#type().signed_element_depth,
            },
        );
    }
    for signal in signals {
        dimensions.insert(
            signal.name().to_string(),
            VariableDimensions {
                packed: signal_packed_dimension_widths(signal.r#type().packed_ranges()),
                unpacked: unpacked_dimension_widths(signal.r#type().unpacked_ranges()),
                signed: signal.r#type().is_signed(),
                is_2state: signal.r#type().kind() == TypeKind::Bit,
                members: signal.r#type().members.clone(),
                signed_element_depth: signal.r#type().signed_element_depth,
            },
        );
    }
    PackedDimensions::new(dimensions, const_env, type_aliases)
}

pub(super) fn signal_packed_dimension_widths(ranges: &[PackedRange]) -> Vec<PackedDimension> {
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

pub(super) fn unpacked_dimension_widths(ranges: &[UnpackedRange]) -> Vec<UnpackedDimension> {
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

pub(super) fn function_packed_dimension_widths(ranges: &[PackedRange]) -> Vec<PackedDimension> {
    signal_packed_dimension_widths(ranges)
        .into_iter()
        .map(|mut dimension| {
            dimension.normalize_single = true;
            dimension
        })
        .collect()
}

pub(super) fn function_param_packed_dimensions(
    data_type: &sv_parser::DataTypeOrImplicit,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> Vec<PackedDimension> {
    function_type_from_ref_node(
        RefNode::DataTypeOrImplicit(data_type),
        syntax_tree,
        const_env,
        type_aliases,
    )
    .map(|r#type| function_packed_dimension_widths(r#type.packed_ranges()))
    .unwrap_or_default()
}

pub(super) fn parameter_marker(name: &str) -> String {
    format!("__parameter::{name}")
}

pub(super) fn local_parameter_marker(name: &str) -> String {
    format!("__parameter::local::{name}")
}

pub(super) fn enum_marker(name: &str) -> String {
    format!("__enum::{name}")
}

pub(super) fn parameter_width_marker(name: &str) -> String {
    format!("__parameter::width::{name}")
}

pub(super) fn parameter_signed_marker(name: &str) -> String {
    format!("__parameter::signed::{name}")
}

pub(super) fn parameter_rank_marker(name: &str) -> String {
    format!("__parameter::rank::{name}")
}

pub(super) fn parameter_dimensions_marker(name: &str) -> String {
    format!("__parameter::dimensions::{name}")
}

pub(super) fn parameter_dimension_marker(name: &str, index: usize, bound: &str) -> String {
    format!("__parameter::dimension::{index}::{bound}::{name}")
}

pub(super) fn parameter_signed_element_marker(name: &str) -> String {
    format!("__parameter::signed_element::{name}")
}

pub(super) fn variable_bits_marker(name: &str) -> String {
    format!("__variable::bits::{name}")
}

pub(super) fn variable_size_marker(name: &str) -> String {
    format!("__variable::size::{name}")
}

pub(super) fn variable_dimensions_marker(name: &str) -> String {
    format!("__variable::dimensions::{name}")
}

pub(super) fn variable_signed_marker(name: &str) -> String {
    format!("__variable::signed::{name}")
}

pub(super) fn variable_size_function_width(
    const_env: &HashMap<String, i128>,
    name: &str,
    first_dimension_only: bool,
) -> Option<usize> {
    let marker = if first_dimension_only {
        variable_size_marker(name)
    } else {
        variable_bits_marker(name)
    };
    usize::try_from(*const_env.get(&marker)?).ok()
}

fn variable_type_is_signed(const_env: &HashMap<String, i128>, name: &str) -> bool {
    const_env
        .get(&variable_signed_marker(name))
        .is_some_and(|signed| *signed != 0)
}

pub(super) fn extend_const_env_with_variable_types<'a>(
    const_env: &mut HashMap<String, i128>,
    variables: impl Iterator<Item = (&'a str, &'a Type)>,
) {
    for (name, r#type) in variables {
        const_env.insert(
            variable_dimensions_marker(name),
            (r#type.unpacked_ranges().len() + r#type.packed_ranges().len()) as i128,
        );
        let dimension_width = |range: &PackedRange| {
            let left = eval_ast_const_expr(range.left(), const_env)?;
            let right = eval_ast_const_expr(range.right(), const_env)?;
            usize::try_from(left.abs_diff(right)).ok()?.checked_add(1)
        };
        let unpacked_widths = r#type
            .unpacked_ranges()
            .iter()
            .map(|range| {
                let left = eval_ast_const_expr(range.left(), const_env)?;
                let right = eval_ast_const_expr(range.right(), const_env)?;
                usize::try_from(left.abs_diff(right)).ok()?.checked_add(1)
            })
            .collect::<Option<Vec<_>>>();
        let packed_widths = r#type
            .packed_ranges()
            .iter()
            .map(dimension_width)
            .collect::<Option<Vec<_>>>();
        let (Some(unpacked_widths), Some(packed_widths)) = (unpacked_widths, packed_widths) else {
            continue;
        };
        let bits = unpacked_widths
            .iter()
            .chain(&packed_widths)
            .try_fold(1usize, |width, dimension| width.checked_mul(*dimension));
        let size = unpacked_widths
            .first()
            .or_else(|| packed_widths.first())
            .copied()
            .unwrap_or(1);
        if let Some(bits) = bits.and_then(|bits| i128::try_from(bits).ok()) {
            const_env.insert(variable_bits_marker(name), bits);
        }
        if let Ok(size) = i128::try_from(size) {
            const_env.insert(variable_size_marker(name), size);
        }
        const_env.insert(variable_signed_marker(name), r#type.is_signed() as i128);
    }
}

pub(super) fn insert_parameter_type_markers(
    const_env: &mut HashMap<String, i128>,
    name: &str,
    r#type: ExprType,
) {
    if let Ok(width) = i128::try_from(r#type.width) {
        const_env.insert(parameter_width_marker(name), width);
        const_env.insert(parameter_signed_marker(name), r#type.signed as i128);
    }
}

pub(super) fn parameter_types_from_const_env(
    const_env: &HashMap<String, i128>,
) -> HashMap<String, ExprType> {
    #[cfg(test)]
    PARAMETER_TYPE_SCAN_ENTRIES.with(|count| count.set(count.get() + const_env.len()));
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

/// Read one parameter's type without reconstructing the whole environment.
pub(super) fn parameter_type_from_const_env(
    const_env: &HashMap<String, i128>,
    name: &str,
) -> Option<ExprType> {
    let width = usize::try_from(*const_env.get(&parameter_width_marker(name))?).ok()?;
    let signed = const_env
        .get(&parameter_signed_marker(name))
        .is_some_and(|signed| *signed != 0);
    Some(ExprType { width, signed })
}

thread_local! {
    static ACTIVE_FUNCTION_DIMENSIONS: RefCell<HashSet<(usize, usize)>> =
        RefCell::new(HashSet::default());
    static ACTIVE_PACKED_DIMENSIONS: RefCell<HashSet<(usize, usize)>> =
        RefCell::new(HashSet::default());
    static ACTIVE_FUNCTION_RETURN_METADATA:
        RefCell<HashMap<(usize, usize), HashMap<String, FunctionReturnMetadata>>> =
        RefCell::new(HashMap::default());
}

#[cfg(test)]
thread_local! {
    static SYNTAX_TYPE_DISCOVERIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static PARAMETER_TYPE_SCAN_ENTRIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
mod tests;

struct ActiveFunctionDimensionsGuard {
    function_span: (usize, usize),
}

impl Drop for ActiveFunctionDimensionsGuard {
    fn drop(&mut self) {
        ACTIVE_FUNCTION_DIMENSIONS.with(|active| {
            active.borrow_mut().remove(&self.function_span);
        });
    }
}

struct ActivePackedDimensionsGuard {
    module_span: (usize, usize),
}

impl Drop for ActivePackedDimensionsGuard {
    fn drop(&mut self) {
        ACTIVE_PACKED_DIMENSIONS.with(|active| {
            active.borrow_mut().remove(&self.module_span);
        });
    }
}

struct ActiveFunctionReturnMetadataGuard {
    scope_span: (usize, usize),
}

impl Drop for ActiveFunctionReturnMetadataGuard {
    fn drop(&mut self) {
        ACTIVE_FUNCTION_RETURN_METADATA.with(|metadata| {
            metadata.borrow_mut().remove(&self.scope_span);
        });
    }
}

pub(super) fn parameter_packed_dimensions(parameters: &[Parameter]) -> VariablePackedDimensions {
    parameters
        .iter()
        .filter(|parameter| !parameter.packed_ranges.is_empty())
        .map(|parameter| {
            (
                parameter.name.clone(),
                VariableDimensions {
                    packed: function_packed_dimension_widths(&parameter.packed_ranges),
                    unpacked: Vec::new(),
                    signed: parameter.declared_signed.unwrap_or(false),
                    is_2state: parameter.declared_is_2state,
                    members: Vec::new(),
                    signed_element_depth: parameter.signed_element_depth,
                },
            )
        })
        .collect()
}
