//! Checks of testbench statements, expressions and destinations.

use super::*;

pub(super) fn reject_selected_destinations_in_expression_statements(
    statements: &[Statement],
) -> Result<(), ParserError> {
    for statement in statements {
        match statement {
            Statement::Assign(statement) => {
                if statement.dst.iter().any(has_selected_testbench_destination)
                    || statement.hier_dst.as_ref().is_some_and(|destination| {
                        !destination.index.0.is_empty()
                            || !destination.select.0.is_empty()
                            || destination.select.1.is_some()
                    })
                {
                    return Err(ParserError::illegal_context(
                        "selected destination in expression testbench function",
                        "read-modify-write selected destinations are only supported for statement-level function calls",
                        None,
                    ));
                }
            }
            Statement::If(statement) => {
                reject_selected_destinations_in_expression_statements(&statement.true_side)?;
                reject_selected_destinations_in_expression_statements(&statement.false_side)?;
            }
            Statement::IfReset(statement) => {
                reject_selected_destinations_in_expression_statements(&statement.true_side)?;
                reject_selected_destinations_in_expression_statements(&statement.false_side)?;
            }
            Statement::Case(statement) => {
                for arm in &statement.arms {
                    reject_selected_destinations_in_expression_statements(&arm.body)?;
                }
                reject_selected_destinations_in_expression_statements(&statement.default)?;
            }
            Statement::For(statement) => {
                reject_selected_destinations_in_expression_statements(&statement.body)?;
            }
            Statement::TbMethodCall(call) => {
                if call
                    .ret
                    .as_deref()
                    .is_some_and(has_selected_testbench_destination)
                {
                    return Err(ParserError::illegal_context(
                        "selected destination in expression testbench function",
                        "read-modify-write selected destinations are only supported for statement-level function calls",
                        None,
                    ));
                }
            }
            Statement::FunctionCall(_)
            | Statement::SystemFunctionCall(_)
            | Statement::Break
            | Statement::Unsupported(_)
            | Statement::Null => {}
        }
    }
    Ok(())
}

pub(super) fn reject_selected_destinations_in_expression_function_call(
    call: &FunctionCall,
    source: &VerylTestbenchSource,
    active_functions: &mut FxHashSet<(VarId, Option<Vec<usize>>)>,
) -> Result<(), ParserError> {
    let key = (call.id, call.index.clone());
    if !active_functions.insert(key.clone()) {
        return Ok(());
    }
    let result = if let Some(body) = function_body(&source.functions, call) {
        reject_selected_destinations_in_expression_statements(&body.statements)
    } else {
        Ok(())
    };
    active_functions.remove(&key);
    result
}

pub(super) fn validate_testbench_function_call(
    call: &FunctionCall,
    lookup: &FrontendLookup,
    source: &VerylTestbenchSource,
    active_functions: &mut FxHashSet<(VarId, Option<Vec<usize>>)>,
) -> Result<(), ParserError> {
    for expression in call.inputs.values() {
        validate_testbench_expression(expression, lookup, source, active_functions)?;
    }

    let key = (call.id, call.index.clone());
    if !active_functions.insert(key.clone()) {
        return Ok(());
    }
    let result = if let Some(body) = function_body(&source.functions, call) {
        validate_testbench_statements(&body.statements, lookup, source, active_functions)
    } else {
        Ok(())
    };
    active_functions.remove(&key);
    result
}

