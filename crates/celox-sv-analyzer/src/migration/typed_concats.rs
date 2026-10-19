//! Typed aggregate values/counts/selections are production native consumers.
use super::*;
use crate::ast::{ConstExpr, Source};
use celox_sv_syntax::{Declarations, DimensionKind, ExprKind};
fn subtract(tree: &mut ValidatedSyntax) {
    for value in &mut tree.declarations.expressions {
        if let ExprKind::Binary { operator, .. } = &mut value.kind
            && operator == "+"
        {
            *operator = "-".into();
        }
    }
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
fn number(source: Source) -> Option<i128> {
    let ir = crate::analyze::analyze_source(source).unwrap();
    ir.modules()[0]
        .parameters()
        .iter()
        .find(|value| value.name() == "N")
        .unwrap()
        .resolved_value()
}
#[test]
fn native_typed_concatenation_operands_drive_parameter_values() {
    let mut tree = parse_source(
        "module M;parameter N={4'(8+2),4'(4+1)};endmodule",
        Path::new("native-typed-concat.sv"),
    )
    .unwrap();
    subtract(&mut tree);
    let source = Source::from_syntax(&tree).unwrap();
    assert_eq!(literal(&source).width, 8);
    assert_eq!(number(source), Some(99));
}
#[test]
fn native_typed_repetition_counts_and_operands_specialize_port_widths() {
    let mut tree=parse_source("module M#(parameter int R=3,parameter N={(R+1){4'(8+2)}})(output logic[$bits(N)-1:0]y);assign y=N;endmodule",Path::new("native-typed-repeat.sv")).unwrap();
    subtract(&mut tree);
    for (override_, expected, width) in [(None, 102, 8), (Some(4), 1638, 12)] {
        let overrides = override_
            .map(|value| HashMap::from_iter([("R".into(), value)]))
            .unwrap_or_default();
        let source =
            Source::from_syntax_with_module_parameter_overrides(&tree, "M", &overrides).unwrap();
        assert_eq!(literal(&source).width, width);
        let ir = crate::analyze::analyze_source(source).unwrap();
        assert_eq!(
            ir.modules()[0].parameters()[1].resolved_value(),
            Some(expected)
        );
        assert_eq!(
            ir.modules()[0].ports()[0].r#type().resolved_width(),
            Some(width)
        );
    }
}
#[test]
fn native_typed_aggregate_bit_constant_and_indexed_slices_drive_results() {
    for (expression, width, expected) in [
        ("{4'(8+2),4'(4+1)}[(4+2):(2+1)]", 2, 1),
        ("{(3+1){4'(8+2)}}[(4+2)+:(3+1)]", 2, 1),
        ("{4'(8+2),4'(4+1)}[2+1]", 1, 1),
        ("{4'(8+2),4'(4+1)}[(4+2)-:(3+1)]", 2, 1),
    ] {
        let mut tree = parse_source(
            &format!("module M;parameter N={expression};endmodule"),
            Path::new("native-typed-aggregate-slice.sv"),
        )
        .unwrap();
        subtract(&mut tree);
        let source = Source::from_syntax(&tree).unwrap();
        assert_eq!(literal(&source).width, width, "{expression}");
        assert_eq!(number(source), Some(expected), "{expression}");
    }
}
#[test]
fn native_typed_aggregate_unknown_slice_uses_native_width() {
    let mut tree = parse_source(
        "module M;parameter N={4'(8+2),4'(4+1)}[1'bx+:(3+1)];endmodule",
        Path::new("native-typed-aggregate-unknown.sv"),
    )
    .unwrap();
    subtract(&mut tree);
    let source = Source::from_syntax(&tree).unwrap();
    let value = literal(&source);
    assert_eq!(value.width, 2);
    assert_eq!(value.mask, num_bigint::BigUint::from(3u8));
    assert_eq!(number(source), None);
}
#[test]
fn native_typed_aggregates_consume_nested_named_range_values() {
    let mut tree = parse_source(
        "module M;parameter logic[15:0]P=16'habcd;parameter N={P[(10+2):(4+2)],1'b1};endmodule",
        Path::new("native-typed-aggregate-nested.sv"),
    )
    .unwrap();
    subtract(&mut tree);
    let source = Source::from_syntax(&tree).unwrap();
    assert_eq!(literal(&source).width, 8);
    assert_eq!(number(source), Some(231));
}
#[test]
fn native_typed_aggregate_parts_and_slice_classes_are_authoritative() {
    for change in [0, 1, 2, 3] {
        let code = match change {
            0 => "module M;parameter N={4'(1),4'(2)};endmodule",
            1 => "module M;parameter N={2{4'(1)}};endmodule",
            _ => "module M;parameter N={4'(1),4'(2)}[4+:2];endmodule",
        };
        let mut tree = parse_source(code, Path::new("native-typed-aggregate-identity.sv")).unwrap();
        for value in &mut tree.declarations.expressions {
            match &mut value.kind {
                ExprKind::Concatenation(parts) if change == 0 => {
                    parts.pop();
                    break;
                }
                ExprKind::Replication { expressions, .. } if change == 1 => {
                    expressions.clear();
                    break;
                }
                ExprKind::Select { dimension, .. } if change >= 2 => {
                    if let DimensionKind::Indexed {
                        base,
                        operator,
                        width,
                    } = &mut dimension.kind
                    {
                        if change == 2 {
                            *operator = "-:".into();
                        } else {
                            dimension.kind = DimensionKind::Range {
                                left: *base,
                                right: *width,
                            };
                        }
                        break;
                    }
                }
                _ => {}
            }
        }
        let result = Source::from_syntax(&tree);
        if change == 1 {
            assert!(result.is_err());
        } else {
            let source = result.unwrap();
            assert_eq!(
                literal(&source).width,
                if change == 0 {
                    4
                } else if change == 2 {
                    2
                } else {
                    3
                }
            );
            assert_eq!(
                number(source),
                Some(if change == 0 {
                    1
                } else if change == 2 {
                    2
                } else {
                    4
                })
            );
        }
    }
}
#[test]
fn deep_native_typed_aggregate_counts_operands_and_bounds_use_worklists() {
    let path = Path::new("deep-native-typed-aggregate.sv");
    let mut tree = parse_source(
        "module M;parameter N={(3+1){4'(8+2)}}[(4+2)+:(3+1)];endmodule",
        path,
    )
    .unwrap();
    let open = "(".repeat(10_000);
    let close = ")".repeat(10_000);
    *tree.declarations=Declarations::parse(&format!("module M;parameter N={{{open}3+1{close}{{4'({open}8+2{close})}}}}[{open}4+2{close}+:{open}3+1{close}];endmodule"),path).unwrap();
    subtract(&mut tree);
    let source = Source::from_syntax(&tree).unwrap();
    assert_eq!(literal(&source).width, 2);
    assert_eq!(number(source), Some(1));
}
