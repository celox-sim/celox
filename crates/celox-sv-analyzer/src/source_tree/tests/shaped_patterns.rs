//! Native aggregate/array pattern shapes through actual assignment consumers.
use super::*;
use crate::ast::{BinaryOp, Expr, Source};
use celox_sv_syntax::ExprKind;
fn parsed(code: &str) -> SourceTree {
    let mut tree = parse_source(code, Path::new("native-shaped-pattern.sv")).unwrap();
    for node in &mut tree.declarations.expressions {
        if let ExprKind::Binary { operator, .. } = &mut node.kind
            && operator == "+"
        {
            *operator = "-".into();
        }
    }
    tree
}
fn operators(value: &Expr) -> Vec<usize> {
    let mut pending = vec![value];
    let mut widths = Vec::new();
    while let Some(value) = pending.pop() {
        match value {
            Expr::Concat(parts) => pending.extend(parts.iter().rev()),
            Expr::Resize { expr, width, .. } => {
                let Expr::Binary {
                    op: BinaryOp::Sub,
                    left,
                    right,
                } = expr.as_ref()
                else {
                    panic!("{expr:?}");
                };
                assert!(matches!(left.as_ref(),Expr::Ident(name) if name=="a"));
                assert!(matches!(right.as_ref(),Expr::Ident(name) if name=="b"));
                widths.push(*width);
            }
            _ => panic!("{value:?}"),
        }
    }
    widths
}
#[test]
fn native_struct_member_pattern_values_use_member_receiving_widths() {
    let tree = parsed(
        "module M(input bit a,b);struct packed{logic[1:0]x;logic[2:0]z;}v;assign v='{x:a+b,z:a+b};endmodule",
    );
    let source = Source::from_syntax(&tree).unwrap();
    assert_eq!(
        operators(source.modules()[0].assignments()[0].rhs()),
        [2, 3]
    );
}
#[test]
fn native_packed_multidimensional_pattern_values_own_nested_operators() {
    let tree = parsed(
        "module M(input bit a,b,output logic[1:0][1:0]y);assign y='{'{a+b,a+b},'{a+b,a+b}};endmodule",
    );
    let source = Source::from_syntax(&tree).unwrap();
    assert_eq!(
        operators(source.modules()[0].assignments()[0].rhs()),
        [1, 1, 1, 1]
    );
}
#[test]
fn native_unpacked_array_default_patterns_broadcast_native_values() {
    for value in ["'{default:a+b}", "'{'{a+b,a+b},'{a+b,a+b}}"] {
        let tree = parsed(&format!(
            "module M(input bit a,b);logic[1:0]v[0:1][1:0];assign v={value};endmodule"
        ));
        let source = Source::from_syntax(&tree).unwrap();
        assert_eq!(
            operators(source.modules()[0].assignments()[0].rhs()),
            vec![2, 2, 2, 2],
            "{value}"
        );
    }
}
#[test]
fn native_unpacked_array_numeric_keys_preserve_flat_storage_order() {
    let tree = parsed("module M(input bit a,b);logic v[0:3];assign v='{2+1:a,default:b};endmodule");
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
fn native_struct_pattern_missing_member_is_not_retried() {
    let mut tree = parsed(
        "module M(input bit a,b);struct packed{logic x;logic z;}v;assign v='{x:a,z:b};endmodule",
    );
    for node in &mut tree.declarations.expressions {
        if let ExprKind::Pattern { items, .. } = &mut node.kind {
            items.pop();
        }
    }
    assert!((Source::from_syntax(&tree)).is_err());
}
