use super::{sv, two_state_condition_always_true, two_state_mask_zero};
use sv::ir::{BinaryOp, Callee, ConstExpr, Expr, InsideItem, SystemFunction, UnaryOp};

fn ident(name: &str) -> Expr {
    Expr::Ident(name.into())
}
fn literal(text: &str) -> Expr {
    Expr::Literal(text.into())
}
fn binary(left: Expr, op: BinaryOp, right: Expr) -> Expr {
    Expr::Binary {
        left: Box::new(left),
        op,
        right: Box::new(right),
    }
}
fn call(function: SystemFunction, args: Vec<Expr>) -> Expr {
    Expr::Call {
        callee: Callee::System(function),
        args: args.into_iter().map(Some).collect(),
        arg_order: Vec::new(),
    }
}
/// `operand !== 1'bx`, the check that is vacuous when `operand` is known.
fn x_check(operand: Expr) -> Expr {
    binary(operand, BinaryOp::NeCase, literal("1'bx"))
}

#[test]
fn x_check_over_stored_data_is_always_true() {
    let reduced = Expr::Unary {
        op: UnaryOp::RedXor,
        expr: Box::new(ident("dat")),
    };
    assert!(two_state_condition_always_true(&x_check(reduced.clone())));
    assert!(two_state_condition_always_true(&binary(
        literal("1'bx"),
        BinaryOp::NeCase,
        reduced
    )));
    assert!(two_state_condition_always_true(&x_check(call(
        SystemFunction::Past,
        vec![ident("dat"), literal("2")]
    ))));
}

#[test]
fn operations_that_make_x_from_known_operands_are_not_known() {
    let select = Expr::Select {
        expr: Box::new(ident("dat")),
        msb: ConstExpr::Literal("-1".into()),
        lsb: ConstExpr::Literal("-1".into()),
        signed: false,
    };
    for operand in [
        select,
        binary(ident("a"), BinaryOp::Div, ident("b")),
        binary(ident("a"), BinaryOp::Mod, ident("b")),
        binary(ident("a"), BinaryOp::Pow, ident("b")),
        call(SystemFunction::Clog2, vec![ident("a")]),
    ] {
        assert!(!two_state_mask_zero(&operand), "{operand:?}");
        assert!(
            !two_state_condition_always_true(&x_check(operand.clone())),
            "{operand:?}"
        );
    }
}

#[test]
fn unknown_literals_stay_unknown_through_their_readers() {
    let inside = |expr: Expr, item: Expr| Expr::Inside {
        expr: Box::new(expr),
        items: vec![InsideItem::Value(item)],
    };
    assert!(two_state_mask_zero(&inside(ident("a"), literal("1'b0"))));
    for operand in [
        inside(literal("1'bx"), literal("1'b0")),
        inside(ident("a"), literal("1'bx")),
        call(SystemFunction::Sampled, vec![literal("1'bx")]),
        call(SystemFunction::Past, vec![literal("1'bx"), literal("2")]),
    ] {
        assert!(!two_state_mask_zero(&operand), "{operand:?}");
        assert!(
            !two_state_condition_always_true(&x_check(operand.clone())),
            "{operand:?}"
        );
    }
}

#[test]
fn constant_operators_and_context_sized_literals_are_left_to_runtime() {
    let resize = Expr::Resize {
        expr: Box::new(literal("4'shf")),
        width: 8,
        signed: true,
    };
    for predicate in [
        binary(resize, BinaryOp::NeCase, literal("8'hff")),
        binary(literal("8'hff"), BinaryOp::NeCase, literal("'1")),
        literal("'1"),
        literal("1'b0"),
        literal("1'bx"),
    ] {
        assert!(
            !two_state_condition_always_true(&predicate),
            "{predicate:?}"
        );
    }
    assert!(two_state_condition_always_true(&literal("1'b1")));
}