pub(super) fn validate_testbench_expression(
    expression: &Expression,
    lookup: &FrontendLookup,
    source: &VerylTestbenchSource,
    active_functions: &mut FxHashSet<(VarId, Option<Vec<usize>>)>,
) -> Result<(), ParserError> {
    if let Expression::Term(factor) = expression
        && factor.comptime().r#type.is_string()
    {
        let access = match factor.as_ref() {
            Factor::Variable(id, index, select, _) => source
                .id_map
                .instance_var(lookup, source.base_instance(lookup), *id)
                .and_then(|id| lookup.instance_variable(source.base_instance(lookup), id))
                .map(|(_, info)| (info, index, select)),
            Factor::HierVariable(reference) => Some((
                resolve_hierarchical_reference_from(
                    lookup,
                    source.base_instance(lookup),
                    reference,
                )?
                .1,
                &reference.index,
                &reference.select,
            )),
            _ => None,
        };
        if let Some((info, index, select)) = access
            && (index.0.len() > info.array_dims.len() || !select.0.is_empty() || select.1.is_some())
        {
            return Err(ParserError::illegal_context(
                "string character selection",
                "character indexing and slicing of strings are not supported in Celox testbenches",
                Some(&factor.comptime().token),
            ));
        }
    }
    match expression {
        Expression::Term(factor) => match factor.as_ref() {
            Factor::HierVariable(reference) => {
                for expression in reference.index.0.iter().chain(reference.select.0.iter()) {
                    validate_testbench_expression(expression, lookup, source, active_functions)?;
                }
                if let Some((_, expression)) = &reference.select.1 {
                    validate_testbench_expression(expression, lookup, source, active_functions)?;
                }
                Ok(())
            }
            Factor::Variable(_, index, select, _) => {
                for expression in index.0.iter().chain(select.0.iter()) {
                    validate_testbench_expression(expression, lookup, source, active_functions)?;
                }
                if let Some((_, expression)) = &select.1 {
                    validate_testbench_expression(expression, lookup, source, active_functions)?;
                }
                Ok(())
            }
            Factor::FunctionCall(call) => {
                validate_testbench_function_call(call, lookup, source, active_functions)?;
                reject_selected_destinations_in_expression_function_call(
                    call,
                    source,
                    active_functions,
                )
            }
            Factor::SystemFunctionCall(call) => {
                validate_testbench_system_function(&call.kind, lookup, source, active_functions)
            }
            Factor::Anonymous(comptime) | Factor::Unknown(comptime)
                if comptime.get_value().is_err() =>
            {
                let token = &comptime.token;
                let source = token.source().get_text();
                let start = token.beg.pos as usize;
                let end = (token.end.pos + token.end.length) as usize;
                let factor_text = source.get(start..end).unwrap_or_default();
                if factor_text.contains('.') {
                    Err(ParserError::illegal_context(
                        "hierarchical variable reference",
                        format!("`{factor_text}` was not resolved by the analyzer"),
                        Some(token),
                    ))
                } else {
                    Err(ParserError::internal(
                        "unresolved factor in testbench expression",
                        format!("{factor:?}"),
                        Some(token),
                    ))
                }
            }
            Factor::Value(_) | Factor::Anonymous(_) | Factor::Unknown(_) => Ok(()),
        },
        Expression::Unary(_, inner, _) => {
            if inner.comptime().r#type.is_string() {
                return Err(ParserError::illegal_context(
                    "unary string operation",
                    "unary operations on strings are not supported in Celox testbenches",
                    Some(&inner.comptime().token),
                ));
            }
            validate_testbench_expression(inner, lookup, source, active_functions)
        }
        Expression::Binary(lhs, _, rhs, _) => {
            if lhs.comptime().r#type.is_string() != rhs.comptime().r#type.is_string() {
                return Err(ParserError::illegal_context(
                    "mixed string operation",
                    "operations mixing string and non-string operands are not supported in Celox testbenches",
                    Some(&expression.comptime().token),
                ));
            }
            validate_testbench_expression(lhs, lookup, source, active_functions)?;
            validate_testbench_expression(rhs, lookup, source, active_functions)
        }
        Expression::Ternary(condition, then_expression, else_expression, _) => {
            validate_testbench_expression(condition, lookup, source, active_functions)?;
            validate_testbench_expression(then_expression, lookup, source, active_functions)?;
            validate_testbench_expression(else_expression, lookup, source, active_functions)
        }
        Expression::Concatenation(items, _) => {
            for (expression, repeat) in items {
                if expression.comptime().r#type.is_string() {
                    return Err(ParserError::illegal_context(
                        "string concatenation or replication",
                        "string concatenation and replication are not supported in Celox testbenches",
                        Some(&expression.comptime().token),
                    ));
                }
                validate_testbench_expression(expression, lookup, source, active_functions)?;
                if let Some(repeat) = repeat {
                    validate_testbench_expression(repeat, lookup, source, active_functions)?;
                }
            }
            Ok(())
        }
        Expression::ArrayLiteral(items, _) => {
            for item in items {
                match item {
                    ArrayLiteralItem::Value(expression, repeat) => {
                        validate_testbench_expression(
                            expression,
                            lookup,
                            source,
                            active_functions,
                        )?;
                        if let Some(repeat) = repeat {
                            validate_testbench_expression(
                                repeat,
                                lookup,
                                source,
                                active_functions,
                            )?;
                        }
                    }
                    ArrayLiteralItem::Defaul(expression) => {
                        validate_testbench_expression(
                            expression,
                            lookup,
                            source,
                            active_functions,
                        )?;
                    }
                }
            }
            Ok(())
        }
        Expression::StructConstructor(_, fields, _) => {
            for (_, expression) in fields {
                validate_testbench_expression(expression, lookup, source, active_functions)?;
            }
            Ok(())
        }
    }
}

