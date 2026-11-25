//! Constant values written as the names of analyses and specializations
//! write them: the same text in every run, without the call site numbers,
//! type ids or addresses that their `Debug` forms may hold.
use super::*;
use crate::system_functions::Callee;

/// Constant expression `expr` as text that is the same in every run.
pub(super) fn const_expr(expr: &ConstExpr) -> String {
    match expr {
        ConstExpr::Literal(text) | ConstExpr::Ident(text) => text.clone(),
        ConstExpr::Select { expr, bit } => format!("{}[{}]", const_expr(expr), const_expr(bit)),
        ConstExpr::SelectRange {
            expr,
            msb,
            lsb,
            signed,
        } => format!(
            "{}[{}:{}]{}",
            const_expr(expr),
            const_expr(msb),
            const_expr(lsb),
            if *signed { "s" } else { "" }
        ),
        ConstExpr::Resize {
            expr,
            width,
            signed,
        } => format!(
            "{width}{}'({})",
            if *signed { "s" } else { "" },
            const_expr(expr)
        ),
        // The call site only tells the evaluations of one call apart.
        ConstExpr::Function { callee, args, .. } => format!(
            "{}({})",
            callee_name(callee),
            args.iter().map(const_expr).collect::<Vec<_>>().join(",")
        ),
        ConstExpr::Unary { op, expr } => format!("{op:?}({})", const_expr(expr)),
        ConstExpr::Binary { left, op, right } => {
            format!("{op:?}({},{})", const_expr(left), const_expr(right))
        }
        ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => format!(
            "({}?{}:{})",
            const_expr(condition),
            const_expr(then_expr),
            const_expr(else_expr)
        ),
    }
}

/// Expression `expr` as text that is the same in every run.
pub(super) fn expr(expr: &Expr) -> String {
    let list = |items: &[Expr]| items.iter().map(self::expr).collect::<Vec<_>>().join(",");
    match expr {
        Expr::Ident(text) | Expr::Literal(text) => text.clone(),
        Expr::Select {
            expr,
            msb,
            lsb,
            signed,
        } => format!(
            "{}[{}:{}]{}",
            self::expr(expr),
            const_expr(msb),
            const_expr(lsb),
            if *signed { "s" } else { "" }
        ),
        Expr::Concat(parts) => format!("{{{}}}", list(parts)),
        Expr::RepeatConcat { count, parts } => {
            format!("{{{}{{{}}}}}", const_expr(count), list(parts))
        }
        Expr::Resize {
            expr,
            width,
            signed,
        } => format!(
            "{width}{}'({})",
            if *signed { "s" } else { "" },
            self::expr(expr)
        ),
        Expr::Unary { op, expr } => format!("{op:?}({})", self::expr(expr)),
        Expr::Binary { left, op, right } => {
            format!("{op:?}({},{})", self::expr(left), self::expr(right))
        }
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => format!(
            "({}?{}:{})",
            self::expr(condition),
            self::expr(then_expr),
            self::expr(else_expr)
        ),
        Expr::Call {
            callee,
            args,
            arg_order,
        } => format!(
            "{}({}){}",
            callee_name(callee),
            args.iter()
                .map(|arg| arg.as_ref().map(self::expr).unwrap_or_default())
                .collect::<Vec<_>>()
                .join(","),
            if arg_order.is_empty() {
                String::new()
            } else {
                format!("{arg_order:?}")
            }
        ),
        Expr::Inside { expr, items } => format!(
            "{} inside {{{}}}",
            self::expr(expr),
            items
                .iter()
                .map(|item| match item {
                    InsideItem::Value(value) => self::expr(value),
                    InsideItem::Range { low, high } => {
                        format!("[{}:{}]", self::expr(low), self::expr(high))
                    }
                })
                .collect::<Vec<_>>()
                .join(",")
        ),
    }
}

/// The callee of a constant call: a subroutine by its name, or a built-in
/// operation, whose fields are widths, flags and other operations.
fn callee_name(callee: &Callee) -> String {
    match callee {
        Callee::Subroutine(name) => name.clone(),
        callee => format!("{callee:?}"),
    }
}
