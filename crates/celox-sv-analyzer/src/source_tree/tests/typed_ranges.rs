//! Actual typed parameter range ownership and declared-coordinate semantics.
use super::*;
use crate::ast::{ConstExpr, Source};
use celox_sv_syntax::{Declarations, DimensionKind, ExprKind};
fn subtract(tree: &mut SourceTree) {
    for expression in &mut tree.declarations.expressions {
        if let ExprKind::Binary { operator, .. } = &mut expression.kind
            && operator == "+"
        {
            *operator = "-".into();
        }
    }
}
fn number(source: Source) -> Option<i128> {
    let ir = crate::analyze::analyze_source(source).unwrap();
    ir.modules()[0]
        .parameters()
        .iter()
        .find(|value| value.name() == "N")
        .unwrap()
        .resolved_value()
}
fn literal(source: &Source) -> crate::typecheck::IntegralLiteral {
    let value = source.modules()[0]
        .parameters()
        .iter()
        .find(|value| value.name() == "N")
        .unwrap()
        .value()
        .unwrap();
    let ConstExpr::Literal(text) = value else {
        panic!("{value:?}")
    };
    crate::typecheck::parse_integral_literal(text).unwrap()
}
#[test]
fn native_typed_constant_bounds_and_preceding_indices_drive_values() {
    for (declaration, selection, expected) in [
        ("logic[15:0]P=16'habcd", "P[(10+2):(4+2)]", 115),
        ("logic[1:0][7:0]P=16'habcd", "P[1+1][(6+2):(2+1)]", 6),
    ] {
        let mut tree = parse_source(
            &format!("module M;parameter {declaration};parameter N={selection};endmodule"),
            Path::new("native-typed-range.sv"),
        )
        .unwrap();
        subtract(&mut tree);
        assert_eq!(number(Source::from_syntax(&tree).unwrap()), Some(expected));
    }
}
#[test]
fn native_typed_indexed_bounds_preserve_both_declaration_directions() {
    for (declaration, operator, expected) in [
        ("15:0", "+:", 3),
        ("15:0", "-:", 2),
        ("0:15", "+:", 2),
        ("0:15", "-:", 1),
    ] {
        let mut tree=parse_source(&format!("module M;parameter logic[{declaration}]P=16'habcd;parameter N=P[(4+2){operator}(3+1)];endmodule"),Path::new("native-typed-indexed-range.sv")).unwrap();
        subtract(&mut tree);
        assert_eq!(
            number(Source::from_syntax(&tree).unwrap()),
            Some(expected),
            "{declaration} {operator}"
        );
    }
}
#[test]
fn native_typed_unknown_indexed_base_uses_native_width_and_packed_stride() {
    for kind in ["logic", "bit"] {
        let mut tree = parse_source(
            &format!(
                "module M;parameter {kind}[1:0][3:0]P=8'ha5;parameter N=P[1'bx+:(3+1)];endmodule"
            ),
            Path::new("native-typed-unknown-range.sv"),
        )
        .unwrap();
        subtract(&mut tree);
        let source = Source::from_syntax(&tree).unwrap();
        let value = literal(&source);
        assert_eq!(value.width, 8);
        assert_eq!(value.mask, num_bigint::BigUint::from(255u32));
        assert_eq!(number(source), None);
    }
}
#[test]
fn native_typed_ranges_clip_to_the_selected_element_before_slicing() {
    for (kind, masked) in [("logic", true), ("bit", false)] {
        let mut tree = parse_source(
            &format!(
                "module M;parameter {kind}[1:0][3:0]P=8'ha5;parameter N=P[2][(3+1):(1+1)];endmodule"
            ),
            Path::new("native-typed-clipped-range.sv"),
        )
        .unwrap();
        subtract(&mut tree);
        let source = Source::from_syntax(&tree).unwrap();
        let value = literal(&source);
        assert_eq!(value.width, 3);
        assert_eq!(value.mask != num_bigint::BigUint::default(), masked);
        assert_eq!(number(source), if masked { None } else { Some(0) });
    }
}
#[test]
fn native_typed_range_kind_operator_and_index_count_are_authoritative() {
    for change in [0, 1, 2] {
        let mut tree = parse_source(
            "module M;parameter logic[1:0][7:0]P=16'habcd;parameter N=P[0][4+:2];endmodule",
            Path::new("native-typed-range-identity.sv"),
        )
        .unwrap();
        let range = tree
            .declarations
            .expressions
            .iter()
            .find_map(|value| match &value.kind {
                ExprKind::Select {
                    expression,
                    dimension,
                } if matches!(dimension.kind, DimensionKind::Indexed { .. }) => {
                    Some((value.tokens, *expression))
                }
                _ => None,
            })
            .unwrap();
        let base = match tree.declarations().expression(range.1).kind {
            ExprKind::Select { expression, .. } => expression,
            _ => panic!(),
        };
        let value = tree
            .declarations
            .expressions
            .iter_mut()
            .find(|value| value.tokens == range.0)
            .unwrap();
        let ExprKind::Select {
            expression,
            dimension,
        } = &mut value.kind
        else {
            unreachable!()
        };
        if change == 2 {
            *expression = base;
        } else {
            let DimensionKind::Indexed {
                base,
                operator,
                width,
            } = &mut dimension.kind
            else {
                unreachable!()
            };
            if change == 0 {
                *operator = "-:".into();
            } else {
                dimension.kind = DimensionKind::Range {
                    left: *base,
                    right: *width,
                };
            }
        }
        let result = Source::from_syntax(&tree);
        let source = result.unwrap();
        if change == 2 {
            assert_eq!(literal(&source).width, 16);
            assert_eq!(number(source), None);
        } else {
            assert_eq!(number(source), Some(if change == 0 { 1 } else { 3 }));
        }
    }
}
#[test]
fn native_typed_indexed_width_errors_are_not_retried() {
    let mut tree = parse_source(
        "module M;parameter logic[15:0]P=16'habcd;parameter N=P[0+:(1+1)];endmodule",
        Path::new("native-typed-range-width.sv"),
    )
    .unwrap();
    subtract(&mut tree);
    assert!(
        matches!(Source::from_syntax(&tree),Err(AnalyzerError::Unsupported(message)) if message.contains("indexed part-select width 0"))
    );
}
#[test]
fn deep_native_typed_range_bounds_bind_on_the_default_stack() {
    let path = Path::new("deep-native-typed-range.sv");
    let mut tree = parse_source(
        "module M;parameter logic[15:0]P=16'habcd;parameter N=P[(10+2):(4+2)];endmodule",
        path,
    )
    .unwrap();
    let open = "(".repeat(10_000);
    let close = ")".repeat(10_000);
    *tree.declarations=Declarations::parse(&format!("module M;parameter logic[15:0]P=16'habcd;parameter N=P[{open}10+2{close}:{open}4+2{close}];endmodule"),path).unwrap();
    subtract(&mut tree);
    assert_eq!(number(Source::from_syntax(&tree).unwrap()), Some(115));
}
