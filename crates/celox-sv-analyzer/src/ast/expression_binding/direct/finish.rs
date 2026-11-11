//! Finishing an expression: its value of the bound operands.
use super::*;

/// The value `finish` makes of `operands`, the bound operands of its
/// expression.
pub(super) fn finish_value(
    finish: Finish,
    mut operands: Vec<Expr>,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
) -> Converted<Expr> {
    let value = match finish {
        Finish::Tagged(plan) => {
            packed_structs::construct(tree, session, plan, operands.pop(), dimensions)?
        }
        Finish::Unary(op) => {
            let operand = operands.pop().ok_or_else(disagreement)?;
            if strings::string_value(&operand, dimensions) {
                return Err(unsupported(format!("operator `{op}` on a string operand")));
            }
            if real_value(&operand, dimensions) && !matches!(op.as_str(), "+" | "-" | "!") {
                return Err(unsupported("operator on a real operand"));
            }
            constants::unary_expr_from_text(&op, operand)
                .ok_or_else(|| unsupported(format!("unary operator `{op}`")))?
        }
        Finish::Binary(op) => finish_binary(op, operands, dimensions)?,
        Finish::Bits => {
            let operand = operands.pop().ok_or_else(disagreement)?;
            let ty = dimensions::bound_bits_type(tree, &operand, dimensions)
                .ok_or_else(|| unsupported("operand of `$bits`"))?;
            Expr::Literal(ty.width.to_string())
        }
        Finish::Mux(container) => {
            let mut else_expr = operands.pop().ok_or_else(disagreement)?;
            let mut then_expr = operands.pop().ok_or_else(disagreement)?;
            let condition = operands.pop().ok_or_else(disagreement)?;
            if strings::string_value(&then_expr, dimensions)
                || strings::string_value(&else_expr, dimensions)
            {
                then_expr = strings::to_string(then_expr, dimensions);
                else_expr = strings::to_string(else_expr, dimensions);
            }
            let value = Expr::Mux {
                condition: Box::new(condition),
                then_expr: Box::new(then_expr),
                else_expr: Box::new(else_expr),
            };
            match container {
                Some(layout) => containers::call(ContainerOp::Conditional, layout, vec![value]),
                None => value,
            }
        }
        Finish::Concat
            if operands
                .iter()
                .any(|operand| strings::string_value(operand, dimensions)) =>
        {
            strings::call(StringOp::Concat, operands)
        }
        Finish::Concat => Expr::Concat(operands),
        Finish::EnumMethod {
            method,
            domain,
            count,
        } => {
            let count = count.then(|| operands.pop()).flatten();
            enums::call(
                tree,
                session,
                method,
                &domain,
                operands.pop(),
                count,
                dimensions,
            )?
        }
        Finish::StringCall(op) => {
            let args = strings::operands(op.clone(), operands, dimensions);
            strings::call(op, args)
        }
        Finish::Format {
            ticks_per_unit,
            ids,
        } => format_patterns::format(
            ticks_per_unit,
            &ids,
            operands,
            tree,
            session,
            native,
            dimensions,
        )?,
        Finish::ScopeRandomize(domains) => {
            // Preserve the original call when no enum is present.
            // Every scalar operation succeeds and returns signed int 1;
            // bitwise AND preserves that type and evaluates all operands.
            if domains.iter().all(Option::is_none) {
                Expr::Call {
                    callee: Callee::System(SystemFunction::ScopeRandomize),
                    args: operands.into_iter().map(Some).collect(),
                    arg_order: Vec::new(),
                }
            } else {
                let mut calls = operands.into_iter().zip(domains).map(|(operand, domain)| {
                    let mut args = vec![Some(operand)];
                    let function = if let Some(domain) = domain {
                        // Each member at its base type's width
                        // and signing, as the variable holds it.
                        args.extend(domain.values().into_iter().map(|value| {
                            Some(Expr::Literal(value.literal(domain.width, domain.signed)))
                        }));
                        SystemFunction::ScopeRandomizeEnum
                    } else {
                        SystemFunction::ScopeRandomize
                    };
                    Expr::Call {
                        callee: Callee::System(function),
                        args,
                        arg_order: Vec::new(),
                    }
                });
                let first = calls.next().ok_or_else(disagreement)?;
                calls.fold(first, |left, right| Expr::Binary {
                    left: Box::new(left),
                    op: BinaryOp::BitAnd,
                    right: Box::new(right),
                })
            }
        }
        Finish::ObjectLoad(property) => {
            let handle = operands.pop().ok_or_else(disagreement)?;
            objects::load(&property, handle)
        }
        Finish::VirtualMember(access) => {
            let (ty, member) = *access;
            let handle = operands.pop().ok_or_else(disagreement)?;
            virtual_interfaces::read(tree, session, handle, &ty, &member, dimensions, Ok)?
        }
        Finish::MethodCall(call) => finish_method_call(call, operands)?,
        Finish::Cast(target) => {
            let operand = remove_real_division_guards(
                guard_zero_divisions(tree, session, operands.pop().ok_or_else(disagreement)?),
                dimensions,
            );
            if dimensions.constant_indexed_base {
                let operand = selects::indexed_expression_value(tree, session, operand, dimensions)
                    .ok_or_else(|| unsupported("constant cast operand"))?;
                const_expr_to_expr(
                    casts::cast_constant_target(
                        tree,
                        session,
                        operand,
                        target,
                        &dimensions.const_env,
                    )
                    .ok_or_else(|| unsupported("constant cast operand"))?,
                )
            } else {
                casts::runtime_cast_target_expr(tree, target, operand, dimensions)?
            }
        }
        Finish::Repeat { .. }
            if operands[1..]
                .iter()
                .any(|operand| strings::string_value(operand, dimensions)) =>
        {
            // A string replication count need not be constant
            // (11.4.12.2).
            let count = operands.remove(0);
            let string = if operands.len() == 1 {
                strings::to_string(operands.remove(0), dimensions)
            } else {
                strings::call(StringOp::Concat, operands)
            };
            strings::call(StringOp::Replicate, vec![string, count])
        }
        Finish::Repeat { literals } => {
            let count = operands.remove(0);
            let constant = expr_to_const(session, count.clone())
                .or_else(|| expr_to_index_const(tree, session, count.clone(), dimensions));
            // A count that names variables is not constant, even
            // as an index expression.
            let constant = constant.filter(|count| {
                !literals
                    || eval_ast_const_expr(&tree.functions, count, &dimensions.const_env).is_some()
            });
            match constant {
                Some(count) => Expr::RepeatConcat {
                    count,
                    parts: operands,
                },
                // A replication of string literals by a count that
                // is not constant is a string replication: the
                // literals convert to strings, and the result is
                // that many copies (Table 6-9, 11.4.12.2).
                None if literals => {
                    let parts: Vec<Expr> = operands
                        .into_iter()
                        .map(|operand| strings::to_string(operand, dimensions))
                        .collect();
                    let string = match <[Expr; 1]>::try_from(parts) {
                        Ok([string]) => string,
                        Err(parts) => strings::call(StringOp::Concat, parts),
                    };
                    strings::call(StringOp::Replicate, vec![string, count])
                }
                None => Err(unsupported("replication count"))?,
            }
        }
        Finish::Inside(ranges) => {
            let mut operands = operands.into_iter();
            let operand = operands.next().ok_or_else(disagreement)?;
            let mut items = Vec::new();
            for range in ranges {
                let low = operands.next().ok_or_else(disagreement)?;
                items.push(if range {
                    InsideItem::Range {
                        low,
                        high: operands.next().ok_or_else(disagreement)?,
                    }
                } else {
                    InsideItem::Value(low)
                });
            }
            Expr::Inside {
                expr: Box::new(operand),
                items,
            }
        }
        Finish::Call {
            name,
            args,
            order,
            system,
        } => {
            if let Some(function) = system {
                finish_system_call(
                    function, name, args, operands, native, tree, session, dimensions,
                )?
            } else {
                finish_subroutine_call(
                    name, args, order, operands, native, tree, session, dimensions,
                )?
            }
        }
    };
    Ok(value)
}

