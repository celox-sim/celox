//! Native casts in lightweight constant ranges and enum value collection.
use super::*;
use crate::ast::Source;
use celox_sv_syntax::ExprKind;
fn subtract(tree: &mut SourceTree) {
    for node in &mut tree.declarations.expressions {
        if let ExprKind::Binary { operator, .. } = &mut node.kind
            && operator == "+"
        {
            *operator = "-".into();
        }
    }
}
#[test]
fn native_lightweight_cast_target_and_operand_operators_drive_enum_values() {
    for (value, expected) in [("8'((10+2))", 8), ("(8+2)'(16'habcd)", 13)] {
        let mut tree = parse_source(
            &format!("module M(output logic[15:0]y);typedef enum logic[15:0]{{A={value}}}E;assign y=A;endmodule"),
            Path::new("native-lightweight-enum-cast.sv"),
        ).unwrap();
        subtract(&mut tree);
        let ir = crate::analyze::analyze_source(Source::from_syntax(&tree).unwrap()).unwrap();
        let crate::ir::Expr::Literal(text) = ir.modules()[0].assignments()[0].rhs() else {
            panic!()
        };
        let value = crate::typecheck::parse_integral_literal(text).unwrap();
        assert_eq!(
            crate::typecheck::integral_literal_as_i128(&value, false),
            Some(expected)
        );
    }
}
#[test]
fn native_lightweight_cast_operands_drive_typedef_bounds() {
    let mut tree = parse_source(
        "module M;typedef logic[8'((10+2)):0]T;T value;endmodule",
        Path::new("native-lightweight-range-cast.sv"),
    )
    .unwrap();
    subtract(&mut tree);
    let ir = crate::analyze::analyze_source(Source::from_syntax(&tree).unwrap()).unwrap();
    assert_eq!(
        ir.modules()[0].signals()[0].r#type().resolved_width(),
        Some(9)
    );
}
#[test]
fn native_lightweight_alias_casts_use_named_widths_in_declaration_bounds() {
    for (kind, expected) in [("bit", 145), ("logic", 145)] {
        let mut tree = parse_source(
            &format!(
                "module M;typedef {kind}[7:0]B;typedef logic[B'((150+5)):0]T;T value;endmodule"
            ),
            Path::new("native-lightweight-alias-cast.sv"),
        )
        .unwrap();
        subtract(&mut tree);
        let ir = crate::analyze::analyze_source(Source::from_syntax(&tree).unwrap()).unwrap();
        assert_eq!(
            ir.modules()[0].signals()[0].r#type().resolved_width(),
            Some(expected + 1)
        );
    }
}
#[test]
fn native_lightweight_cast_targets_and_operands_are_authoritative() {
    for change_target in [false, true] {
        let mut tree = parse_source(
            "module M;typedef logic[8'(3):0]T;T value;endmodule",
            Path::new("native-lightweight-cast-guard.sv"),
        )
        .unwrap();
        for node in &mut tree.declarations.expressions {
            if let ExprKind::Literal(text) = &mut node.kind
                && text == if change_target { "8" } else { "3" }
            {
                *text = "9".into();
            }
        }
        let ir = crate::analyze::analyze_source(Source::from_syntax(&tree).unwrap()).unwrap();
        assert_eq!(
            ir.modules()[0].signals()[0].r#type().resolved_width(),
            Some(if change_target { 4 } else { 10 })
        );
    }
}