pub(super) fn validate_testbench_system_function(
    kind: &SystemFunctionKind,
    lookup: &FrontendLookup,
    source: &VerylTestbenchSource,
    active_functions: &mut FxHashSet<(VarId, Option<Vec<usize>>)>,
) -> Result<(), ParserError> {
    let mut validate = |input: &SystemFunctionInput| {
        validate_testbench_expression(&input.0, lookup, source, active_functions)
    };
    match kind {
        SystemFunctionKind::Bits(input)
        | SystemFunctionKind::Size(input, _)
        | SystemFunctionKind::Clog2(input)
        | SystemFunctionKind::Onehot(input)
        | SystemFunctionKind::Signed(input)
        | SystemFunctionKind::Unsigned(input) => validate(input),
        SystemFunctionKind::Display(inputs) | SystemFunctionKind::Write(inputs) => {
            for input in inputs {
                validate(input)?;
            }
            Ok(())
        }
        SystemFunctionKind::Assert { cond, args, .. } => {
            validate(cond)?;
            for input in args {
                validate(input)?;
            }
            Ok(())
        }
        SystemFunctionKind::Readmemh(input, _) => validate(input),
        SystemFunctionKind::Finish => Ok(()),
    }
}

pub(super) fn validate_testbench_statements(
    statements: &[Statement],
    lookup: &FrontendLookup,
    source: &VerylTestbenchSource,
    active_functions: &mut FxHashSet<(VarId, Option<Vec<usize>>)>,
) -> Result<(), ParserError> {
    for statement in statements {
        match statement {
            Statement::Assign(statement) => {
                for destination in &statement.dst {
                    validate_testbench_destination(destination, lookup, source, active_functions)?;
                }
                if let Some(destination) = &statement.hier_dst {
                    validate_testbench_hierarchical_destination(
                        destination,
                        lookup,
                        source,
                        active_functions,
                    )?;
                }
                validate_testbench_expression(&statement.expr, lookup, source, active_functions)?
            }
            Statement::If(statement) => {
                validate_testbench_expression(&statement.cond, lookup, source, active_functions)?;
                validate_testbench_statements(
                    &statement.true_side,
                    lookup,
                    source,
                    active_functions,
                )?;
                validate_testbench_statements(
                    &statement.false_side,
                    lookup,
                    source,
                    active_functions,
                )?;
            }
            Statement::IfReset(statement) => {
                validate_testbench_statements(
                    &statement.true_side,
                    lookup,
                    source,
                    active_functions,
                )?;
                validate_testbench_statements(
                    &statement.false_side,
                    lookup,
                    source,
                    active_functions,
                )?;
            }
            Statement::Case(statement) => {
                validate_testbench_expression(
                    &statement.case_target,
                    lookup,
                    source,
                    active_functions,
                )?;
                for arm in &statement.arms {
                    for pattern in &arm.patterns {
                        match pattern {
                            CasePattern::Eq(expression) => {
                                validate_testbench_expression(
                                    expression,
                                    lookup,
                                    source,
                                    active_functions,
                                )?;
                            }
                            CasePattern::Range { lo, hi, .. } => {
                                validate_testbench_expression(
                                    lo,
                                    lookup,
                                    source,
                                    active_functions,
                                )?;
                                validate_testbench_expression(
                                    hi,
                                    lookup,
                                    source,
                                    active_functions,
                                )?;
                            }
                        }
                    }
                    validate_testbench_statements(&arm.body, lookup, source, active_functions)?;
                }
                validate_testbench_statements(
                    &statement.default,
                    lookup,
                    source,
                    active_functions,
                )?;
            }
            Statement::For(statement) => {
                let (start, end) = match &statement.range {
                    ForRange::Forward { start, end, .. }
                    | ForRange::Reverse { start, end, .. }
                    | ForRange::Stepped { start, end, .. } => (start, end),
                };
                for bound in [start, end] {
                    if let ForBound::Expression(expression) = bound {
                        validate_testbench_expression(
                            expression,
                            lookup,
                            source,
                            active_functions,
                        )?;
                    }
                }
                validate_testbench_statements(&statement.body, lookup, source, active_functions)?;
            }
            Statement::SystemFunctionCall(call) => {
                validate_testbench_system_function(&call.kind, lookup, source, active_functions)?;
            }
            Statement::FunctionCall(call) => {
                validate_testbench_function_call(call, lookup, source, active_functions)?;
            }
            Statement::TbMethodCall(call) => {
                // Selected return destinations are lowered together with ordinary assignments.
                if let Some(destination) = call.ret.as_deref() {
                    validate_testbench_destination(destination, lookup, source, active_functions)?;
                }
                match &call.method {
                    TbMethod::Component { args, .. } => {
                        for argument in args {
                            validate_testbench_expression(
                                &argument.0,
                                lookup,
                                source,
                                active_functions,
                            )?;
                        }
                    }
                    TbMethod::RandomSeed { value } => {
                        validate_testbench_expression(value, lookup, source, active_functions)?;
                    }
                    TbMethod::RandomGetRange { min, max, .. } => {
                        validate_testbench_expression(min, lookup, source, active_functions)?;
                        validate_testbench_expression(max, lookup, source, active_functions)?;
                    }
                    TbMethod::RandomGet { .. } | TbMethod::RandomGetSeed => {}
                    TbMethod::ClockNext { count, period } => {
                        if let Some(expression) = count {
                            validate_testbench_expression(
                                expression,
                                lookup,
                                source,
                                active_functions,
                            )?;
                        }
                        if let Some(expression) = period {
                            validate_testbench_expression(
                                expression,
                                lookup,
                                source,
                                active_functions,
                            )?;
                        }
                    }
                    TbMethod::ResetAssert { duration, .. } => {
                        if let Some(expression) = duration {
                            validate_testbench_expression(
                                expression,
                                lookup,
                                source,
                                active_functions,
                            )?;
                        }
                    }
                    TbMethod::FileOpen { name, .. } => {
                        validate_testbench_expression(&name.0, lookup, source, active_functions)?
                    }
                    TbMethod::FileWrite { args } => {
                        for argument in args {
                            validate_testbench_expression(
                                &argument.0,
                                lookup,
                                source,
                                active_functions,
                            )?;
                        }
                    }
                    TbMethod::FileClose | TbMethod::FileFlush => {}
                }
            }
            Statement::Break | Statement::Unsupported(_) | Statement::Null => {}
        }
    }
    Ok(())
}

