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

pub(crate) fn case_arm_condition_expr(target: &Expression, patterns: &[CasePattern]) -> Expression {
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
