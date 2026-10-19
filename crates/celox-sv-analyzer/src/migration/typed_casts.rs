//! Native typed constant-cast targets/operands and shared sizing semantics.
use super::*;
use crate::ast::{ConstExpr, Source};
use celox_sv_syntax::{DataTypeKind, Declarations, ExprKind};
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
fn native_constant_cast_size_and_operand_operators_drive_values() {
    for (expression, width, expected) in [("(8+2)'(16'habcd)", 6, 13), ("8'((10+2))", 8, 8)] {
        let mut tree = parse_source(
            &format!("module M;parameter N={expression};endmodule"),
            Path::new("native-typed-cast.sv"),
        )
        .unwrap();
        subtract(&mut tree);
        let source = Source::from_syntax(&tree).unwrap();
        assert_eq!(literal(&source).width, width);
        assert_eq!(number(source), Some(expected));
    }
}
#[test]
fn native_constant_casts_consume_typed_selection_operands_and_package_aliases() {
    for (code, expected) in [
        (
            "module M;parameter logic[1:0][7:0]A=16'habce;parameter N=4'(A[1+1][2+1]);endmodule",
            1,
        ),
        (
            "package p;typedef bit[7:0]B;endpackage module M;parameter N=p::B'((8+2));endmodule",
            6,
        ),
    ] {
        let mut tree = parse_source(code, Path::new("native-typed-cast-inputs.sv")).unwrap();
        subtract(&mut tree);
        assert_eq!(number(Source::from_syntax(&tree).unwrap()), Some(expected));
    }
}
#[test]
fn native_constant_casts_preserve_signing_numeric_sizes_and_fill_literals() {
    for (expression, expected) in [
        ("signed'(8'hf0)", -16),
        ("unsigned'(-8'sd1)", 255),
        ("4'(-8'sd1)", -1),
        ("int'(8'hff)", 255),
        ("byte'('1)", -1),
        ("byte'('0)", 0),
    ] {
        let tree = parse_source(
            &format!("module M;parameter N={expression};endmodule"),
            Path::new("native-typed-cast-signing.sv"),
        )
        .unwrap();
        assert_eq!(
            number(Source::from_syntax(&tree).unwrap()),
            Some(expected),
            "{expression}"
        );
    }
}
#[test]
fn native_constant_casts_preserve_two_and_four_state_masks() {
    for (kind, masked) in [("bit", false), ("logic", true)] {
        let tree = parse_source(
            &format!("module M;typedef {kind}[7:0]T;parameter N=T'(8'b10x1xxxx);endmodule"),
            Path::new("native-typed-cast-masks.sv"),
        )
        .unwrap();
        let source = Source::from_syntax(&tree).unwrap();
        let value = literal(&source);
        assert_eq!(value.width, 8);
        assert_eq!(value.mask != num_bigint::BigUint::default(), masked);
        assert_eq!(number(source), if masked { None } else { Some(144) });
    }
}
#[test]
fn native_constant_cast_target_and_operand_literals_are_authoritative() {
    for change in [0, 1, 2] {
        let mut tree = parse_source(
            if change == 0 {
                "module M;parameter N=int'(8);endmodule"
            } else {
                "module M;parameter N=4'(8);endmodule"
            },
            Path::new("native-typed-cast-identity.sv"),
        )
        .unwrap();
        if change == 0 {
            for value in &mut tree.declarations.types {
                if let DataTypeKind::Builtin(name) = &mut value.kind
                    && name == "int"
                {
                    *name = "byte".into();
                }
            }
        } else {
            for value in &mut tree.declarations.expressions {
                if let ExprKind::Literal(text) = &mut value.kind
                    && text == if change == 1 { "4" } else { "8" }
                {
                    *text = "9".into();
                }
            }
        }
        let source = Source::from_syntax(&tree).unwrap();
        assert_eq!(
            literal(&source).width,
            if change == 0 {
                8
            } else if change == 1 {
                9
            } else {
                4
            }
        );
        assert_eq!(number(source), Some(if change == 2 { -7 } else { 8 }));
    }
}
#[test]
fn constant_query_cast_target_remains_an_explicit_supported_class() {
    let tree = parse_source(
        "module M;parameter N=($bits(logic[7:0]))'(0);endmodule",
        Path::new("query-typed-cast.sv"),
    )
    .unwrap();
    let source = Source::from_syntax(&tree).unwrap();
    assert_eq!(literal(&source).width, 8);
    assert_eq!(number(source), Some(0));
}
#[test]
fn deep_native_constant_cast_targets_and_operands_use_worklists() {
    let path = Path::new("deep-native-typed-cast.sv");
    let mut tree = parse_source("module M;parameter N=(8+2)'((10+2));endmodule", path).unwrap();
    let open = "(".repeat(10_000);
    let close = ")".repeat(10_000);
    *tree.declarations = Declarations::parse(
        &format!("module M;parameter N=({open}8+2{close})'({open}10+2{close});endmodule"),
        path,
    )
    .unwrap();
    subtract(&mut tree);
    let source = Source::from_syntax(&tree).unwrap();
    assert_eq!(literal(&source).width, 6);
    assert_eq!(number(source), Some(8));
}
