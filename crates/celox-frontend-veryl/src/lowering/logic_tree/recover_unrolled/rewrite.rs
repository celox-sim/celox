//! Rewriting loop leaves of recovered statements.

use super::*;

pub(super) enum RewriteMode {
    Concrete(usize),
    Dynamic,
}

pub(super) fn rewrite_statements(
    statements: &mut [Statement],
    loop_var: VarId,
    mode: RewriteMode,
) -> bool {
    statements
        .iter_mut()
        .all(|statement| rewrite_statement(statement, loop_var, &mode))
}

pub(super) fn rewrite_statement(
    statement: &mut Statement,
    loop_var: VarId,
    mode: &RewriteMode,
) -> bool {
    match statement {
        Statement::Assign(assign) if assign.dst.len() == 1 => {
            let destination = &mut assign.dst[0];
            rewrite_index(&mut destination.index, loop_var, mode).is_some()
                && rewrite_select(&mut destination.select, loop_var, mode).is_some()
                && rewrite_expression(&mut assign.expr, loop_var, mode).is_some()
        }
        Statement::If(statement) => {
            rewrite_expression(&mut statement.cond, loop_var, mode).is_some()
                && statement
                    .true_side
                    .iter_mut()
                    .all(|statement| rewrite_statement(statement, loop_var, mode))
                && statement
                    .false_side
                    .iter_mut()
                    .all(|statement| rewrite_statement(statement, loop_var, mode))
        }
        _ => false,
    }
}

pub(super) fn rewrite_index(
    index: &mut VarIndex,
    loop_var: VarId,
    mode: &RewriteMode,
) -> Option<bool> {
    let mut depends = false;
    for expression in &mut index.0 {
        depends |= rewrite_expression(expression, loop_var, mode)?;
    }
    Some(depends)
}

pub(super) fn rewrite_select(
    select: &mut VarSelect,
    loop_var: VarId,
    mode: &RewriteMode,
) -> Option<bool> {
    let mut depends = false;
    for expression in &mut select.0 {
        depends |= rewrite_expression(expression, loop_var, mode)?;
    }
    if let Some((_, expression)) = &mut select.1 {
        depends |= rewrite_expression(expression, loop_var, mode)?;
    }
    Some(depends)
}

pub(super) fn rewrite_expression(
    expression: &mut Expression,
    loop_var: VarId,
    mode: &RewriteMode,
) -> Option<bool> {
    let depends = match expression {
        Expression::Term(factor) => match factor.as_mut() {
            Factor::Variable(variable, index, select, comptime) => {
                let index_depends = rewrite_index(index, loop_var, mode)?;
                let select_depends = rewrite_select(select, loop_var, mode)?;
                let self_depends = *variable == loop_var;
                if self_depends {
                    rewrite_loop_leaf(comptime, mode);
                } else if index_depends || select_depends {
                    invalidate_dependent_comptime(comptime, mode);
                }
                self_depends || index_depends || select_depends
            }
            Factor::HierVariable(_) => return None,
            Factor::Value(_) => false,
            Factor::FunctionCall(call) if call.outputs.is_empty() => {
                let mut depends = false;
                for input in call.inputs.values_mut() {
                    depends |= rewrite_expression(input, loop_var, mode)?;
                }
                if depends {
                    invalidate_dependent_comptime(&mut call.comptime, mode);
                }
                depends
            }
            Factor::SystemFunctionCall(_)
            | Factor::FunctionCall(_)
            | Factor::Anonymous(_)
            | Factor::Unknown(_) => return None,
        },
        Expression::Unary(_, inner, _) => rewrite_expression(inner, loop_var, mode)?,
        Expression::Binary(lhs, _, rhs, _) => {
            rewrite_expression(lhs, loop_var, mode)? | rewrite_expression(rhs, loop_var, mode)?
        }
        Expression::Ternary(cond, then_expr, else_expr, _) => {
            rewrite_expression(cond, loop_var, mode)?
                | rewrite_expression(then_expr, loop_var, mode)?
                | rewrite_expression(else_expr, loop_var, mode)?
        }
        Expression::Concatenation(items, _) => {
            let mut depends = false;
            for (value, repeat) in items {
                depends |= rewrite_expression(value, loop_var, mode)?;
                if let Some(repeat) = repeat {
                    depends |= rewrite_expression(repeat, loop_var, mode)?;
                }
            }
            depends
        }
        Expression::ArrayLiteral(items, _) => {
            let mut depends = false;
            for item in items {
                match item {
                    ArrayLiteralItem::Value(value, repeat) => {
                        depends |= rewrite_expression(value, loop_var, mode)?;
                        if let Some(repeat) = repeat {
                            depends |= rewrite_expression(repeat, loop_var, mode)?;
                        }
                    }
                    ArrayLiteralItem::Defaul(value) => {
                        depends |= rewrite_expression(value, loop_var, mode)?;
                    }
                }
            }
            depends
        }
        Expression::StructConstructor(_, fields, _) => {
            let mut depends = false;
            for (_, value) in fields {
                depends |= rewrite_expression(value, loop_var, mode)?;
            }
            depends
        }
    };
    if depends && !matches!(expression, Expression::Term(_)) {
        invalidate_dependent_comptime(expression.comptime_mut(), mode);
    }
    Some(depends)
}

pub(super) fn rewrite_loop_leaf(comptime: &mut Comptime, mode: &RewriteMode) {
    match mode {
        RewriteMode::Concrete(value) => {
            let width = comptime.r#type.total_width().unwrap_or(0);
            comptime.value =
                ValueVariant::Numeric(Value::new(*value as u64, width, comptime.r#type.signed));
            comptime.is_const = true;
            comptime.expr_context.is_const = true;
            comptime.evaluated = true;
        }
        RewriteMode::Dynamic => {
            comptime.value = ValueVariant::Unknown;
            comptime.is_const = false;
            comptime.is_global = false;
            comptime.expr_context.is_const = false;
            comptime.expr_context.is_global = false;
            comptime.evaluated = false;
        }
    }
}

pub(super) fn invalidate_dependent_comptime(comptime: &mut Comptime, mode: &RewriteMode) {
    comptime.value = ValueVariant::Unknown;
    comptime.evaluated = false;
    if matches!(mode, RewriteMode::Dynamic) {
        comptime.is_const = false;
        comptime.is_global = false;
        comptime.expr_context.is_const = false;
        comptime.expr_context.is_global = false;
    }
}
