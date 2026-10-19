//! Real native foreach header ownership and dimension/scope compatibility.
use super::*;
use crate::ast::{BinaryOp, Expr, LValue, Source, Stmt};
use celox_sv_syntax::{ExprKind, LoopSyntax, StatementKind};

fn parsed(declarations: &str, body: &str) -> ValidatedSyntax {
    parse_source(
        &format!("module M;{declarations} initial {body} endmodule"),
        Path::new("native-foreach.sv"),
    )
    .unwrap()
}
fn parts(stmt: &Stmt) -> (&str, &str, &str, BinaryOp, &[Stmt]) {
    let Stmt::Loop {
        init,
        condition: Some(Expr::Binary { op, right, .. }),
        body,
        ..
    } = stmt
    else {
        panic!("{stmt:?}")
    };
    let Stmt::Local {
        name,
        init: Some(Expr::Literal(left)),
    } = &init[0]
    else {
        panic!()
    };
    let Expr::Literal(right) = &**right else {
        panic!()
    };
    (name, left, right, *op, body)
}
#[test]
fn native_foreach_array_reference_drives_real_iteration_bounds() {
    let mut tree = parsed("logic[3:0]a[1:3],b[9:8];", "foreach(a[i]) begin end");
    let array = tree
        .declarations
        .statements
        .iter()
        .find_map(|stmt| match &stmt.kind {
            StatementKind::Loop {
                kind: LoopSyntax::Foreach(header),
                ..
            } => Some(header.array),
            _ => None,
        })
        .unwrap();
    let tokens = tree.declarations.expression(array).tokens;
    for expression in &mut tree.declarations.expressions {
        if expression.tokens == tokens
            && let ExprKind::Name(name) = &mut expression.kind
        {
            name.spelling = "b".into();
        }
    }
    let source = Source::from_syntax(&tree).unwrap();
    let (_, left, right, op, _) = parts(&source.modules()[0].initial_processes()[0].body()[0]);
    assert_eq!((left, right, op), ("9", "8", BinaryOp::Ge));
    // Whole-source grammar validation and the native/header identity join must
    // both accept the existing compilation-unit receiver spelling.
    parsed("logic[3:0]a[1:3];", "foreach($unit::a[i]) begin end");
}
#[test]
fn native_foreach_index_names_drive_actual_local_declarations() {
    let mut tree = parsed("logic[3:0]a[1:3];", "foreach(a[i]) begin end");
    for statement in &mut tree.declarations.statements {
        if let StatementKind::Loop {
            kind: LoopSyntax::Foreach(header),
            ..
        } = &mut statement.kind
        {
            header.variables[0].as_mut().unwrap().spelling = "native_index".into();
        }
    }
    let source = Source::from_syntax(&tree).unwrap();
    let module = &source.modules()[0];
    let (name, left, right, op, _) = parts(&module.initial_processes()[0].body()[0]);
    assert!(name.starts_with("native_index@"), "{name}");
    assert!(module.locals().iter().any(|local| local.name == name));
    assert_eq!((left, right, op), ("1", "3", BinaryOp::Le));
}
#[test]
fn native_foreach_skipped_slots_keep_dimension_positions_and_restore_shadowed_locals() {
    let tree = parsed(
        "logic[3:1]a[5:4][2:4];",
        "begin automatic int j=0;foreach(a[,j,k]) begin end j=1;end",
    );
    let source = Source::from_syntax(&tree).unwrap();
    let body = source.modules()[0].initial_processes()[0].body();
    let Stmt::Local { name: outer, .. } = &body[0] else {
        panic!()
    };
    let (inner, left, right, op, nested) = parts(&body[1]);
    assert_ne!(outer, inner);
    assert_eq!((left, right, op), ("2", "4", BinaryOp::Le));
    let (_, left, right, op, _) = parts(&nested[0]);
    assert_eq!((left, right, op), ("3", "1", BinaryOp::Ge));
    let Stmt::Assign {
        lhs: LValue::Ident(restored),
        ..
    } = &body[2]
    else {
        panic!()
    };
    assert_eq!(outer, restored);
}
#[test]
fn native_foreach_removed_and_skipped_slots_drive_bounds() {
    for remove in [true, false] {
        let mut tree = parsed("logic[3:0]a[1:3];", "foreach(a[i,j]) begin end");
        for statement in &mut tree.declarations.statements {
            if let StatementKind::Loop {
                kind: LoopSyntax::Foreach(header),
                ..
            } = &mut statement.kind
            {
                if remove {
                    header.variables.pop();
                } else {
                    header.variables[0] = None;
                }
            }
        }
        let source = Source::from_syntax(&tree).unwrap();
        let (_, left, right, op, _) = parts(&source.modules()[0].initial_processes()[0].body()[0]);
        assert_eq!(
            (left, right, op),
            if remove {
                ("1", "3", BinaryOp::Le)
            } else {
                ("3", "0", BinaryOp::Ge)
            }
        );
    }
}
#[test]
fn native_foreach_escaped_names_trailing_slots_and_excess_dimensions_keep_contracts() {
    let tree = parsed(
        "logic[3:0] \\a.with.dot [1:3];",
        "foreach(\\a.with.dot [\\i.with.dot ,]) begin end",
    );
    let source = Source::from_syntax(&tree).unwrap();
    let (name, left, right, _, _) = parts(&source.modules()[0].initial_processes()[0].body()[0]);
    assert!(name.starts_with("\\i.with.dot@"), "{name}");
    assert_eq!((left, right), ("1", "3"));
    let tree = parsed("logic[3:0]a[1:3];", "foreach(a[i,j,k]) begin end");
    let error = Source::from_syntax(&tree).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("foreach index beyond the array dimensions"),
        "{error}"
    );
}
