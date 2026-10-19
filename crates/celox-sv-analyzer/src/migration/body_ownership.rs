//! Native body ownership through actual process and subroutine lowering.
use super::*;
use celox_sv_syntax::StatementKind;

#[cfg(test)]
mod tests {
    use super::*;
    fn rejects_body(tree: &mut ValidatedSyntax, native: Shared<celox_sv_syntax::Declarations>) {
        let previous = std::mem::replace(&mut tree.declarations, native);
        assert!(crate::ast::Source::from_syntax(tree).is_err());
        tree.declarations = previous;
    }

    #[test]
    fn native_statement_ids_drive_body_order_and_conditional_children() {
        let mut tree=super::super::parse_source("module M(input logic a,output logic[3:0] y);always_comb begin y=1;y=2;if(a)y=3;else y=4;end endmodule",Path::new("native-child-order.sv")).unwrap();
        for statement in &mut tree.declarations.statements {
            match &mut statement.kind {
                StatementKind::Block { items, .. } => items.swap(0, 1),
                StatementKind::Conditional {
                    branches,
                    otherwise,
                } => std::mem::swap(&mut branches[0].body, otherwise.as_mut().unwrap()),
                _ => {}
            }
        }
        // Deliberately change only native control after validation.
        // If the consumer still followed CST child lists, these values/order
        // would remain1,2 and then3/else4 instead.
        let source = crate::ast::Source::from_syntax(&tree).unwrap();
        let body = source.modules()[0].comb_processes()[0].body();
        assert!(
            matches!(&body[0],crate::ast::Stmt::Assign{rhs:crate::ast::Expr::Literal(value),..} if value=="2")
        );
        assert!(
            matches!(&body[1],crate::ast::Stmt::Assign{rhs:crate::ast::Expr::Literal(value),..} if value=="1")
        );
        let crate::ast::Stmt::If {
            then_body,
            else_body,
            ..
        } = &body[2]
        else {
            panic!()
        };
        assert!(
            matches!(&then_body[0],crate::ast::Stmt::Assign{rhs:crate::ast::Expr::Literal(value),..} if value=="4")
        );
        assert!(
            matches!(&else_body[0],crate::ast::Stmt::Assign{rhs:crate::ast::Expr::Literal(value),..} if value=="3")
        );
    }

    #[test]
    fn deep_native_block_scope_lowering_is_iterative_and_flattens_once() {
        let depth = 10_000;
        let mut flat = String::from("module M;initial begin ");
        let mut deep = String::from("module M;initial ");
        for index in 0..depth {
            flat.push_str(&format!("automatic int v{index};"));
            deep.push_str(&format!("begin automatic int v{index};"));
        }
        flat.push_str("end endmodule");
        deep.push_str(&"end ".repeat(depth));
        deep.push_str("endmodule");
        let mut tree =
            super::super::parse_source(&flat, Path::new("flat-typed-leaves.sv")).unwrap();
        let native =
            celox_sv_syntax::Declarations::parse(&deep, Path::new("deep-native-control.sv"))
                .unwrap();
        // Native structural blocks need no original statement identities. Only
        // the equivalent flat typed declaration leaves are borrowed.
        tree.declarations = native.into();
        let source = crate::ast::Source::from_syntax(&tree).unwrap();
        assert_eq!(source.modules()[0].locals().len(), depth);
        assert_eq!(
            source.modules()[0].initial_processes()[0].body().len(),
            depth
        );
    }

    #[test]
    fn native_local_initializer_presence_is_authoritative() {
        let source = "module M(input logic a,output logic y);always_comb begin automatic logic x=a;y=x;end endmodule";
        let mut tree = super::super::parse_source(source, Path::new("local-coverage.sv")).unwrap();
        let mut native = tree.declarations.clone();
        let StatementKind::Declaration(celox_sv_syntax::LocalDeclaration::Variables(declaration)) =
            &mut native
                .statements
                .iter_mut()
                .find(|statement| matches!(statement.kind, StatementKind::Declaration(_)))
                .unwrap()
                .kind
        else {
            unreachable!()
        };
        declaration.variables[0].value = None;
        tree.declarations = native;
        let source = crate::ast::Source::from_syntax(&tree).unwrap();
        assert!(matches!(
            source.modules()[0].comb_processes()[0].body()[0],
            crate::ast::Stmt::Local { init: None, .. }
        ));
    }

