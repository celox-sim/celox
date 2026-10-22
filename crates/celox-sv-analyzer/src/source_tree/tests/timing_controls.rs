//! Native timing payload ownership, independent of validated legacy operators.
use super::*;
use crate::ast::{BinaryOp, Expr, FfEdge, Source, Stmt};
use crate::procedural::EventEdge;
use celox_sv_syntax::{EventControl, ExprKind, TimingControlKind, TimingEdge};

fn parsed(body: &str) -> SourceTree {
    parse_source(
        &format!("module M(input logic clk,rst,en,a,b,d,output logic q);{body} endmodule"),
        Path::new("native-timing.sv"),
    )
    .unwrap()
}
fn subtract(tree: &mut SourceTree) {
    for expression in &mut tree.declarations.expressions {
        if let ExprKind::Binary { operator, .. } = &mut expression.kind
            && operator == "+"
        {
            *operator = "-".into();
        }
    }
}
#[test]
fn native_ff_edges_drive_real_process_sensitivity() {
    let mut tree = parsed("always_ff @(posedge clk or negedge rst) q<=d;");
    for control in &mut tree.declarations.timing_controls {
        if let TimingControlKind::Event(EventControl::Items(items)) = &mut control.kind {
            items[0].edge = TimingEdge::Neg;
            items[1].edge = TimingEdge::Pos;
        }
    }
    let source = Source::from_syntax(&tree).unwrap();
    let events = source.modules()[0].ff_processes()[0].events();
    assert_eq!((events[0].edge(), events[0].signal()), (FfEdge::Neg, "clk"));
    assert_eq!((events[1].edge(), events[1].signal()), (FfEdge::Pos, "rst"));
}
#[test]
fn native_delay_and_typical_delay_operands_drive_suspensions() {
    for delay in ["#(9+3)", "#(1:9+3:20)"] {
        let mut tree = parsed(&format!("initial {delay} q=1;"));
        subtract(&mut tree);
        let source = Source::from_syntax(&tree).unwrap();
        assert!(
            matches!(
                &source.modules()[0].initial_processes()[0].body()[0],
                Stmt::Delay {
                    amount: Expr::Binary {
                        op: BinaryOp::Sub,
                        ..
                    },
                    ..
                }
            ),
            "{delay}"
        );
    }
}
#[test]
fn native_timed_event_edges_and_expression_operators_drive_wait_events() {
    let mut tree = parsed("initial @((a+1) or negedge b) q=1;");
    subtract(&mut tree);
    for control in &mut tree.declarations.timing_controls {
        if let TimingControlKind::Event(EventControl::Items(items)) = &mut control.kind {
            items[1].edge = TimingEdge::Pos;
        }
    }
    let source = Source::from_syntax(&tree).unwrap();
    let Stmt::WaitEvent(items) = &source.modules()[0].initial_processes()[0].body()[0] else {
        panic!()
    };
    assert_eq!(items[0].edge, EventEdge::Any);
    assert!(matches!(
        &items[0].expr,
        Expr::Binary {
            op: BinaryOp::Sub,
            ..
        }
    ));
    assert_eq!(items[1].edge, EventEdge::Pos);
}
#[test]
fn native_timing_event_membership_and_delay_roles_are_authoritative() {
    for ff in [false, true] {
        let mut tree = parsed(if ff {
            "always_ff @(posedge clk or negedge rst) q<=d;"
        } else {
            "initial @(a or b) q=1;"
        });
        for control in &mut tree.declarations.timing_controls {
            if let TimingControlKind::Event(EventControl::Items(items)) = &mut control.kind {
                items.pop();
            }
        }
        if ff {
            let source = Source::from_syntax(&tree).unwrap();
            assert_eq!(source.modules()[0].ff_processes()[0].events().len(), 1);
        } else {
            let source = Source::from_syntax(&tree).unwrap();
            let Stmt::WaitEvent(items) = &source.modules()[0].initial_processes()[0].body()[0]
            else {
                panic!()
            };
            assert_eq!(items.len(), 1);
        }
    }
    let mut tree = parsed("initial #(1+2) q=1;");
    for control in &mut tree.declarations.timing_controls {
        if let TimingControlKind::Delay { parenthesized, .. } = &mut control.kind {
            *parenthesized = false;
        }
    }
    assert!(Source::from_syntax(&tree).is_err());
}
#[test]
fn native_timing_preserves_existing_restrictions_including_ff_iff_validation() {
    for (body, diagnostic) in [
        ("initial @(a iff en) q=1;", "iff-qualified event"),
        ("always_comb #1 q=d;", "delay control"),
        ("always_ff @(edge clk) q<=d;", "always_ff event expression"),
        (
            "always_ff @(posedge (clk+1)) q<=d;",
            "always_ff event expression",
        ),
    ] {
        let tree = parsed(body);
        let error = Source::from_syntax(&tree).unwrap_err();
        assert!(error.to_string().contains(diagnostic), "{body}: {error}");
    }
    let tree = parsed("always_ff @(posedge clk iff en) q<=d;");
    let error = Source::from_syntax(&tree).unwrap_err();
    assert!(error.to_string().contains("iff-qualified always_ff event"));
}
