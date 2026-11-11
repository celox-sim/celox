//! The bit ranges a list of statements may write.

use super::*;

pub(super) fn constant_case_pattern_matches(target: &Expression, pattern: &CasePattern) -> bool {
    fn compare(op: Op, lhs: &Value, rhs: &Value) -> bool {
        let signed = lhs.signed() && rhs.signed();
        op.eval_value_binary(lhs, rhs, 1, signed, &mut MaskCache::default())
            .to_u64()
            == Some(1)
    }

    if !target.comptime().is_const {
        return false;
    }
    // A compound target was folded in its own width and signedness. A label
    // that widens it or makes the comparison unsigned recomputes it, so the
    // folded value does not decide that comparison.
    let expression_width = |expression: &Expression| {
        crate::context_width::get_expr_width(expression)
            .or_else(|| expression.comptime().r#type.total_width())
    };
    let target_width = expression_width(target);
    let target_signed = crate::context_width::expression_signed(target);
    let folded_in_context = |label: &Expression| {
        matches!(target, Expression::Term(_))
            || (expression_width(label)
                .zip(target_width)
                .is_some_and(|(label, target)| label <= target)
                && (!target_signed || crate::context_width::expression_signed(label)))
    };
    let Ok(target) = target.comptime().get_value() else {
        return false;
    };
    match pattern {
        CasePattern::Eq(expression) => {
            expression.comptime().is_const
                && folded_in_context(expression)
                && expression
                    .comptime()
                    .get_value()
                    .is_ok_and(|pattern| compare(Op::EqWildcard, target, pattern))
        }
        CasePattern::Range { lo, hi, inclusive } => {
            if !lo.comptime().is_const
                || !hi.comptime().is_const
                || !folded_in_context(lo)
                || !folded_in_context(hi)
            {
                return false;
            }
            let (Ok(lo), Ok(hi)) = (lo.comptime().get_value(), hi.comptime().get_value()) else {
                return false;
            };
            compare(Op::LessEq, lo, target)
                && compare(if *inclusive { Op::LessEq } else { Op::Less }, target, hi)
        }
    }
}

pub(super) fn collect_written_accesses(
    module: &Module,
    statements: &[Statement],
    out: &mut HashMap<VarId, Vec<BitAccess>>,
) -> Result<(), ParserError> {
    for stmt in statements {
        match stmt {
            Statement::Assign(assign) => {
                collect_written_expression(module, &assign.expr, out)?;
                for dst in &assign.dst {
                    collect_written_destination(module, out, dst)?;
                }
            }
            Statement::If(if_stmt) => {
                collect_written_expression(module, &if_stmt.cond, out)?;
                collect_written_accesses(module, &if_stmt.true_side, out)?;
                collect_written_accesses(module, &if_stmt.false_side, out)?;
            }
            Statement::Case(case_stmt) => {
                collect_written_expression(module, &case_stmt.case_target, out)?;
                let mut known_match = false;
                for arm in &case_stmt.arms {
                    let mut arm_known_match = false;
                    for pattern in &arm.patterns {
                        match pattern {
                            CasePattern::Eq(expression) => {
                                collect_written_expression(module, expression, out)?;
                            }
                            CasePattern::Range { lo, hi, .. } => {
                                collect_written_expression(module, lo, out)?;
                                collect_written_expression(module, hi, out)?;
                            }
                        }
                        if constant_case_pattern_matches(&case_stmt.case_target, pattern) {
                            arm_known_match = true;
                            break;
                        }
                    }
                    collect_written_accesses(module, &arm.body, out)?;
                    if arm_known_match {
                        known_match = true;
                        break;
                    }
                }
                if !known_match {
                    collect_written_accesses(module, &case_stmt.default, out)?;
                }
            }
            Statement::For(for_stmt) => {
                let (start, end) = match &for_stmt.range {
                    ForRange::Forward { start, end, .. }
                    | ForRange::Reverse { start, end, .. }
                    | ForRange::Stepped { start, end, .. } => (start, end),
                };
                for bound in [start, end] {
                    if let ForBound::Expression(expression) = bound {
                        collect_written_expression(module, expression, out)?;
                    }
                }
                collect_written_accesses(module, &for_stmt.body, out)?;
            }
            Statement::IfReset(if_reset) => {
                collect_written_accesses(module, &if_reset.true_side, out)?;
                collect_written_accesses(module, &if_reset.false_side, out)?;
            }
            Statement::FunctionCall(call) => {
                for input in call.inputs.values() {
                    collect_written_expression(module, input, out)?;
                }
                for dsts in call.outputs.values() {
                    for dst in dsts {
                        collect_written_destination(module, out, dst)?;
                    }
                }
            }
            Statement::SystemFunctionCall(call) => {
                collect_written_system_function_call(module, call, out)?;
            }
            Statement::TbMethodCall(_)
            | Statement::Break
            | Statement::Unsupported(_)
            | Statement::Null => {}
        }
    }
    Ok(())
}

pub(super) fn eval_fully_known_constexpr(expression: &Expression) -> Option<BigUint> {
    let (value, mask, _, _) = celox_value_from_comptime(expression.comptime())?;
    if !mask.is_zero() {
        return None;
    }
    Some(value)
}

pub(crate) fn collect_written_expression(
    module: &Module,
    expression: &Expression,
    out: &mut HashMap<VarId, Vec<BitAccess>>,
) -> Result<(), ParserError> {
    match expression {
        Expression::Term(factor) => match factor.as_ref() {
            Factor::FunctionCall(call) => {
                for input in call.inputs.values() {
                    collect_written_expression(module, input, out)?;
                }
                for destinations in call.outputs.values() {
                    for destination in destinations {
                        collect_written_destination(module, out, destination)?;
                    }
                }
                Ok(())
            }
            Factor::Variable(_, index, select, _) => {
                for expression in index.0.iter().chain(select.0.iter()) {
                    collect_written_expression(module, expression, out)?;
                }
                Ok(())
            }
            Factor::HierVariable(reference) => Err(ParserError::illegal_context(
                "hierarchical variable reference",
                format!(
                    "`{}` is only valid in a native testbench block",
                    reference.var_path
                ),
                Some(&reference.comptime.token),
            )),
            Factor::SystemFunctionCall(call) => {
                collect_written_system_function_call(module, call, out)
            }
            Factor::Value(_) | Factor::Anonymous(_) | Factor::Unknown(_) => Ok(()),
        },
        Expression::Unary(_, inner, _) => collect_written_expression(module, inner, out),
        Expression::Binary(lhs, op, rhs, _) => {
            collect_written_expression(module, lhs, out)?;
            let lhs_value = eval_fully_known_constexpr(lhs);
            let skips_rhs = match op {
                Op::LogicAnd => lhs_value
                    .as_ref()
                    .is_some_and(|value| value == &BigUint::from(0u8)),
                Op::LogicOr => lhs_value
                    .as_ref()
                    .is_some_and(|value| value != &BigUint::from(0u8)),
                _ => false,
            };
            if skips_rhs {
                return Ok(());
            }
            collect_written_expression(module, rhs, out)
        }
        Expression::Ternary(cond, then_expr, else_expr, _) => {
            collect_written_expression(module, cond, out)?;
            if let Some(value) = eval_fully_known_constexpr(cond) {
                return if value == BigUint::from(0u8) {
                    collect_written_expression(module, else_expr, out)
                } else {
                    collect_written_expression(module, then_expr, out)
                };
            }
            collect_written_expression(module, then_expr, out)?;
            collect_written_expression(module, else_expr, out)
        }
        Expression::Concatenation(parts, _) => {
            for (part, _) in parts {
                collect_written_expression(module, part, out)?;
            }
            Ok(())
        }
        Expression::ArrayLiteral(items, _) => {
            for item in items {
                match item {
                    ArrayLiteralItem::Value(expression, _) => {
                        collect_written_expression(module, expression, out)?;
                    }
                    ArrayLiteralItem::Defaul(expression) => {
                        collect_written_expression(module, expression, out)?;
                    }
                }
            }
            Ok(())
        }
        Expression::StructConstructor(_, fields, _) => {
            for (_, field) in fields {
                collect_written_expression(module, field, out)?;
            }
            Ok(())
        }
    }
}

pub(super) fn collect_written_system_function_call(
    module: &Module,
    call: &SystemFunctionCall,
    out: &mut HashMap<VarId, Vec<BitAccess>>,
) -> Result<(), ParserError> {
    let mut collect_input =
        |input: &SystemFunctionInput| collect_written_expression(module, &input.0, out);
    match &call.kind {
        // These operands are queried for shape only and are not evaluated at
        // runtime, so nested output arguments are not writes of this process.
        SystemFunctionKind::Bits(_) | SystemFunctionKind::Size(..) => Ok(()),
        SystemFunctionKind::Clog2(input)
        | SystemFunctionKind::Onehot(input)
        | SystemFunctionKind::Signed(input)
        | SystemFunctionKind::Unsigned(input) => collect_input(input),
        SystemFunctionKind::Readmemh(input, output) => {
            collect_input(input)?;
            if let veryl_analyzer::ir::SystemFunctionOutput::Local(destinations) = output {
                for destination in destinations {
                    let width = resolve_total_width(module, &module.variables[&destination.id])?;
                    out.entry(destination.id)
                        .or_default()
                        .push(BitAccess::new(0, width - 1));
                }
            }
            Ok(())
        }
        SystemFunctionKind::Display(inputs) | SystemFunctionKind::Write(inputs) => {
            for input in inputs {
                collect_input(input)?;
            }
            Ok(())
        }
        SystemFunctionKind::Assert { cond, args, .. } => {
            collect_input(cond)?;
            for input in args {
                collect_input(input)?;
            }
            Ok(())
        }
        SystemFunctionKind::Finish => Ok(()),
    }
}

pub(super) fn collect_written_destination(
    module: &Module,
    out: &mut HashMap<VarId, Vec<BitAccess>>,
    dst: &veryl_analyzer::ir::AssignDestination,
) -> Result<(), ParserError> {
    for expression in dst.index.0.iter().chain(dst.select.0.iter()) {
        collect_written_expression(module, expression, out)?;
    }
    let access = eval_var_select(module, dst.id, &dst.index, &dst.select)?;
    out.entry(dst.id).or_default().push(access);
    Ok(())
}
