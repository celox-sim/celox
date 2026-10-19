//! Typed call actual ranges use native bound IDs, including runtime bases.
use super::*;
use crate::ast::Source;
use celox_sv_syntax::{Declarations, DimensionKind, ExprKind};
fn subtract(tree: &mut ValidatedSyntax) {
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
#[test]
fn native_typed_actual_constant_and_indexed_ranges_drive_numeric_calls() {
    for (selection, expected) in [
        ("P[(10+2):(4+2)]", 5),
        ("P[(4+2)+:(3+1)]", 2),
        ("P[(8+2)-:(3+1)]", 1),
    ] {
        let mut tree=parse_source(&format!("module M;parameter logic[15:0]P=16'habcd;parameter N=$countones({selection});endmodule"),Path::new("native-typed-actual-range.sv")).unwrap();
        subtract(&mut tree);
        assert_eq!(
            number(Source::from_syntax(&tree).unwrap()),
            Some(expected),
            "{selection}"
        );
    }
}
#[test]
fn native_typed_actual_ranges_preserve_preceding_dimensions_and_ascending_layout() {
    for (declaration, selection, expected) in [
        ("logic[1:0][3:0]A=8'h42", "A[1+1][(2+1)+:(3+1)]", 1),
        ("logic[0:15]A=16'habcd", "A[(1+1):(10+2)]", 343),
    ] {
        let mut tree = parse_source(
            &format!(
                "module M;parameter {declaration};parameter N=$unsigned({selection});endmodule"
            ),
            Path::new("native-typed-actual-layout.sv"),
        )
        .unwrap();
        subtract(&mut tree);
        assert_eq!(
            number(Source::from_syntax(&tree).unwrap()),
            Some(expected),
            "{selection}"
        );
    }
}
#[test]
fn native_typed_actual_ranges_preserve_masks_and_unknown_base_width() {
    let mut tree = parse_source(
        "module M;parameter bit[15:0]P=16'habcd;parameter N=$isunknown(P[1'bx+:(3+1)]);endmodule",
        Path::new("native-typed-actual-unknown.sv"),
    )
    .unwrap();
    subtract(&mut tree);
    assert_eq!(number(Source::from_syntax(&tree).unwrap()), Some(1));
}
#[test]
fn native_typed_actual_range_operators_and_bounds_are_authoritative() {
    for change in 0..3 {
        let mut tree = parse_source(
            "module M;parameter logic[15:0]P=16'habcd;parameter N=$unsigned(P[4+:2]);endmodule",
            Path::new("native-typed-actual-range-guard.sv"),
        )
        .unwrap();
        for value in &mut tree.declarations.expressions {
            match &mut value.kind {
                ExprKind::Name(name) if change == 0 && name.spelling == "P" => {
                    name.spelling = "other".into();
                    break;
                }
                ExprKind::Select { dimension, .. } if change > 0 => {
                    let DimensionKind::Indexed {
                        base,
                        operator,
                        width,
                    } = &mut dimension.kind
                    else {
                        continue;
                    };
                    if change == 1 {
                        *operator = "-:".into();
                    } else {
                        dimension.kind = DimensionKind::Range {
                            left: *base,
                            right: *width,
                        };
                    }
                    break;
                }
                _ => {}
            }
        }
        let result = Source::from_syntax(&tree);
        if change == 0 {
            assert!(result.and_then(crate::analyze::analyze_source).is_err());
        } else {
            assert_eq!(
                number(result.unwrap()),
                Some(if change == 1 { 1 } else { 3 })
            );
        }
    }
}
#[test]
fn native_typed_actual_range_qualifiers_preserve_package_names() {
    let mut tree=parse_source("package p;parameter logic[15:0]P=16'habcd;endpackage module M;parameter N=$countones(p::P[(10+2):(4+2)]);endmodule",Path::new("native-typed-actual-package.sv")).unwrap();
    subtract(&mut tree);
    assert_eq!(number(Source::from_syntax(&tree).unwrap()), Some(5));
}
#[test]
fn deep_native_typed_actual_range_coordinates_use_the_default_stack() {
    let path = Path::new("native-typed-actual-deep-range.sv");
    let mut tree = parse_source(
        "module M;parameter logic[15:0]P=16'habcd;parameter N=$unsigned(P[(4+2)+:(3+1)]);endmodule",
        path,
    )
    .unwrap();
    let open = "(".repeat(10_000);
    let close = ")".repeat(10_000);
    *tree.declarations=Declarations::parse(&format!("module M;parameter logic[15:0]P=16'habcd;parameter N=$unsigned(P[{open}4+2{close}+:{open}3+1{close}]);endmodule"),path).unwrap();
    subtract(&mut tree);
    assert_eq!(number(Source::from_syntax(&tree).unwrap()), Some(3));
}
