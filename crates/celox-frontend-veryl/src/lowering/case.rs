use veryl_analyzer::ir::{CasePattern, Comptime, Expression, Op};
use veryl_parser::token_range::TokenRange;

fn unknown_comptime() -> Box<Comptime> {
    Box::new(Comptime::create_unknown(TokenRange::default()))
}

fn case_pattern_condition(target: &Expression, pattern: &CasePattern) -> Expression {
    match pattern {
        CasePattern::Eq(value) => Expression::Binary(
            Box::new(target.clone()),
            Op::EqWildcard,
            value.clone(),
            unknown_comptime(),
        ),
        CasePattern::Range { lo, hi, inclusive } => {
            let lo_cond = Expression::Binary(
                lo.clone(),
                Op::LessEq,
                Box::new(target.clone()),
                unknown_comptime(),
            );
            let hi_op = if *inclusive { Op::LessEq } else { Op::Less };
            let hi_cond = Expression::Binary(
                Box::new(target.clone()),
                hi_op,
                hi.clone(),
                unknown_comptime(),
            );
            Expression::Binary(
                Box::new(lo_cond),
                Op::LogicAnd,
                Box::new(hi_cond),
                unknown_comptime(),
            )
        }
    }
}

pub fn case_arm_condition_expr(target: &Expression, patterns: &[CasePattern]) -> Expression {
    let target = &unfold_context_determined_constants(target);
    let mut iter = patterns.iter();
    let first = iter.next().expect("CaseArm must have at least one pattern");
    iter.fold(case_pattern_condition(target, first), |acc, pattern| {
        Expression::Binary(
            Box::new(acc),
            Op::LogicOr,
            Box::new(case_pattern_condition(target, pattern)),
            unknown_comptime(),
        )
    })
}

/// Returns `target` with the folded values of its context-determined
/// operators cleared.
///
/// The analyzer folds a constant case target in its self-determined width,
/// before the width of each label comparison is known, so `J + 8'h01` with
/// `J = 8'hff` folds to 0 even when it is compared with `16'h0100`. Without
/// the folded results the target is recomputed from its operands in each
/// comparison's width and signedness, as a runtime target is. Operands that
/// are self-determined (shift amounts, ternary conditions, comparison and
/// cast operands) keep their folded values, which do not depend on the
/// comparison.
pub fn unfold_context_determined_constants(target: &Expression) -> Expression {
    let mut target = target.clone();
    clear_context_determined_folds(&mut target);
    target
}

fn clear_context_determined_folds(expression: &mut Expression) {
    match expression {
        Expression::Unary(op, operand, comptime) => {
            if matches!(op, Op::Add | Op::Sub | Op::BitNot) {
                comptime.is_const = false;
                clear_context_determined_folds(operand);
            }
        }
        Expression::Binary(lhs, op, rhs, comptime) => match op {
            Op::Add
            | Op::Sub
            | Op::Mul
            | Op::Div
            | Op::Rem
            | Op::BitAnd
            | Op::BitOr
            | Op::BitXor
            | Op::BitXnor
            | Op::BitNand
            | Op::BitNor => {
                comptime.is_const = false;
                clear_context_determined_folds(lhs);
                clear_context_determined_folds(rhs);
            }
            Op::ArithShiftL | Op::ArithShiftR | Op::LogicShiftL | Op::LogicShiftR | Op::Pow => {
                comptime.is_const = false;
                clear_context_determined_folds(lhs);
            }
            _ => {}
        },
        Expression::Ternary(_, yes, no, comptime) => {
            comptime.is_const = false;
            clear_context_determined_folds(yes);
            clear_context_determined_folds(no);
        }
        Expression::Term(_)
        | Expression::Concatenation(..)
        | Expression::ArrayLiteral(..)
        | Expression::StructConstructor(..) => {}
    }
}
