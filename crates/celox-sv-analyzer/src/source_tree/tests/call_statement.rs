use super::*;
use crate::ast::{BinaryOp, Expr, Stmt};

#[test]
fn native_statement_callees_arguments_and_discarded_values_reach_production() {
    for (declaration, invocation) in [
        (
            "task automatic drive(input int left=1,right=2);y=left+right;endtask",
            "drive(.right(a+1),.left(a*2))",
        ),
        (
            "function automatic int drive(input int left=1,right=2);return left+right;endfunction",
            "void'(drive(.right(a+1),.left(a*2)))",
        ),
    ] {
        let source = format!(
            "module M(input int a,output int y);{declaration} initial {invocation};endmodule"
        );
        let mut tree = parse_source(&source, Path::new("native-call-statement.sv")).unwrap();
        for expression in &mut tree.declarations.expressions {
            if let celox_sv_syntax::ExprKind::Binary { operator, .. } = &mut expression.kind
                && operator == "*"
            {
                *operator = "-".into();
            }
        }
        let source = crate::ast::Source::from_syntax(&tree).unwrap();
        let Stmt::Call {
            name,
            args,
            arg_order,
        } = &source.modules()[0].initial_processes()[0].body()[0]
        else {
            panic!()
        };
        assert_eq!(name, "drive");
        assert_eq!(arg_order, &[1, 0]);
        assert!(matches!(
            &args[0],
            Some(Expr::Binary {
                op: BinaryOp::Sub,
                ..
            })
        ));
        assert!(matches!(
            &args[1],
            Some(Expr::Binary {
                op: BinaryOp::Add,
                ..
            })
        ));
    }
}
#[test]
fn native_bare_empty_and_omitted_statement_calls_keep_binding_contract() {
    for (call, missing, order) in [
        ("drive", vec![], vec![]),
        ("drive()", vec![], vec![]),
        ("drive(1,)", vec![false, true], vec![]),
        ("drive(,.right())", vec![true, true], vec![1, 0]),
    ] {
        let source = format!(
            "module M;task drive(input int left=1,right=2);endtask initial {call};endmodule"
        );
        let tree = parse_source(&source, Path::new("native-call-omissions.sv")).unwrap();
        let source = crate::ast::Source::from_syntax(&tree).unwrap();
        let Stmt::Call {
            args, arg_order, ..
        } = &source.modules()[0].initial_processes()[0].body()[0]
        else {
            panic!()
        };
        assert_eq!(
            args.iter().map(Option::is_none).collect::<Vec<_>>(),
            missing,
            "{call}"
        );
        assert_eq!(arg_order, &order, "{call}");
    }
}
#[test]
fn native_statement_name_and_formal_disagreements_do_not_retry() {
    for formal in [false, true] {
        let mut tree = parse_source(
            "module M;task drive(input int left);endtask initial drive(.left(1));endmodule",
            Path::new("native-call-statement-guard.sv"),
        )
        .unwrap();
        for expression in &mut tree.declarations.expressions {
            match &mut expression.kind {
                celox_sv_syntax::ExprKind::Name(name) if !formal && name.spelling == "drive" => {
                    name.spelling = "other".into()
                }
                celox_sv_syntax::ExprKind::Call { arguments, .. } if formal => {
                    arguments[0].name.as_mut().unwrap().spelling = "other".into()
                }
                _ => {}
            }
        }
        assert!((crate::ast::Source::from_syntax(&tree)).is_err());
    }
}
#[test]
fn native_discard_wrapper_type_disagreement_is_a_diagnostic() {
    let mut tree = parse_source(
        "module M;function int f;return 1;endfunction initial void'(f());endmodule",
        Path::new("native-discard-guard.sv"),
    )
    .unwrap();
    for data_type in &mut tree.declarations.types {
        if let celox_sv_syntax::DataTypeKind::Builtin(name) = &mut data_type.kind
            && name == "void"
        {
            *name = "int".into()
        }
    }
    assert!((crate::ast::Source::from_syntax(&tree)).is_err());
}
#[test]
fn native_statement_callee_class_disagreement_is_a_diagnostic() {
    let mut tree = parse_source(
        "module M;task drive;endtask initial drive();endmodule",
        Path::new("native-statement-callee-class.sv"),
    )
    .unwrap();
    for expression in &mut tree.declarations.expressions {
        if matches!(&expression.kind, celox_sv_syntax::ExprKind::Name(name) if name.spelling == "drive")
        {
            expression.kind = celox_sv_syntax::ExprKind::Literal("7".into());
        }
    }
    assert!((crate::ast::Source::from_syntax(&tree)).is_err());
}
