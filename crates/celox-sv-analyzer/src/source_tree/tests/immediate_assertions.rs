//! Native assertion predicates and action identities reach real semantic bodies.
use super::*;
use crate::ast::{BinaryOp, Expr, Source, Stmt};
use crate::procedural::SystemTaskArg;
use celox_sv_syntax::{AssertionDeferral, AssertionHeader, ExprKind, StatementKind};
fn parsed(statement: &str) -> SourceTree {
    parse_source(
        &format!("module M(input logic[7:0]a,output logic[7:0]y);initial {statement} endmodule"),
        Path::new("native-assertion.sv"),
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
fn unwrapped(mut expression: &Expr) -> &Expr {
    while let Expr::Unary { expr, .. } | Expr::Resize { expr, .. } = expression {
        expression = expr;
    }
    expression
}
fn assigned_literal(statements: &[Stmt]) -> &str {
    let Stmt::Assign {
        rhs: Expr::Literal(value),
        ..
    } = &statements[0]
    else {
        panic!("{statements:?}")
    };
    value
}
#[test]
fn native_immediate_and_deferred_predicate_operators_drive_real_conditions() {
    for (keyword, action) in [
        ("assert", "y=1;"),
        ("assume", "y=1;"),
        ("cover", "y=1;"),
        // A deferred action is one subroutine call (IEEE 1800-2023 16.4).
        ("assert #0", "$info(\"p\");"),
        ("assume final", "$info(\"p\");"),
    ] {
        let mut tree = parsed(&format!("{keyword}(a+1) {action}"));
        subtract(&mut tree);
        let source = Source::from_syntax(&tree).unwrap();
        let Stmt::If { condition, .. } = &source.modules()[0].initial_processes()[0].body()[0]
        else {
            panic!()
        };
        assert!(
            matches!(
                unwrapped(condition),
                Expr::Binary {
                    op: BinaryOp::Sub,
                    ..
                }
            ),
            "{keyword}: {condition:?}"
        );
    }
    let mut tree=parse_source("`define CHECK(v) assert(v+1) y=1;\nmodule M(input logic[7:0]a,output logic[7:0]y);initial `CHECK(a) endmodule",Path::new("macro-assertion.sv")).unwrap();
    subtract(&mut tree);
    let source = Source::from_syntax(&tree).unwrap();
    let Stmt::If { condition, .. } = &source.modules()[0].initial_processes()[0].body()[0] else {
        panic!()
    };
    assert!(matches!(
        unwrapped(condition),
        Expr::Binary {
            op: BinaryOp::Sub,
            ..
        }
    ));
}
#[test]
fn native_assertion_action_ids_drive_real_pass_and_fail_branches() {
    let mut tree = parsed("assert(a) y=1; else y=2;");
    for statement in &mut tree.declarations.statements {
        if let StatementKind::Assertion {
            success, failure, ..
        } = &mut statement.kind
        {
            std::mem::swap(success, failure);
        }
    }
    let source = Source::from_syntax(&tree).unwrap();
    let Stmt::If {
        then_body,
        else_body,
        ..
    } = &source.modules()[0].initial_processes()[0].body()[0]
    else {
        panic!()
    };
    assert_eq!(assigned_literal(then_body), "2");
    assert_eq!(assigned_literal(else_body), "1");
}
#[test]
fn native_assertion_default_failure_cover_and_else_only_actions_keep_contracts() {
    for keyword in ["assert", "assume", "cover"] {
        let tree = parsed(&format!("{keyword}(a);"));
        let source = Source::from_syntax(&tree).unwrap();
        let Stmt::If {
            then_body,
            else_body,
            ..
        } = &source.modules()[0].initial_processes()[0].body()[0]
        else {
            panic!()
        };
        assert!(then_body.is_empty());
        if keyword == "cover" {
            assert!(else_body.is_empty());
        } else {
            let [Stmt::SystemTask { args, .. }] = else_body.as_slice() else {
                panic!()
            };
            assert_eq!(args, &[SystemTaskArg::Str(format!("{keyword} failed"))]);
        }
    }
    let tree = parsed("assert(a) else y=2;");
    let source = Source::from_syntax(&tree).unwrap();
    let Stmt::If {
        then_body,
        else_body,
        ..
    } = &source.modules()[0].initial_processes()[0].body()[0]
    else {
        panic!()
    };
    assert!(then_body.is_empty());
    assert_eq!(assigned_literal(else_body), "2");
}
#[test]
fn native_assertion_header_and_action_roles_are_authoritative() {
    for change_deferral in [false, true] {
        let mut tree = parsed("assert(a) y=1; else y=2;");
        for statement in &mut tree.declarations.statements {
            if let StatementKind::Assertion {
                header, success, ..
            } = &mut statement.kind
            {
                if change_deferral {
                    let AssertionHeader::Immediate { deferred, .. } = header else {
                        panic!()
                    };
                    *deferred = Some(AssertionDeferral::Final);
                } else {
                    *success = None;
                }
            }
        }
        // The native header decides the deferral: a deferred action must be
        // one subroutine call (IEEE 1800-2023 16.4).
        if change_deferral {
            let error = Source::from_syntax(&tree).unwrap_err();
            assert!(
                error.to_string().contains("deferred assertion action"),
                "{error}"
            );
            continue;
        }
        let source = Source::from_syntax(&tree).unwrap();
        let Stmt::If {
            then_body,
            else_body,
            ..
        } = &source.modules()[0].initial_processes()[0].body()[0]
        else {
            panic!()
        };
        assert!(then_body.is_empty());
        assert_eq!(assigned_literal(else_body), "2");
    }
}
#[test]
fn native_assertion_action_scopes_and_unsupported_classes_keep_existing_behavior() {
    let tree =
        parsed("assert(a) begin automatic int v=1;y=v;end else begin automatic int v=2;y=v;end");
    let source = Source::from_syntax(&tree).unwrap();
    let Stmt::If {
        then_body,
        else_body,
        ..
    } = &source.modules()[0].initial_processes()[0].body()[0]
    else {
        panic!()
    };
    let Stmt::Local { name: pass, .. } = &then_body[0] else {
        panic!()
    };
    let Stmt::Local { name: fail, .. } = &else_body[0] else {
        panic!()
    };
    assert_ne!(pass, fail);
    for (statement, diagnostic) in [
        ("cover #0(a) y=1;", "deferred assertion action"),
        ("assert property(a);", "concurrent assertion"),
    ] {
        let tree = parsed(statement);
        let error = Source::from_syntax(&tree).unwrap_err();
        assert!(
            error.to_string().contains(diagnostic),
            "{statement}: {error}"
        );
    }
}
