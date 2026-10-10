//! Unsupported-construct checks and bounded procedural-loop validation.

use super::*;

pub(super) fn reject_unsupported_multidimensional_packed_bounds(
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

/// How an `always` construct is simulated.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum AlwaysKind {
    Comb,
    Ff,
    Unsupported,
}

/// `always_comb` and `always @*` are combinational; `always_ff` and an
/// `always` sensitive to clock edges are sequential. Other sensitivity lists
/// (incomplete level-sensitive lists, `always_latch`) are not supported.
pub(super) fn always_kind(always: &sv_parser::AlwaysConstruct) -> AlwaysKind {
    match always.nodes.0 {
        sv_parser::AlwaysKeyword::AlwaysComb(_) => AlwaysKind::Comb,
        sv_parser::AlwaysKeyword::AlwaysFf(_) => AlwaysKind::Ff,
        sv_parser::AlwaysKeyword::AlwaysLatch(_) => AlwaysKind::Unsupported,
        sv_parser::AlwaysKeyword::Always(_) => {
            let sv_parser::StatementItem::ProceduralTimingControlStatement(timing) =
                &always.nodes.1.nodes.2
            else {
                return AlwaysKind::Unsupported;
            };
            let sv_parser::ProceduralTimingControl::EventControl(control) = &timing.nodes.0 else {
                return AlwaysKind::Unsupported;
            };
            match &**control {
                sv_parser::EventControl::Asterisk(_)
                | sv_parser::EventControl::ParenAsterisk(_) => AlwaysKind::Comb,
                sv_parser::EventControl::EventExpression(_)
                    if RefNode::ProceduralTimingControl(&timing.nodes.0)
                        .into_iter()
                        .any(|node| matches!(node, RefNode::EdgeIdentifier(_))) =>
                {
                    AlwaysKind::Ff
                }
                _ => AlwaysKind::Unsupported,
            }
        }
    }
}

/// The statement an combinational `always` evaluates, without its event control.
pub(super) fn always_comb_body(
    always: &sv_parser::AlwaysConstruct,
) -> Option<&sv_parser::Statement> {
    match always.nodes.0 {
        sv_parser::AlwaysKeyword::AlwaysComb(_) => Some(&always.nodes.1),
        sv_parser::AlwaysKeyword::Always(_) if always_kind(always) == AlwaysKind::Comb => {
            let sv_parser::StatementItem::ProceduralTimingControlStatement(timing) =
                &always.nodes.1.nodes.2
            else {
                return None;
            };
            match &timing.nodes.1 {
                sv_parser::StatementOrNull::Statement(statement) => Some(statement),
                _ => None,
            }
        }
        _ => None,
    }
}

pub(super) fn reject_silently_ignored_constructs(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
    parameter_dimensions: &ScopedMap<VariableDimensions>,
    parameter_values: &HashMap<String, Expr>,
) -> Result<(), AnalyzerError> {
    let mut indexed_dimensions =
        PackedDimensions::new(parameter_dimensions.clone(), const_env, type_aliases);
    indexed_dimensions.parameter_values = parameter_values.clone().into();
    reject_silently_ignored_constructs_with_dimensions(node, syntax_tree, &indexed_dimensions)
}

