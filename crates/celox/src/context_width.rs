use veryl_analyzer::ir::{ArrayLiteralItem, Expression, Factor, Op};

/// Calculate the context width for a given expression and parent context.
pub fn get_context_width(expr: &Expression, parent_width: Option<usize>) -> Option<usize> {
    match expr {
        Expression::Binary(lhs, op, rhs) => {
            if matches!(op, Op::As) {
                // `as` cast: RHS is type metadata, doesn't inherit context width.
                return None;
            }
            if matches!(
                op,
                Op::ArithShiftL | Op::ArithShiftR | Op::LogicShiftL | Op::LogicShiftR | Op::Pow
            ) {
                return parent_width.map(|w| w.max(get_expr_width(lhs).unwrap_or(w)));
            }
            if matches!(
                op,
                Op::Eq
                    | Op::Ne
                    | Op::EqWildcard
                    | Op::NeWildcard
                    | Op::Less
                    | Op::LessEq
                    | Op::Greater
                    | Op::GreaterEq
                    | Op::LogicAnd
                    | Op::LogicNot
                    | Op::LogicOr
            ) {
                return Some(1);
            }
            // 親の幅があればそれを優先
            if let Some(w) = parent_width {
                return Some(w);
            }
            // なければ左右の最大幅
            let lhs_width = get_expr_width(lhs);
            let rhs_width = get_expr_width(rhs);
            lhs_width
                .or(rhs_width)
                .map(|lw| lw.max(rhs_width.unwrap_or(lw)))
        }
        Expression::Unary(op, expr) => {
            if matches!(
                op,
                Op::BitAnd
                    | Op::BitNand
                    | Op::BitNor
                    | Op::BitXnor
                    | Op::BitXor
                    | Op::BitOr
                    | Op::LogicNot
            ) {
                return Some(1);
            }
            get_context_width(expr, parent_width)
        }
        Expression::Ternary(_cond, then, els) => {
            let lw = get_context_width(then, parent_width);
            let rw = get_context_width(els, parent_width);
            lw.or(rw).map(|w| lw.unwrap_or(w).max(rw.unwrap_or(w)))
        }
        Expression::Concatenation(..)
        | Expression::ArrayLiteral(..)
        | Expression::StructConstructor(..) => get_expr_width(expr),
        Expression::Term(factor) => {
            get_factor_width(factor).map(|w| w.max(parent_width.unwrap_or(w)))
        }
    }
}

/// Helper: get width from an Expression (if possible)
pub fn get_expr_width(expr: &Expression) -> Option<usize> {
    match expr {
        Expression::Term(factor) => get_factor_width(factor),
        Expression::Binary(lhs, op, rhs) => match op {
            Op::Eq
            | Op::Ne
            | Op::Less
            | Op::LessEq
            | Op::Greater
            | Op::GreaterEq
            | Op::EqWildcard
            | Op::NeWildcard
            | Op::LogicAnd
            | Op::LogicOr => Some(1),
            Op::As => get_expr_width(rhs),
            Op::LogicShiftL | Op::LogicShiftR | Op::ArithShiftL | Op::ArithShiftR => {
                get_expr_width(lhs)
            }
            _ => {
                let lw = get_expr_width(lhs);
                let rw = get_expr_width(rhs);
                lw.or(rw).map(|w| lw.unwrap_or(w).max(rw.unwrap_or(w)))
            }
        },
        Expression::Unary(op, expr) => match op {
            Op::BitAnd
            | Op::BitOr
            | Op::BitXor
            | Op::BitNand
            | Op::BitNor
            | Op::BitXnor
            | Op::LogicNot => Some(1),
            _ => get_expr_width(expr),
        },
        Expression::Ternary(_cond, then, els) => {
            let lw = get_expr_width(then);
            let rw = get_expr_width(els);
            lw.or(rw).map(|w| lw.unwrap_or(w).max(rw.unwrap_or(w)))
        }
        Expression::Concatenation(exprs) => {
            let mut total = 0;
            for (sub, rep) in exprs {
                let w = get_expr_width(sub)?;
                let count = if let Some(rep_expr) = rep {
                    if let Expression::Term(f) = rep_expr {
                        if let Factor::Value(v, _) = f.as_ref() {
                            v.get_value().ok().and_then(|val| val.to_usize())?
                        } else {
                            return None;
                        }
                    } else {
                        return None;
                    }
                } else {
                    1
                };
                total += w * count;
            }
            Some(total)
        }
        Expression::ArrayLiteral(items) => {
            let mut total = 0;
            for item in items {
                match item {
                    ArrayLiteralItem::Value(expr, rep) => {
                        let w = get_expr_width(expr)?;
                        let count = if let Some(rep_expr) = rep {
                            if let Expression::Term(f) = rep_expr {
                                if let Factor::Value(v, _) = f.as_ref() {
                                    v.get_value().ok().and_then(|val| val.to_usize())?
                                } else {
                                    return None;
                                }
                            } else {
                                return None;
                            }
                        } else {
                            1
                        };
                        total += w * count;
                    }
                    ArrayLiteralItem::Defaul(_) => return None, // Default makes it hard to estimate total width without context
                }
            }
            Some(total)
        }
        Expression::StructConstructor(ty, _) => {
            ty.total_width().map(|w| ty.array.total().unwrap_or(1) * w)
        }
    }
}

fn get_factor_width(factor: &Factor) -> Option<usize> {
    match factor {
        Factor::Value(comp, _) | Factor::Variable(_, _, _, comp, _) => comp
            .r#type
            .total_width()
            .map(|w| comp.r#type.array.total().unwrap_or(1) * w),
        Factor::FunctionCall(call, _) => call.ret.as_ref().and_then(|x| {
            x.r#type
                .total_width()
                .map(|w| x.r#type.array.total().unwrap_or(1) * w)
        }),
        _ => None,
    }
}
