//! Native typedef-enum base/value ownership through actual constant consumers.
use super::*;
use crate::ast::Source;
use celox_sv_syntax::{DataTypeKind, ExprKind};
fn parsed(code: &str) -> SourceTree {
    let mut tree = parse_source(code, Path::new("native-enum-constant.sv")).unwrap();
    for expr in &mut tree.declarations.expressions {
        if let ExprKind::Binary { operator, .. } = &mut expr.kind
            && operator == "+"
        {
            *operator = "-".into();
        }
    }
    tree
}
fn literals(tree: &SourceTree) -> Vec<(usize, bool, i128)> {
    let ir = crate::analyze::analyze_source(Source::from_syntax(tree).unwrap()).unwrap();
    let mut pending = vec![ir.modules()[0].assignments()[0].rhs()];
    let mut values = Vec::new();
    while let Some(value) = pending.pop() {
        match value {
            crate::ir::Expr::Concat(parts) => pending.extend(parts.iter().rev()),
            crate::ir::Expr::Unary { expr, .. } | crate::ir::Expr::Resize { expr, .. } => {
                pending.push(expr)
            }
            crate::ir::Expr::Literal(text) => {
                let literal = crate::typecheck::parse_integral_literal(text).unwrap();
                values.push((
                    literal.width,
                    literal.signed,
                    crate::typecheck::integral_literal_as_i128(&literal, literal.signed).unwrap(),
                ));
            }
            _ => panic!("{value:?}"),
        }
    }
    values
}
fn base(tree: &mut SourceTree) -> &mut celox_sv_syntax::DataType {
    let id = tree
        .declarations
        .types
        .iter()
        .find_map(|data| {
            if let DataTypeKind::Enum { base: Some(id), .. } = data.kind {
                Some(id)
            } else {
                None
            }
        })
        .unwrap();
    let tokens = tree.declarations.data_type(id).tokens;
    tree.declarations
        .types
        .iter_mut()
        .find(|data| data.tokens == tokens)
        .unwrap()
}
#[test]
fn native_enum_value_operators_bind_previous_members_and_implicit_successors() {
    let tree = parsed(
        "module M(output logic[23:0]y);typedef enum logic[7:0]{A=10+2,B=A+2,C}E;assign y={A,B,C};endmodule",
    );
    assert_eq!(
        literals(&tree),
        [(8, false, 8), (8, false, 6), (8, false, 7)]
    );
}
#[test]
fn native_enum_packed_base_bound_operators_drive_member_literal_widths() {
    let tree = parsed(
        "module M(output logic[31:0]y);typedef enum logic[3+1:0]{A=1,B=2}E;assign y=~A;endmodule",
    );
    assert_eq!(literals(&tree), [(3, false, 1)]);
}
#[test]
fn native_enum_atom_width_and_vector_signing_drive_materialized_members() {
    let mut atom =
        parsed("module M(output logic[31:0]y);typedef enum int{A=1}E;assign y=~A;endmodule");
    base(&mut atom).kind = DataTypeKind::Builtin("byte".into());
    assert_eq!(literals(&atom), [(8, true, 1)]);
    let mut signed = parsed(
        "module M(output logic[31:0]y);typedef enum bit[7:0]{A=8'hff}E;assign y=A;endmodule",
    );
    base(&mut signed).signed = Some(true);
    assert_eq!(literals(&signed), [(8, true, -1)]);
}
#[test]
fn native_enum_plain_alias_base_and_default_int_keep_scope_and_width() {
    let tree = parsed(
        "module M(output logic[23:0]y);typedef bit[7:0]Base;typedef enum Base{A=10+2,B=A+2,C}E;assign y={A,B,C};endmodule",
    );
    assert_eq!(
        literals(&tree),
        [(8, false, 8), (8, false, 6), (8, false, 7)]
    );
    let default =
        parsed("module M(output logic[31:0]y);typedef enum{A,B,C}E;assign y=~A;endmodule");
    assert_eq!(literals(&default), [(32, true, 0)]);
}
#[test]
fn native_enum_member_and_initializer_presence_control_materialized_values() {
    let mut missing = parsed(
        "module M(output logic[7:0]y);typedef enum logic[7:0]{A=1,B=2}E;assign y=B;endmodule",
    );
    for data in &mut missing.declarations.types {
        if let DataTypeKind::Enum { items, .. } = &mut data.kind {
            items.pop();
        }
    }
    // The removed member remains an ordinary unresolved identifier instead of
    // acquiring the original enum initializer. Later net binding is independent.
    let source = Source::from_syntax(&missing).unwrap();
    assert!(
        matches!(source.modules()[0].assignments()[0].rhs(), crate::ast::Expr::Ident(name) if name == "B")
    );

    let mut implicit = parsed(
        "module M(output logic[15:0]y);typedef enum logic[7:0]{A=1,B=2}E;assign y={A,B};endmodule",
    );
    for data in &mut implicit.declarations.types {
        if let DataTypeKind::Enum { items, .. } = &mut data.kind {
            items[0].value = None;
        }
    }
    assert_eq!(literals(&implicit), [(8, false, 0), (8, false, 2)]);
}

#[test]
fn native_macro_enum_values_flow_through_package_imports() {
    let tree = parsed(
        "`define V(x) ((x)+2)\npackage p;typedef enum logic[7:0]{A=`V(10),B=A+2}E;endpackage module M(output logic[15:0]y);import p::*;assign y={A,B};endmodule",
    );
    assert_eq!(literals(&tree), [(8, false, 8), (8, false, 6)]);
}
