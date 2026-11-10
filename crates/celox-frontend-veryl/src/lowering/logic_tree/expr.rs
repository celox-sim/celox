use super::*;

use crate::{
    bitaccess::{celox_value_from_comptime, celox_value_from_comptime_in_context},
    context_width::{
        ValueContext, binary_semantics, cast_semantics, expression_signed, get_expr_width,
        resolve_binary_op, system_function_dimension, system_function_type_size,
    },
};
use num_traits::ToPrimitive as _;
use veryl_analyzer::ir::{CasePattern, Type, ValueVariant};

use super::state::{FunctionControlState, FunctionLoopControlState};

mod array_literal;
mod case_condition;
mod function_body;
mod system_function;
pub(crate) use array_literal::*;
pub(in crate::lowering::logic_tree) use case_condition::*;
pub(in crate::lowering::logic_tree) use function_body::*;
use system_function::*;

pub(super) enum ExpressionStore<'a> {
    ReadOnly(&'a SymbolicStore<VarId>),
    Effectful(&'a mut SymbolicStore<VarId>),
}

impl ExpressionStore<'_> {
    pub(super) fn current(&self) -> &SymbolicStore<VarId> {
        match self {
            Self::ReadOnly(store) => store,
            Self::Effectful(store) => store,
        }
    }

    fn effectful_mut(&mut self) -> Option<&mut SymbolicStore<VarId>> {
        match self {
            Self::ReadOnly(_) => None,
            Self::Effectful(store) => Some(store),
        }
    }
}

fn eval_function_call_expression(
    module: &Module,
    store: &mut ExpressionStore<'_>,
    call: &veryl_analyzer::ir::FunctionCall,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<((NodeId, HashSet<VarAtomBase<VarId>>), BoundaryMap<VarId>), ParserError> {
    let Some(function) = module.functions.get(&call.id) else {
        return Err(ParserError::internal(
            "function call",
            format!("unknown function id: {:?}", call.id),
            Some(&call.comptime.token),
        ));
    };

    let Some(function_body) = (if let Some(index) = &call.index {
        function.get_function(index)
    } else {
        function.get_function(&[])
    }) else {
        return Err(ParserError::internal(
            "function call specialization",
            format!("{call}"),
            Some(&call.comptime.token),
        ));
    };

    let Some(ret_id) = function_body.ret else {
        return Err(ParserError::illegal_context(
            "void function call in comb expression",
            format!("{call}"),
            Some(&call.comptime.token),
        ));
    };

    let mut arg_bounds = BoundaryMap::default();
    let mut evaluated_inputs = Vec::with_capacity(call.inputs.len());
    for (arg_id, arg_expr) in super::ordered_function_inputs(function, &function_body, call)? {
        let Some(arg_var) = module.variables.get(&arg_id) else {
            return Err(ParserError::internal(
                "function argument variable",
                format!("unknown arg id: {:?}", arg_id),
                Some(&call.comptime.token),
            ));
        };
        let arg_width = resolve_total_width(module, arg_var)?;
        let value = eval_function_input_assignment_in_store(
            module,
            store,
            arg_expr,
            &arg_var.r#type,
            arg_width,
            arena,
        )?;
        let ((arg_node, sources), bounds) = value;
        let arg_node = if arg_var.r#type.is_2state() && !arg_expr.comptime().r#type.is_2state() {
            arena.alloc(SLTNode::Unary(UnaryOp::ToTwoState, arg_node))?
        } else {
            arg_node
        };
        arg_bounds = merge_boundaries(arg_bounds, bounds);
        evaluated_inputs.push((arg_id, arg_node, sources, arg_width));
    }

    for arg_path in function_body.arg_map.keys() {
        if !crate::function_call_has_arg(&call.inputs, arg_path)
            && !crate::function_call_has_arg(&call.outputs, arg_path)
        {
            return Err(super::invalid_function_call_argument_error(
                function,
                arg_path,
                "formal argument has neither an input expression nor an output destination",
                call,
            ));
        }
    }

    let mut local_store = store.current().fork();
    for (arg_id, arg_node, sources, arg_width) in evaluated_inputs {
        local_store.insert(
            arg_id,
            RangeStore::new(Some((arg_node, sources)), arg_width),
        );
    }

    let ((ret_node, ret_sources), ret_bounds, final_local_store) =
        eval_function_body_return(module, &local_store, &function_body, ret_id, arena)?;

    let output_bounds = if let Some(caller_store) = store.effectful_mut() {
        let current_store = std::mem::take(caller_store);
        let (next_store, bounds) = super::apply_function_call_outputs(
            module,
            function,
            current_store,
            BoundaryMap::default(),
            call,
            &function_body,
            &final_local_store,
            arena,
        )?;
        *caller_store = next_store;
        bounds
    } else {
        BoundaryMap::default()
    };

    Ok((
        (ret_node, ret_sources),
        merge_boundaries(arg_bounds, merge_boundaries(ret_bounds, output_bounds)),
    ))
}

pub fn eval_expression(
    module: &Module,
    store: &SymbolicStore<VarId>,
    expr: &Expression,
    arena: &mut SLTNodeArena<VarId>,
    context_width: Option<usize>,
) -> Result<((NodeId, HashSet<VarAtomBase<VarId>>), BoundaryMap<VarId>), ParserError> {
    let context = context_width.map(|width| ValueContext {
        width,
        signed: expression_signed(expr),
    });
    let mut store = ExpressionStore::ReadOnly(store);
    eval_expression_in_context(module, &mut store, expr, arena, context)
}

pub(crate) fn eval_expression_effectful(
    module: &Module,
    store: &mut SymbolicStore<VarId>,
    expr: &Expression,
    arena: &mut SLTNodeArena<VarId>,
    context_width: Option<usize>,
) -> Result<((NodeId, HashSet<VarAtomBase<VarId>>), BoundaryMap<VarId>), ParserError> {
    let context = context_width.map(|width| ValueContext {
        width,
        signed: expression_signed(expr),
    });
    let mut store = ExpressionStore::Effectful(store);
    eval_expression_in_context(module, &mut store, expr, arena, context)
}