pub(super) fn validate_testbench_hierarchical_destination(
    destination: &HierAssignDestination,
    lookup: &FrontendLookup,
    source: &VerylTestbenchSource,
    active_functions: &mut FxHashSet<(VarId, Option<Vec<usize>>)>,
) -> Result<(), ParserError> {
    for expression in destination
        .index
        .0
        .iter()
        .chain(destination.select.0.iter())
    {
        validate_testbench_expression(expression, lookup, source, active_functions)?;
    }
    if let Some((_, expression)) = &destination.select.1 {
        validate_testbench_expression(expression, lookup, source, active_functions)?;
    }
    let reference = hierarchical_destination_reference(destination);
    let (_, info) =
        resolve_hierarchical_reference_from(lookup, source.base_instance(lookup), &reference)?;
    ExprCompiler::validate_target_bounds_parts(info, &destination.index, &destination.select)?;
    let compiler = ExprCompiler {
        lookup,
        id_map: &source.id_map,
        constant_strings: &source.constant_strings,
        functions: &source.functions,
        base_instance: source.base_instance(lookup),
    };
    if compiler.resolve_hierarchical_target(destination).is_none() {
        return Err(ParserError::illegal_context(
            "hierarchical assignment destination",
            "destination selection cannot be represented by the testbench runtime",
            Some(&destination.token),
        ));
    }
    Ok(())
}

pub(super) fn validate_testbench_destination(
    destination: &veryl_analyzer::ir::AssignDestination,
    lookup: &FrontendLookup,
    source: &VerylTestbenchSource,
    active_functions: &mut FxHashSet<(VarId, Option<Vec<usize>>)>,
) -> Result<(), ParserError> {
    for expression in &destination.index.0 {
        validate_testbench_expression(expression, lookup, source, active_functions)?;
    }
    for expression in &destination.select.0 {
        validate_testbench_expression(expression, lookup, source, active_functions)?;
    }
    if let Some((_, expression)) = &destination.select.1 {
        validate_testbench_expression(expression, lookup, source, active_functions)?;
    }
    ExprCompiler {
        lookup,
        id_map: &source.id_map,
        constant_strings: &source.constant_strings,
        functions: &source.functions,
        base_instance: source.base_instance(lookup),
    }
    .validate_target_bounds(destination)?;
    Ok(())
}
