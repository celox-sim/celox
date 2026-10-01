//! Packed/unpacked dimension metadata and size-query type discovery.

use super::*;

pub(super) fn size_system_function_expr_type(
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
                _ => type_from_ref_node_with_env(
                    RefNode::DataType(data_type),
                    syntax_tree,
                    const_env,
                    type_aliases,
                ),
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
            if name != "$bits" && name != "$size" {
                return None;
            }
            // Function formals and locals shadow generated signal type markers.
            if let Some(dimensions) = containing_function_dimensions(
                RefNode::Expression(argument),
                syntax_tree,
                const_env,
                type_aliases,
            ) && let Some(ConstExpr::Ident(identifier)) =
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
            if let Some(ConstExpr::Ident(identifier)) = const_expr_from_expr(argument, syntax_tree)
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
            ) {
                return Some(r#type);
            }
            let argument = const_expr_from_expr(argument, syntax_tree)?;
            if let ConstExpr::Ident(alias) = &argument
                && let Some(r#type) = type_aliases.get(alias)
            {
                (name, r#type.clone())
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

fn size_function_expression_type(
    argument: &sv_parser::Expression,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
    first_dimension_only: bool,
) -> Option<ExprType> {
    let mut packed_dimensions = containing_packed_dimensions(
        RefNode::Expression(argument),
        syntax_tree,
        const_env,
        type_aliases,
    )
    .unwrap_or_else(|| PackedDimensions {
        const_env: const_env.clone(),
        type_aliases: type_aliases.clone(),
        ..PackedDimensions::default()
    });
    packed_dimensions.function_return_types = containing_function_return_types(
        RefNode::Expression(argument),
        syntax_tree,
        const_env,
        type_aliases,
    );
    let expression = expr_from_expression_with_types(argument, syntax_tree, &packed_dimensions)?;
    let width = if first_dimension_only {
        selected_expression_first_dimension_width(argument, syntax_tree, &packed_dimensions)
            .or_else(|| match &expression {
                Expr::Ident(name) => variable_size_function_width(const_env, name, true),
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
                _ => expr_static_width(&expression, &packed_dimensions),
            })
    } else {
        expr_static_width(&expression, &packed_dimensions)
    }?;
    let mut identifier_signedness = parameter_types_from_const_env(const_env)
        .into_iter()
        .map(|(name, r#type)| (name, r#type.signed))
        .collect::<HashMap<_, _>>();
    const VARIABLE_SIGNED_PREFIX: &str = "__variable::signed::";
    identifier_signedness.extend(const_env.iter().filter_map(|(marker, signed)| {
        marker
            .strip_prefix(VARIABLE_SIGNED_PREFIX)
            .map(|name| (name.to_string(), *signed != 0))
    }));
    identifier_signedness.extend(
        packed_dimensions
            .iter()
            .map(|(name, dimensions)| (name.clone(), dimensions.signed)),
    );
    let signed = expr_signedness_with_return_types(
        &expression,
        &identifier_signedness,
        &HashMap::default(),
        &packed_dimensions.function_return_types,
    )?;
    Some(ExprType {
        width: width.max(1),
        signed,
    })
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
    let name = identifier_text(
        RefNode::HierarchicalIdentifier(&hierarchical.nodes.1),
        syntax_tree,
    )?;
    let dimensions = packed_dimensions.get(&name)?;
    let select = &hierarchical.nodes.2;
    if let Some(range) = &select.nodes.2 {
        let (msb, lsb) = part_select_bounds(
            &range.nodes.1,
            syntax_tree,
            Some(&name),
            select.nodes.1.nodes.0.len(),
            packed_dimensions,
        )?;
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
    let (target_start, target_end) = ref_node_source_span(target.clone())?;
    for node in syntax_tree {
        let module = match node {
            RefNode::ModuleDeclarationAnsi(module) => RefNode::ModuleDeclarationAnsi(module),
            RefNode::ModuleDeclarationNonansi(module) => RefNode::ModuleDeclarationNonansi(module),
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
        let signals =
            signals_from_module_node(module.clone(), syntax_tree, const_env, type_aliases).ok()?;
        let mut dimensions =
            packed_dimensions_from_ports_and_signals(&ports, &signals, const_env, type_aliases);
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
    for child in syntax_tree {
        let RefNode::FunctionDeclaration(declaration) = child else {
            continue;
        };
        let (start, end) = ref_node_source_span(RefNode::FunctionDeclaration(declaration))?;
        if target_start < start || target_end > end {
            continue;
        }
        let function_span = (start, end);
        if !ACTIVE_FUNCTION_DIMENSIONS.with(|active| active.borrow_mut().insert(function_span)) {
            return None;
        }
        let _guard = ActiveFunctionDimensionsGuard { function_span };
        let mut dimensions = HashMap::default();
        let (params, locals) = match &declaration.nodes.2 {
            sv_parser::FunctionBodyDeclaration::WithPort(body) => {
                let params = body
                    .nodes
                    .3
                    .nodes
                    .1
                    .as_ref()
                    .map(|ports| tf_params(ports, syntax_tree, const_env, type_aliases))
                    .unwrap_or_default();
                let locals = function_local_packed_dimensions_from_block_items(
                    &body.nodes.5,
                    syntax_tree,
                    const_env,
                    type_aliases,
                )?;
                (params, locals)
            }
            sv_parser::FunctionBodyDeclaration::WithoutPort(body) => {
                let params = tf_item_params(&body.nodes.4, syntax_tree, const_env, type_aliases);
                let items = body.nodes.4.iter().filter_map(|item| match item {
                    sv_parser::TfItemDeclaration::BlockItemDeclaration(item) => Some(&**item),
                    sv_parser::TfItemDeclaration::TfPortDeclaration(_) => None,
                });
                let locals = function_local_packed_dimensions_from_block_item_iter(
                    items,
                    syntax_tree,
                    const_env,
                    type_aliases,
                )?;
                (params, locals)
            }
        };
        dimensions.extend(params.into_iter().map(|param| {
            (
                param.name,
                VariableDimensions {
                    packed: param.packed_dimensions,
                    unpacked: Vec::new(),
                    signed: param.signed,
                    is_2state: param.is_2state,
                    members: Vec::new(),
                },
            )
        }));
        dimensions.extend(locals);
        return Some(dimensions);
    }
    None
}

fn containing_function_return_types(
    target: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> HashMap<String, FunctionReturnMetadata> {
    let Some((target_start, target_end)) = ref_node_source_span(target) else {
        return HashMap::default();
    };
    for node in syntax_tree {
        let module = match node {
            RefNode::ModuleDeclarationAnsi(module) => RefNode::ModuleDeclarationAnsi(module),
            RefNode::ModuleDeclarationNonansi(module) => RefNode::ModuleDeclarationNonansi(module),
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
        let mut result = HashMap::default();
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
    HashMap::default()
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
                packed: packed_dimension_widths(port.r#type().packed_ranges()),
                unpacked: unpacked_dimension_widths(port.r#type().unpacked_ranges()),
                signed: port.r#type().is_signed(),
                is_2state: port.r#type().kind() == TypeKind::Bit,
                members: port.r#type().members.clone(),
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
                members: signal.r#type().members.clone(),
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
    packed_dimension_widths(ranges)
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

pub(super) fn variable_bits_marker(name: &str) -> String {
    format!("__variable::bits::{name}")
}

pub(super) fn variable_size_marker(name: &str) -> String {
    format!("__variable::size::{name}")
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

thread_local! {
    static ACTIVE_FUNCTION_DIMENSIONS: RefCell<HashSet<(usize, usize)>> =
        RefCell::new(HashSet::default());
    static ACTIVE_PACKED_DIMENSIONS: RefCell<HashSet<(usize, usize)>> =
        RefCell::new(HashSet::default());
    static ACTIVE_FUNCTION_RETURN_METADATA:
        RefCell<HashMap<(usize, usize), HashMap<String, FunctionReturnMetadata>>> =
        RefCell::new(HashMap::default());
}

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
                },
            )
        })
        .collect()
}
