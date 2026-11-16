//! System function calls in expressions.

use super::*;

pub(super) fn eval_system_function_call(
    module: &Module,
    store: &mut ExpressionStore<'_>,
    call: &SystemFunctionCall,
    arena: &mut SLTNodeArena<VarId>,
    context: Option<ValueContext>,
) -> Result<((NodeId, HashSet<VarAtomBase<VarId>>), BoundaryMap<VarId>), ParserError> {
    let context_width = context.map(|context| context.width);
    match &call.kind {
        SystemFunctionKind::Bits(input) => {
            let width = system_function_input_bits_width(module, store.current(), &input.0, arena)?;
            let result = arena.alloc(SLTNode::Constant(
                BigUint::from(width),
                BigUint::from(0u8),
                32,
                false,
            ))?;
            let result = coerce_node_width(
                arena,
                result,
                context_width,
                context.map(|context| context.signed).unwrap_or(false),
            )?;
            Ok(((result, HashSet::default()), HashMap::default()))
        }
        SystemFunctionKind::Size(input, dimension) => {
            let dimension = system_function_dimension(dimension.as_ref()).ok_or_else(|| {
                ParserError::illegal_context(
                    "$size dimension",
                    "the dimension must be a compile-time value",
                    Some(&call.comptime.token),
                )
            })?;
            let size =
                system_function_input_size(module, store.current(), &input.0, arena, dimension)?;
            let result = arena.alloc(SLTNode::Constant(
                BigUint::from(size),
                BigUint::from(0u8),
                32,
                false,
            ))?;
            let result = coerce_node_width(
                arena,
                result,
                context_width,
                context.map(|context| context.signed).unwrap_or(false),
            )?;
            Ok(((result, HashSet::default()), HashMap::default()))
        }
        SystemFunctionKind::Clog2(input) => {
            let ((arg, sources), bounds) =
                eval_expression_in_context(module, store, &input.0, arena, None)?;
            let width = get_width(arg, arena);
            let mut result = arena.alloc(SLTNode::Constant(
                BigUint::from(0u8),
                BigUint::from(0u8),
                32,
                false,
            ))?;
            for k in 1..=width {
                let threshold = arena.alloc(SLTNode::Constant(
                    BigUint::from(1u8) << (k - 1),
                    BigUint::from(0u8),
                    width,
                    false,
                ))?;
                let cond = arena.alloc(SLTNode::Binary(arg, BinaryOp::GtU, threshold))?;
                let value = arena.alloc(SLTNode::Constant(
                    BigUint::from(k),
                    BigUint::from(0u8),
                    32,
                    false,
                ))?;
                result = arena.alloc(SLTNode::Mux {
                    cond,
                    then_expr: value,
                    else_expr: result,
                })?;
            }
            let result = coerce_node_width(
                arena,
                result,
                context_width,
                context.map(|context| context.signed).unwrap_or(false),
            )?;
            Ok(((result, sources), bounds))
        }
        SystemFunctionKind::Onehot(input) => {
            let ((arg, sources), bounds) =
                eval_expression_in_context(module, store, &input.0, arena, None)?;
            let width = get_width(arg, arena);
            let zero = arena.alloc(SLTNode::Constant(
                BigUint::from(0u8),
                BigUint::from(0u8),
                width,
                false,
            ))?;
            let one = arena.alloc(SLTNode::Constant(
                BigUint::from(1u8),
                BigUint::from(0u8),
                width,
                false,
            ))?;
            let arg_minus_one = arena.alloc(SLTNode::Binary(arg, BinaryOp::Sub, one))?;
            let overlap = arena.alloc(SLTNode::Binary(arg, BinaryOp::And, arg_minus_one))?;
            let non_zero = arena.alloc(SLTNode::Binary(arg, BinaryOp::Ne, zero))?;
            let no_overlap = arena.alloc(SLTNode::Binary(overlap, BinaryOp::Eq, zero))?;
            let result = arena.alloc(SLTNode::Binary(non_zero, BinaryOp::LogicAnd, no_overlap))?;
            let result = coerce_node_width(
                arena,
                result,
                context_width,
                context.map(|context| context.signed).unwrap_or(false),
            )?;
            Ok(((result, sources), bounds))
        }
        SystemFunctionKind::Signed(input) | SystemFunctionKind::Unsigned(input) => {
            let ((arg, sources), bounds) =
                eval_expression_in_context(module, store, &input.0, arena, None)?;
            let signed = matches!(call.kind, SystemFunctionKind::Signed(_));
            Ok((
                (
                    coerce_node_width(
                        arena,
                        arg,
                        context_width,
                        context.map(|context| context.signed).unwrap_or(signed),
                    )?,
                    sources,
                ),
                bounds,
            ))
        }
        _ => Err(ParserError::illegal_context(
            "system function call in comb expression",
            format!("module `{}`: {call}", module.name),
            Some(&call.comptime.token),
        )),
    }
}

pub(super) fn system_function_type_bits_width(ty: &Type) -> Option<usize> {
    ty.total_width()
        .map(|width| width * ty.total_array().unwrap_or(1))
}

pub(super) fn system_function_input_bits_width(
    module: &Module,
    store: &SymbolicStore<VarId>,
    expr: &Expression,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<usize, ParserError> {
    let comptime = expr.comptime();
    match &comptime.value {
        ValueVariant::Type(ty) => Ok(system_function_type_bits_width(ty).unwrap_or(0)),
        _ => match system_function_type_bits_width(&comptime.r#type) {
            Some(width) => Ok(width),
            None => eval_expression(module, store, expr, arena, None)
                .map(|((node, _), _)| get_width(node, arena)),
        },
    }
}

pub(super) fn system_function_input_size(
    module: &Module,
    store: &SymbolicStore<VarId>,
    expr: &Expression,
    arena: &mut SLTNodeArena<VarId>,
    dimension: usize,
) -> Result<usize, ParserError> {
    let comptime = expr.comptime();
    match &comptime.value {
        ValueVariant::Type(ty) => Ok(system_function_type_size(ty, dimension).unwrap_or(0)),
        _ => match system_function_type_size(&comptime.r#type, dimension) {
            Some(size) => Ok(size),
            // Only the leading dimension can fall back to the evaluated
            // expression's own width.
            None if dimension == 1 => eval_expression(module, store, expr, arena, None)
                .map(|((node, _), _)| get_width(node, arena)),
            None => Ok(0),
        },
    }
}
