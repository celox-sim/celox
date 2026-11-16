//! Array literals and function input arguments.

use super::*;

pub(in crate::lowering::logic_tree) fn eval_array_literal_expression_effectful(
    module: &Module,
    store: &mut SymbolicStore<VarId>,
    items: &[ArrayLiteralItem],
    expected_width: Option<usize>,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<((NodeId, HashSet<VarAtomBase<VarId>>), BoundaryMap<VarId>), ParserError> {
    let mut store = ExpressionStore::Effectful(store);
    eval_array_literal_expression_in_store(module, &mut store, items, expected_width, arena)
}

pub(super) fn eval_array_literal_expression_in_store(
    module: &Module,
    store: &mut ExpressionStore<'_>,
    items: &[ArrayLiteralItem],
    expected_width: Option<usize>,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<((NodeId, HashSet<VarAtomBase<VarId>>), BoundaryMap<VarId>), ParserError> {
    eval_array_literal_expression_with_item_context(
        module,
        store,
        items,
        expected_width,
        None,
        arena,
    )
}

pub(super) fn eval_function_input_assignment_in_store(
    module: &Module,
    store: &mut ExpressionStore<'_>,
    expr: &Expression,
    target_type: &Type,
    target_width: usize,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<((NodeId, HashSet<VarAtomBase<VarId>>), BoundaryMap<VarId>), ParserError> {
    if target_type.array.is_empty() {
        return eval_assignment_expression_in_store(
            module,
            store,
            &crate::float::converted(expr, target_type),
            arena,
            target_width,
        );
    }

    let element_width = target_type.total_width().ok_or_else(|| {
        ParserError::illegal_context(
            "function input argument",
            format!("unresolved element width for {target_type}"),
            Some(&expr.token_range()),
        )
    })?;
    let value = if let Expression::ArrayLiteral(items, _) = expr {
        eval_array_literal_expression_with_item_context(
            module,
            store,
            items,
            Some(target_width),
            Some(ArrayLiteralItemContext {
                array_shape: &target_type.array.as_slice()[1..],
                element_width,
            }),
            arena,
        )?
    } else {
        eval_array_literal_item_in_store(
            module,
            store,
            expr,
            Some(ArrayLiteralItemContext {
                array_shape: target_type.array.as_slice(),
                element_width,
            }),
            arena,
        )?
    };

    finish_assignment_expression(expr, arena, target_width, value)
}

pub(crate) fn eval_function_input_assignment_effectful(
    module: &Module,
    store: &mut SymbolicStore<VarId>,
    expr: &Expression,
    target_type: &Type,
    target_width: usize,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<((NodeId, HashSet<VarAtomBase<VarId>>), BoundaryMap<VarId>), ParserError> {
    let mut store = ExpressionStore::Effectful(store);
    eval_function_input_assignment_in_store(
        module,
        &mut store,
        expr,
        target_type,
        target_width,
        arena,
    )
}

#[derive(Clone, Copy)]
pub(super) struct ArrayLiteralItemContext<'a> {
    pub(super) array_shape: &'a [Option<usize>],
    pub(super) element_width: usize,
}

pub(super) fn eval_array_literal_item_in_store(
    module: &Module,
    store: &mut ExpressionStore<'_>,
    expr: &Expression,
    context: Option<ArrayLiteralItemContext<'_>>,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<((NodeId, HashSet<VarAtomBase<VarId>>), BoundaryMap<VarId>), ParserError> {
    let Some(context) = context else {
        return eval_expression_in_context(module, store, expr, arena, None);
    };

    // A nested literal occupies the remaining unpacked dimensions. Keep
    // carrying the formal shape down so every level gets the same index-order
    // conversion, and the scalar leaves are sized as formal elements.
    if let Expression::ArrayLiteral(items, _) = expr
        && !context.array_shape.is_empty()
    {
        let target_width = context
            .array_shape
            .iter()
            .try_fold(context.element_width, |width, dim| {
                width.checked_mul((*dim)?)
            })
            .ok_or_else(|| {
                ParserError::illegal_context(
                    "array literal item",
                    "unresolved or overflowing nested array shape",
                    Some(&expr.token_range()),
                )
            })?;
        let value = eval_array_literal_expression_with_item_context(
            module,
            store,
            items,
            Some(target_width),
            Some(ArrayLiteralItemContext {
                array_shape: &context.array_shape[1..],
                element_width: context.element_width,
            }),
            arena,
        )?;
        return finish_assignment_expression(expr, arena, target_width, value);
    }

    let source_type = crate::float::type_of(expr);

    // A scalar leaf is assigned to one formal element, not concatenated at
    // its natural width and widened after the aggregate has been assembled.
    if source_type.array.is_empty() && context.array_shape.is_empty() {
        return eval_assignment_expression_in_store(
            module,
            store,
            expr,
            arena,
            context.element_width,
        );
    }

    if source_type.array.as_slice() != context.array_shape || source_type.array.is_empty() {
        return eval_expression_in_context(module, store, expr, arena, None);
    }

    // An array-valued item occupies one aggregate slot in the surrounding
    // assignment pattern. Evaluate it once, then apply the destination's
    // scalar conversion independently to every element. Coercing the flattened
    // value as a whole would put all extension bits at one end of the array.
    let ((source, sources), bounds) = eval_expression_in_context(module, store, expr, arena, None)?;
    let source_element_width = source_type.total_width().ok_or_else(|| {
        ParserError::illegal_context(
            "array literal item",
            format!("unresolved element width for {source_type}"),
            Some(&expr.token_range()),
        )
    })?;
    let element_count = source_type.total_array().ok_or_else(|| {
        ParserError::illegal_context(
            "array literal item",
            format!("unresolved array shape for {source_type}"),
            Some(&expr.token_range()),
        )
    })?;
    let expected_source_width =
        source_element_width
            .checked_mul(element_count)
            .ok_or_else(|| {
                ParserError::illegal_context(
                    "array literal item",
                    "array item width overflows usize",
                    Some(&expr.token_range()),
                )
            })?;
    let source_width = get_width(source, arena);
    if source_width != expected_source_width {
        return Err(ParserError::illegal_context(
            "array literal item",
            format!(
                "expression width {source_width} does not match its array type width {expected_source_width}"
            ),
            Some(&expr.token_range()),
        ));
    }

    // Array index zero is stored at the least-significant end. Concat accepts
    // most-significant parts first, so visit semantic elements in reverse.
    let mut parts = Vec::with_capacity(element_count);
    for index in (0..element_count).rev() {
        let lsb = index * source_element_width;
        let element = arena.alloc(SLTNode::Slice {
            expr: source,
            access: BitAccess::new(lsb, lsb + source_element_width - 1),
        })?;
        let element = coerce_node_width(
            arena,
            element,
            Some(context.element_width),
            expression_signed(expr),
        )?;
        parts.push((element, context.element_width));
    }

    Ok(((arena.alloc(SLTNode::Concat(parts))?, sources), bounds))
}

pub(super) fn eval_array_literal_default_in_store(
    module: &Module,
    store: &mut ExpressionStore<'_>,
    expr: &Expression,
    context: Option<ArrayLiteralItemContext<'_>>,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<((NodeId, HashSet<VarAtomBase<VarId>>), BoundaryMap<VarId>), ParserError> {
    // A scalar default fills scalar leaves even when the current literal level
    // still represents additional unpacked dimensions. Convert it once as a
    // formal element; the caller then replicates that converted element across
    // all remaining aggregate width.
    if let Some(context) = context
        && expr.comptime().r#type.array.is_empty()
    {
        eval_assignment_expression_in_store(module, store, expr, arena, context.element_width)
    } else {
        eval_array_literal_item_in_store(module, store, expr, context, arena)
    }
}

pub(super) fn eval_array_literal_expression_with_item_context(
    module: &Module,
    store: &mut ExpressionStore<'_>,
    items: &[ArrayLiteralItem],
    expected_width: Option<usize>,
    item_context: Option<ArrayLiteralItemContext<'_>>,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<((NodeId, HashSet<VarAtomBase<VarId>>), BoundaryMap<VarId>), ParserError> {
    let mut parts = Vec::new();
    let mut all_bounds = BoundaryMap::default();
    let mut total_sources = HashSet::default();

    let mut explicit_width = 0usize;
    let mut default_part: Option<(NodeId, usize)> = None;

    for item in items {
        match item {
            ArrayLiteralItem::Value(sub_expr, repeat) => {
                let ((part_expr, part_sources), p_bounds) =
                    eval_array_literal_item_in_store(module, store, sub_expr, item_context, arena)?;
                all_bounds = merge_boundaries(all_bounds, p_bounds);
                total_sources.extend(part_sources);

                let width = get_width(part_expr, arena);
                let rep_count = if let Some(rep_expr) = repeat {
                    let Some(rep_count) = eval_constexpr(rep_expr).and_then(|x| x.to_u64()) else {
                        return Err(ParserError::illegal_context(
                            "array literal non-constant repeat",
                            format!("{:?}", rep_expr),
                            Some(&rep_expr.token_range()),
                        ));
                    };
                    rep_count
                } else {
                    1
                };

                for _ in 0..rep_count {
                    parts.push((part_expr, width));
                }
                explicit_width += width * rep_count as usize;
            }
            ArrayLiteralItem::Defaul(default_expr) => {
                if default_part.is_some() {
                    let token = default_expr.token_range();
                    return Err(ParserError::illegal_context(
                        "array literal multiple default",
                        format!("{:?}", items),
                        Some(&token),
                    ));
                }

                let ((part_expr, part_sources), p_bounds) = eval_array_literal_default_in_store(
                    module,
                    store,
                    default_expr,
                    item_context,
                    arena,
                )?;
                all_bounds = merge_boundaries(all_bounds, p_bounds);
                total_sources.extend(part_sources);
                let width = get_width(part_expr, arena);
                default_part = Some((part_expr, width));
            }
        }
    }

    if let Some((default_expr, default_width)) = default_part {
        let Some(target_width) = expected_width else {
            let token = items.first().map(|i| i.token_range());
            return Err(ParserError::illegal_context(
                "array literal default without context width",
                format!("{:?}", items),
                token.as_ref(),
            ));
        };

        if explicit_width > target_width {
            let token = items.first().map(|i| i.token_range());
            return Err(ParserError::illegal_context(
                "array literal width overflow",
                format!("explicit_width={explicit_width}, target_width={target_width}"),
                token.as_ref(),
            ));
        }

        let remaining = target_width - explicit_width;
        if default_width == 0 || !remaining.is_multiple_of(default_width) {
            return Err(ParserError::illegal_context(
                "array literal default width mismatch",
                format!(
                    "remaining={remaining}, default_width={default_width}, target_width={target_width}"
                ),
                None,
            ));
        }

        for _ in 0..(remaining / default_width) {
            parts.push((default_expr, default_width));
        }
    }

    // Unpacked array index zero is stored at the least-significant end, while
    // source-order evaluation must remain left-to-right.
    if item_context.is_some() {
        parts.reverse();
    }
    let result = arena.alloc(SLTNode::Concat(parts))?;
    Ok(((result, total_sources), all_bounds))
}
