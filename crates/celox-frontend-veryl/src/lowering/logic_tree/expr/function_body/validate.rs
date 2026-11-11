//! Statements and expressions a function body may contain.

use super::*;

fn validate_function_body_expression(
    module: &Module,
    expr: &Expression,
) -> Result<(), ParserError> {
    match expr {
        Expression::Term(factor) => match factor.as_ref() {
            Factor::SystemFunctionCall(call) => {
                validate_function_body_system_function(module, call)
            }
            Factor::Variable(_, _, _, _)
            | Factor::HierVariable(_)
            | Factor::FunctionCall(_)
            | Factor::Value(_)
            | Factor::Anonymous(_)
            | Factor::Unknown(_) => Ok(()),
        },
        Expression::Unary(_, inner, _) => validate_function_body_expression(module, inner),
        Expression::Binary(lhs, _, rhs, _) => {
            validate_function_body_expression(module, lhs)?;
            validate_function_body_expression(module, rhs)
        }
        Expression::Ternary(cond, then_expr, else_expr, _) => {
            validate_function_body_expression(module, cond)?;
            validate_function_body_expression(module, then_expr)?;
            validate_function_body_expression(module, else_expr)
        }
        Expression::Concatenation(items, _) => {
            for (item_expr, repeat_expr) in items {
                validate_function_body_expression(module, item_expr)?;
                if let Some(repeat_expr) = repeat_expr {
                    validate_function_body_expression(module, repeat_expr)?;
                }
            }
            Ok(())
        }
        Expression::ArrayLiteral(items, _) => {
            for item in items {
                match item {
                    ArrayLiteralItem::Value(item_expr, repeat_expr) => {
                        validate_function_body_expression(module, item_expr)?;
                        if let Some(repeat_expr) = repeat_expr {
                            validate_function_body_expression(module, repeat_expr)?;
                        }
                    }
                    ArrayLiteralItem::Defaul(default_expr) => {
                        validate_function_body_expression(module, default_expr)?;
                    }
                }
            }
            Ok(())
        }
        Expression::StructConstructor(_, fields, _) => {
            for (_, field_expr) in fields {
                validate_function_body_expression(module, field_expr)?;
            }
            Ok(())
        }
    }
}

fn validate_function_body_system_function(
    module: &Module,
    call: &SystemFunctionCall,
) -> Result<(), ParserError> {
    match &call.kind {
        SystemFunctionKind::Bits(input)
        | SystemFunctionKind::Size(input, _)
        | SystemFunctionKind::Clog2(input)
        | SystemFunctionKind::Onehot(input)
        | SystemFunctionKind::Signed(input)
        | SystemFunctionKind::Unsigned(input) => {
            validate_function_body_expression(module, &input.0)
        }
        SystemFunctionKind::Display(args) | SystemFunctionKind::Write(args) => {
            for arg in args {
                validate_function_body_expression(module, &arg.0)?;
            }
            Ok(())
        }
        SystemFunctionKind::Assert { cond, args, .. } => {
            validate_function_body_expression(module, &cond.0)?;
            for arg in args {
                validate_function_body_expression(module, &arg.0)?;
            }
            Ok(())
        }
        SystemFunctionKind::Readmemh(input, _) => {
            validate_function_body_expression(module, &input.0)
        }
        SystemFunctionKind::Finish => Ok(()),
    }
}

pub(super) fn validate_function_body_statement(
    module: &Module,
    stmt: &Statement,
) -> Result<(), ParserError> {
    match stmt {
        Statement::Assign(assign) => {
            validate_function_body_expression(module, &assign.expr)?;
            for dst in &assign.dst {
                for index_expr in &dst.index.0 {
                    validate_function_body_expression(module, index_expr)?;
                }
                for select_expr in &dst.select.0 {
                    validate_function_body_expression(module, select_expr)?;
                }
                if let Some((_, range_expr)) = &dst.select.1 {
                    validate_function_body_expression(module, range_expr)?;
                }
            }
            Ok(())
        }
        Statement::If(if_stmt) => {
            validate_function_body_expression(module, &if_stmt.cond)?;
            for stmt in &if_stmt.true_side {
                validate_function_body_statement(module, stmt)?;
            }
            for stmt in &if_stmt.false_side {
                validate_function_body_statement(module, stmt)?;
            }
            Ok(())
        }
        Statement::Case(case_stmt) => {
            validate_function_body_expression(module, &case_stmt.case_target)?;
            for arm in &case_stmt.arms {
                for pattern in &arm.patterns {
                    match pattern {
                        CasePattern::Eq(expr) => validate_function_body_expression(module, expr)?,
                        CasePattern::Range { lo, hi, .. } => {
                            validate_function_body_expression(module, lo)?;
                            validate_function_body_expression(module, hi)?;
                        }
                    }
                }
                for stmt in &arm.body {
                    validate_function_body_statement(module, stmt)?;
                }
            }
            for stmt in &case_stmt.default {
                validate_function_body_statement(module, stmt)?;
            }
            Ok(())
        }
        Statement::For(for_stmt) => {
            // The analyzer preserves loop ownership for source breaks;
            // eval_function_for tracks loop exits separately from returns.
            match &for_stmt.range {
                ForRange::Forward { start, end, .. }
                | ForRange::Reverse { start, end, .. }
                | ForRange::Stepped { start, end, .. } => {
                    if let ForBound::Expression(expr) = start {
                        validate_function_body_expression(module, expr)?;
                    }
                    if let ForBound::Expression(expr) = end {
                        validate_function_body_expression(module, expr)?;
                    }
                }
            }
            for stmt in &for_stmt.body {
                validate_function_body_statement(module, stmt)?;
            }
            Ok(())
        }
        Statement::IfReset(ir) => Err(ParserError::illegal_context(
            "statement in comb function body",
            format!("{stmt}"),
            Some(&ir.token),
        )),
        Statement::SystemFunctionCall(fc) => validate_function_body_system_function(module, fc),
        Statement::FunctionCall(call) => {
            for expr in call.inputs.values() {
                validate_function_body_expression(module, expr)?;
            }
            for dsts in call.outputs.values() {
                for dst in dsts {
                    for index_expr in &dst.index.0 {
                        validate_function_body_expression(module, index_expr)?;
                    }
                    for select_expr in &dst.select.0 {
                        validate_function_body_expression(module, select_expr)?;
                    }
                    if let Some((_, range_expr)) = &dst.select.1 {
                        validate_function_body_expression(module, range_expr)?;
                    }
                }
            }
            Ok(())
        }
        Statement::TbMethodCall(_)
        | Statement::Unsupported(_)
        | Statement::Break
        | Statement::Null => Ok(()),
    }
}
