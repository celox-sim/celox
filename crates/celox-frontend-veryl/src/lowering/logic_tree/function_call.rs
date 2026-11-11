//! Function call arguments in declaration order, and statement-form function calls.

use super::*;

/// Veryl validates function arity before producing usable IR. A formal argument
/// absent from both connection maps is therefore invalid IR, not an unsupported
/// lowering shape.
pub(in crate::lowering) fn invalid_function_call_argument_error(
    function: &Function,
    arg_path: &VarPath,
    detail: &'static str,
    call: &FunctionCall,
) -> ParserError {
    ParserError::invalid_function_argument_binding(
        function.path.to_string(),
        arg_path.to_string(),
        detail,
        Some(&call.comptime.token),
    )
}

/// Returns input expressions in source evaluation order.
///
/// Veryl 0.20.3 preserves the source order in `FunctionCall::inputs`. Keep that
/// order when mapping flattened argument paths to their specialized variables:
/// evaluating in formal declaration order can reorder side effects in named
/// arguments.
pub(in crate::lowering) fn ordered_function_inputs<'a>(
    function: &Function,
    function_body: &FunctionBody,
    call: &'a FunctionCall,
) -> Result<Vec<(VarId, &'a Expression)>, ParserError> {
    let mut declared_ids = HashSet::default();
    for arg in &function.args {
        for (arg_path, _, _) in &arg.members {
            let Some(arg_id) = function_body.arg_map.get(arg_path) else {
                return Err(invalid_function_call_argument_error(
                    function,
                    arg_path,
                    "input argument does not match any formal argument",
                    call,
                ));
            };
            declared_ids.insert(*arg_id);
        }
    }

    let mut inputs = Vec::with_capacity(call.inputs.len());
    for (arg_path, arg_expr) in &call.inputs {
        let Some(arg_id) = function_body.arg_map.get(arg_path) else {
            return Err(invalid_function_call_argument_error(
                function,
                arg_path,
                "input argument does not match any formal argument",
                call,
            ));
        };
        if !declared_ids.contains(arg_id) {
            return Err(invalid_function_call_argument_error(
                function,
                arg_path,
                "input argument is absent from the function declaration",
                call,
            ));
        }
        inputs.push((*arg_id, arg_expr));
    }

    Ok(inputs)
}

/// Returns output destinations in source evaluation order.
///
/// Veryl 0.20.3 preserves the source order in `FunctionCall::outputs`.
/// Destination expressions may have effects, so applying them in formal
/// declaration order can change simulation behavior for named arguments.
pub(in crate::lowering) fn ordered_function_outputs<'a>(
    function: &Function,
    function_body: &FunctionBody,
    call: &'a FunctionCall,
) -> Result<Vec<(VarId, &'a [veryl_analyzer::ir::AssignDestination])>, ParserError> {
    let mut declared_ids = HashSet::default();
    for arg in &function.args {
        for (arg_path, _, _) in &arg.members {
            let Some(arg_id) = function_body.arg_map.get(arg_path) else {
                return Err(invalid_function_call_argument_error(
                    function,
                    arg_path,
                    "output argument does not match any formal argument",
                    call,
                ));
            };
            declared_ids.insert(*arg_id);
        }
    }

    let mut outputs = Vec::with_capacity(call.outputs.len());
    for (arg_path, destinations) in &call.outputs {
        let Some(arg_id) = function_body.arg_map.get(arg_path) else {
            return Err(invalid_function_call_argument_error(
                function,
                arg_path,
                "output argument does not match any formal argument",
                call,
            ));
        };
        if !declared_ids.contains(arg_id) {
            return Err(invalid_function_call_argument_error(
                function,
                arg_path,
                "output argument is absent from the function declaration",
                call,
            ));
        }
        outputs.push((*arg_id, destinations.as_slice()));
    }

    Ok(outputs)
}