/// Walk each syntax node once, excluding inactive/unelaborated generate bodies.
/// Generate items are validated separately with their own lexical environment.
fn validation_nodes(node: RefNode<'_>, is_module: bool) -> impl Iterator<Item = RefNode<'_>> {
    let mut generate_depth = 0usize;
    node.into_iter().event().filter_map(move |event| {
        let (entering, child) = match event {
            sv_parser::NodeEvent::Enter(child) => (true, child),
            sv_parser::NodeEvent::Leave(child) => (false, child),
        };
        if is_module
            && matches!(
                child,
                RefNode::ConditionalGenerateConstruct(_) | RefNode::LoopGenerateConstruct(_)
            )
        {
            if entering {
                generate_depth += 1;
            } else {
                generate_depth -= 1;
            }
            return None;
        }
        (entering && generate_depth == 0).then_some(child)
    })
}

fn reject_silently_ignored_constructs_with_dimensions(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    indexed_dimensions: &PackedDimensions,
) -> Result<(), AnalyzerError> {
    let const_env = &indexed_dimensions.const_env;
    let type_aliases = &indexed_dimensions.type_aliases;
    let is_module = matches!(node, RefNode::ModuleDeclarationAnsi(_));
    // These are the constant contexts that use selection-aware lowering.
    // Other contexts (such as declaration ranges) still use the lightweight
    // constant parser and must reject indexed selections rather than drop them.
    let lowered_constant_indexed_ranges: Vec<_> = validation_nodes(node.clone(), is_module)
        .filter_map(|child| match child {
            RefNode::IndexedRange(_)
            | RefNode::Select(_)
            | RefNode::PartSelectRange(_)
            | RefNode::NetLvalue(_)
            | RefNode::VariableLvalue(_) => Some(child),
            RefNode::ParamAssignment(parameter) => parameter
                .nodes
                .2
                .as_ref()
                .map(|(_, expression)| RefNode::ConstantParamExpression(expression)),
            _ => None,
        })
        .flat_map(|root| root.into_iter())
        .filter_map(|child| match child {
            RefNode::ConstantIndexedRange(range) => Some(range),
            _ => None,
        })
        .collect();
    // Typed indexed lowering validates casts in bases, widths and supported
    // parameter initializers. Keep the generic cast checks for other contexts.
    let typed_indexed_casts: Vec<_> = validation_nodes(node.clone(), is_module)
        .filter_map(|child| match child {
            RefNode::IndexedRange(_) | RefNode::ConstantIndexedRange(_) => Some(child),
            RefNode::ParamAssignment(parameter) => parameter
                .nodes
                .2
                .as_ref()
                .filter(|(_, expression)| {
                    expression.into_iter().any(|child| {
                        matches!(
                            child,
                            RefNode::IndexedRange(_) | RefNode::ConstantIndexedRange(_)
                        )
                    })
                })
                .map(|(_, expression)| RefNode::ConstantParamExpression(expression)),
            _ => None,
        })
        .flat_map(|root| root.into_iter())
        .filter(|child| matches!(child, RefNode::Cast(_) | RefNode::ConstantCast(_)))
        .collect();
    let subroutine_casts: Vec<_> = validation_nodes(node.clone(), is_module)
        .filter(|child| {
            matches!(
                child,
                RefNode::FunctionDeclaration(_) | RefNode::TaskDeclaration(_)
            )
        })
        .flat_map(|root| root.into_iter())
        .filter(|child| matches!(child, RefNode::Cast(_)))
        .collect();
    for child in validation_nodes(node.clone(), is_module) {
        // A parameter initializer with an indexed select is lowered through
        // the typed path; report why it cannot be.
        if let RefNode::ParamAssignment(parameter) = &child
            && let Some((_, expression)) = parameter.nodes.2.as_ref()
            && expression.into_iter().any(|child| {
                matches!(
                    child,
                    RefNode::ConstantIndexedRange(_) | RefNode::IndexedRange(_)
                )
            })
        {
            selects::indexed_parameter_initializer(
                expression,
                syntax_tree,
                indexed_dimensions,
                None,
            )?;
        }
        match child {
            RefNode::SystemTfCall(call)
                if system_tf_call_parts(call, syntax_tree)
                    .is_some_and(|(name, _)| name == "$unpacked_dimensions") =>
            {
                dimensions::unpacked_dimensions_call_value(
                    call, syntax_tree, const_env, type_aliases, Some(indexed_dimensions),
                ).ok_or_else(|| unsupported("operand of `$unpacked_dimensions`"))?;
            }
            // Subroutine bodies may cast to the width of a local parameter;
            // their lowering rejects the casts it cannot express.
            RefNode::Cast(cast)
                if !typed_indexed_casts.contains(&RefNode::Cast(cast))
                    && !subroutine_casts.contains(&RefNode::Cast(cast))
                    && !cast_is_supported(cast, syntax_tree, const_env, type_aliases) =>
            {
                return Err(AnalyzerError::Unsupported("cast expression".to_string()));
            }
            RefNode::ConstantCast(cast)
                if !typed_indexed_casts.contains(&RefNode::ConstantCast(cast))
                    && !constant_cast_is_supported(cast, syntax_tree, const_env, type_aliases) =>
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
                if always_kind(always) == AlwaysKind::Unsupported {
                    return Err(AnalyzerError::Unsupported(
                        "always and always_latch processes".to_string(),
                    ));
                }
                let body = RefNode::Statement(&always.nodes.1);
                if always_kind(always) == AlwaysKind::Comb
                    && body
                        .clone()
                        .into_iter()
                        .any(|node| matches!(node, RefNode::NonblockingAssignment(_)))
                {
                    return Err(AnalyzerError::Unsupported(
                        "nonblocking assignment inside always_comb".to_string(),
                    ));
                }
                if always_kind(always) == AlwaysKind::Ff
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
            }
            RefNode::NetDeclAssignment(assignment)
                if assignment.nodes.2.is_some() && !assignment.nodes.1.is_empty() =>
            {
                return Err(AnalyzerError::Unsupported(
                    "net declaration assignment".to_string(),
                ));
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
            RefNode::IndexedRange(range) => {
                indexed_select_base(
                    RefNode::Expression(&range.nodes.0),
                    syntax_tree,
                    indexed_dimensions,
                )?;
                check_indexed_width(&range.nodes.2, syntax_tree, indexed_dimensions)?;
            }
            RefNode::ConstantIndexedRange(range) => {
                if !lowered_constant_indexed_ranges.contains(&range) {
                    return Err(AnalyzerError::Unsupported("indexed part-select".to_string()));
                }
                let base = indexed_select_base(
                    RefNode::ConstantExpression(&range.nodes.0),
                    syntax_tree,
                    indexed_dimensions,
                )?;
                if eval_ast_const_expr(&base, const_env).is_none() {
                    return Err(AnalyzerError::Unsupported(
                        "indexed part-select start that is not constant".to_string(),
                    ));
                }
                check_indexed_width(&range.nodes.2, syntax_tree, indexed_dimensions)?;
            }
            RefNode::DataTypeStructUnion(data)
                if packed_structs::parse_type(data, syntax_tree, const_env, type_aliases).is_none() => {
                return Err(AnalyzerError::Unsupported(
                    "unpacked struct, union, or unsupported packed struct member".to_string(),
                ));
            }
            RefNode::ConstantFunctionCall(call)
                if matches!(
                    &call.nodes.0.nodes.0,
                    sv_parser::SubroutineCall::TfCall(call) if call.nodes.2.is_some()
                        && !reference_name(RefNode::PsOrHierarchicalTfIdentifier(&call.nodes.0), syntax_tree)
                            .is_some_and(|name| const_functions::is_constant_function(&name))
                ) =>
            {
                // A function whose body could not be converted is not a
                // constant function; report why.
                if let sv_parser::SubroutineCall::TfCall(call) = &call.nodes.0.nodes.0
                    && let Some(error) = reference_name(RefNode::PsOrHierarchicalTfIdentifier(&call.nodes.0),
                        syntax_tree,
                    )
                    .and_then(|name| const_functions::conversion_error(&name))
                {
                    return Err(error);
                }
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
                    .any(non_input_function_port) =>
            {
                return Err(AnalyzerError::Unsupported(
                    "ref function argument".to_string(),
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
            RefNode::PackedDimensionRange(range) => {
                let range = &range.nodes.0.nodes.1;
                for bound in [&range.nodes.0, &range.nodes.2] {
                    if const_expr_from_ref_node_with_env(
                        RefNode::ConstantExpression(bound),
                        syntax_tree,
                        const_env,
                        type_aliases,
                    )?
                    .is_none()
                    {
                        return Err(AnalyzerError::Unsupported(
                            "unsupported packed range".to_string(),
                        ));
                    }
                }
            }
            // Package scopes and imports resolve through the imported
            // packages; compilation-unit declarations are not analyzed.
            RefNode::PackageExportDeclaration(_) => {
                return Err(AnalyzerError::Unsupported(
                    "package export declaration".to_string(),
                ));
            }
            RefNode::PackageScope(sv_parser::PackageScope::Unit(_)) => {
                return Err(AnalyzerError::Unsupported(
                    "compilation-unit scope reference `$unit::`".to_string(),
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
                                Ok(Some(ConstExpr::Ident(_)))
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
    if is_module {
        let active = generate::items(node, syntax_tree, const_env, type_aliases)?;
        let mut views = generate::ScopeViews::new(indexed_dimensions);
        for item in &active {
            let dimensions = views.dimensions(item);
            reject_silently_ignored_constructs_with_dimensions(
                item.node.node(),
                syntax_tree,
                dimensions,
            )?;
        }
    }

    Ok(())
}

fn non_input_function_port(node: RefNode<'_>) -> bool {
    let direction = match node {
        RefNode::TfPortItem(port) => port.nodes.1.as_ref(),
        RefNode::TfPortDeclaration(port) => Some(&port.nodes.1),
        _ => return false,
    };
    // `output` and `inout` arguments are written back by the call statement;
    // pass-by-reference is not lowered.
    match direction {
        None => false,
        Some(sv_parser::TfPortDirection::PortDirection(direction)) => {
            matches!(&**direction, sv_parser::PortDirection::Ref(_))
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

/// Reject an indexed part-select width that is not a positive constant.
fn check_indexed_width(
    width: &sv_parser::ConstantExpression,
    syntax_tree: &SyntaxTree,
    dimensions: &PackedDimensions,
) -> Result<(), AnalyzerError> {
    let width = indexed_select_base(RefNode::ConstantExpression(width), syntax_tree, dimensions)?;
    if eval_ast_const_expr(&width, &dimensions.const_env).is_some_and(|width| width > 0) {
        Ok(())
    } else {
        Err(AnalyzerError::Unsupported(
            "indexed part-select width that is not a positive constant".to_string(),
        ))
    }
}

#[cfg(test)]
mod traversal_tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn excludes_nested_generate_bodies_but_keeps_surrounding_declarations() {
        let tree = crate::syntax::parse_source(
            "module Top(); logic outside_before; if (1) begin : g logic hidden; if (0) begin : nested logic deep; end end for (genvar i=0; i<2; i++) begin : loop_scope logic looped; end logic outside_after; endmodule",
            Path::new("validation_scopes.sv"),
        ).unwrap();
        let node = tree
            .into_iter()
            .find(|node| matches!(node, RefNode::ModuleDeclarationAnsi(_)))
            .unwrap();
        let declarations = |module| {
            validation_nodes(node.clone(), module)
                .filter_map(|node| match node {
                    RefNode::VariableIdentifier(identifier) => {
                        identifier_text(RefNode::VariableIdentifier(identifier), &tree)
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(declarations(true), ["outside_before", "outside_after"]);
        assert_eq!(
            declarations(false),
            [
                "outside_before",
                "hidden",
                "deep",
                "looped",
                "outside_after"
            ]
        );
    }
}
