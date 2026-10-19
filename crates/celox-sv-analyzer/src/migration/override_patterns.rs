//! Actual caller-bound pattern structure/value/key/count ownership.
use super::*;
use crate::ast::{BoundPattern, Source};
use crate::{ModuleInterfaces, ParsedSource};
use celox_sv_syntax::ExprKind;
fn pattern(expression: &str) -> BoundPattern {
    let mut tree = parse_source(
        &format!("module M;C#(.P({expression}))c();endmodule"),
        Path::new("native-override-pattern.sv"),
    )
    .unwrap();
    for node in &mut tree.declarations.expressions {
        if let ExprKind::Binary { operator, .. } = &mut node.kind
            && operator == "+"
        {
            *operator = "-".into();
        }
    }
    Source::from_syntax(&tree).unwrap().modules()[0].instances()[0].parameter_overrides()[0]
        .bound_pattern()
        .unwrap()
        .clone()
}
fn leaves(pattern: BoundPattern, dimensions: &str) -> Vec<i128> {
    let child = ParsedSource::parse(
        &format!("module C#(parameter logic{dimensions} P='0);endmodule"),
        Path::new("native-pattern-child.sv"),
    )
    .unwrap();
    let packages = ParsedSource::analyze_packages(&[&child]).unwrap();
    let ir = child
        .analyze_module(
            "C",
            &HashMap::default(),
            &[],
            &[("P".into(), pattern)],
            &ModuleInterfaces::default(),
            &packages,
        )
        .unwrap();
    let statement = &ir.modules()[0].initial_processes()[0].body()[0];
    let crate::ir::Stmt::Assign { rhs, .. } = statement else {
        panic!("{statement:?}");
    };
    let mut pending = vec![rhs];
    let mut leaves = Vec::new();
    while let Some(expr) = pending.pop() {
        match expr {
            crate::ir::Expr::Concat(parts) => pending.extend(parts.iter().rev()),
            crate::ir::Expr::Resize { expr, width: 8, .. } => {
                let crate::ir::Expr::Binary {
                    left,
                    op: crate::ir::BinaryOp::Sub,
                    right,
                } = expr.as_ref()
                else {
                    panic!("{expr:?}");
                };
                let (crate::ir::Expr::Literal(left), crate::ir::Expr::Literal(right)) =
                    (left.as_ref(), right.as_ref())
                else {
                    panic!("{expr:?}");
                };
                assert_eq!(right, "2");
                leaves.push(left.parse().unwrap());
            }
            _ => panic!("{expr:?}"),
        }
    }
    leaves
}
#[test]
fn native_pattern_list_and_keyed_values_bind_in_the_caller() {
    for (expression, expected) in [
        ("'{10+2,8+2}", vec![10, 8]),
        ("'{1+1:10+2,default:8+2}", vec![8, 10]),
    ] {
        assert_eq!(
            leaves(pattern(expression), "[1:0][7:0]"),
            expected,
            "{expression}"
        );
    }
}
#[test]
fn native_pattern_repetition_count_and_leaf_operators_drive_specialization() {
    assert_eq!(leaves(pattern("'{3+1{10+2}}"), "[1:0][7:0]"), vec![10, 10]);
}
#[test]
fn native_nested_pattern_structure_preserves_element_order() {
    assert_eq!(
        leaves(pattern("'{'{10+2,8+2},'{6+2,4+2}}"), "[1:0][1:0][7:0]"),
        vec![10, 8, 6, 4]
    );
}

#[test]
fn native_pattern_keys_and_item_counts_are_authoritative() {
    for keyed in [false, true] {
        let mut tree = parse_source(
            "module M;C#(.P('{0:1,default:2}))c();endmodule",
            Path::new("native-pattern-guard.sv"),
        )
        .unwrap();
        for node in &mut tree.declarations.expressions {
            if let ExprKind::Pattern { items, .. } = &mut node.kind {
                if keyed {
                    items[0].key = None;
                } else {
                    items.pop();
                }
            }
        }
        if keyed {
            assert!(Source::from_syntax(&tree).is_err());
        } else {
            let source = Source::from_syntax(&tree).unwrap();
            assert_eq!(
                source.modules()[0].instances()[0].parameter_overrides()[0].bound_pattern(),
                Some(&pattern("'{0:1}"))
            );
        }
    }
}
