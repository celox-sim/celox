use crate::parser::bitaccess::eval_constexpr;
use num_traits::ToPrimitive as _;
use veryl_analyzer::ir::{
    ArrayLiteralItem, Expression, Factor, Op, SystemFunctionKind, ValueVariant,
};

use crate::ir::BinaryOp;

/// The width and signedness in which an expression is evaluated by its parent.
///
/// Width and signedness have to travel together.  In particular, a mixed
/// signed/unsigned binary expression zero-extends *both* operands to the common
/// width; extending each operand from its declaration signedness is incorrect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ValueContext {
    pub width: usize,
    pub signed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CastSemantics {
    pub width: usize,
    pub source_is_2state: bool,
    /// Signedness used while resizing the source to the cast width.
    pub source_signed: bool,
    /// Signedness of the value after the cast.
    pub result_signed: bool,
    /// State kind of the value after the cast.
    pub result_is_2state: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BinarySemantics {
    pub lhs_context: Option<ValueContext>,
    pub rhs_context: Option<ValueContext>,
    /// Width of the binary instruction itself.  Comparisons and logical
    /// operations produce one bit even when their operands are wider.
    pub result_width: usize,
    /// Signedness intrinsic to the result before a surrounding expression
    /// applies another context.
    pub result_signed: bool,
    /// Signedness after context propagation, used to select signed opcodes.
    pub lhs_signed: bool,
    pub rhs_signed: bool,
}

/// Signedness of an expression before a parent expression applies its common
/// operand context.
///
/// The analyzer can overwrite `expr_context.signed` while propagating an outer
/// context through a sibling. Source-level casts and term types therefore
/// remain authoritative at the semantic boundary.
pub(crate) fn expression_signed(expr: &Expression) -> bool {
    match expr {
        Expression::Term(factor) => factor_signed(factor),
        Expression::Binary(lhs, op, rhs, _) => match op {
            Op::As => cast_semantics(lhs, rhs)
                .map(|semantics| semantics.result_signed)
                .unwrap_or(false),
            Op::Eq
            | Op::EqWildcard
            | Op::Ne
            | Op::NeWildcard
            | Op::Less
            | Op::LessEq
            | Op::Greater
            | Op::GreaterEq
            | Op::LogicAnd
            | Op::LogicOr => false,
            Op::LogicShiftL | Op::LogicShiftR | Op::ArithShiftL | Op::ArithShiftR | Op::Pow => {
                expression_signed(lhs)
            }
            _ => expression_signed(lhs) && expression_signed(rhs),
        },
        Expression::Unary(op, inner, _) => match op {
            Op::BitAnd
            | Op::BitNand
            | Op::BitOr
            | Op::BitNor
            | Op::BitXor
            | Op::BitXnor
            | Op::LogicNot => false,
            _ => expression_signed(inner),
        },
        Expression::Ternary(_, then_expr, else_expr, _) => {
            expression_signed(then_expr) && expression_signed(else_expr)
        }
        Expression::Concatenation(..) | Expression::ArrayLiteral(..) => false,
        // Packed struct constructor expressions are unsigned. Veryl currently
        // rejects a `signed` modifier on a struct type, but keep this explicit
        // instead of depending on that frontend restriction.
        Expression::StructConstructor(_, _, _) => false,
    }
}

pub(crate) fn factor_signed(factor: &Factor) -> bool {
    match factor {
        Factor::SystemFunctionCall(call) => match call.kind {
            SystemFunctionKind::Signed(_) => true,
            SystemFunctionKind::Unsigned(_) => false,
            _ => call.comptime.r#type.signed,
        },
        // Constant folding resets expr_context and can leave the copied type
        // describing the pre-selection base. The evaluated Value is the only
        // remaining unsigned-select fact in that AIR shape. Cases where
        // folding also erases a type-cast boundary are an upstream AIR loss,
        // tracked in docs/internals/veryl-analyzer-upstream-issues.md.
        Factor::Value(comptime) => comptime
            .get_value()
            .map(|value| value.signed())
            .unwrap_or(comptime.expr_context.signed),
        // VarSelect is a packed bit/part selection. Its value is unsigned;
        // VarIndex has already been split out and does not change signedness.
        Factor::Variable(_, _, select, comptime) => {
            select.is_empty() && comptime.expr_context.signed
        }
        Factor::FunctionCall(call) => call.comptime.r#type.signed,
        Factor::Anonymous(comptime) | Factor::Unknown(comptime) => comptime.r#type.signed,
    }
}

/// Resolve `as` as two distinct operations: resize from the source type, then
/// reinterpret as the target type. A numeric width cast changes only the width:
/// signedness and the 2/4-state kind both pass through from the source.
pub(crate) fn cast_semantics(source: &Expression, target: &Expression) -> Option<CastSemantics> {
    let Expression::Term(factor) = target else {
        return None;
    };
    let Factor::Value(comptime) = factor.as_ref() else {
        return None;
    };
    let source_signed = expression_signed(source);
    let source_is_2state = source.comptime().r#type.is_2state();
    match &comptime.value {
        ValueVariant::Type(ty) => Some(CastSemantics {
            width: ty.total_width()?,
            source_is_2state,
            source_signed,
            result_signed: ty.signed,
            result_is_2state: ty.is_2state(),
        }),
        ValueVariant::Numeric(width) => Some(CastSemantics {
            width: width.to_usize()?,
            source_is_2state,
            source_signed,
            result_signed: source_signed,
            result_is_2state: source_is_2state,
        }),
        _ => None,
    }
}

/// Resolve operand contexts shared by the comb and FF lowerers.
pub(crate) fn binary_semantics(
    op: Op,
    lhs_width: usize,
    rhs_width: usize,
    lhs_signed: bool,
    rhs_signed: bool,
    context: Option<ValueContext>,
) -> BinarySemantics {
    let common_signed = context
        .map(|context| context.signed)
        .unwrap_or(lhs_signed && rhs_signed);
    match op {
        Op::Eq
        | Op::EqWildcard
        | Op::Ne
        | Op::NeWildcard
        | Op::Less
        | Op::LessEq
        | Op::Greater
        | Op::GreaterEq => {
            let width = lhs_width.max(rhs_width);
            let signed = lhs_signed && rhs_signed;
            let operand = Some(ValueContext { width, signed });
            BinarySemantics {
                lhs_context: operand,
                rhs_context: operand,
                result_width: 1,
                result_signed: false,
                lhs_signed: signed,
                rhs_signed: signed,
            }
        }
        Op::LogicAnd | Op::LogicOr => BinarySemantics {
            lhs_context: None,
            rhs_context: None,
            result_width: 1,
            result_signed: false,
            lhs_signed,
            rhs_signed,
        },
        Op::LogicShiftL | Op::LogicShiftR | Op::ArithShiftL | Op::ArithShiftR | Op::Pow => {
            let width = lhs_width.max(context.map(|context| context.width).unwrap_or(0));
            let signed = context.map(|context| context.signed).unwrap_or(lhs_signed);
            BinarySemantics {
                lhs_context: Some(ValueContext { width, signed }),
                rhs_context: None,
                result_width: width,
                result_signed: signed,
                lhs_signed: signed,
                rhs_signed,
            }
        }
        _ => {
            let width = lhs_width
                .max(rhs_width)
                .max(context.map(|context| context.width).unwrap_or(0));
            let operand = Some(ValueContext {
                width,
                signed: common_signed,
            });
            BinarySemantics {
                lhs_context: operand,
                rhs_context: operand,
                result_width: width,
                result_signed: common_signed,
                lhs_signed: common_signed,
                rhs_signed: common_signed,
            }
        }
    }
}

pub(crate) fn resolve_binary_op(op: Op, lhs_signed: bool, rhs_signed: bool) -> BinaryOp {
    let signed = lhs_signed && rhs_signed;
    match op {
        Op::Add => BinaryOp::Add,
        Op::Sub => BinaryOp::Sub,
        Op::Mul => BinaryOp::Mul,
        Op::Div if signed => BinaryOp::DivS,
        Op::Div => BinaryOp::DivU,
        Op::Rem if signed => BinaryOp::RemS,
        Op::Rem => BinaryOp::RemU,
        Op::BitAnd => BinaryOp::And,
        Op::BitOr => BinaryOp::Or,
        Op::BitXor => BinaryOp::Xor,
        Op::LogicShiftL | Op::ArithShiftL => BinaryOp::Shl,
        Op::LogicShiftR => BinaryOp::Shr,
        Op::ArithShiftR if lhs_signed => BinaryOp::Sar,
        Op::ArithShiftR => BinaryOp::Shr,
        Op::Eq => BinaryOp::Eq,
        Op::EqWildcard => BinaryOp::EqWildcard,
        Op::Ne => BinaryOp::Ne,
        Op::NeWildcard => BinaryOp::NeWildcard,
        Op::Less if signed => BinaryOp::LtS,
        Op::Less => BinaryOp::LtU,
        Op::LessEq if signed => BinaryOp::LeS,
        Op::LessEq => BinaryOp::LeU,
        Op::Greater if signed => BinaryOp::GtS,
        Op::Greater => BinaryOp::GtU,
        Op::GreaterEq if signed => BinaryOp::GeS,
        Op::GreaterEq => BinaryOp::GeU,
        Op::LogicAnd => BinaryOp::LogicAnd,
        Op::LogicOr => BinaryOp::LogicOr,
        _ => unreachable!("operator must be lowered by its dedicated path: {op:?}"),
    }
}

/// Helper: get width from an Expression (if possible)
pub fn get_expr_width(expr: &Expression) -> Option<usize> {
    match expr {
        Expression::Term(factor) => get_factor_width(factor),
        Expression::Binary(lhs, op, rhs, _) => match op {
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
            Op::As => cast_semantics(lhs, rhs).map(|semantics| semantics.width),
            Op::LogicShiftL | Op::LogicShiftR | Op::ArithShiftL | Op::ArithShiftR | Op::Pow => {
                get_expr_width(lhs)
            }
            _ => {
                let lw = get_expr_width(lhs);
                let rw = get_expr_width(rhs);
                lw.or(rw).map(|w| lw.unwrap_or(w).max(rw.unwrap_or(w)))
            }
        },
        Expression::Unary(op, expr, _) => match op {
            Op::BitAnd
            | Op::BitOr
            | Op::BitXor
            | Op::BitNand
            | Op::BitNor
            | Op::BitXnor
            | Op::LogicNot => Some(1),
            _ => get_expr_width(expr),
        },
        Expression::Ternary(_cond, then, els, _) => {
            let lw = get_expr_width(then);
            let rw = get_expr_width(els);
            lw.or(rw).map(|w| lw.unwrap_or(w).max(rw.unwrap_or(w)))
        }
        Expression::Concatenation(exprs, _) => {
            let mut total = 0;
            for (sub, rep) in exprs {
                let w = get_expr_width(sub)?;
                let count = if let Some(rep_expr) = rep {
                    eval_constexpr(rep_expr).and_then(|v| v.to_usize())?
                } else {
                    1
                };
                total += w * count;
            }
            Some(total)
        }
        Expression::ArrayLiteral(items, _) => {
            let mut total = 0;
            for item in items {
                match item {
                    ArrayLiteralItem::Value(expr, rep) => {
                        let w = get_expr_width(expr)?;
                        let count = if let Some(rep_expr) = rep {
                            eval_constexpr(rep_expr).and_then(|v| v.to_usize())?
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
        Expression::StructConstructor(ty, _, _) => {
            ty.total_width().map(|w| ty.array.total().unwrap_or(1) * w)
        }
    }
}

fn get_factor_width(factor: &Factor) -> Option<usize> {
    match factor {
        Factor::Value(comp) | Factor::Variable(_, _, _, comp) => {
            if let ValueVariant::Numeric(v) = &comp.value {
                if comp.r#type.total_width().is_none() {
                    return v.to_usize();
                }
            }
            comp.r#type
                .total_width()
                .map(|w| comp.r#type.array.total().unwrap_or(1) * w)
        }
        Factor::FunctionCall(call) => call
            .comptime
            .r#type
            .total_width()
            .map(|w| call.comptime.r#type.array.total().unwrap_or(1) * w),
        Factor::SystemFunctionCall(call) => match &call.kind {
            SystemFunctionKind::Bits(_)
            | SystemFunctionKind::Size(_)
            | SystemFunctionKind::Clog2(_) => Some(32),
            SystemFunctionKind::Onehot(_) => Some(1),
            SystemFunctionKind::Signed(input) | SystemFunctionKind::Unsigned(input) => {
                get_expr_width(&input.0)
                    .or_else(|| input.0.comptime().r#type.total_width())
                    .or_else(|| {
                        input
                            .0
                            .comptime()
                            .get_value()
                            .ok()
                            .map(|value| value.width())
                    })
            }
            _ => None,
        },
        _ => None,
    }
}
