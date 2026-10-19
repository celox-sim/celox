use super::*;
use crate::ast::{BinaryOp, ConstExpr, Expr, LValue, Source, Stmt};

#[test]
fn native_continuous_assignment_lists_own_order_and_operand_values() {
    let mut tree = parse_source(
        "module M(input int x,output wire[31:0] a,b);assign a=x+1,b=x+2;endmodule",
        Path::new("native-assign-list-order.sv"),
    )
    .unwrap();
    tree.declarations.assign_statements[0]
        .assignments
        .swap(0, 1);
    native_subtraction(&mut tree);
    let source = Source::from_syntax(&tree).unwrap();
    let assignments = source.modules()[0].assignments();
    assert_eq!(
        assignments.iter().map(|a| a.lhs()).collect::<Vec<_>>(),
        ["b", "a"]
    );
    assert!(assignments.iter().all(|a| matches!(
        a.rhs(),
        Expr::Binary {
            op: BinaryOp::Sub,
            ..
        }
    )));
}

#[test]
fn native_continuous_assignment_identity_and_operand_coverage_are_required() {
    for defect in [
        "missing",
        "different-class",
        "missing-operand",
        "duplicate-range",
    ] {
        let mut tree = parse_source(
            "module M(input int x,output wire[31:0] a,b);assign a=x+1,b=x+2;initial;endmodule",
            Path::new("native-assign-list-coverage.sv"),
        )
        .unwrap();
        match defect {
            "missing" => {
                for item in &mut tree.declarations.body_items {
                    if matches!(item.kind, celox_sv_syntax::BodyItemKind::Assign(_)) {
                        item.kind = celox_sv_syntax::BodyItemKind::Unconverted {
                            category: "missing assign".into(),
                            items: Vec::new(),
                        };
                    }
                }
            }
            "different-class" => {
                let assign = tree
                    .declarations
                    .body_items
                    .iter()
                    .find_map(|i| match i.kind {
                        celox_sv_syntax::BodyItemKind::Assign(id) => Some(id),
                        _ => None,
                    })
                    .unwrap();
                for item in &mut tree.declarations.body_items {
                    if matches!(item.kind, celox_sv_syntax::BodyItemKind::Procedure(_)) {
                        item.kind = celox_sv_syntax::BodyItemKind::Assign(assign);
                    }
                }
            }
            "missing-operand" => {
                tree.declarations.assign_statements[0].assignments.pop();
            }
            _ => {
                let first = tree.declarations.assign_statements[0].assignments[0].tokens;
                tree.declarations.assign_statements[0].assignments[1].tokens = first;
            }
        }
        let result = Source::from_syntax(&tree);
        if defect == "missing" {
            assert!(result.unwrap().modules()[0].assignments().is_empty());
        } else if defect == "missing-operand" {
            assert_eq!(result.unwrap().modules()[0].assignments().len(), 1);
        } else {
            assert!(result.is_err(), "{defect}");
        }
    }
}

#[test]
fn native_continuous_assignment_ids_preserve_selected_generate_and_loop_scopes() {
    let tree = parse_source(
        "module M(output wire[3:0] y);if(0)begin:unused assign #1 y=0;end else begin:used for(genvar i=0;i<4;i++)begin:bit_driver assign y[i]=i;end end endmodule",
        Path::new("native-assign-active-scopes.sv"),
    ).unwrap();
    let source = Source::from_syntax(&tree).unwrap();
    let assignments = source.modules()[0].assignments();
    assert_eq!(assignments.len(), 4);
    for (i, assignment) in assignments.iter().enumerate() {
        assert!(
            matches!(assignment.lhs_value(),LValue::Select{name,msb:ConstExpr::Literal(index),lsb:ConstExpr::Literal(last),..} if name=="y" && crate::typecheck::eval_const_expr(&ConstExpr::Literal(index.clone()).into(), &HashMap::default()) == Some(i as i128) && index==last),
            "{assignment:?}"
        );
        assert_eq!(assignment.rhs(), &Expr::Literal(format!("32'sd{i}")));
    }
}

#[test]
fn native_continuous_assignment_delay_restrictions_preserve_captured_scopes() {
    for body in [
        "assign #1 y=0;",
        "if(1)begin:g assign #1 y=0;end",
        "checker C;assign #1 y=0;endchecker",
        "module N;assign #1 y=0;endmodule",
    ] {
        let code = format!("module M(output wire y);{body} endmodule");
        let tree = parse_source(&code, Path::new("native-assign-delay-validation.sv")).unwrap();
        let error =
            Source::from_syntax_module_with_parameter_overrides(&tree, "M", &HashMap::default())
                .unwrap_err();
        assert!(
            error.to_string().contains("delayed continuous assignment"),
            "{body}: {error}"
        );
    }
}