/// The value of binary operator `op` of its bound operands.
fn finish_binary(
    op: String,
    mut operands: Vec<Expr>,
    dimensions: &PackedDimensions,
) -> Converted<Expr> {
    let right = operands.pop().ok_or_else(disagreement)?;
    let left = operands.pop().ok_or_else(disagreement)?;
    if strings::string_value(&left, dimensions) || strings::string_value(&right, dimensions) {
        return strings::binary(&op, left, right, dimensions);
    }
    if (real_value(&left, dimensions) || real_value(&right, dimensions))
        && !matches!(
            op.as_str(),
            "+" | "-"
                | "*"
                | "/"
                | "**"
                | "=="
                | "!="
                | "<"
                | "<="
                | ">"
                | ">="
                | "&&"
                | "||"
                | "->"
                | "<->"
        )
    {
        return Err(unsupported("operator on a real operand"));
    }
    let implication = op == "->";
    let equivalence = op == "<->";
    let left = if implication || equivalence {
        Expr::Unary {
            op: UnaryOp::LogicNot,
            expr: Box::new(left),
        }
    } else {
        left
    };
    // Each truth-value conversion evaluates its operand once (11.4.7).
    let right = if equivalence {
        Expr::Unary {
            op: UnaryOp::LogicNot,
            expr: Box::new(right),
        }
    } else {
        right
    };
    let xnor = matches!(op.as_str(), "~^" | "^~");
    let value = Expr::Binary {
        left: Box::new(left),
        op: if equivalence {
            BinaryOp::Eq
        } else if implication {
            BinaryOp::LogicOr
        } else if xnor {
            BinaryOp::BitXor
        } else {
            constants::binary_op_from_text(&op)
                .ok_or_else(|| unsupported(format!("binary operator `{op}`")))?
        },
        right: Box::new(right),
    };
    Ok(if xnor {
        Expr::Unary {
            op: UnaryOp::BitNot,
            expr: Box::new(value),
        }
    } else {
        value
    })
}