    #[test]
    fn constructed_and_other_typed_block_declarations_keep_explicit_leaf_classes() {
        for declaration in [
            "int data[];",
            "int data[]=new[2];",
            "C value=new(1);",
            "C value=new old;",
            "let identity(x)=x;",
            "nettype logic net_t;",
        ] {
            let source = format!("module M;initial begin {declaration} end endmodule");
            super::super::parse_source(&source, Path::new("typed-local-leaf.sv"))
                .unwrap_or_else(|error| panic!("{declaration}: {error}"));
        }
    }

    #[test]
    fn valid_typed_procedural_leaves_remain_parseable_before_semantic_rejection() {
        for body in [
            "randcase 1: y=0; 2: y=1; endcase",
            "expect(a) y=1; else y=0;",
            "cb.y <= ##1 a;",
            "y = new[2];",
            "y = new();",
        ] {
            let source = format!("module M; logic a,y; initial begin {body} end endmodule");
            super::super::parse_source(&source, Path::new("typed-procedure.sv"))
                .unwrap_or_else(|error| panic!("{body}: {error}"));
        }
    }

    #[test]
    fn compilation_unit_checkers_keep_an_explicit_unmigrated_body_scope() {
        let tree = super::super::parse_source(
            "checker C(input logic a); always_comb assert(a); endchecker module M; endmodule",
            Path::new("checker-scope.sv"),
        )
        .unwrap();
        assert!(tree.declarations.procedures.is_empty());
        assert_eq!(tree.declarations.units.len(), 1);
    }

    #[test]
    fn native_case_labels_and_for_header_fields_are_authoritative() {
        let mut tree=super::super::parse_source("module M;int a,y;initial begin case(a) 0:y=1;endcase for(int i=0;i<a;i++) y=i;end endmodule",Path::new("control-join.sv")).unwrap();
        let mut native = tree.declarations.clone();
        let StatementKind::Case { arms, .. } = &mut native
            .statements
            .iter_mut()
            .find(|statement| matches!(statement.kind, StatementKind::Case { .. }))
            .unwrap()
            .kind
        else {
            unreachable!()
        };
        arms[0].matches.clear();
        rejects_body(&mut tree, native);
        let mut native = tree.declarations.clone();
        let StatementKind::Loop {
            kind: celox_sv_syntax::LoopSyntax::For(header),
            ..
        } = &mut native
            .statements
            .iter_mut()
            .find(|statement| matches!(statement.kind, StatementKind::Loop { .. }))
            .unwrap()
            .kind
        else {
            unreachable!()
        };
        header.condition = None;
        let previous = std::mem::replace(&mut tree.declarations, native);
        let source = crate::ast::Source::from_syntax(&tree).unwrap();
        assert!(matches!(
            source.modules()[0].initial_processes()[0].body()[1],
            crate::ast::Stmt::Loop {
                condition: None,
                ..
            }
        ));
        tree.declarations = previous;
        let mut native = tree.declarations.clone();
        let StatementKind::Loop {
            kind: celox_sv_syntax::LoopSyntax::For(header),
            ..
        } = &mut native
            .statements
            .iter_mut()
            .find(|statement| matches!(statement.kind, StatementKind::Loop { .. }))
            .unwrap()
            .kind
        else {
            unreachable!()
        };
        header.steps.clear();
        tree.declarations = native;
        let source = crate::ast::Source::from_syntax(&tree).unwrap();
        let crate::ast::Stmt::Loop { step, .. } =
            &source.modules()[0].initial_processes()[0].body()[1]
        else {
            panic!()
        };
        assert!(step.is_empty());
    }