pub(super) fn eval_expression_in_context(
    module: &Module,
    store: &mut ExpressionStore<'_>,
    expr: &Expression,
    arena: &mut SLTNodeArena<VarId>,
    context: Option<ValueContext>,
) -> Result<((NodeId, HashSet<VarAtomBase<VarId>>), BoundaryMap<VarId>), ParserError> {
    let context = if crate::float::precision(crate::float::type_of(expr)).is_some() {
        None
    } else {
        context
    };
    let context_width = context.map(|context| context.width);
    // Short-circuit: compile-time constant compound expression → emit Constant node.
    // The folded value still participates in its enclosing width context.  Skipping
    // that coercion used to produce mismatched wildcard operands for enum cases and
    // can also lose carry bits when a folded operand is widened by its parent.
    if !matches!(expr, Expression::Term(_)) {
        let ct = expr.comptime();
        if ct.is_const
            && !crate::float::needs_numeric_lowering(expr)
            && crate::float::type_of(expr).kind == ct.r#type.kind
        {
            if let Some((celox_value, mask_xz, width, signed)) =
                celox_value_from_comptime_in_context(ct, context_width)
            {
                let node = arena.alloc(SLTNode::Constant(celox_value, mask_xz, width, signed))?;
                let node = coerce_node_width(
                    arena,
                    node,
                    context_width,
                    context.map(|context| context.signed).unwrap_or(signed),
                )?;
                return Ok(((node, HashSet::default()), BoundaryMap::default()));
            }
        }
    }

    match expr {
        Expression::Term(factor) => eval_factor(module, store, factor, arena, context),
        Expression::Binary(lhs, op, rhs, _) => eval_binary_expression(
            module,
            store,
            expr,
            lhs,
            op,
            rhs,
            arena,
            context,
            context_width,
        ),
        Expression::Concatenation(exprs, _) => {
            eval_concatenation_expression(module, store, exprs, arena, context, context_width)
        }
        Expression::Unary(op, expr, _) => {
            eval_unary_expression(module, store, op, expr, arena, context, context_width)
        }
        Expression::Ternary(cond, then_expr, else_expr, _) => eval_ternary_expression(
            module,
            store,
            expr,
            cond,
            then_expr,
            else_expr,
            arena,
            context,
            context_width,
        ),
        Expression::StructConstructor(ty, fields, _) => eval_struct_constructor_expression(
            module,
            store,
            ty,
            fields,
            arena,
            context,
            context_width,
        ),
        Expression::ArrayLiteral(items, _) => {
            let ((result, sources), bounds) =
                eval_array_literal_expression_in_store(module, store, items, context_width, arena)?;
            let result = coerce_node_width(
                arena,
                result,
                context_width,
                context.map(|context| context.signed).unwrap_or(false),
            )?;
            Ok(((result, sources), bounds))
        }
    }
}

