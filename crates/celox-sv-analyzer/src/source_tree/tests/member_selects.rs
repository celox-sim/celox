//! Native member leaf coordinates, with shared strict member-relative checks.
use super::*;
use crate::ast::{ConstExpr, Expr, LValue, Source, Stmt};
use celox_sv_syntax::{DimensionKind, ExprKind};
fn parsed(field: &str, statement: &str) -> SourceTree {
    let mut tree = parse_source(
        &format!("module M(input logic[15:0]a,output logic[15:0]y);struct packed{{logic{field}hi;bit[1:0]tail;}}s;{statement} endmodule"),
        Path::new("native-member-select.sv"),
    ).unwrap();
    for node in &mut tree.declarations.expressions {
        if let ExprKind::Binary { operator, .. } = &mut node.kind
            && operator == "+"
        {
            *operator = "-".into();
        }
    }
    tree
}
fn expr_bounds(expr: &Expr) -> (&str, &str) {
    match expr {
        Expr::Select {
            msb: ConstExpr::Literal(msb),
            lsb: ConstExpr::Literal(lsb),
            ..
        } => (msb, lsb),
        Expr::Resize { expr, .. } | Expr::Unary { expr, .. } => expr_bounds(expr),
        _ => panic!("{expr:?}"),
    }
}
fn target_bounds(target: &LValue) -> (&str, &str) {
    let LValue::Select {
        msb: ConstExpr::Literal(msb),
        lsb: ConstExpr::Literal(lsb),
        ..
    } = target
    else {
        panic!()
    };
    (msb, lsb)
}
#[test]
fn native_member_constant_ranges_drive_real_read_and_procedural_target_coordinates() {
    let read = parsed("[15:0]", "assign y=s.hi[7+1:0];");
    let source = Source::from_syntax(&read).unwrap();
    assert_eq!(
        expr_bounds(source.modules()[0].assignments()[0].rhs()),
        ("8", "2")
    );
    let write = parsed("[15:0]", "always_comb s.hi[7+1:0]=a;");
    let source = Source::from_syntax(&write).unwrap();
    let Stmt::Assign { lhs, .. } = &source.modules()[0].comb_processes()[0].body()[0] else {
        panic!()
    };
    assert_eq!(target_bounds(lhs), ("8", "2"));
}
#[test]
fn native_member_multidimensional_bit_indices_drive_actual_read_positions() {
    let tree = parsed("[1:0][3:0]", "assign y=s.hi[1+1][3+1];");
    let source = Source::from_syntax(&tree).unwrap();
    assert_eq!(
        expr_bounds(source.modules()[0].assignments()[0].rhs()),
        ("4", "4")
    );
}
#[test]
fn native_member_indexed_start_and_width_operators_drive_read_and_write_positions() {
    let read = parsed("[15:0]", "assign y=s.hi[2+1 +:(6+2)];");
    let source = Source::from_syntax(&read).unwrap();
    assert_eq!(
        expr_bounds(source.modules()[0].assignments()[0].rhs()),
        ("6", "3")
    );
    let write = parsed("[15:0]", "always_comb s.hi[2+1 +:(6+2)]=a;");
    let source = Source::from_syntax(&write).unwrap();
    let Stmt::Assign { lhs, .. } = &source.modules()[0].comb_processes()[0].body()[0] else {
        panic!()
    };
    assert_eq!(target_bounds(lhs), ("6", "3"));
}
#[test]
fn native_member_selection_kind_is_authoritative() {
    let mut tree = parsed("[15:0]", "assign y=s.hi[7:0];");
    for node in &mut tree.declarations.expressions {
        if let ExprKind::Select { dimension, .. } = &mut node.kind
            && let DimensionKind::Range { left, .. } = dimension.kind
        {
            dimension.kind = DimensionKind::Index(left);
        }
    }
    let source = Source::from_syntax(&tree).unwrap();
    assert_eq!(
        expr_bounds(source.modules()[0].assignments()[0].rhs()),
        ("9", "9")
    );
}
#[test]
fn native_member_coordinate_checks_keep_nonzero_declared_origins_and_neighbor_bounds() {
    for statement in [
        "assign y=s.hi[3];",
        "always_comb s.hi[3]=a;",
        "assign y=s.hi[8:6];",
    ] {
        let tree = parsed("[7:4]", statement);
        assert!(
            matches!(
                Source::from_syntax(&tree),
                Err(AnalyzerError::Unsupported(_))
            ),
            "{statement}"
        );
    }
    let tree = parsed("[7:4]", "assign y=s.hi[6];");
    let source = Source::from_syntax(&tree).unwrap();
    assert_eq!(
        expr_bounds(source.modules()[0].assignments()[0].rhs()),
        ("4", "4")
    );
}

#[test]
fn native_net_member_indices_ranges_and_indexed_sizes_drive_target_coordinates() {
    for (field, target, expected) in [
        ("[15:0]", "s.hi[7+1:0]", ("8", "2")),
        ("[15:0]", "s.hi[2+1 +:(6+2)]", ("6", "3")),
        ("[1:0][3:0]", "s.hi[1+1][3+1]", ("4", "4")),
    ] {
        let tree = parsed(field, &format!("assign {target}=a;"));
        let source = Source::from_syntax(&tree).unwrap();
        assert_eq!(
            target_bounds(source.modules()[0].assignments()[0].lhs_value()),
            expected,
            "{target}"
        );
    }
}
#[test]
fn native_net_member_selected_dimension_controls_target() {
    let mut tree = parsed("[15:0]", "assign s.hi[7:0]=a;");
    for node in &mut tree.declarations.expressions {
        if let ExprKind::Select { dimension, .. } = &mut node.kind
            && let DimensionKind::Range { left, .. } = dimension.kind
        {
            dimension.kind = DimensionKind::Index(left);
        }
    }
    let source = Source::from_syntax(&tree).unwrap();
    assert_eq!(
        target_bounds(source.modules()[0].assignments()[0].lhs_value()),
        ("9", "9")
    );
}
#[test]
fn native_net_member_coordinates_preserve_declared_origin_and_neighbor_rejection() {
    for target in ["s.hi[3]", "s.hi[8:6]"] {
        let tree = parsed("[7:4]", &format!("assign {target}=a;"));
        assert!(
            matches!(
                Source::from_syntax(&tree),
                Err(AnalyzerError::Unsupported(_))
            ),
            "{target}"
        );
    }
    let tree = parsed("[7:4]", "assign s.hi[6]=a;");
    let source = Source::from_syntax(&tree).unwrap();
    assert_eq!(
        target_bounds(source.modules()[0].assignments()[0].lhs_value()),
        ("4", "4")
    );
}
