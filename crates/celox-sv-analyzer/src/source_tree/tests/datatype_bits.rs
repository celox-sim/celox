//! Native datatype query operands feed runtime, parameter and nested type users.
use super::*;
use crate::ast::{Expr, Source};
use celox_sv_syntax::{DataTypeKind, ExprKind};
fn parsed(operand: &str, parameter: bool) -> SourceTree {
    let statement = if parameter {
        format!("localparam N=$bits({operand});assign y=N;")
    } else {
        format!("assign y=$bits({operand});")
    };
    let mut tree = parse_source(
        &format!("module M(output int y);{statement}endmodule"),
        Path::new("native-datatype-bits.sv"),
    )
    .unwrap();
    for node in &mut tree.declarations.expressions {
        if let ExprKind::Binary { operator, .. } = &mut node.kind
            && operator == "+"
        {
            *operator = "-".into();
        }
    }
    tree
}
fn number(tree: &SourceTree, parameter: bool) -> i128 {
    let source = Source::from_syntax(tree).unwrap();
    if parameter {
        let ir = crate::analyze::analyze_source(source).unwrap();
        return ir.modules()[0].parameters()[0].resolved_value().unwrap();
    }
    let Expr::Literal(text) = source.modules()[0].assignments()[0].rhs() else {
        panic!()
    };
    text.parse().unwrap()
}
#[test]
fn native_datatype_query_bounds_drive_runtime_and_parameter_values() {
    for parameter in [false, true] {
        assert_eq!(number(&parsed("logic[7+1:0]", parameter), parameter), 7);
    }
}
#[test]
fn native_datatype_query_atom_metadata_drives_real_result_widths() {
    for parameter in [false, true] {
        let mut tree = parsed("int", parameter);
        for data in &mut tree.declarations.types {
            if let DataTypeKind::Builtin(name) = &mut data.kind
                && name == "int"
            {
                *name = "byte".into();
            }
        }
        assert_eq!(number(&tree, parameter), 8);
    }
}
#[test]
fn native_datatype_query_packed_field_bounds_drive_aggregate_width() {
    for parameter in [false, true] {
        let tree = parsed("struct packed {logic[3+1:0]a;bit[1:0]b;}", parameter);
        assert_eq!(number(&tree, parameter), 5);
    }
}
#[test]
fn native_datatype_query_rejects_unresolved_types_and_uses_native_fields() {
    let mut primitive = parsed("logic[7:0]", false);
    for data in &mut primitive.declarations.types {
        if matches!(data.kind, DataTypeKind::Builtin(ref name) if name == "logic") {
            data.kind = DataTypeKind::Named(Vec::new());
        }
    }
    assert!(Source::from_syntax(&primitive).is_err());
    let mut aggregate = parsed("struct packed {logic a;bit b;}", false);
    for data in &mut aggregate.declarations.types {
        if let DataTypeKind::Struct { fields, .. } = &mut data.kind {
            fields.pop();
        }
    }
    assert_eq!(number(&aggregate, false), 1);
}
#[test]
fn native_datatype_query_preserves_explicit_complex_bound_owner() {
    let tree = parsed("logic[$bits(logic[3:0]):0]", false);
    // The original width kernel counts nested datatype ranges. Keep its
    // existing result until that independent width contract migrates.
    assert_eq!(number(&tree, false), 20);
}