fn eval_binary_expression(
    module: &Module,
    store: &mut ExpressionStore<'_>,
    expr: &Expression,
    lhs: &Expression,
    op: &Op,
    rhs: &Expression,
    arena: &mut SLTNodeArena<VarId>,
    context: Option<ValueContext>,
    context_width: Option<usize>,
) -> Result<((NodeId, HashSet<VarAtomBase<VarId>>), BoundaryMap<VarId>), ParserError> {
    if matches!(op, Op::As) {
        if let Some(conversion) =
            crate::float::conversion(crate::float::type_of(lhs), crate::float::type_of(expr))
        {
            let ((node, sources), bounds) =
                eval_expression_in_context(module, store, lhs, arena, None)?;
            let node = arena.alloc(SLTNode::Unary(conversion, node))?;
            return Ok(((node, sources), bounds));
        }
        let Some(cast) = cast_semantics(lhs, rhs) else {
            return Err(ParserError::internal(
                "as cast target",
                format!("{:?}", rhs),
                Some(&rhs.token_range()),
            ));
        };
        let ((l_expr, l_sources), l_bounds) = eval_expression_in_context(
            module,
            store,
            lhs,
            arena,
            Some(ValueContext {
                width: cast.width,
                signed: cast.source_signed,
            }),
        )?;

        // For RHS, if it's a type or numeric width, we don't evaluate it as an expression.
        let r_bounds = if let Expression::Term(f) = rhs {
            if let Factor::Value(v) = f.as_ref() {
                if matches!(v.value, ValueVariant::Type(_) | ValueVariant::Numeric(_)) {
                    BoundaryMap::default()
                } else {
                    eval_expression_in_context(module, store, rhs, arena, None)?.1
                }
            } else {
                eval_expression_in_context(module, store, rhs, arena, None)?.1
            }
        } else {
            eval_expression_in_context(module, store, rhs, arena, None)?.1
        };

        // Resize uses the source signedness.  Reinterpretation changes
        // the signedness seen by the enclosing context, not the bits.
        let cast_value = coerce_node_width(arena, l_expr, Some(cast.width), cast.source_signed)?;
        let cast_value = if cast.result_is_2state && !cast.source_is_2state {
            arena.alloc(SLTNode::Unary(UnaryOp::ToTwoState, cast_value))?
        } else {
            cast_value
        };
        let result_node = coerce_node_width(
            arena,
            cast_value,
            context_width,
            context
                .map(|context| context.signed)
                .unwrap_or(cast.result_signed),
        )?;
        return Ok((
            (result_node, l_sources),
            merge_boundaries(l_bounds, r_bounds),
        ));
    }

    if crate::float::precision(crate::float::type_of(lhs)).is_some()
        || crate::float::precision(crate::float::type_of(rhs)).is_some()
    {
        let target = if matches!(
            crate::float::precision(crate::float::type_of(lhs)),
            Some(celox_design::FloatPrecision::F64)
        ) {
            crate::float::type_of(lhs)
        } else if crate::float::precision(crate::float::type_of(rhs)).is_some() {
            crate::float::type_of(rhs)
        } else {
            crate::float::type_of(lhs)
        };
        let precision = crate::float::precision(target).unwrap();
        let left = crate::float::converted(lhs, target);
        let right = crate::float::converted(rhs, target);
        let ((left, mut sources), lb) =
            eval_expression_in_context(module, store, &left, arena, None)?;
        let ((right, rs), rb) = eval_expression_in_context(module, store, &right, arena, None)?;
        sources.extend(rs);
        let op = crate::float::binary(*op).expect("validated floating-point operator");
        let node = arena.alloc(SLTNode::Binary(
            left,
            BinaryOp::Float { op, precision },
            right,
        ))?;
        return Ok(((node, sources), merge_boundaries(lb, rb)));
    }

    let lhs_width = get_expr_width(lhs)
        .or_else(|| lhs.comptime().r#type.total_width())
        .unwrap_or(1);
    let rhs_width = get_expr_width(rhs)
        .or_else(|| rhs.comptime().r#type.total_width())
        .unwrap_or(1);
    let lhs_signed = expression_signed(lhs);
    let rhs_signed = expression_signed(rhs);
    let semantics = binary_semantics(*op, lhs_width, rhs_width, lhs_signed, rhs_signed, context);

    if matches!(op, Op::Pow) {
        let ((l_expr, l_sources), l_bounds) =
            eval_expression_in_context(module, store, lhs, arena, semantics.lhs_context)?;
        let ((r_expr, r_sources), r_bounds) =
            eval_expression_in_context(module, store, rhs, arena, semantics.rhs_context)?;
        let result_node = celox_frontend_core::symbolic::pow::lower_runtime_pow(
            arena,
            l_expr,
            r_expr,
            semantics.result_width,
            rhs_signed,
            semantics.result_signed,
        )?;
        let result_node = coerce_node_width(
            arena,
            result_node,
            context_width,
            context
                .map(|context| context.signed)
                .unwrap_or(semantics.result_signed),
        )?;
        let mut sources = l_sources;
        sources.extend(r_sources);
        return Ok(((result_node, sources), merge_boundaries(l_bounds, r_bounds)));
    }
    let ((l_expr, l_sources), l_bounds) =
        eval_expression_in_context(module, store, lhs, arena, semantics.lhs_context)?;
    let ((r_expr, r_sources), r_bounds) = if store.effectful_mut().is_some()
        && matches!(op, Op::LogicAnd | Op::LogicOr)
    {
        let base_store = store.current().fork();
        let mut rhs_store = base_store.fork();
        let mut rhs_state = ExpressionStore::Effectful(&mut rhs_store);
        let rhs_value =
            eval_expression_in_context(module, &mut rhs_state, rhs, arena, semantics.rhs_context)?;
        // Only a definite dominant value skips the RHS.  Map the
        // four-state shortcut predicate through ToTwoState so X/Z
        // continues into the RHS, matching the FF lowering.
        let not_lhs = arena.alloc(SLTNode::Unary(UnaryOp::LogicNot, l_expr))?;
        let shortcut_truth = if matches!(op, Op::LogicAnd) {
            not_lhs
        } else {
            arena.alloc(SLTNode::Unary(UnaryOp::LogicNot, not_lhs))?
        };
        let shortcut = arena.alloc(SLTNode::Unary(UnaryOp::ToTwoState, shortcut_truth))?;
        let execute_rhs = arena.alloc(SLTNode::Unary(UnaryOp::LogicNot, shortcut))?;
        let merged_store = super::merge_symbolic_versions(
            module,
            &rhs_store,
            &base_store,
            execute_rhs,
            &l_sources,
            arena,
        )?;
        *store
            .effectful_mut()
            .expect("effectful binary expression must retain a mutable store") = merged_store;
        rhs_value
    } else {
        eval_expression_in_context(module, store, rhs, arena, semantics.rhs_context)?
    };

    let mut sources = l_sources;
    sources.extend(r_sources);

    // BitXnor/BitNand/BitNor は既存演算に分解
    let result_node = match op {
        Op::BitXnor => {
            let xor_node = arena.alloc(SLTNode::Binary(l_expr, BinaryOp::Xor, r_expr))?;
            arena.alloc(SLTNode::Unary(UnaryOp::BitNot, xor_node))?
        }
        Op::BitNand => {
            let and_node = arena.alloc(SLTNode::Binary(l_expr, BinaryOp::And, r_expr))?;
            arena.alloc(SLTNode::Unary(UnaryOp::BitNot, and_node))?
        }
        Op::BitNor => {
            let or_node = arena.alloc(SLTNode::Binary(l_expr, BinaryOp::Or, r_expr))?;
            arena.alloc(SLTNode::Unary(UnaryOp::BitNot, or_node))?
        }
        Op::Sub => {
            let bin_op = resolve_binary_op(*op, semantics.lhs_signed, semantics.rhs_signed);
            let sub_node = arena.alloc(SLTNode::Binary(l_expr, bin_op, r_expr))?;
            let width = {
                let lw = get_width(l_expr, arena);
                let rw = get_width(r_expr, arena);
                lw.max(rw)
            };
            arena.alloc(SLTNode::Slice {
                expr: sub_node,
                access: BitAccess::new(0, width - 1),
            })?
        }
        _ => {
            let bin_op = resolve_binary_op(*op, semantics.lhs_signed, semantics.rhs_signed);
            arena.alloc(SLTNode::Binary(l_expr, bin_op, r_expr))?
        }
    };

    let result_node = coerce_node_width(
        arena,
        result_node,
        Some(semantics.result_width),
        semantics.result_signed,
    )?;
    let result_node = coerce_node_width(
        arena,
        result_node,
        context_width,
        context
            .map(|context| context.signed)
            .unwrap_or(semantics.result_signed),
    )?;

    Ok(((result_node, sources), merge_boundaries(l_bounds, r_bounds)))
}

fn eval_concatenation_expression(
    module: &Module,
    store: &mut ExpressionStore<'_>,
    exprs: &[(Expression, Option<Expression>)],
    arena: &mut SLTNodeArena<VarId>,
    context: Option<ValueContext>,
    context_width: Option<usize>,
) -> Result<((NodeId, HashSet<VarAtomBase<VarId>>), BoundaryMap<VarId>), ParserError> {
    let mut parts = Vec::new();
    let mut all_bounds = BoundaryMap::default();
    let mut total_sources = HashSet::default();

    for (sub_expr, repeat) in exprs {
        let ((part_expr, part_sources), p_bounds) =
            eval_expression_in_context(module, store, sub_expr, arena, None)?;
        all_bounds = merge_boundaries(all_bounds, p_bounds);
        let width = get_width(part_expr, arena);

        total_sources.extend(part_sources);

        let rep_count = if let Some(rep_expr) = repeat {
            let v = eval_constexpr(rep_expr);
            v.ok_or_else(|| {
                ParserError::illegal_context(
                    "concatenation non-constant repeat",
                    format!("{:?}", rep_expr),
                    Some(&rep_expr.token_range()),
                )
            })?
            .iter_u64_digits()
            .next()
            .unwrap()
        } else {
            1
        };
        for _ in 0..rep_count {
            parts.push((part_expr, width));
        }
    }
    let result = arena.alloc(SLTNode::Concat(parts))?;
    let result = coerce_node_width(
        arena,
        result,
        context_width,
        context.map(|context| context.signed).unwrap_or(false),
    )?;
    Ok(((result, total_sources), all_bounds))
}

fn eval_unary_expression(
    module: &Module,
    store: &mut ExpressionStore<'_>,
    op: &Op,
    expr: &Expression,
    arena: &mut SLTNodeArena<VarId>,
    context: Option<ValueContext>,
    context_width: Option<usize>,
) -> Result<((NodeId, HashSet<VarAtomBase<VarId>>), BoundaryMap<VarId>), ParserError> {
    if let Some(precision) = crate::float::precision(crate::float::type_of(expr)) {
        let ((node, sources), bounds) =
            eval_expression_in_context(module, store, expr, arena, None)?;
        let node = if *op == Op::Sub {
            arena.alloc(SLTNode::Unary(
                UnaryOp::Float {
                    op: celox_design::FloatUnaryOp::Neg,
                    precision,
                },
                node,
            ))?
        } else {
            node
        };
        return Ok(((node, sources), bounds));
    }
    let result_signed = expression_signed(expr);
    let operand_context = if matches!(
        op,
        Op::BitAnd | Op::BitNand | Op::BitOr | Op::BitNor | Op::BitXor | Op::BitXnor | Op::LogicNot
    ) {
        None
    } else {
        context
    };
    let ((expr, sources), bounds) =
        eval_expression_in_context(module, store, expr, arena, operand_context)?;
    // Reduction Nand/Nor/Xnor は既存のリダクション + Not に分解
    let result_node = match op {
        Op::BitNand => {
            let and_node = arena.alloc(SLTNode::Unary(UnaryOp::And, expr))?;
            arena.alloc(SLTNode::Unary(UnaryOp::LogicNot, and_node))?
        }
        Op::BitNor => {
            let or_node = arena.alloc(SLTNode::Unary(UnaryOp::Or, expr))?;
            arena.alloc(SLTNode::Unary(UnaryOp::LogicNot, or_node))?
        }
        Op::BitXnor => {
            let xor_node = arena.alloc(SLTNode::Unary(UnaryOp::Xor, expr))?;
            arena.alloc(SLTNode::Unary(UnaryOp::LogicNot, xor_node))?
        }
        _ => arena.alloc(SLTNode::Unary(convert_unary_op(op), expr))?,
    };
    let result_node = coerce_node_width(
        arena,
        result_node,
        context_width,
        context
            .map(|context| context.signed)
            .unwrap_or(result_signed),
    )?;
    Ok(((result_node, sources), bounds))
}

fn eval_ternary_expression(
    module: &Module,
    store: &mut ExpressionStore<'_>,
    expr: &Expression,
    cond: &Expression,
    then_expr: &Expression,
    else_expr: &Expression,
    arena: &mut SLTNodeArena<VarId>,
    context: Option<ValueContext>,
    context_width: Option<usize>,
) -> Result<((NodeId, HashSet<VarAtomBase<VarId>>), BoundaryMap<VarId>), ParserError> {
    let target_type = crate::float::type_of(expr);
    let then_value = crate::float::converted(then_expr, target_type);
    let else_value = crate::float::converted(else_expr, target_type);
    let then_expr = &then_value;
    let else_expr = &else_value;
    let branch_width = get_expr_width(then_expr)
        .or_else(|| then_expr.comptime().r#type.total_width())
        .unwrap_or(1)
        .max(
            get_expr_width(else_expr)
                .or_else(|| else_expr.comptime().r#type.total_width())
                .unwrap_or(1),
        )
        .max(context_width.unwrap_or(0));
    let branch_context = Some(ValueContext {
        width: branch_width,
        signed: context
            .map(|context| context.signed)
            .unwrap_or_else(|| expression_signed(then_expr) && expression_signed(else_expr)),
    });
    let ((cond_expr, cond_sources), cond_bounds) =
        eval_expression_in_context(module, store, cond, arena, None)?;
    let (((then_expr, then_sources), then_bounds), ((else_expr, else_sources), else_bounds)) =
        if store.effectful_mut().is_some() {
            let base_store = store.current().fork();
            let mut then_store = base_store.fork();
            let mut then_state = ExpressionStore::Effectful(&mut then_store);
            let then_value = eval_expression_in_context(
                module,
                &mut then_state,
                then_expr,
                arena,
                branch_context,
            )?;

            // A known condition evaluates only its selected arm. An X/Z
            // condition evaluates both arms in order, so the else arm must
            // observe writes made by the then arm.
            let not_cond = arena.alloc(SLTNode::Unary(UnaryOp::LogicNot, cond_expr))?;
            let known_false = arena.alloc(SLTNode::Unary(UnaryOp::ToTwoState, not_cond))?;
            let true_truth = arena.alloc(SLTNode::Unary(UnaryOp::LogicNot, not_cond))?;
            let known_true = arena.alloc(SLTNode::Unary(UnaryOp::ToTwoState, true_truth))?;
            let execute_then = arena.alloc(SLTNode::Unary(UnaryOp::LogicNot, known_false))?;
            let after_then = super::merge_symbolic_versions(
                module,
                &then_store,
                &base_store,
                execute_then,
                &cond_sources,
                arena,
            )?;

            let mut else_store = after_then.clone();
            let mut else_state = ExpressionStore::Effectful(&mut else_store);
            let else_value = eval_expression_in_context(
                module,
                &mut else_state,
                else_expr,
                arena,
                branch_context,
            )?;

            let execute_else = arena.alloc(SLTNode::Unary(UnaryOp::LogicNot, known_true))?;
            let final_store = super::merge_symbolic_versions(
                module,
                &else_store,
                &after_then,
                execute_else,
                &cond_sources,
                arena,
            )?;
            *store
                .effectful_mut()
                .expect("effectful ternary must retain a mutable store") = final_store;
            (then_value, else_value)
        } else {
            (
                eval_expression_in_context(module, store, then_expr, arena, branch_context)?,
                eval_expression_in_context(module, store, else_expr, arena, branch_context)?,
            )
        };

    let mut sources = cond_sources;
    sources.extend(then_sources);
    sources.extend(else_sources);

    Ok((
        (
            arena.alloc(SLTNode::Mux {
                cond: cond_expr,
                then_expr,
                else_expr,
            })?,
            sources,
        ),
        merge_boundaries(cond_bounds, merge_boundaries(then_bounds, else_bounds)),
    ))
}

fn eval_struct_constructor_expression(
    module: &Module,
    store: &mut ExpressionStore<'_>,
    ty: &Type,
    fields: &[(resource_table::StrId, Expression)],
    arena: &mut SLTNodeArena<VarId>,
    context: Option<ValueContext>,
    context_width: Option<usize>,
) -> Result<((NodeId, HashSet<VarAtomBase<VarId>>), BoundaryMap<VarId>), ParserError> {
    let mut parts = Vec::new();
    let mut all_bounds = BoundaryMap::default();
    let mut total_sources = HashSet::default();

    for (name, field_expr) in fields {
        let Some(member_type) = ty.get_member_type(*name) else {
            return Err(ParserError::internal(
                "struct constructor member",
                format!("unknown member: {:?} in {:?}", name, ty),
                Some(&field_expr.token_range()),
            ));
        };
        let Some(member_width) = member_type.total_width() else {
            return Err(ParserError::internal(
                "struct constructor member width",
                format!("member: {:?}, type: {:?}", name, member_type),
                Some(&field_expr.token_range()),
            ));
        };

        let ((part_expr, part_sources), p_bounds) = eval_expression_in_context(
            module,
            store,
            field_expr,
            arena,
            Some(ValueContext {
                width: member_width,
                signed: expression_signed(field_expr),
            }),
        )?;
        all_bounds = merge_boundaries(all_bounds, p_bounds);
        total_sources.extend(part_sources);
        let part_expr = coerce_node_width(
            arena,
            part_expr,
            Some(member_width),
            expression_signed(field_expr),
        )?;
        let part_expr = if member_type.is_2state() && !field_expr.comptime().r#type.is_2state() {
            arena.alloc(SLTNode::Unary(UnaryOp::ToTwoState, part_expr))?
        } else {
            part_expr
        };

        parts.push((part_expr, member_width));
    }

    let result = arena.alloc(SLTNode::Concat(parts))?;
    let result = coerce_node_width(
        arena,
        result,
        context_width,
        context.map(|context| context.signed).unwrap_or(ty.signed),
    )?;
    Ok(((result, total_sources), all_bounds))
}

pub(crate) fn eval_assignment_expression_effectful(
    module: &Module,
    store: &mut SymbolicStore<VarId>,
    expr: &Expression,
    arena: &mut SLTNodeArena<VarId>,
    target_width: usize,
) -> Result<((NodeId, HashSet<VarAtomBase<VarId>>), BoundaryMap<VarId>), ParserError> {
    if target_width == 0 {
        return Err(ParserError::illegal_context(
            "assignment expression",
            "target width must be nonzero",
            Some(&expr.token_range()),
        ));
    }

    let mut store = ExpressionStore::Effectful(store);
    eval_assignment_expression_in_store(module, &mut store, expr, arena, target_width)
}

fn eval_assignment_expression_in_store(
    module: &Module,
    store: &mut ExpressionStore<'_>,
    expr: &Expression,
    arena: &mut SLTNodeArena<VarId>,
    target_width: usize,
) -> Result<((NodeId, HashSet<VarAtomBase<VarId>>), BoundaryMap<VarId>), ParserError> {
    let context = Some(ValueContext {
        width: target_width,
        signed: expression_signed(expr),
    });
    let value = eval_expression_in_context(module, store, expr, arena, context)?;
    finish_assignment_expression(expr, arena, target_width, value)
}

fn finish_assignment_expression(
    expr: &Expression,
    arena: &mut SLTNodeArena<VarId>,
    target_width: usize,
    ((node, sources), boundaries): ((NodeId, HashSet<VarAtomBase<VarId>>), BoundaryMap<VarId>),
) -> Result<((NodeId, HashSet<VarAtomBase<VarId>>), BoundaryMap<VarId>), ParserError> {
    let source_width = get_width(node, arena);
    if source_width == 0 {
        return Err(ParserError::illegal_context(
            "assignment expression",
            "a zero-width expression cannot be assigned",
            Some(&expr.token_range()),
        ));
    }

    // The RHS expression controls extension.  The destination type does not
    // turn an unsigned value into a sign-extended one.  Explicit casts and the
    // signed/unsigned system functions are retained by the override helper.
    let signed = expression_signed(expr);
    let node = coerce_node_width(arena, node, Some(target_width), signed)?;
    Ok(((node, sources), boundaries))
}

fn eval_factor(
    module: &Module,
    store: &mut ExpressionStore<'_>,
    factor: &Factor,
    arena: &mut SLTNodeArena<VarId>,
    context: Option<ValueContext>,
) -> Result<((NodeId, HashSet<VarAtomBase<VarId>>), BoundaryMap<VarId>), ParserError> {
    let context_width = context.map(|context| context.width);
    let context_signed = context
        .map(|context| context.signed)
        .unwrap_or_else(|| crate::context_width::factor_signed(factor));
    match factor {
        Factor::Variable(var_id, index, select, comptime) => {
            // Compile-time constant (e.g. genvar inside generate block): emit a
            // constant node directly instead of loading from memory.
            // Also handles constant[const_index] (e.g. IDX[p] in generate loops).
            if comptime.is_const {
                let is_bare = index.0.is_empty() && select.0.is_empty() && select.1.is_none();
                let is_static_sel = !is_bare && crate::bitaccess::is_static_access(index, select);

                let constant = if is_bare {
                    celox_value_from_comptime_in_context(comptime, context_width)
                } else {
                    celox_value_from_comptime(comptime)
                };
                if (is_bare || is_static_sel)
                    && let Some((celox_value, mask_xz, full_width, signed)) = constant
                {
                    let (val, mask, width) = if is_bare {
                        (celox_value, mask_xz, full_width)
                    } else {
                        // Evaluate the static bit-select on the constant value
                        let access = eval_var_select(module, *var_id, index, select)?;
                        let extracted_width = access.msb - access.lsb + 1;
                        let extracted_val = (&celox_value >> access.lsb)
                            & ((BigUint::from(1u64) << extracted_width) - BigUint::from(1u64));
                        let extracted_mask = (&mask_xz >> access.lsb)
                            & ((BigUint::from(1u64) << extracted_width) - BigUint::from(1u64));
                        (extracted_val, extracted_mask, extracted_width)
                    };

                    let expr =
                        arena.alloc(SLTNode::Constant(val, mask, width, signed && is_bare))?;
                    let expr =
                        coerce_node_width(arena, expr, context_width, context_signed && is_bare)?;
                    return Ok(((expr, HashSet::default()), BoundaryMap::default()));
                }
            }

            let is_static_access = crate::bitaccess::is_static_access(index, select);
            if is_static_access {
                let access = eval_var_select(module, *var_id, index, select)?;

                let mut bounds = BoundaryMap::default();
                let access_end = access.msb.checked_add(1).ok_or_else(|| {
                    ParserError::illegal_context(
                        "static variable read",
                        "source boundary overflows usize",
                        Some(&comptime.token),
                    )
                })?;
                let var_bounds = bounds.entry(*var_id).or_default();
                var_bounds.insert(access.lsb);
                var_bounds.insert(access_end);

                let (expr, sources) = if let Some(range_store) = store.current().get(var_id) {
                    let parts = range_store.get_parts_ref(access).map_err(|error| {
                        super::range_store_error(
                            "static variable read",
                            error,
                            Some(&comptime.token),
                        )
                    })?;
                    // If any requested part is unassigned, the result still
                    // depends on that range's process input value.
                    let has_unassigned = parts.iter().any(|(val, _)| val.is_none());
                    let (expr, mut sources) =
                        combine_parts_with_default(*var_id, access.lsb, parts, arena)?;
                    if has_unassigned {
                        sources.insert(VarAtomBase::new(*var_id, access.lsb, access.msb));
                    }
                    (expr, sources)
                } else {
                    // Read-only variables have no mutable symbolic definition.
                    // Materialize their input range directly instead.
                    let expr = arena.alloc(SLTNode::Input {
                        variable: *var_id,
                        signed: false,
                        index: vec![],
                        access,
                    })?;
                    let mut sources = HashSet::default();
                    sources.insert(VarAtomBase::new(*var_id, access.lsb, access.msb));
                    (expr, sources)
                };
                // Symbolic substitution may replace this variable with an
                // expression whose producer has different signedness. Width
                // coercion follows the source variable's analyzer context,
                // not the provenance of the substituted SLT node.
                let expr = coerce_node_width(arena, expr, context_width, context_signed)?;
                Ok(((expr, sources), bounds))
            } else {
                let mut all_sources = HashSet::default();

                let var = &module.variables[var_id];
                let width = resolve_total_width(module, var)?;
                if width == 0 {
                    return Err(ParserError::illegal_context(
                        "dynamic variable read",
                        "source variable has zero width",
                        Some(&comptime.token),
                    ));
                }
                let DynamicSelectOffset {
                    node: offset_node,
                    indices: dynamic_indices,
                    sources: offset_sources,
                    boundaries: all_bounds,
                    indices_valid,
                    valid,
                    clamp,
                    partial,
                    ..
                } = super::eval_dynamic_select_offset(
                    module,
                    store,
                    *var_id,
                    index,
                    select,
                    arena,
                    Some(&comptime.token),
                )?;
                all_sources.extend(offset_sources);

                // 2. Check SymbolicStore to determine if "already written"
                let access_full = BitAccess::new(0, width - 1);
                let parts = store
                    .current()
                    .get(var_id)
                    .map(|range_store| {
                        range_store.get_parts_ref(access_full).map_err(|error| {
                            super::range_store_error(
                                "dynamic variable read",
                                error,
                                Some(&comptime.token),
                            )
                        })
                    })
                    .transpose()?;
                let is_unmodified = parts
                    .as_ref()
                    .is_none_or(|parts| parts.iter().all(|(val, _)| val.is_none()));

                let element_width =
                    crate::bitaccess::get_access_width(module, *var_id, index, select)?;
                if element_width == 0 || element_width > width {
                    return Err(ParserError::illegal_context(
                        "dynamic variable read",
                        format!("selected width {element_width} must be in 1..={width}"),
                        Some(&comptime.token),
                    ));
                }

                let two_state = module.variables[var_id].r#type.is_2state();
                // An invalid index reads X, or 0 from a two-state variable
                // (IEEE 1800-2023 7.4.6); a partly out-of-range part select
                // reads so only for its out-of-range bits (11.5.1).
                let extracted_expr = if let Some(partial) = partial {
                    let placement = partial.window(arena, element_width)?;
                    let window = if is_unmodified {
                        // Load only the clamped window. The row indices must
                        // address the variable even when they are invalid.
                        let mut window_indices =
                            dynamic_indices[..dynamic_indices.len() - 1].to_vec();
                        window_indices =
                            super::safe_slt_indices(arena, window_indices, indices_valid, clamp)?;
                        window_indices.push(SLTIndex {
                            node: placement.start,
                            stride: partial.element_bits,
                            kind: SLTIndexKind::Packed,
                        });
                        let raw_input = arena.alloc(SLTNode::Input {
                            variable: *var_id,
                            signed: false,
                            index: window_indices,
                            access: BitAccess::new(0, width - 1),
                        })?;
                        arena.alloc(SLTNode::Slice {
                            expr: raw_input,
                            access: BitAccess::new(0, element_width - 1),
                        })?
                    } else {
                        let (current_expr, current_sources) = combine_parts_with_default(
                            *var_id,
                            0,
                            parts.expect("a modified variable has a symbolic range"),
                            arena,
                        )?;
                        all_sources.extend(current_sources);
                        let element_bits = arena.alloc(SLTNode::Constant(
                            BigUint::from(partial.element_bits),
                            BigUint::from(0u8),
                            64,
                            false,
                        ))?;
                        let start_bits = arena.alloc(SLTNode::Binary(
                            placement.start,
                            BinaryOp::Mul,
                            element_bits,
                        ))?;
                        let offset = arena.alloc(SLTNode::Binary(
                            partial.row_offset,
                            BinaryOp::Add,
                            start_bits,
                        ))?;
                        let shifted =
                            arena.alloc(SLTNode::Binary(current_expr, BinaryOp::Shr, offset))?;
                        arena.alloc(SLTNode::Slice {
                            expr: shifted,
                            access: BitAccess::new(0, element_width - 1),
                        })?
                    };
                    let selected =
                        partial.align(arena, window, &placement, element_width, two_state)?;
                    super::slt_or_unknown(arena, indices_valid, selected, element_width, two_state)?
                } else if is_unmodified {
                    // --- Code for the approach of aligning at load time ---
                    // Keep the SLT input footprint conservative for dependency analysis.
                    // The SIR lowerer recognizes the following Slice(Input(dynamic)) shape
                    // and emits a narrow dynamic load.
                    let dynamic_indices =
                        super::safe_slt_indices(arena, dynamic_indices, valid, clamp)?;
                    let raw_input = arena.alloc(SLTNode::Input {
                        variable: *var_id,
                        signed: module.variables[var_id].r#type.signed,
                        index: dynamic_indices,
                        access: BitAccess::new(0, width - 1),
                    })?;
                    let selected = arena.alloc(SLTNode::Slice {
                        expr: raw_input,
                        access: BitAccess::new(0, element_width - 1),
                    })?;
                    super::slt_or_unknown(arena, valid, selected, element_width, two_state)?
                } else {
                    // --- If already written ---
                    // Combine latest values in register and align with Shr
                    let (current_expr, current_sources) = combine_parts_with_default(
                        *var_id,
                        0,
                        parts.expect("a modified variable has a symbolic range"),
                        arena,
                    )?;
                    all_sources.extend(current_sources);

                    let shifted =
                        arena.alloc(SLTNode::Binary(current_expr, BinaryOp::Shr, offset_node))?;
                    let selected = arena.alloc(SLTNode::Slice {
                        expr: shifted,
                        access: BitAccess::new(0, element_width - 1),
                    })?;
                    super::slt_or_unknown(arena, valid, selected, element_width, two_state)?
                };

                let extracted_expr =
                    coerce_node_width(arena, extracted_expr, context_width, context_signed)?;

                let prefix_access = eval_var_select(module, *var_id, index, select)?;
                all_sources.insert(VarAtomBase::new(
                    *var_id,
                    prefix_access.lsb,
                    prefix_access.msb,
                ));
                Ok(((extracted_expr, all_sources), all_bounds))
            }
        }
        Factor::HierVariable(reference) => Err(ParserError::illegal_context(
            "hierarchical variable reference",
            format!(
                "`{}` is only valid in a native testbench block",
                reference.var_path
            ),
            Some(&reference.comptime.token),
        )),
        Factor::Value(v) => {
            let (celox_value, mask_xz, effective_width, signed) =
                celox_value_from_comptime_in_context(v, context_width)
                    .expect("Factor::Value should always have a numeric value");
            let node = arena.alloc(SLTNode::Constant(
                celox_value,
                mask_xz,
                effective_width,
                signed,
            ))?;
            let node = coerce_node_width(arena, node, context_width, context_signed)?;
            Ok(((node, HashSet::default()), BoundaryMap::default()))
        }
        Factor::SystemFunctionCall(call) => {
            eval_system_function_call(module, store, call, arena, context)
        }
        Factor::FunctionCall(call) => {
            let ((node, sources), bounds) =
                eval_function_call_expression(module, store, call, arena)?;
            let node = coerce_node_width(arena, node, context_width, context_signed)?;
            Ok(((node, sources), bounds))
        }
        Factor::Anonymous(_) | Factor::Unknown(_) => Err(ParserError::internal(
            "unresolved factor in comb expression",
            format!("{:?}", factor),
            Some(&factor.token_range()),
        )),
    }
}
pub fn get_width<A: Hash + Eq + Clone>(expr: NodeId, arena: &SLTNodeArena<A>) -> usize {
    celox_slt::get_width(expr, arena)
}

pub(super) fn merge_boundaries(
    mut base: BoundaryMap<VarId>,
    other: BoundaryMap<VarId>,
) -> BoundaryMap<VarId> {
    for (id, bits) in other {
        base.entry(id).or_default().extend(bits);
    }
    base
}

pub fn coerce_node_width<A: Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    expr: NodeId,
    target_width: Option<usize>,
    sign_extend: bool,
) -> Result<NodeId, SLTNodeFactsError> {
    let Some(target_width) = target_width else {
        return Ok(expr);
    };
    let expr_width = get_width(expr, arena);
    if expr_width == 0 && target_width != 0 {
        return Err(SLTNodeFactsError::new(
            "WIDTH.COERCE_SOURCE_NON_ZERO",
            expr,
            format!(
                "cannot coerce zero-width n{} to width {target_width}",
                expr.0
            ),
        ));
    }
    if target_width == 0 && expr_width != 0 {
        return Err(SLTNodeFactsError::new(
            "WIDTH.COERCE_TARGET_NON_ZERO",
            expr,
            format!("cannot coerce width-{expr_width} n{} to zero width", expr.0),
        ));
    }
    if expr_width < target_width {
        let pad_width = target_width - expr_width;
        let pad = if sign_extend {
            let msb_slice = arena.alloc(SLTNode::Slice {
                expr,
                access: BitAccess::new(expr_width - 1, expr_width - 1),
            })?;
            let pad = if pad_width == 1 {
                msb_slice
            } else {
                arena.alloc(SLTNode::Concat(
                    std::iter::repeat_n((msb_slice, 1), pad_width).collect(),
                ))?
            };
            (pad, pad_width)
        } else {
            let zero = arena.alloc(SLTNode::Constant(
                BigUint::from(0u8),
                BigUint::from(0u32),
                pad_width,
                false,
            ))?;
            (zero, pad_width)
        };
        arena.alloc(SLTNode::Concat(vec![pad, (expr, expr_width)]))
    } else if expr_width > target_width {
        arena.alloc(SLTNode::Slice {
            expr,
            access: BitAccess::new(0, target_width - 1),
        })
    } else {
        Ok(expr)
    }
}

pub(super) fn is_signed(module: &Module, expr: NodeId, arena: &SLTNodeArena<VarId>) -> bool {
    match arena.get(expr) {
        SLTNode::Input { variable: id, .. } => module.variables[id].r#type.signed,
        SLTNode::Constant(_, _, _, signed) => *signed,
        SLTNode::Binary(lhs, op, rhs) => match op {
            BinaryOp::Float { .. } => false,
            BinaryOp::Eq
            | BinaryOp::Ne
            | BinaryOp::EqCase
            | BinaryOp::NeCase
            | BinaryOp::LtU
            | BinaryOp::LtS
            | BinaryOp::LeU
            | BinaryOp::LeS
            | BinaryOp::GtU
            | BinaryOp::GtS
            | BinaryOp::GeU
            | BinaryOp::GeS
            | BinaryOp::LogicAnd
            | BinaryOp::LogicOr
            | BinaryOp::EqWildcard
            | BinaryOp::NeWildcard
            | BinaryOp::DivU
            | BinaryOp::RemU => false,
            BinaryOp::DivS | BinaryOp::RemS => true,
            BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar => is_signed(module, *lhs, arena),
            BinaryOp::Add
            | BinaryOp::Sub
            | BinaryOp::Mul
            | BinaryOp::And
            | BinaryOp::Or
            | BinaryOp::Xor => is_signed(module, *lhs, arena) && is_signed(module, *rhs, arena),
        },
        SLTNode::Unary(UnaryOp::Float { op, .. }, _) => {
            matches!(op, celox_design::FloatUnaryOp::ToSigned { .. })
        }
        SLTNode::Unary(
            UnaryOp::LogicNot
            | UnaryOp::And
            | UnaryOp::Or
            | UnaryOp::Xor
            | UnaryOp::PopCount
            | UnaryOp::CountLeadingZeros
            | UnaryOp::CountTrailingZeros
            | UnaryOp::ToPlanes
            | UnaryOp::FromPlanes,
            _,
        ) => false,
        SLTNode::Unary(
            UnaryOp::Ident | UnaryOp::ToTwoState | UnaryOp::Minus | UnaryOp::BitNot,
            inner,
        ) => is_signed(module, *inner, arena),
        SLTNode::Capture { expr, .. } => is_signed(module, *expr, arena),
        SLTNode::Mux {
            then_expr,
            else_expr,
            ..
        } => is_signed(module, *then_expr, arena) && is_signed(module, *else_expr, arena),
        SLTNode::ForFold { result, .. } => match result {
            SLTForFoldResult::State(result) => module.variables[&result.id].r#type.signed,
            SLTForFoldResult::Transient { .. } => false,
        },
        SLTNode::ForFoldGroup { .. } => false,
        SLTNode::Slice { .. } => false,
        SLTNode::Concat(_) => false,
    }
}

pub fn convert_unary_op(op: &Op) -> UnaryOp {
    match op {
        Op::Add => UnaryOp::Ident,
        Op::Sub => UnaryOp::Minus,
        Op::BitNot => UnaryOp::BitNot,
        Op::LogicNot => UnaryOp::LogicNot,
        // リダクション演算子としての使用
        Op::BitAnd => UnaryOp::And,
        Op::BitOr => UnaryOp::Or,
        Op::BitXor => UnaryOp::Xor,
        // Unary form lowered by decomposition before conversion
        Op::BitXnor | Op::BitNand | Op::BitNor => {
            unreachable!(
                "reduction derived op must be lowered before convert_unary_op: {:?}",
                op
            )
        }
        // Binary-only operators
        Op::Pow
        | Op::Div
        | Op::Rem
        | Op::Mul
        | Op::ArithShiftL
        | Op::ArithShiftR
        | Op::LogicShiftL
        | Op::LogicShiftR
        | Op::LessEq
        | Op::GreaterEq
        | Op::Less
        | Op::Greater
        | Op::Eq
        | Op::EqWildcard
        | Op::Ne
        | Op::NeWildcard
        | Op::LogicAnd
        | Op::LogicOr => {
            unreachable!(
                "binary operator must not be lowered by convert_unary_op: {:?}",
                op
            )
        }
        // Node kinds lowered by dedicated paths
        Op::Ternary => unreachable!("ternary expression must not be lowered by convert_unary_op"),
        Op::Concatenation => {
            unreachable!("concatenation must be lowered by concat-specific path")
        }
        Op::ArrayLiteral => {
            unreachable!("array literal must not be lowered by convert_unary_op")
        }
        Op::Condition => unreachable!("condition node must not be lowered by convert_unary_op"),
        Op::Repeat => unreachable!("repeat node must be lowered by repeat-specific path"),
        Op::As => unreachable!("As is binary and must not be lowered by convert_unary_op"),
    }
}

#[cfg(test)]
mod tests;
