//! Lowering of expressions to combinational nodes.

use super::*;

fn lower_expr(
    expr: &sv::ir::Expr,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
    arena: &mut SLTNodeArena<SourceVarId>,
) -> Option<(celox_slt::NodeId, HashSet<VarAtomBase<SourceVarId>>)> {
    lower_expr_with_context(
        expr,
        variables,
        name_to_id,
        constants,
        parameter_types,
        arena,
        None,
        None,
    )
}

pub(super) fn lower_expr_with_context(
    expr: &sv::ir::Expr,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
    arena: &mut SLTNodeArena<SourceVarId>,
    context_width: Option<usize>,
    context_signed: Option<bool>,
) -> Option<(celox_slt::NodeId, HashSet<VarAtomBase<SourceVarId>>)> {
    if floating::handles(expr, variables, name_to_id) {
        return floating::lower(
            expr,
            variables,
            name_to_id,
            constants,
            parameter_types,
            arena,
        );
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
                ));
            };
            let var = variables.get(&id)?;
            let width = var.width;
            let node = arena
                .alloc(SLTNode::Input {
                    variable: id,
                    signed: var.signed,
                    index: Vec::new(),
                    access: BitAccess::new(0, width - 1),
                })
                .ok()?;
            let mut sources = HashSet::default();
            sources.insert(VarAtomBase::new(id, 0, width - 1));
            Some((
                coerce_node_width(
                    arena,
                    node,
                    context_width,
                    context_signed.unwrap_or(var.signed),
                )
                .ok()?,
                sources,
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
                return lower_expr_with_context(
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
                let (offset, mut sources) = lower_dynamic_array_element_index_slt(
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
                    id,
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
                sources.insert(VarAtomBase::new(id, 0, variable.width.checked_sub(1)?));
                return Some((
                    coerce_node_width(
                        arena,
                        node,
                        context_width,
                        context_signed.unwrap_or(*signed),
                    )
                    .ok()?,
                    sources,
                ));
            }
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
                return lower_expr_with_context(
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
            let (inner, mut sources) = lower_expr(
                expr,
                variables,
                name_to_id,
                constants,
                parameter_types,
                arena,
            )?;
            let msb_value =
                sv::typecheck::eval_const_expr_with_types(msb, constants, parameter_types)?;
            let lsb_value =
                sv::typecheck::eval_const_expr_with_types(lsb, constants, parameter_types)?;
            let (msb, lsb) = if let sv::ir::Expr::Ident(name) = &**expr {
                let variable = name_to_id.get(name).and_then(|id| variables.get(id))?;
                (
                    packed_index_offset(variable, msb_value)?,
                    packed_index_offset(variable, lsb_value)?,
                )
            } else {
                (
                    usize::try_from(msb_value).ok()?,
                    usize::try_from(lsb_value).ok()?,
                )
            };
            let access = BitAccess::new(msb.min(lsb), msb.max(lsb));
            let node = arena
                .alloc(SLTNode::Slice {
                    expr: inner,
                    access,
                })
                .ok()?;
            sources = select_sources(expr, sources, access)?;
            Some((
                coerce_node_width(
                    arena,
                    node,
                    context_width,
                    context_signed.unwrap_or(*signed),
                )
                .ok()?,
                sources,
            ))
        }
        sv::ir::Expr::Concat(parts) => {
            let mut nodes = Vec::new();
            let mut sources = HashSet::default();
            for part in parts {
                let (node, part_sources) = lower_expr_with_context(
                    part,
                    variables,
                    name_to_id,
                    constants,
                    parameter_types,
                    arena,
                    expr_unbased_fill_literal(part).map(|_| 1),
                    None,
                )?;
                let width = celox_slt::get_width(node, arena);
                if width != 0 {
                    nodes.push((node, width));
                }
                sources.extend(part_sources);
            }
            Some((arena.alloc(SLTNode::Concat(nodes)).ok()?, sources))
        }
        sv::ir::Expr::RepeatConcat { count, parts } => {
            let count =
                sv::typecheck::eval_const_expr_with_types(count, constants, parameter_types)?;
            let count = usize::try_from(count).ok()?;
            let mut repeated = Vec::new();
            let mut sources = HashSet::default();
            for _ in 0..count {
                for part in parts {
                    let (node, part_sources) = lower_expr_with_context(
                        part,
                        variables,
                        name_to_id,
                        constants,
                        parameter_types,
                        arena,
                        expr_unbased_fill_literal(part).map(|_| 1),
                        None,
                    )?;
                    let width = celox_slt::get_width(node, arena);
                    repeated.push((node, width));
                    sources.extend(part_sources);
                }
            }
            Some((arena.alloc(SLTNode::Concat(repeated)).ok()?, sources))
        }
        sv::ir::Expr::Literal(literal) => {
            if let Some(fill) = unbased_fill_literal(literal) {
                // In a self-determined context an unbased unsized literal is
                // one bit wide (IEEE 1800-2023 5.7.1).
                return Some((
                    lower_unbased_fill_literal_slt(arena, fill, context_width.unwrap_or(1))?,
                    HashSet::default(),
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
            let (inner, sources) = lower_expr_with_context(
                expr,
                variables,
                name_to_id,
                constants,
                parameter_types,
                arena,
                operand_context,
                context_signed,
            )?;
            // A self-determined operand (such as a concatenation) is extended
            // to the context width before the operator applies.
            let inner = coerce_node_width(
                arena,
                inner,
                operand_context,
                context_signed.unwrap_or(false),
            )
            .ok()?;
            if *op == sv::ir::UnaryOp::Plus {
                // Unary plus is arithmetic: an unknown operand bit makes the
                // whole result unknown (IEEE 1800-2023 11.4.3).
                let width = celox_slt::get_width(inner, arena);
                let zero = arena
                    .alloc(SLTNode::Constant(
                        BigUint::default(),
                        BigUint::default(),
                        width,
                        procedural::node_is_signed(arena, inner),
                    ))
                    .ok()?;
                return Some((
                    arena
                        .alloc(SLTNode::Binary(inner, BinaryOp::Add, zero))
                        .ok()?,
                    sources,
                ));
            }
            Some((
                arena
                    .alloc(SLTNode::Unary(unary_op_from_sv(*op)?, inner))
                    .ok()?,
                sources,
            ))
        }
        sv::ir::Expr::Resize {
            expr,
            width,
            signed,
        } => {
            let (inner, sources) = lower_expr_with_context(
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
            ))
        }
        sv::ir::Expr::Binary { left, op, right } => {
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
            // An unsigned context makes the operands of a context-determined
            // operator unsigned, and `>>>` a logical shift (IEEE 1800-2023
            // 11.8.2).
            let unsigned_context = context_determined && context_signed == Some(false);
            let left_signed = !unsigned_context
                && sv_expr_is_signed_with_parameters(left, variables, name_to_id, parameter_types);
            let operands_signed = left_signed
                && sv_expr_is_signed_with_parameters(right, variables, name_to_id, parameter_types);
            let operator_signed = if matches!(op, sv::ir::BinaryOp::Sar) {
                left_signed
            } else {
                operands_signed
            };
            let natural_width =
                sv_expr_natural_width(expr, variables, name_to_id, constants, parameter_types);
            let operation_context = context_width
                .map(|width| width.max(natural_width.unwrap_or(width)))
                .or(natural_width);
            if matches!(op, sv::ir::BinaryOp::Pow) {
                // The base is context-determined and gives the result its
                // type; the exponent is self-determined (IEEE 1800-2023 11.6.1).
                let (base, mut sources) = lower_expr_with_context(
                    left,
                    variables,
                    name_to_id,
                    constants,
                    parameter_types,
                    arena,
                    operation_context,
                    Some(left_signed),
                )?;
                let base = coerce_node_width(arena, base, operation_context, left_signed).ok()?;
                let exponent_signed = sv_expr_is_signed_with_parameters(
                    right,
                    variables,
                    name_to_id,
                    parameter_types,
                );
                let (exponent, exponent_sources) = lower_expr_with_context(
                    right,
                    variables,
                    name_to_id,
                    constants,
                    parameter_types,
                    arena,
                    None,
                    Some(exponent_signed),
                )?;
                sources.extend(exponent_sources);
                let width = celox_slt::get_width(base, arena);
                let node = celox_frontend_core::symbolic::pow::lower_runtime_pow(
                    arena,
                    base,
                    exponent,
                    width,
                    exponent_signed,
                    left_signed,
                )
                .ok()?;
                return Some((node, sources));
            }
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
            let left_fill = (comparison || shift)
                .then(|| expr_unbased_fill_literal(left))
                .flatten();
            let right_fill = (comparison || shift)
                .then(|| expr_unbased_fill_literal(right))
                .flatten();
            let ((mut left, mut sources), (mut right, right_sources)) =
                match (left_fill, right_fill) {
                    (Some(left_fill), Some(right_fill)) => {
                        let left_width = if shift { left_context.unwrap_or(1) } else { 1 };
                        (
                            (
                                lower_unbased_fill_literal_slt(arena, left_fill, left_width)?,
                                HashSet::default(),
                            ),
                            (
                                lower_unbased_fill_literal_slt(arena, right_fill, 1)?,
                                HashSet::default(),
                            ),
                        )
                    }
                    (Some(fill), None) => {
                        let right = lower_expr_with_context(
                            right,
                            variables,
                            name_to_id,
                            constants,
                            parameter_types,
                            arena,
                            right_context,
                            Some(operands_signed),
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
                            ),
                            right,
                        )
                    }
                    (None, Some(fill)) => {
                        let left = lower_expr_with_context(
                            left,
                            variables,
                            name_to_id,
                            constants,
                            parameter_types,
                            arena,
                            left_context,
                            Some(if shift { left_signed } else { operands_signed }),
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
                            ),
                        )
                    }
                    (None, None) => (
                        lower_expr_with_context(
                            left,
                            variables,
                            name_to_id,
                            constants,
                            parameter_types,
                            arena,
                            left_context,
                            Some(if shift { left_signed } else { operands_signed }),
                        )?,
                        lower_expr_with_context(
                            right,
                            variables,
                            name_to_id,
                            constants,
                            parameter_types,
                            arena,
                            right_context,
                            Some(operands_signed),
                        )?,
                    ),
                };
            sources.extend(right_sources);
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
            if comparison {
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
            ))
        }
        sv::ir::Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            let arms_signed = sv_expr_is_signed_with_parameters(
                then_expr,
                variables,
                name_to_id,
                parameter_types,
            ) && sv_expr_is_signed_with_parameters(
                else_expr,
                variables,
                name_to_id,
                parameter_types,
            ) && context_signed != Some(false);
            let arm_context =
                sv_expr_natural_width(expr, variables, name_to_id, constants, parameter_types)
                    .map(|natural_width| {
                        context_width.map_or(natural_width, |width| width.max(natural_width))
                    })
                    .or(context_width);
            let float_condition = floating::condition(condition, variables, name_to_id);
            let (condition, mut sources) = lower_expr(
                &float_condition,
                variables,
                name_to_id,
                constants,
                parameter_types,
                arena,
            )?;
            let (mut then_expr, then_sources) = lower_expr_with_context(
                then_expr,
                variables,
                name_to_id,
                constants,
                parameter_types,
                arena,
                arm_context,
                Some(arms_signed),
            )?;
            let (mut else_expr, else_sources) = lower_expr_with_context(
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
            ))
        }
        sv::ir::Expr::Call {
            callee: sv::ir::Callee::Lookup { width, signed },
            args,
            ..
        } => {
            let mut sources = HashSet::default();
            let node = lower_lookup(
                args,
                *width,
                *signed,
                arena,
                &mut |expr, arena, context_width, context_signed| {
                    let (node, expr_sources) = lower_expr_with_context(
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
                    Some(node)
                },
            )?;
            Some((node, sources))
        }
        sv::ir::Expr::Call {
            callee: sv::ir::Callee::Stream(stream),
            args,
            ..
        } => {
            let mut sources = HashSet::default();
            let node = lower_stream(
                args,
                stream,
                arena,
                &mut |expr, arena, context_width, context_signed| {
                    let (node, expr_sources) = lower_expr_with_context(
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
                    Some(node)
                },
            )?;
            Some((
                coerce_node_width(arena, node, context_width, context_signed.unwrap_or(false))
                    .ok()?,
                sources,
            ))
        }
        sv::ir::Expr::Inside { expr, items } => lower_expr_with_context(
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

            for arg in args {
                let arg = arg.as_ref()?;
                let width =
                    sv_expr_natural_width(arg, variables, name_to_id, constants, parameter_types)?;
                let (inner, arg_sources) = lower_expr_with_context(
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
            ))
        }
        sv::ir::Expr::Call { .. } => None,
    }
}

fn select_can_narrow_source_ranges(expr: &sv::ir::Expr) -> bool {
    match expr {
        sv::ir::Expr::Ident(_) => true,
        sv::ir::Expr::Select { expr, .. } => select_can_narrow_source_ranges(expr),
        _ => false,
    }
}

pub(super) fn select_sources<A: std::hash::Hash + Eq + Clone>(
    expr: &sv::ir::Expr,
    sources: HashSet<VarAtomBase<A>>,
    access: BitAccess,
) -> Option<HashSet<VarAtomBase<A>>> {
    if !select_can_narrow_source_ranges(expr) {
        return Some(sources);
    }
    sources
        .into_iter()
        .map(|source| {
            Some(VarAtomBase::new(
                source.id,
                source.access.lsb.checked_add(access.lsb)?,
                source.access.lsb.checked_add(access.msb)?,
            ))
        })
        .collect()
}
