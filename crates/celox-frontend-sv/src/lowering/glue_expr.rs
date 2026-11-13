//! Lowering of the parent-side expressions of port connections.

use super::*;

pub(super) fn lower_glue_parent_expr(
    expr: &sv::ir::Expr,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
    arena: &mut SLTNodeArena<GlueAddr>,
    context_width: Option<usize>,
    context_signed: Option<bool>,
) -> Option<(
    celox_slt::NodeId,
    HashSet<VarAtomBase<GlueAddr>>,
    Vec<SourceVarId>,
)> {
    if floating::handles(expr, variables, name_to_id) {
        let mut native_arena = SLTNodeArena::new();
        let (node, sources) = floating::lower(
            expr,
            variables,
            name_to_id,
            constants,
            parameter_types,
            &mut native_arena,
        )?;
        let mut cache = HashMap::default();
        let mapped = native_arena
            .get(node)
            .map_addr(node, &native_arena, arena, &mut cache, &|id| {
                GlueAddr::Parent(*id)
            })
            .ok()?;
        let source_ids = sources.iter().map(|source| source.id).collect();
        let sources = sources
            .into_iter()
            .map(|source| {
                VarAtomBase::new(
                    GlueAddr::Parent(source.id),
                    source.access.lsb,
                    source.access.msb,
                )
            })
            .collect();
        return Some((mapped, sources, source_ids));
    }
    match expr {
        sv::ir::Expr::Ident(name) => {
            let Some(id) = name_to_id.get(name).copied() else {
                let value = constants.get(name)?;
                let (width, signed) = parameter_types.get(name).copied().unwrap_or((32, false));
                let node = arena
                    .alloc(SLTNode::Constant(
                        parameter_value_bits(*value, width),
                        BigUint::from(0u32),
                        width,
                        signed,
                    ))
                    .ok()?;
                return Some((
                    coerce_node_width(arena, node, context_width, context_signed.unwrap_or(signed))
                        .ok()?,
                    HashSet::default(),
                    Vec::new(),
                ));
            };
            let var = variables.get(&id)?;
            let width = var.width;
            let node = arena
                .alloc(SLTNode::Input {
                    variable: GlueAddr::Parent(id),
                    signed: var.signed,
                    index: Vec::new(),
                    access: BitAccess::new(0, width - 1),
                })
                .ok()?;
            let mut sources = HashSet::default();
            sources.insert(VarAtomBase::new(GlueAddr::Parent(id), 0, width - 1));
            Some((
                coerce_node_width(
                    arena,
                    node,
                    context_width,
                    context_signed.unwrap_or(var.signed),
                )
                .ok()?,
                sources,
                vec![id],
            ))
        }
        sv::ir::Expr::Select {
            expr,
            msb,
            lsb,
            signed,
        } => {
            if let Some(rewritten) = runtime_select_as_shift(
                expr,
                msb,
                lsb,
                *signed,
                variables,
                name_to_id,
                constants,
                parameter_types,
                false,
            ) {
                return lower_glue_parent_expr(
                    &rewritten,
                    variables,
                    name_to_id,
                    constants,
                    parameter_types,
                    arena,
                    context_width,
                    context_signed,
                );
            }
            if let Some((id, element_width, access)) = dynamic_array_element_subselection(
                expr,
                msb,
                lsb,
                variables,
                name_to_id,
                constants,
                parameter_types,
            ) {
                let (offset, mut sources, mut source_ids) = lower_dynamic_array_element_index_glue(
                    lsb,
                    variables,
                    name_to_id,
                    constants,
                    parameter_types,
                    arena,
                    element_width,
                )?;
                let variable = variables.get(&id)?;
                let element_count = variable.width.checked_div(element_width)?;
                let (offset, valid) = dynamic_array_index_guard_slt(arena, offset, element_count)?;
                let node = lower_dynamic_array_selection_slt(
                    arena,
                    GlueAddr::Parent(id),
                    *signed,
                    offset,
                    access,
                    element_width,
                    variable,
                )?;
                let node = guard_dynamic_array_read_slt(
                    arena,
                    valid,
                    node,
                    access.msb - access.lsb + 1,
                    variable.is_4state,
                )?;
                sources.insert(VarAtomBase::new(
                    GlueAddr::Parent(id),
                    0,
                    variable.width.checked_sub(1)?,
                ));
                source_ids.push(id);
                source_ids.sort();
                source_ids.dedup();
                return Some((
                    coerce_node_width(
                        arena,
                        node,
                        context_width,
                        context_signed.unwrap_or(*signed),
                    )
                    .ok()?,
                    sources,
                    source_ids,
                ));
            }
            // As in `lower_expr_with_context`: a run-time bit the element
            // lowering cannot express is read from the flattened array.
            if let Some(rewritten) = runtime_select_as_shift(
                expr,
                msb,
                lsb,
                *signed,
                variables,
                name_to_id,
                constants,
                parameter_types,
                true,
            ) {
                return lower_glue_parent_expr(
                    &rewritten,
                    variables,
                    name_to_id,
                    constants,
                    parameter_types,
                    arena,
                    context_width,
                    context_signed,
                );
            }
            let (inner, sources, source_ids) = lower_glue_parent_expr(
                expr,
                variables,
                name_to_id,
                constants,
                parameter_types,
                arena,
                None,
                None,
            )?;
            let msb_value =
                sv::typecheck::eval_const_expr_with_types(msb, constants, parameter_types)?;
            let lsb_value =
                sv::typecheck::eval_const_expr_with_types(lsb, constants, parameter_types)?;
            let (msb, lsb) =
                packed_expr_select_offsets(expr, msb_value, lsb_value, variables, name_to_id)?;
            let access = BitAccess::new(msb.min(lsb), msb.max(lsb));
            let node = arena
                .alloc(SLTNode::Slice {
                    expr: inner,
                    access,
                })
                .ok()?;
            let sources = select_sources(expr, sources, access)?;
            Some((
                coerce_node_width(
                    arena,
                    node,
                    context_width,
                    context_signed.unwrap_or(*signed),
                )
                .ok()?,
                sources,
                source_ids,
            ))
        }
        sv::ir::Expr::Concat(parts) => {
            let mut nodes = Vec::new();
            let mut sources = HashSet::default();
            let mut source_ids = Vec::new();
            for part in parts {
                let (node, part_sources, part_source_ids) =
                    if let Some(fill) = expr_unbased_fill_literal(part) {
                        (
                            lower_unbased_fill_literal_slt(arena, fill, 1)?,
                            HashSet::default(),
                            Vec::new(),
                        )
                    } else {
                        lower_glue_parent_expr(
                            part,
                            variables,
                            name_to_id,
                            constants,
                            parameter_types,
                            arena,
                            None,
                            None,
                        )?
                    };
                let width = celox_slt::get_width(node, arena);
                if width != 0 {
                    nodes.push((node, width));
                }
                sources.extend(part_sources);
                source_ids.extend(part_source_ids);
            }
            source_ids.sort();
            source_ids.dedup();
            let node = arena.alloc(SLTNode::Concat(nodes)).ok()?;
            Some((
                coerce_node_width(arena, node, context_width, context_signed.unwrap_or(false))
                    .ok()?,
                sources,
                source_ids,
            ))
        }
        sv::ir::Expr::RepeatConcat { count, parts } => {
            let count =
                sv::typecheck::eval_const_expr_with_types(count, constants, parameter_types)?;
            let count = usize::try_from(count).ok()?;
            let mut nodes = Vec::new();
            let mut sources = HashSet::default();
            let mut source_ids = Vec::new();
            for _ in 0..count {
                for part in parts {
                    let (node, part_sources, part_source_ids) =
                        if let Some(fill) = expr_unbased_fill_literal(part) {
                            (
                                lower_unbased_fill_literal_slt(arena, fill, 1)?,
                                HashSet::default(),
                                Vec::new(),
                            )
                        } else {
                            lower_glue_parent_expr(
                                part,
                                variables,
                                name_to_id,
                                constants,
                                parameter_types,
                                arena,
                                None,
                                None,
                            )?
                        };
                    let width = celox_slt::get_width(node, arena);
                    nodes.push((node, width));
                    sources.extend(part_sources);
                    source_ids.extend(part_source_ids);
                }
            }
            source_ids.sort();
            source_ids.dedup();
            let node = arena.alloc(SLTNode::Concat(nodes)).ok()?;
            Some((
                coerce_node_width(arena, node, context_width, context_signed.unwrap_or(false))
                    .ok()?,
                sources,
                source_ids,
            ))
        }
        sv::ir::Expr::Resize {
            expr,
            width,
            signed,
        } => {
            let (inner, sources, source_ids) = lower_glue_parent_expr(
                expr,
                variables,
                name_to_id,
                constants,
                parameter_types,
                arena,
                Some(*width),
                Some(*signed),
            )?;
            let resized = coerce_node_width(arena, inner, Some(*width), *signed).ok()?;
            Some((
                coerce_node_width(
                    arena,
                    resized,
                    context_width,
                    context_signed.unwrap_or(*signed),
                )
                .ok()?,
                sources,
                source_ids,
            ))
        }
        sv::ir::Expr::Literal(literal) => {
            if let Some(width) = context_width
                && let Some(fill) = unbased_fill_literal(literal)
            {
                return Some((
                    lower_unbased_fill_literal_slt(arena, fill, width)?,
                    HashSet::default(),
                    Vec::new(),
                ));
            }
            let literal = sv::typecheck::parse_integral_literal(literal)?;
            let signed = literal.signed;
            let node = arena
                .alloc(SLTNode::Constant(
                    literal.value,
                    literal.mask,
                    literal.width,
                    signed,
                ))
                .ok()?;
            Some((
                coerce_node_width(arena, node, context_width, context_signed.unwrap_or(signed))
                    .ok()?,
                HashSet::default(),
                Vec::new(),
            ))
        }
        sv::ir::Expr::Unary { op, expr } => {
            let one_bit_result = matches!(
                op,
                sv::ir::UnaryOp::LogicNot
                    | sv::ir::UnaryOp::RedAnd
                    | sv::ir::UnaryOp::RedOr
                    | sv::ir::UnaryOp::RedXor
            );
            let operand_context = (!one_bit_result).then_some(context_width).flatten();
            let operand_signed = context_signed.or_else(|| {
                Some(sv_glue_expr_is_signed(
                    expr,
                    variables,
                    name_to_id,
                    parameter_types,
                ))
            });
            let (inner, sources, source_ids) = lower_glue_parent_expr(
                expr,
                variables,
                name_to_id,
                constants,
                parameter_types,
                arena,
                operand_context,
                operand_signed,
            )?;
            Some((
                arena
                    .alloc(SLTNode::Unary(unary_op_from_sv(*op)?, inner))
                    .ok()?,
                sources,
                source_ids,
            ))
        }
        sv::ir::Expr::Binary { left, op, right } => {
            let left_signed = sv_glue_expr_is_signed(left, variables, name_to_id, parameter_types);
            let operands_signed = left_signed
                && sv_glue_expr_is_signed(right, variables, name_to_id, parameter_types);
            let operator_signed = if matches!(op, sv::ir::BinaryOp::Sar) {
                left_signed
            } else {
                operands_signed
            };
            let comparison = matches!(
                op,
                sv::ir::BinaryOp::Eq
                    | sv::ir::BinaryOp::Ne
                    | sv::ir::BinaryOp::EqCase
                    | sv::ir::BinaryOp::NeCase
                    | sv::ir::BinaryOp::EqWildcard
                    | sv::ir::BinaryOp::NeWildcard
                    | sv::ir::BinaryOp::Lt
                    | sv::ir::BinaryOp::Le
                    | sv::ir::BinaryOp::Gt
                    | sv::ir::BinaryOp::Ge
            );
            let shift = matches!(
                op,
                sv::ir::BinaryOp::Shl | sv::ir::BinaryOp::Shr | sv::ir::BinaryOp::Sar
            );
            let context_determined = !comparison
                && !matches!(op, sv::ir::BinaryOp::LogicAnd | sv::ir::BinaryOp::LogicOr);
            let natural_width =
                sv_expr_natural_width(expr, variables, name_to_id, constants, parameter_types);
            let operation_context = context_width
                .map(|width| width.max(natural_width.unwrap_or(width)))
                .or(natural_width);
            let comparison_context = comparison
                .then(|| {
                    sv_comparison_operand_width(
                        left,
                        right,
                        variables,
                        name_to_id,
                        constants,
                        parameter_types,
                    )
                })
                .flatten();
            let left_context = if comparison {
                comparison_context
            } else {
                context_determined.then_some(operation_context).flatten()
            };
            let right_context = if comparison {
                comparison_context
            } else {
                (context_determined && !shift)
                    .then_some(operation_context)
                    .flatten()
            };
            let left_context_signed = Some(if shift { left_signed } else { operands_signed });
            let right_context_signed = Some(operands_signed);
            let context_sized_comparison = comparison;
            let left_fill = (context_sized_comparison || shift)
                .then(|| expr_unbased_fill_literal(left))
                .flatten();
            let right_fill = (context_sized_comparison || shift)
                .then(|| expr_unbased_fill_literal(right))
                .flatten();
            let (
                (mut left, mut sources, mut source_ids),
                (mut right, right_sources, right_source_ids),
            ) = match (left_fill, right_fill) {
                (Some(left_fill), Some(right_fill)) => {
                    let left_width = if shift { left_context.unwrap_or(1) } else { 1 };
                    (
                        (
                            lower_unbased_fill_literal_slt(arena, left_fill, left_width)?,
                            HashSet::default(),
                            Vec::new(),
                        ),
                        (
                            lower_unbased_fill_literal_slt(arena, right_fill, 1)?,
                            HashSet::default(),
                            Vec::new(),
                        ),
                    )
                }
                (Some(fill), None) => {
                    let right = lower_glue_parent_expr(
                        right,
                        variables,
                        name_to_id,
                        constants,
                        parameter_types,
                        arena,
                        right_context,
                        right_context_signed,
                    )?;
                    let width = if shift {
                        left_context.unwrap_or(1)
                    } else {
                        celox_slt::get_width(right.0, arena)
                    };
                    (
                        (
                            lower_unbased_fill_literal_slt(arena, fill, width)?,
                            HashSet::default(),
                            Vec::new(),
                        ),
                        right,
                    )
                }
                (None, Some(fill)) => {
                    let left = lower_glue_parent_expr(
                        left,
                        variables,
                        name_to_id,
                        constants,
                        parameter_types,
                        arena,
                        left_context,
                        left_context_signed,
                    )?;
                    let width = if shift {
                        1
                    } else {
                        celox_slt::get_width(left.0, arena)
                    };
                    (
                        left,
                        (
                            lower_unbased_fill_literal_slt(arena, fill, width)?,
                            HashSet::default(),
                            Vec::new(),
                        ),
                    )
                }
                (None, None) => (
                    lower_glue_parent_expr(
                        left,
                        variables,
                        name_to_id,
                        constants,
                        parameter_types,
                        arena,
                        left_context,
                        left_context_signed,
                    )?,
                    lower_glue_parent_expr(
                        right,
                        variables,
                        name_to_id,
                        constants,
                        parameter_types,
                        arena,
                        right_context,
                        right_context_signed,
                    )?,
                ),
            };
            sources.extend(right_sources);
            source_ids.extend(right_source_ids);
            source_ids.sort();
            source_ids.dedup();
            if context_determined {
                left = coerce_node_width(
                    arena,
                    left,
                    left_context,
                    if shift { left_signed } else { operands_signed },
                )
                .ok()?;
                right = coerce_node_width(arena, right, right_context, operands_signed).ok()?;
            }
            if context_sized_comparison {
                let common_width =
                    celox_slt::get_width(left, arena).max(celox_slt::get_width(right, arena));
                left = coerce_node_width(arena, left, Some(common_width), operands_signed).ok()?;
                right =
                    coerce_node_width(arena, right, Some(common_width), operands_signed).ok()?;
            }
            Some((
                arena
                    .alloc(SLTNode::Binary(
                        left,
                        binary_op_from_sv(*op, operator_signed)?,
                        right,
                    ))
                    .ok()?,
                sources,
                source_ids,
            ))
        }
        sv::ir::Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            let arms_signed = sv_glue_expr_is_signed(then_expr, variables, name_to_id, parameter_types)
                    && sv_glue_expr_is_signed(else_expr, variables, name_to_id, parameter_types)
                    // An unsigned context makes the whole expression unsigned.
                    && context_signed != Some(false);
            let arm_context =
                sv_expr_natural_width(expr, variables, name_to_id, constants, parameter_types)
                    .map(|natural_width| {
                        context_width.map_or(natural_width, |width| width.max(natural_width))
                    })
                    .or(context_width);
            let float_condition = floating::condition(condition, variables, name_to_id);
            let (condition, mut sources, mut source_ids) = lower_glue_parent_expr(
                &float_condition,
                variables,
                name_to_id,
                constants,
                parameter_types,
                arena,
                None,
                None,
            )?;
            let (mut then_expr, then_sources, then_source_ids) = lower_glue_parent_expr(
                then_expr,
                variables,
                name_to_id,
                constants,
                parameter_types,
                arena,
                arm_context,
                Some(arms_signed),
            )?;
            let (mut else_expr, else_sources, else_source_ids) = lower_glue_parent_expr(
                else_expr,
                variables,
                name_to_id,
                constants,
                parameter_types,
                arena,
                arm_context,
                Some(arms_signed),
            )?;
            sources.extend(then_sources);
            sources.extend(else_sources);
            source_ids.extend(then_source_ids);
            source_ids.extend(else_source_ids);
            source_ids.sort();
            source_ids.dedup();
            let width =
                celox_slt::get_width(then_expr, arena).max(celox_slt::get_width(else_expr, arena));
            then_expr = coerce_node_width(arena, then_expr, Some(width), arms_signed).ok()?;
            else_expr = coerce_node_width(arena, else_expr, Some(width), arms_signed).ok()?;
            Some((
                arena
                    .alloc(SLTNode::Mux {
                        cond: condition,
                        then_expr,
                        else_expr,
                    })
                    .ok()?,
                sources,
                source_ids,
            ))
        }
        sv::ir::Expr::Call {
            callee: sv::ir::Callee::Lookup { width, signed },
            args,
            ..
        } => {
            let mut sources = HashSet::default();
            let mut source_ids = Vec::new();
            let node = lower_lookup(
                args,
                *width,
                *signed,
                arena,
                &mut |expr, arena, context_width, context_signed| {
                    let (node, expr_sources, expr_source_ids) = lower_glue_parent_expr(
                        expr,
                        variables,
                        name_to_id,
                        constants,
                        parameter_types,
                        arena,
                        context_width,
                        context_signed,
                    )?;
                    sources.extend(expr_sources);
                    source_ids.extend(expr_source_ids);
                    Some(node)
                },
            )?;
            source_ids.sort();
            source_ids.dedup();
            Some((node, sources, source_ids))
        }
        sv::ir::Expr::Call {
            callee: sv::ir::Callee::Stream(stream),
            args,
            ..
        } => {
            let mut sources = HashSet::default();
            let mut source_ids = Vec::new();
            let node = lower_stream(
                args,
                stream,
                arena,
                &mut |expr, arena, context_width, context_signed| {
                    let (node, expr_sources, expr_source_ids) = lower_glue_parent_expr(
                        expr,
                        variables,
                        name_to_id,
                        constants,
                        parameter_types,
                        arena,
                        context_width,
                        context_signed,
                    )?;
                    sources.extend(expr_sources);
                    source_ids.extend(expr_source_ids);
                    Some(node)
                },
            )?;
            source_ids.sort();
            source_ids.dedup();
            Some((
                coerce_node_width(arena, node, context_width, context_signed.unwrap_or(false))
                    .ok()?,
                sources,
                source_ids,
            ))
        }
        sv::ir::Expr::Inside { expr, items } => lower_glue_parent_expr(
            &inside_as_comparisons(expr, items),
            variables,
            name_to_id,
            constants,
            parameter_types,
            arena,
            context_width,
            context_signed,
        ),
        sv::ir::Expr::Call {
            callee: sv::ir::Callee::System(function),
            args,
            ..
        } if function.result_type().is_some() => {
            let mut operands = Vec::with_capacity(args.len());
            let mut sources = HashSet::default();
            let mut source_ids = Vec::new();
            for arg in args {
                let arg = arg.as_ref()?;
                let width =
                    sv_expr_natural_width(arg, variables, name_to_id, constants, parameter_types)?;
                let (inner, arg_sources, arg_source_ids) = lower_glue_parent_expr(
                    arg,
                    variables,
                    name_to_id,
                    constants,
                    parameter_types,
                    arena,
                    Some(width),
                    None,
                )?;
                operands.push(inner);
                sources.extend(arg_sources);
                source_ids.extend(arg_source_ids);
            }
            Some((
                lower_bit_vector_function_slt(
                    arena,
                    *function,
                    &operands,
                    context_width,
                    context_signed,
                )?,
                sources,
                source_ids,
            ))
        }
        sv::ir::Expr::Call { .. } => None,
    }
}

fn lower_dynamic_array_element_index_glue(
    offset: &sv::ir::ConstExpr,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
    arena: &mut SLTNodeArena<GlueAddr>,
    element_width: usize,
) -> Option<(NodeId, HashSet<VarAtomBase<GlueAddr>>, Vec<SourceVarId>)> {
    let offset_expr = expr_from_select_position(offset, constants, parameter_types)?;
    let (offset, sources, source_ids) = lower_glue_parent_expr(
        &offset_expr,
        variables,
        name_to_id,
        constants,
        parameter_types,
        arena,
        None,
        None,
    )?;
    let element_index = if element_width == 1 {
        offset
    } else {
        let divisor = arena
            .alloc(SLTNode::Constant(
                BigUint::from(element_width),
                BigUint::default(),
                64,
                false,
            ))
            .ok()?;
        arena
            .alloc(SLTNode::Binary(offset, BinaryOp::DivU, divisor))
            .ok()?
    };
    Some((element_index, sources, source_ids))
}
