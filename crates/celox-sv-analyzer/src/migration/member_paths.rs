//! Native member-path consumption through real value and variable-target users.
use super::*;
use crate::ast::{ConstExpr, Expr, LValue, Source, Stmt};
use celox_sv_syntax::ExprKind;
fn parsed(statement: &str) -> ValidatedSyntax {
    parse_source(
        &format!("module M(input logic[3:0]a,output logic[3:0]y);struct packed{{logic signed[3:0]hi;bit[1:0]lo;}}s;{statement} endmodule"),
        Path::new("native-member-path.sv"),
    ).unwrap()
}
fn bounds(expr: &Expr) -> (&str, &str, &str, bool) {
    match expr {
        Expr::Select {
            expr,
            msb: ConstExpr::Literal(msb),
            lsb: ConstExpr::Literal(lsb),
            signed,
        } => {
            let Expr::Ident(name) = &**expr else { panic!() };
            (name, msb, lsb, *signed)
        }
        Expr::Resize { expr, .. } | Expr::Unary { expr, .. } => bounds(expr),
        _ => panic!("{expr:?}"),
    }
}
#[test]
fn native_whole_member_values_preserve_offset_sign_and_state_conversion() {
    let tree = parsed("assign y=s.hi;assign y=s.lo;");
    let source = Source::from_syntax(&tree).unwrap();
    let assignments = source.modules()[0].assignments();
    assert_eq!(bounds(assignments[0].rhs()), ("s", "5", "2", true));
    assert_eq!(bounds(assignments[1].rhs()), ("s", "1", "0", false));
    assert!(matches!(assignments[1].rhs(), Expr::Resize { .. }));
}
#[test]
fn native_whole_member_variable_targets_preserve_offset_and_state_metadata() {
    let tree = parsed("always_comb begin s.hi=a;s.lo=a[1:0];end");
    let source = Source::from_syntax(&tree).unwrap();
    let body = source.modules()[0].comb_processes()[0].body();
    for (statement, expected) in body
        .iter()
        .zip([("5", "2", true, false), ("1", "0", false, true)])
    {
        let Stmt::Assign {
            lhs:
                LValue::Select {
                    name,
                    msb: ConstExpr::Literal(msb),
                    lsb: ConstExpr::Literal(lsb),
                    signed,
                    is_2state,
                    ..
                },
            ..
        } = statement
        else {
            panic!("{statement:?}")
        };
        assert_eq!(name, "s");
        assert_eq!((msb.as_str(), lsb.as_str(), *signed, *is_2state), expected);
    }
    assert_eq!(body.len(), 2);
}
#[test]
fn native_whole_member_value_names_are_authoritative() {
    for statement in ["assign y=s.hi;", "always_comb s.hi=a;"] {
        let mut tree = parsed(statement);
        for node in &mut tree.declarations.expressions {
            if let ExprKind::Member { name, .. } = &mut node.kind
                && name.spelling == "hi"
            {
                name.spelling = "lo".into();
            }
        }
        let result = Source::from_syntax(&tree);
        if statement.starts_with("assign") {
            let source = result.unwrap();
            assert_eq!(
                bounds(source.modules()[0].assignments()[0].rhs()),
                ("s", "1", "0", false)
            );
        } else {
            let source = result.unwrap();
            let Stmt::Assign {
                lhs:
                    LValue::Select {
                        name,
                        msb,
                        lsb,
                        is_2state,
                        ..
                    },
                ..
            } = &source.modules()[0].comb_processes()[0].body()[0]
            else {
                panic!()
            };
            assert_eq!(name, "s");
            assert_eq!(
                (msb, lsb),
                (
                    &ConstExpr::Literal("1".into()),
                    &ConstExpr::Literal("0".into())
                )
            );
            assert!(*is_2state);
        }
    }
}
#[test]
fn native_whole_member_paths_keep_escaped_dots_inside_identifier_components() {
    let tree = parse_source(
        "module M(output logic[3:0]y);struct packed{logic[3:0]\\a.b ;bit[1:0]tail;}\\s.root ;assign y=\\s.root .\\a.b ;endmodule",
        Path::new("native-escaped-member-path.sv"),
    ).unwrap();
    let source = Source::from_syntax(&tree).unwrap();
    assert_eq!(
        bounds(source.modules()[0].assignments()[0].rhs()),
        ("\\s.root", "5", "2", false)
    );
}
#[test]
fn native_nested_whole_member_paths_accumulate_existing_layout_offsets() {
    let tree = parse_source(
        "module M(output logic[2:0]y);typedef struct packed{logic[2:0]hi;logic tail;}Inner;struct packed{Inner inner;bit[1:0]tail;}s;assign y=s.inner.hi;endmodule",
        Path::new("native-nested-member-path.sv"),
    ).unwrap();
    let source = Source::from_syntax(&tree).unwrap();
    assert_eq!(
        bounds(source.modules()[0].assignments()[0].rhs()),
        ("s", "5", "3", false)
    );
}
