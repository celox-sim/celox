//! Native whole net-member targets and member type-query ownership.
use super::*;
use crate::ast::{ConstExpr, Expr, LValue, Source};
use celox_sv_syntax::ExprKind;
fn parsed(statement: &str) -> ValidatedSyntax {
    parse_source(
        &format!("module M(input logic[3:0]a,output int y);struct packed{{logic signed[3:0]hi;bit[1:0]lo;}}s;{statement} endmodule"),
        Path::new("native-member-owner.sv"),
    ).unwrap()
}
#[test]
fn native_whole_net_member_targets_keep_offsets_and_state_kind() {
    let tree = parsed("assign s.hi=a;assign s.lo=a[1:0];");
    let source = Source::from_syntax(&tree).unwrap();
    for (assignment, expected) in source.modules()[0]
        .assignments()
        .iter()
        .zip([("5", "2", true, false), ("1", "0", false, true)])
    {
        let LValue::Select {
            name,
            msb: ConstExpr::Literal(msb),
            lsb: ConstExpr::Literal(lsb),
            signed,
            is_2state,
            ..
        } = assignment.lhs_value()
        else {
            panic!()
        };
        assert_eq!(name, "s");
        assert_eq!((msb.as_str(), lsb.as_str(), *signed, *is_2state), expected);
    }
    assert_eq!(source.modules()[0].assignments().len(), 2);
}
#[test]
fn native_member_queries_bind_runtime_and_parameter_paths_without_reading_values() {
    for parameter in [false, true] {
        let tree = parsed(if parameter {
            "localparam N=$bits(s.hi);assign y=N;"
        } else {
            "assign y=$bits(s.hi);"
        });
        let source = Source::from_syntax(&tree).unwrap();
        if parameter {
            let ir = crate::analyze::analyze_source(source).unwrap();
            assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(4));
        } else {
            assert_eq!(
                source.modules()[0].assignments()[0].rhs(),
                &Expr::Literal("4".into())
            );
        }
    }
}
#[test]
fn native_net_target_and_query_member_names_control_results() {
    for statement in [
        "assign s.hi=a;",
        "assign y=$bits(s.hi);",
        "localparam N=$bits(s.hi);assign y=N;",
    ] {
        let mut tree = parsed(statement);
        for node in &mut tree.declarations.expressions {
            if let ExprKind::Member { name, .. } = &mut node.kind
                && name.spelling == "hi"
            {
                name.spelling = "lo".into();
            }
        }
        let source = Source::from_syntax(&tree).unwrap();
        if statement.starts_with("assign s") {
            let LValue::Select {
                name,
                msb,
                lsb,
                is_2state,
                ..
            } = source.modules()[0].assignments()[0].lhs_value()
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
        } else if statement.starts_with("localparam") {
            let ir = crate::analyze::analyze_source(source).unwrap();
            assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(2));
        } else {
            assert_eq!(
                source.modules()[0].assignments()[0].rhs(),
                &Expr::Literal("2".into())
            );
        }
    }
}

#[test]
fn native_net_member_targets_preserve_escaped_dotted_components() {
    let tree = parse_source(
        "module M(input logic[3:0]a);struct packed{logic[3:0]\\hi.x ;bit[1:0]tail;}\\s.root ;assign \\s.root .\\hi.x =a;endmodule",
        Path::new("native-escaped-net-member.sv"),
    ).unwrap();
    let source = Source::from_syntax(&tree).unwrap();
    let LValue::Select {
        name,
        msb: ConstExpr::Literal(msb),
        lsb: ConstExpr::Literal(lsb),
        ..
    } = source.modules()[0].assignments()[0].lhs_value()
    else {
        panic!()
    };
    assert_eq!(
        (name.as_str(), msb.as_str(), lsb.as_str()),
        ("\\s.root", "5", "2")
    );
}