pub(super) fn eval_statement_form_function_call(
    module: &Module,
    mut store: SymbolicStore<VarId>,
    mut boundaries: BoundaryMap<VarId>,
    call: &veryl_analyzer::ir::FunctionCall,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<(SymbolicStore<VarId>, BoundaryMap<VarId>), ParserError> {
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

    let mut evaluated_inputs = Vec::with_capacity(call.inputs.len());

    for (arg_id, arg_expr) in ordered_function_inputs(function, &function_body, call)? {
        let formal = module.variables.get(&arg_id).ok_or_else(|| {
            ParserError::illegal_context(
                "function input argument",
                "formal variable is absent from the semantic module",
                Some(&call.comptime.token),
            )
        })?;
        let arg_width = resolve_total_width(module, formal)?;
        let ((arg_node, arg_sources), arg_bounds) = expr::eval_function_input_assignment_effectful(
            module,
            &mut store,
            arg_expr,
            &formal.r#type,
            arg_width,
            arena,
        )?;
        let arg_node = if formal.r#type.is_2state() && !arg_expr.comptime().r#type.is_2state() {
            arena.alloc(SLTNode::Unary(UnaryOp::ToTwoState, arg_node))?
        } else {
            arg_node
        };
        boundaries = merge_boundaries(boundaries, arg_bounds);
        evaluated_inputs.push((arg_id, arg_node, arg_sources, arg_width));
    }

    for arg_path in function_body.arg_map.keys() {
        if !function_call_has_arg(&call.inputs, arg_path)
            && !function_call_has_arg(&call.outputs, arg_path)
        {
            return Err(invalid_function_call_argument_error(
                function,
                arg_path,
                "formal argument has neither an input expression nor an output destination",
                call,
            ));
        }
    }

    let mut local_store = store.fork();
    for (arg_id, arg_node, arg_sources, arg_width) in evaluated_inputs {
        local_store.insert(
            arg_id,
            RangeStore::new(Some((arg_node, arg_sources)), arg_width),
        );
    }

    let (final_local_store, local_boundaries) = if let Some(ret_id) = function_body.ret {
        let ((_, _), local_boundaries, final_local_store) =
            eval_function_body_return(module, &local_store, &function_body, ret_id, arena)?;
        (final_local_store, local_boundaries)
    } else {
        function_body.statements.iter().try_fold(
            (local_store, BoundaryMap::default()),
            |(local_store, local_boundaries), stmt| {
                eval_statement(module, local_store, local_boundaries, stmt, arena)
            },
        )?
    };
    boundaries = merge_boundaries(boundaries, local_boundaries);

    apply_function_call_outputs(
        module,
        function,
        store,
        boundaries,
        call,
        &function_body,
        &final_local_store,
        arena,
    )
}

pub(super) fn apply_function_call_outputs(
    module: &Module,
    function: &Function,
    mut store: SymbolicStore<VarId>,
    mut boundaries: BoundaryMap<VarId>,
    call: &veryl_analyzer::ir::FunctionCall,
    function_body: &veryl_analyzer::ir::FunctionBody,
    final_local_store: &SymbolicStore<VarId>,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<(SymbolicStore<VarId>, BoundaryMap<VarId>), ParserError> {
    for (arg_id, dsts) in ordered_function_outputs(function, function_body, call)? {
        (store, boundaries) = apply_function_output(
            module,
            store,
            boundaries,
            arg_id,
            dsts,
            call,
            final_local_store,
            arena,
        )?;
    }

    Ok((store, boundaries))
}

pub(in crate::lowering) fn apply_function_output(
    module: &Module,
    store: SymbolicStore<VarId>,
    boundaries: BoundaryMap<VarId>,
    arg_id: VarId,
    dsts: &[veryl_analyzer::ir::AssignDestination],
    call: &veryl_analyzer::ir::FunctionCall,
    final_local_store: &SymbolicStore<VarId>,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<(SymbolicStore<VarId>, BoundaryMap<VarId>), ParserError> {
    let (output_expr, output_sources, output_is_2state) =
        function_output_value(module, arg_id, call, final_local_store, arena)?;
    let output_expr = if let [destination] = dsts {
        if let Some(op) = crate::float::conversion(
            &module.variables[&arg_id].r#type,
            &destination.comptime.r#type,
        ) {
            arena.alloc(SLTNode::Unary(op, output_expr))?
        } else {
            output_expr
        }
    } else {
        output_expr
    };
    // Copy-out reads the formal variable. Its declared signedness can differ
    // from the expression stored by the last assignment in the callee.
    let output_signed = module.variables[&arg_id].r#type.signed;
    assign_node_to_dsts(
        module,
        store,
        boundaries,
        dsts,
        output_expr,
        output_sources,
        output_is_2state,
        output_signed,
        arena,
    )
}

pub(in crate::lowering) fn function_output_value(
    module: &Module,
    arg_id: VarId,
    call: &veryl_analyzer::ir::FunctionCall,
    final_local_store: &SymbolicStore<VarId>,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<(NodeId, HashSet<VarAtomBase<VarId>>, bool), ParserError> {
    let formal = module.variables.get(&arg_id).ok_or_else(|| {
        ParserError::illegal_context(
            "function output value",
            "formal variable is absent from the semantic module",
            Some(&call.comptime.token),
        )
    })?;
    let formal_width = resolve_total_width(module, formal)?;
    if formal_width == 0 {
        return Err(ParserError::illegal_context(
            "function output value",
            "formal output has zero width",
            Some(&call.comptime.token),
        ));
    }
    let access = BitAccess::new(0, formal_width - 1);
    let range_store = final_local_store.get(&arg_id).ok_or_else(|| {
        ParserError::illegal_context(
            "function output value",
            "formal output is absent from the final symbolic store",
            Some(&call.comptime.token),
        )
    })?;
    let parts = range_store.get_parts_ref(access).map_err(|error| {
        range_store_error("function output value", error, Some(&call.comptime.token))
    })?;
    let (output_expr, output_sources) = combine_parts_with_default(arg_id, 0, parts, arena)?;
    Ok((output_expr, output_sources, formal.r#type.is_2state()))
}
