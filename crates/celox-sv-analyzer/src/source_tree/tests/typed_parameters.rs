//! Actual typed parameter-initializer ownership (not the lightweight binder).
use super::*;
use crate::ast::{ConstExpr, Source};
use celox_sv_syntax::{Declarations, DimensionKind, ExprKind};
fn subtract(tree: &mut SourceTree) {
    for value in &mut tree.declarations.expressions {
        if let ExprKind::Binary { operator, .. } = &mut value.kind
            && operator == "+"
        {
            *operator = "-".into();
        }
    }
}
#[test]
fn native_typed_multidimensional_initializer_and_overrides_drive_widths() {
    let mut tree=parse_source("module M#(parameter logic[1:0][3:0]A=8'h42,parameter int I=1,parameter int N=A[I+1][1+0])(output logic[N:0]y);assign y=0;endmodule",Path::new("native-typed-parameter.sv")).unwrap();
    subtract(&mut tree);
    for (override_, expected) in [(None, 1), (Some(2), 0)] {
        let overrides = override_
            .map(|value| HashMap::from_iter([("I".into(), value)]))
            .unwrap_or_default();
        let source =
            Source::from_syntax_with_module_parameter_overrides(&tree, "M", &overrides).unwrap();
        let ir = crate::analyze::analyze_source(source).unwrap();
        assert_eq!(
            ir.modules()[0].parameters()[2].resolved_value(),
            Some(expected)
        );
        assert_eq!(
            ir.modules()[0].ports()[0].r#type().resolved_width(),
            Some(expected as usize + 1)
        );
    }
}
#[test]
fn native_typed_initializer_outer_operators_and_mux_roles_drive_values() {
    for (initializer, expected, swap) in [
        ("(10+2)-A[1+1][1+0]", 7, false),
        ("1?A[1+1][1+0]:0", 0, true),
    ] {
        let mut tree = parse_source(
            &format!(
                "module M;parameter logic[1:0][3:0]A=8'h42;parameter int N={initializer};endmodule"
            ),
            Path::new("native-typed-parameter-control.sv"),
        )
        .unwrap();
        subtract(&mut tree);
        if swap {
            for value in &mut tree.declarations.expressions {
                if let ExprKind::Conditional {
                    then_expression,
                    else_expression,
                    ..
                } = &mut value.kind
                {
                    std::mem::swap(then_expression, else_expression);
                }
            }
        }
        let source = Source::from_syntax(&tree).unwrap();
        let ir = crate::analyze::analyze_source(source).unwrap();
        assert_eq!(
            ir.modules()[0].parameters()[1].resolved_value(),
            Some(expected)
        );
    }
}
#[test]
fn native_typed_initializer_unary_operator_is_authoritative() {
    let mut tree = parse_source(
        "module M;parameter logic[1:0][3:0]A=8'h42;parameter int N=~A[0][1];endmodule",
        Path::new("native-typed-parameter-unary.sv"),
    )
    .unwrap();
    for value in &mut tree.declarations.expressions {
        if let ExprKind::Unary { operator, .. } = &mut value.kind
            && operator == "~"
        {
            *operator = "+".into();
        }
    }
    let source = Source::from_syntax(&tree).unwrap();
    let ir = crate::analyze::analyze_source(source).unwrap();
    assert_eq!(ir.modules()[0].parameters()[1].resolved_value(), Some(1));
}
#[test]
fn native_typed_parameter_selections_preserve_out_of_range_masks() {
    for (kind, masked) in [("logic", true), ("bit", false)] {
        let tree = parse_source(
            &format!("module M;parameter {kind}[1:0][3:0]A=8'hab;parameter N=A[2][0];endmodule"),
            Path::new("native-typed-parameter-masks.sv"),
        )
        .unwrap();
        let source = Source::from_syntax(&tree).unwrap();
        let value = source.modules()[0].parameters()[1].value().unwrap();
        let ConstExpr::Literal(text) = value else {
            panic!("{value:?}")
        };
        let literal = crate::typecheck::parse_integral_literal(text).unwrap();
        assert_eq!(literal.mask != num_bigint::BigUint::default(), masked);
        let ir = crate::analyze::analyze_source(source).unwrap();
        assert_eq!(
            ir.modules()[0].parameters()[1].resolved_value(),
            if masked { None } else { Some(0) }
        );
    }
}
#[test]
fn native_typed_initializer_shape_errors_are_not_retried() {
    for change in [false, true] {
        let mut tree = parse_source(
            "module M;parameter logic[1:0][3:0]A=8'hab;parameter N=A[0][1];endmodule",
            Path::new("native-typed-parameter-identity.sv"),
        )
        .unwrap();
        for value in &mut tree.declarations.expressions {
            if change {
                if let ExprKind::Name(name) = &mut value.kind
                    && name.spelling == "A"
                {
                    name.spelling = "other".into();
                }
            } else if let ExprKind::Select { dimension, .. } = &mut value.kind
                && let DimensionKind::Index(index) = dimension.kind
            {
                dimension.kind = DimensionKind::Range {
                    left: index,
                    right: index,
                };
                break;
            }
        }
        assert!((Source::from_syntax(&tree)).is_err());
    }
}
#[test]
fn deep_native_typed_parameter_indices_bind_on_the_default_stack() {
    let path = Path::new("deep-native-typed-parameter.sv");
    let mut tree = parse_source(
        "module M;parameter logic[1:0][3:0]A=8'h42;parameter N=A[0][(2+1)];endmodule",
        path,
    )
    .unwrap();
    *tree.declarations = Declarations::parse(
        &format!(
            "module M;parameter logic[1:0][3:0]A=8'h42;parameter N=A[0][{}2+1{}];endmodule",
            "(".repeat(10_000),
            ")".repeat(10_000)
        ),
        path,
    )
    .unwrap();
    subtract(&mut tree);
    let source = Source::from_syntax(&tree).unwrap();
    let ir = crate::analyze::analyze_source(source).unwrap();
    assert_eq!(ir.modules()[0].parameters()[1].resolved_value(), Some(1));
}