fn native_subtraction(tree: &mut SourceTree) {
    for expression in &mut tree.declarations.expressions {
        if let celox_sv_syntax::ExprKind::Binary { operator, .. } = &mut expression.kind
            && operator == "+"
        {
            *operator = "-".into();
        }
    }
}
fn assert_subtracted_index(lhs: &LValue) {
    let LValue::Select { msb, lsb, .. } = lhs else {
        panic!("{lhs:?}")
    };
    assert_eq!(msb, lsb);
    assert!(
        matches!(
            lsb,
            ConstExpr::Binary {
                op: BinaryOp::Sub,
                ..
            }
        ) || crate::typecheck::eval_const_expr(&lsb.clone().into(), &HashMap::default()) == Some(0),
        "{lsb:?}"
    );
}
#[test]
fn native_left_indices_drive_continuous_and_procedural_production_lowering() {
    for source in [
        "module M(input logic d,output wire[15:0] y);assign y[1+1]=d;endmodule",
        "module M(input logic[3:0] i,input logic d,output logic[15:0] y);initial y[i+1]=d;endmodule",
        "module M(input logic[3:0] i,output logic[15:0] y);initial y[i+1]++;endmodule",
    ] {
        let mut tree = parse_source(source, Path::new("native-target-ownership.sv")).unwrap();
        native_subtraction(&mut tree);
        let source = crate::ast::Source::from_syntax(&tree).unwrap();
        let module = &source.modules()[0];
        if let Some(assignment) = module.assignments().first() {
            assert_subtracted_index(assignment.lhs_value());
        } else {
            let Stmt::Assign { lhs, .. } = &module.initial_processes()[0].body()[0] else {
                panic!()
            };
            assert_subtracted_index(lhs);
        }
    }
}
#[test]
fn native_for_assignment_and_increment_targets_drive_production_lowering() {
    for step in ["y[i+1]+=1", "y[i+1]++"] {
        let source = format!(
            "module M(input logic[3:0] i,output logic[15:0] y);initial for(y[i+1]=0;y[i]<1;{step});endmodule"
        );
        let mut tree = parse_source(&source, Path::new("native-for-target.sv")).unwrap();
        native_subtraction(&mut tree);
        let source = crate::ast::Source::from_syntax(&tree).unwrap();
        let Stmt::Loop { init, step, .. } = &source.modules()[0].initial_processes()[0].body()[0]
        else {
            panic!()
        };
        for assignment in init.iter().chain(step) {
            let Stmt::Assign { lhs, .. } = assignment else {
                panic!("{assignment:?}")
            };
            assert_subtracted_index(lhs);
        }
    }
}
#[test]
fn native_assignment_target_names_are_authoritative() {
    for source in [
        "module M(input logic d,output wire[15:0] y);assign y[1]=d;endmodule",
        "module M(input logic d,output logic[15:0] y);initial y[1]=d;endmodule",
    ] {
        let mut tree = parse_source(source, Path::new("native-target-guard.sv")).unwrap();
        for expression in &mut tree.declarations.expressions {
            if let celox_sv_syntax::ExprKind::Name(name) = &mut expression.kind
                && name.spelling == "y"
            {
                name.spelling = "other".into();
            }
        }
        let source = crate::ast::Source::from_syntax(&tree).unwrap();
        let module = &source.modules()[0];
        let lhs = if source.modules()[0].assignments().is_empty() {
            let Stmt::Assign { lhs, .. } = &module.initial_processes()[0].body()[0] else {
                panic!()
            };
            lhs
        } else {
            module.assignments()[0].lhs_value()
        };
        assert!(matches!(lhs,LValue::Select{name,..} if name=="other"));
    }
}
#[test]
fn native_target_class_disagreement_never_selects_a_typed_leaf() {
    for source in [
        "module M(input logic d,output wire[15:0] y);assign y[1]=d;endmodule",
        "module M(input logic d,output logic[15:0] y);initial y[1]=d;endmodule",
    ] {
        let mut tree = parse_source(source, Path::new("native-target-class.sv")).unwrap();
        let left = if let Some(assign) = tree.declarations.assign_statements.first() {
            assign.assignments[0].left
        } else {
            tree.declarations
                .statements
                .iter()
                .find_map(|statement| match statement.kind {
                    celox_sv_syntax::StatementKind::Assignment { left, .. } => Some(left),
                    _ => None,
                })
                .unwrap()
        };
        let tokens = tree.declarations.expression(left).tokens;
        let index = tree
            .declarations
            .expressions
            .iter()
            .position(|expression| expression.tokens == tokens)
            .unwrap();
        tree.declarations.expressions[index].kind = celox_sv_syntax::ExprKind::Literal("7".into());
        assert!((crate::ast::Source::from_syntax(&tree)).is_err());
    }
}
