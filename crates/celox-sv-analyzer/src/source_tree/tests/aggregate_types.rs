//! Aggregate syntax ownership through real signal/port/type consumers.
use super::*;
use crate::ast::Source;
use celox_sv_syntax::{DataTypeKind, ExprKind};
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
fn native_nested_aggregate_field_and_outer_ranges_drive_signal_width() {
    let mut tree=parse_source("module M;typedef struct packed {struct packed {logic[3+1:0]a;} inner;bit[1:0]b;}[1+1:0] T;T value;endmodule",Path::new("native-aggregate-bounds.sv")).unwrap();
    subtract(&mut tree);
    let ir = crate::analyze::analyze_source(Source::from_syntax(&tree).unwrap()).unwrap();
    assert_eq!(
        ir.modules()[0].signals()[0].r#type().resolved_width(),
        Some(5)
    );
}
#[test]
fn native_aggregate_primitive_kind_and_signing_drive_real_signals() {
    let mut tree = parse_source(
        "module M;typedef struct packed {logic[3:0]a;bit[1:0]b;} T;T value;endmodule",
        Path::new("native-aggregate-kind.sv"),
    )
    .unwrap();
    for ty in &mut tree.declarations.types {
        match &mut ty.kind {
            DataTypeKind::Builtin(name) if name == "logic" => *name = "bit".into(),
            DataTypeKind::Struct { .. } => ty.signed = Some(true),
            _ => {}
        }
    }
    let ir = crate::analyze::analyze_source(Source::from_syntax(&tree).unwrap()).unwrap();
    let ty = ir.modules()[0].signals()[0].r#type();
    assert_eq!(ty.kind(), crate::ir::TypeKind::Bit);
    assert!(ty.is_signed());
    assert_eq!(ty.resolved_width(), Some(6));
}
#[test]
fn native_inline_aggregate_ansi_port_ranges_drive_width() {
    let mut tree = parse_source(
        "module M(output struct packed {bit[3+1:0]a;bit b;} y);assign y=0;endmodule",
        Path::new("native-aggregate-port.sv"),
    )
    .unwrap();
    subtract(&mut tree);
    let ir = crate::analyze::analyze_source(Source::from_syntax(&tree).unwrap()).unwrap();
    assert_eq!(
        ir.modules()[0].ports()[0].r#type().resolved_width(),
        Some(4)
    );
}
#[test]
fn native_aggregate_field_membership_owns_layout_and_rejects_duplicates() {
    for duplicate in [false, true] {
        let mut tree = parse_source(
            "module M;typedef struct packed {logic a;logic b;} T;T value;endmodule",
            Path::new("native-aggregate-identity.sv"),
        )
        .unwrap();
        for ty in &mut tree.declarations.types {
            if let DataTypeKind::Struct { fields, .. } = &mut ty.kind {
                if duplicate {
                    fields[1].name = fields[0].name.clone();
                } else {
                    fields.pop();
                }
            }
        }
        if duplicate {
            assert!(Source::from_syntax(&tree).is_err());
        } else {
            let ir = crate::analyze::analyze_source(Source::from_syntax(&tree).unwrap()).unwrap();
            assert_eq!(
                ir.modules()[0].signals()[0].r#type().resolved_width(),
                Some(1)
            );
        }
    }
}

#[test]
fn native_field_order_changes_production_member_selection_offsets() {
    let mut tree=parse_source("module M(output logic[3:0]y);typedef struct packed {logic[3:0]high;bit[1:0]low;} T;T value;assign y=value.high;endmodule",Path::new("native-aggregate-member.sv")).unwrap();
    for ty in &mut tree.declarations.types {
        if let DataTypeKind::Struct { fields, .. } = &mut ty.kind {
            fields.reverse();
        }
    }
    let source = Source::from_syntax(&tree).unwrap();
    let crate::ast::Expr::Select { lsb, msb, .. } = source.modules()[0].assignments()[0].rhs()
    else {
        panic!("{:?}", source.modules()[0].assignments()[0].rhs());
    };
    assert_eq!(lsb, &crate::ast::ConstExpr::Literal("0".into()));
    assert_eq!(msb, &crate::ast::ConstExpr::Literal("3".into()));
}