    #[test]
    fn native_empty_statement_class_is_authoritative() {
        let mut tree = super::super::parse_source(
            "module M(input logic a, output logic y); always_comb y=~a; endmodule",
            Path::new("missing-statement.sv"),
        )
        .unwrap();
        let root = tree.declarations.procedures[0].body;
        let tokens = tree.declarations.statement(root).tokens;
        tree.declarations
            .statements
            .iter_mut()
            .find(|s| s.tokens == tokens)
            .unwrap()
            .kind = StatementKind::Empty;
        let source = crate::ast::Source::from_syntax(&tree).unwrap();
        assert!(source.modules()[0].comb_processes()[0].body().is_empty());
    }

    #[test]
    fn native_assignment_operator_binds_and_timing_or_branch_disagreements_are_errors() {
        let mut tree = super::super::parse_source(
            "module M(input logic a,b, output logic y); always_comb if(a) y=b; else y=a; endmodule",
            Path::new("statement-join.sv"),
        )
        .unwrap();
        let mut native = tree.declarations.clone();
        let StatementKind::Assignment { operator, .. } = &mut native
            .statements
            .iter_mut()
            .find(|statement| matches!(statement.kind, StatementKind::Assignment { .. }))
            .unwrap()
            .kind
        else {
            unreachable!()
        };
        *operator = "+=".into();
        let previous = std::mem::replace(&mut tree.declarations, native);
        let source = crate::ast::Source::from_syntax(&tree).unwrap();
        let crate::ast::Stmt::If { then_body, .. } =
            &source.modules()[0].comb_processes()[0].body()[0]
        else {
            panic!();
        };
        assert!(matches!(
            &then_body[0],
            crate::ast::Stmt::Assign {
                rhs: crate::ast::Expr::Binary {
                    op: crate::ast::BinaryOp::Add,
                    ..
                },
                ..
            }
        ));
        tree.declarations = previous;
        let mut native = tree.declarations.clone();
        let StatementKind::Assignment { timing, .. } = &mut native
            .statements
            .iter_mut()
            .find(|statement| matches!(statement.kind, StatementKind::Assignment { .. }))
            .unwrap()
            .kind
        else {
            unreachable!()
        };
        *timing = Some(Default::default());
        rejects_body(&mut tree, native);
        let mut native = tree.declarations.clone();
        let StatementKind::Conditional { otherwise, .. } = &mut native
            .statements
            .iter_mut()
            .find(|statement| matches!(statement.kind, StatementKind::Conditional { .. }))
            .unwrap()
            .kind
        else {
            unreachable!()
        };
        *otherwise = None;
        tree.declarations = native;
        let source = crate::ast::Source::from_syntax(&tree).unwrap();
        let crate::ast::Stmt::If { else_body, .. } =
            &source.modules()[0].comb_processes()[0].body()[0]
        else {
            panic!()
        };
        assert!(else_body.is_empty());
    }
}

#[test]
fn native_body_assignment_mode_controls_initial_process_lowering() {
    let mut tree = parse_source(
        "module M(output logic y);initial y=1;endmodule",
        Path::new("native-assignment-mode.sv"),
    )
    .unwrap();
    for statement in &mut tree.declarations.statements {
        if let StatementKind::Assignment {
            operator,
            nonblocking,
            ..
        } = &mut statement.kind
        {
            *operator = "<=".into();
            *nonblocking = true;
        }
    }
    let source = crate::ast::Source::from_syntax(&tree).unwrap();
    assert!(matches!(
        source.modules()[0].initial_processes()[0].body()[0],
        crate::ast::Stmt::Assign {
            nonblocking: true,
            ..
        }
    ));
}

#[test]
fn native_body_membership_rejects_repeated_or_cyclic_children() {
    for cycle in [false, true] {
        let mut tree = parse_source(
            "module M(output logic y);initial begin y=1;end endmodule",
            Path::new("native-body-membership.sv"),
        )
        .unwrap();
        let root = tree.declarations.procedures[0].body;
        let tokens = tree.declarations.statement(root).tokens;
        let StatementKind::Block { items, .. } = &mut tree
            .declarations
            .statements
            .iter_mut()
            .find(|s| s.tokens == tokens)
            .unwrap()
            .kind
        else {
            unreachable!()
        };
        items.push(if cycle { root } else { items[0] });
        assert!(matches!(
            crate::ast::Source::from_syntax(&tree),
            Err(AnalyzerError::Parse(_))
        ));
    }
}
