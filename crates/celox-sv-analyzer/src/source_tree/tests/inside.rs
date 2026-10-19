use super::*;
use crate::ast::{BinaryOp, Expr, InsideItem};
#[test]
fn native_membership_operand_values_and_range_endpoints_drive_production() {
    let mut tree = parse_source("module M(input int a,b,c,d,output logic y);assign y=(a+1) inside {b+2,[c+3:d+4],a inside {b+5}};endmodule",Path::new("native-inside-ownership.sv")).unwrap();
    for expression in &mut tree.declarations.expressions {
        if let celox_sv_syntax::ExprKind::Binary { operator, .. } = &mut expression.kind
            && operator == "+"
        {
            *operator = "-".into()
        }
    }
    let source = crate::ast::Source::from_syntax(&tree).unwrap();
    let Expr::Inside { expr, items } = source.modules()[0].assignments()[0].rhs() else {
        panic!()
    };
    let subtraction = |value: &Expr| {
        assert!(
            matches!(
                value,
                Expr::Binary {
                    op: BinaryOp::Sub,
                    ..
                }
            ),
            "{value:?}"
        )
    };
    subtraction(expr);
    let [
        InsideItem::Value(first),
        InsideItem::Range { low, high },
        InsideItem::Value(Expr::Inside { items: nested, .. }),
    ] = items.as_slice()
    else {
        panic!("{items:?}")
    };
    for value in [first, low, high] {
        subtraction(value)
    }
    let [InsideItem::Value(value)] = nested.as_slice() else {
        panic!()
    };
    subtraction(value);
}
#[test]
fn native_membership_items_and_kinds_are_authoritative() {
    for change_class in [false, true] {
        let mut tree = parse_source(
            "module M(input int a,output logic y);assign y=a inside {1,[2:3]};endmodule",
            Path::new("native-inside-shape.sv"),
        )
        .unwrap();
        for expression in &mut tree.declarations.expressions {
            if let celox_sv_syntax::ExprKind::Set(items) = &mut expression.kind {
                if change_class {
                    items[1].kind = items[0].kind.clone()
                } else {
                    items.pop();
                }
            }
        }
        let source = crate::ast::Source::from_syntax(&tree).unwrap();
        let Expr::Inside { items, .. } = source.modules()[0].assignments()[0].rhs() else {
            panic!()
        };
        assert_eq!(items.len(), if change_class { 2 } else { 1 });
        assert!(
            items
                .iter()
                .all(|item| matches!(item, InsideItem::Value(_)))
        );
    }
}
