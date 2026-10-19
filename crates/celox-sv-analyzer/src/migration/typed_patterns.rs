//! Native typed-pattern target/shape and value ownership through actual RHSs.
use super::*;
use crate::ast::{BinaryOp, Expr, Source};
use celox_sv_syntax::{DataTypeKind, ExprKind};
fn parsed(code: &str) -> ValidatedSyntax {
    let mut tree = parse_source(code, Path::new("native-typed-pattern.sv")).unwrap();
    for node in &mut tree.declarations.expressions {
        if let ExprKind::Binary { operator, .. } = &mut node.kind
            && operator == "+"
        {
            *operator = "-".into();
        }
    }
    tree
}
fn widths(root: &Expr) -> Vec<usize> {
    let mut pending = vec![root];
    let mut widths = Vec::new();
    while let Some(value) = pending.pop() {
        match value {
            Expr::Concat(parts) => pending.extend(parts.iter().rev()),
            Expr::Call { args, .. } => pending.extend(args.iter().rev().flatten()),
            Expr::Resize { expr, width, .. } => {
                assert!(matches!(
                    expr.as_ref(),
                    Expr::Binary {
                        op: BinaryOp::Sub,
                        ..
                    }
                ));
                widths.push(*width);
            }
            _ => panic!("{value:?}"),
        }
    }
    widths
}
#[test]
fn native_typed_alias_and_struct_pattern_values_own_operators_and_widths() {
    for (declaration, pattern, expected) in [
        ("typedef logic[1:0]T;", "T'{a+b,a+b}", vec![1, 1]),
        (
            "typedef struct packed{logic[1:0]x;logic[2:0]z;}T;",
            "T'{x:a+b,z:a+b}",
            vec![2, 3],
        ),
    ] {
        let tree = parsed(&format!(
            "module M(input bit a,b,output logic[4:0]y);{declaration}assign y={pattern};endmodule"
        ));
        let source = Source::from_syntax(&tree).unwrap();
        assert_eq!(widths(source.modules()[0].assignments()[0].rhs()), expected);
    }
}
#[test]
fn native_integer_atom_pattern_target_metadata_drives_receiving_shape() {
    let mut tree =
        parsed("module M(input bit a,b,output logic[7:0]y);assign y=byte'{default:a+b};endmodule");
    for data in &mut tree.declarations.types {
        if let DataTypeKind::Builtin(name) = &mut data.kind
            && name == "byte"
        {
            *name = "shortint".into();
        }
    }
    let source = Source::from_syntax(&tree).unwrap();
    assert_eq!(
        widths(source.modules()[0].assignments()[0].rhs()),
        vec![1; 16]
    );
}
#[test]
fn native_nested_typed_array_pattern_items_use_the_same_workstream() {
    let tree = parsed(
        "module M(input bit a,b);typedef logic[1:0]T;T v[0:1];assign v='{T'{a+b,a+b},T'{a+b,a+b}};endmodule",
    );
    let source = Source::from_syntax(&tree).unwrap();
    assert_eq!(
        widths(source.modules()[0].assignments()[0].rhs()),
        [1, 1, 1, 1]
    );
}
#[test]
fn native_typed_pattern_function_actuals_bind_native_values() {
    let tree = parsed(
        "module M(input bit a,b,output logic[1:0]y);typedef logic[1:0]T;function automatic T f(input T v);return v;endfunction assign y=f(T'{a+b,a+b});endmodule",
    );
    let source = Source::from_syntax(&tree).unwrap();
    assert_eq!(widths(source.modules()[0].assignments()[0].rhs()), [1, 1]);
}
#[test]
fn native_typed_pattern_target_identity_mismatch_is_not_retried() {
    let mut tree = parsed(
        "module M(input bit a,b,output logic[1:0]y);typedef logic[1:0]T;assign y=T'{a,b};endmodule",
    );
    for expr in &mut tree.declarations.expressions {
        if let ExprKind::Name(name) = &mut expr.kind
            && name.spelling == "T"
        {
            name.spelling = "missing".into();
        }
    }
    assert!((Source::from_syntax(&tree)).is_err());
}

#[test]
fn native_package_qualified_typed_pattern_targets_keep_alias_scope() {
    let tree = parsed(
        "package p;typedef logic[1:0]T;endpackage module M(input bit a,b,output logic[1:0]y);assign y=p::T'{a+b,a+b};endmodule",
    );
    let source = Source::from_syntax(&tree).unwrap();
    assert_eq!(widths(source.modules()[0].assignments()[0].rhs()), [1, 1]);
}
