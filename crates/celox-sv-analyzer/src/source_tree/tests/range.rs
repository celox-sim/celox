use super::*;
use crate::ast::{ConstExpr, Expr, LValue, Stmt};
fn bound(value: &ConstExpr) -> i128 {
    let value: crate::ir::ConstExpr = value.clone().into();
    crate::typecheck::eval_const_expr(&value, &HashMap::from_iter([("i".into(), 8)])).unwrap()
}
#[test]
fn native_range_bounds_width_start_and_prefix_indices_drive_production() {
    let source = "module M(input logic[31:0] a,input logic[1:0][7:0] b,input logic[3:0] i,output logic[31:0] y,z,w,q);assign y=a[(10+2):(6+2)],z=a[(8+2)+:(4+2)],w=a[(i+2)+:(4+2)],q=b[1][(5+2):(3+2)];endmodule";
    let mut tree = parse_source(source, Path::new("native-range-values.sv")).unwrap();
    for expression in &mut tree.declarations.expressions {
        if let celox_sv_syntax::ExprKind::Binary { operator, .. } = &mut expression.kind
            && operator == "+"
        {
            *operator = "-".into()
        }
    }
    let source = crate::ast::Source::from_syntax(&tree).unwrap();
    for (assignment, (high, low)) in
        source.modules()[0]
            .assignments()
            .iter()
            .zip([(8, 4), (7, 6), (7, 6), (11, 9)])
    {
        let Expr::Select {
            msb, lsb, signed, ..
        } = assignment.rhs()
        else {
            panic!("{assignment:?}")
        };
        assert_eq!(bound(msb), high);
        assert_eq!(bound(lsb), low);
        assert!(!signed);
    }
}
#[test]
fn native_assignment_range_bounds_start_width_and_prefix_indices_drive_production() {
    use crate::ast::{LValue, Stmt};
    for (prefix, start) in [("assign", "8"), ("initial", "i")] {
        for (target, expected) in [
            ("y[(10+2):(6+2)]".to_owned(), (8, 4)),
            (format!("y[({start}+2)+:(4+2)]"), (7, 6)),
            ("b[1][(5+2):(3+2)]".to_owned(), (11, 9)),
        ] {
            let source = format!(
                "module M(input logic[3:0] i,output logic[31:0] y,output logic[1:0][7:0] b);{prefix} {target}=0;endmodule"
            );
            let mut tree = parse_source(&source, Path::new("native-target-range.sv")).unwrap();
            for expression in &mut tree.declarations.expressions {
                if let celox_sv_syntax::ExprKind::Binary { operator, .. } = &mut expression.kind
                    && operator == "+"
                {
                    *operator = "-".into()
                }
            }
            let source = crate::ast::Source::from_syntax(&tree).unwrap();
            let module = &source.modules()[0];
            let lhs = if prefix == "assign" {
                module.assignments()[0].lhs_value()
            } else {
                let Stmt::Assign { lhs, .. } = &module.initial_processes()[0].body()[0] else {
                    panic!()
                };
                lhs
            };
            let LValue::Select {
                msb, lsb, signed, ..
            } = lhs
            else {
                panic!("{lhs:?}")
            };
            assert_eq!((bound(msb), bound(lsb)), expected, "{prefix} {target}");
            assert!(!signed);
        }
    }
}
#[test]
fn native_assignment_range_width_errors_and_operator_are_authoritative() {
    for prefix in ["assign", "initial"] {
        let source = format!("module M(output logic[7:0] y);{prefix} y[1+:(1+1)]=0;endmodule");
        let mut tree = parse_source(&source, Path::new("native-target-range-error.sv")).unwrap();
        for expression in &mut tree.declarations.expressions {
            if let celox_sv_syntax::ExprKind::Binary { operator, .. } = &mut expression.kind
                && operator == "+"
            {
                *operator = "-".into()
            }
        }
        let error = crate::ast::Source::from_syntax(&tree).unwrap_err();
        assert!(error.to_string().contains("width 0"), "{error}");
        let mut tree = parse_source(&source, Path::new("native-target-range-error.sv")).unwrap();
        for expression in &mut tree.declarations.expressions {
            if let celox_sv_syntax::ExprKind::Select { dimension, .. } = &mut expression.kind
                && let celox_sv_syntax::DimensionKind::Indexed { operator, .. } =
                    &mut dimension.kind
            {
                *operator = "-:".into()
            }
        }
        let source = crate::ast::Source::from_syntax(&tree).unwrap();
        let module = &source.modules()[0];
        let lhs = if prefix == "assign" {
            module.assignments()[0].lhs_value()
        } else {
            let Stmt::Assign { lhs, .. } = &module.initial_processes()[0].body()[0] else {
                panic!()
            };
            lhs
        };
        let LValue::Select { msb, lsb, .. } = lhs else {
            panic!()
        };
        assert_eq!((bound(msb), bound(lsb)), (1, 0));
    }
}
#[test]
fn native_indexed_range_width_and_shape_errors_remain_explicit() {
    for width in ["0", "-1", "n"] {
        let source = format!(
            "module M(input logic[7:0] a,input logic[2:0] i,input int n,output logic[7:0] y);assign y=a[i+:{width}];endmodule"
        );
        let error =
            crate::analyze_source(&source, Path::new("native-range-errors.sv")).unwrap_err();
        assert!(
            error.to_string().contains("indexed part-select width"),
            "{error}"
        );
    }
    for (width, message) in [("1+1", "width 0"), ("1+2", "width -1")] {
        let source = format!(
            "module M(input logic[7:0] a,output logic[7:0] y);assign y=a[1+:({width})];endmodule"
        );
        let mut tree = parse_source(&source, Path::new("native-width-error.sv")).unwrap();
        for expression in &mut tree.declarations.expressions {
            if let celox_sv_syntax::ExprKind::Binary { operator, .. } = &mut expression.kind
                && operator == "+"
            {
                *operator = "-".into()
            }
        }
        let error = crate::ast::Source::from_syntax(&tree).unwrap_err();
        assert!(error.to_string().contains(message), "{error}");
    }
    let mut tree = parse_source(
        "module M(input logic[7:0] a,output logic[7:0] y);assign y=a[1+:2];endmodule",
        Path::new("native-range-shape.sv"),
    )
    .unwrap();
    for expression in &mut tree.declarations.expressions {
        if let celox_sv_syntax::ExprKind::Select { dimension, .. } = &mut expression.kind
            && let celox_sv_syntax::DimensionKind::Indexed { operator, .. } = &mut dimension.kind
        {
            *operator = "-:".into()
        }
    }
    let source = crate::ast::Source::from_syntax(&tree).unwrap();
    let crate::ast::Expr::Select { msb, lsb, .. } = source.modules()[0].assignments()[0].rhs()
    else {
        panic!()
    };
    assert_eq!(
        (msb, lsb),
        (
            &crate::ast::ConstExpr::Literal("1".into()),
            &crate::ast::ConstExpr::Literal("0".into())
        )
    );
}
