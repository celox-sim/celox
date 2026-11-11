//! Authored parameter defaults reach initialized storage through native IDs.
use super::*;
use crate::ast::{BinaryOp, Expr, Source, Stmt};
use celox_sv_syntax::ExprKind;
fn parsed(code: &str) -> SourceTree {
    let mut tree = parse_source(code, Path::new("native-array-parameter-pattern.sv")).unwrap();
    for node in &mut tree.declarations.expressions {
        if let ExprKind::Binary { operator, .. } = &mut node.kind
            && operator == "+"
        {
            *operator = "-".into();
        }
    }
    tree
}
fn leaves(source: &Source) -> Vec<(usize, i128)> {
    let Stmt::Assign { rhs, .. } = &source.modules()[0].initial_processes()[0].body()[0] else {
        panic!()
    };
    let mut pending = vec![rhs];
    let mut leaves = Vec::new();
    while let Some(value) = pending.pop() {
        match value {
            Expr::Unary {
                op: crate::ast::UnaryOp::ToTwoState,
                expr,
            } => pending.push(expr),
            Expr::Concat(parts) => pending.extend(parts.iter().rev()),
            Expr::Resize { expr, width, .. } => {
                let Expr::Binary {
                    left,
                    op: BinaryOp::Sub,
                    right,
                } = expr.as_ref()
                else {
                    panic!("{expr:?}");
                };
                let (Expr::Literal(left), Expr::Literal(right)) = (left.as_ref(), right.as_ref())
                else {
                    panic!()
                };
                assert_eq!(right, "2");
                leaves.push((*width, left.parse().unwrap()));
            }
            _ => panic!("{value:?}"),
        }
    }
    leaves
}
/// The width and value of the packed parameter `P`, which a pattern of
/// constants gives its value at elaboration.
fn packed_value(source: &Source) -> (usize, u128) {
    let parameter = source.modules()[0]
        .parameters()
        .iter()
        .find(|parameter| parameter.name() == "P")
        .expect("the pattern gives a value parameter");
    let Some(crate::ast::ConstExpr::Literal(text)) = parameter.value() else {
        panic!("{:?}", parameter.value())
    };
    let literal = crate::typecheck::parse_integral_literal(text).unwrap();
    (literal.width, u128::try_from(literal.value).unwrap())
}
#[test]
fn native_packed_and_typed_parameter_pattern_defaults_own_value_operators() {
    for declaration in [
        "localparam logic[1:0][7:0]P='{10+2,8+2};",
        "typedef logic[1:0][7:0]T;localparam T P=T'{10+2,8+2};",
    ] {
        let tree = parsed(&format!("module M;{declaration}endmodule"));
        // `{8'(10 - 2), 8'(8 - 2)}`
        assert_eq!(
            packed_value(&Source::from_syntax(&tree).unwrap()),
            (16, 0x0806)
        );
    }
}
#[test]
fn native_unpacked_parameter_pattern_defaults_preserve_flat_order_and_keys() {
    for (declaration, expected) in [
        ("localparam int P[2]='{10+2,8+2};", vec![(32, 8), (32, 10)]),
        (
            "localparam byte P[4]='{2+1:10+2,default:8+2};",
            vec![(8, 8), (8, 8), (8, 10), (8, 8)],
        ),
    ] {
        let tree = parsed(&format!("module M;{declaration}endmodule"));
        assert_eq!(leaves(&Source::from_syntax(&tree).unwrap()), expected);
    }
}
#[test]
fn native_parameter_pattern_repeat_counts_follow_native_alias_widths() {
    // `logic [2:1]` takes two elements, each the low bit of `10 - 3`.
    let tree = parsed("module M;typedef logic[3+1:1]T;localparam T P=T'{3+1{10+3}};endmodule");
    assert_eq!(
        packed_value(&Source::from_syntax(&tree).unwrap()),
        (2, 0b11)
    );
}
#[test]
fn native_parameter_pattern_missing_items_never_retry_original_defaults() {
    let mut tree = parsed("module M;localparam logic[1:0]P='{1,0};endmodule");
    for node in &mut tree.declarations.expressions {
        if let ExprKind::Pattern { items, .. } = &mut node.kind {
            items.pop();
        }
    }
    assert!((Source::from_syntax(&tree)).is_err());
}
