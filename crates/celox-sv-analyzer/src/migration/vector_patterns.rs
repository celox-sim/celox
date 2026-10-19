//! Native scalar/vector assignment and call-actual patterns through real values.
use super::*;
use crate::ast::{BinaryOp, ConstExpr, Expr, Source};
use celox_sv_syntax::ExprKind;
fn parsed(code: &str) -> ValidatedSyntax {
    let mut tree = parse_source(code, Path::new("native-vector-pattern.sv")).unwrap();
    for node in &mut tree.declarations.expressions {
        if let ExprKind::Binary { operator, .. } = &mut node.kind
            && operator == "+"
        {
            *operator = "-".into();
        }
    }
    tree
}
fn check_subtractions(root: &Expr, expected: usize) {
    let mut pending = vec![root];
    let mut count = 0;
    while let Some(value) = pending.pop() {
        match value {
            Expr::Concat(parts) => pending.extend(parts),
            Expr::Resize { expr, .. } => pending.push(expr),
            Expr::Binary { op, left, right } => {
                assert_eq!(*op, BinaryOp::Sub);
                count += 1;
                assert!(matches!(left.as_ref(),Expr::Ident(name) if name=="a"));
                assert!(matches!(right.as_ref(),Expr::Ident(name) if name=="b"));
            }
            Expr::Call { args, .. } => pending.extend(args.iter().flatten()),
            _ => panic!("{value:?}"),
        }
    }
    assert_eq!(count, expected);
}
#[test]
fn native_vector_pattern_list_values_and_nested_scalar_patterns_own_operators() {
    for value in ["'{a+b,a+b}", "'{'{a+b},'{a+b}}"] {
        let tree = parsed(&format!(
            "module M(input bit a,b,output logic[1:0]y);assign y={value};endmodule"
        ));
        let source = Source::from_syntax(&tree).unwrap();
        check_subtractions(source.modules()[0].assignments()[0].rhs(), 2);
    }
}
#[test]
fn native_vector_pattern_numeric_keys_drive_actual_item_order() {
    let tree =
        parsed("module M(input bit a,b,output logic[3:0]y);assign y='{2+1:a,default:b};endmodule");
    let source = Source::from_syntax(&tree).unwrap();
    let Expr::Concat(parts) = source.modules()[0].assignments()[0].rhs() else {
        panic!()
    };
    let names = parts
        .iter()
        .map(|part| {
            let Expr::Resize { expr, width: 1, .. } = part else {
                panic!("{part:?}")
            };
            let Expr::Ident(name) = expr.as_ref() else {
                panic!()
            };
            name.as_str()
        })
        .collect::<Vec<_>>();
    assert_eq!(names, ["b", "b", "a", "b"]);
}
#[test]
fn native_vector_pattern_repeat_counts_match_native_port_widths() {
    let tree =
        parsed("module M(input bit a,b,output logic[3+1:1]y);assign y='{3+1{a+b}};endmodule");
    let source = Source::from_syntax(&tree).unwrap();
    check_subtractions(source.modules()[0].assignments()[0].rhs(), 2);
    assert_eq!(
        source.modules()[0].ports()[2].r#type().packed_ranges()[0].left(),
        &ConstExpr::Binary {
            left: Box::new(ConstExpr::Literal("3".into())),
            op: BinaryOp::Sub,
            right: Box::new(ConstExpr::Literal("1".into()))
        }
    );
}
#[test]
fn native_vector_pattern_function_actual_values_use_native_operators() {
    let tree = parsed(
        "module M(input bit a,b,output logic[1:0]y);function automatic logic[1:0]f(input logic[1:0]v);return v;endfunction assign y=f('{a+b,a+b});endmodule",
    );
    let source = Source::from_syntax(&tree).unwrap();
    check_subtractions(source.modules()[0].assignments()[0].rhs(), 2);
}
#[test]
fn native_vector_pattern_missing_items_are_errors_without_retry() {
    let mut tree = parsed("module M(input bit a,b,output logic[1:0]y);assign y='{a,b};endmodule");
    for node in &mut tree.declarations.expressions {
        if let ExprKind::Pattern { items, .. } = &mut node.kind {
            items.pop();
        }
    }
    assert!((Source::from_syntax(&tree)).is_err());
}
