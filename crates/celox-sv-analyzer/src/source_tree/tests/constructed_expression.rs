use super::*;
use crate::ast::{BinaryOp, ConstExpr, Expr};
#[test]
fn native_cast_width_operand_and_replication_values_drive_production() {
    let source = "module M(input logic[7:0] a,b,output logic[15:0] y,z);assign y=(8+2)'(a+b),z={(4+2){a+b}};endmodule";
    let mut tree = parse_source(source, Path::new("native-constructed-values.sv")).unwrap();
    for expression in &mut tree.declarations.expressions {
        if let celox_sv_syntax::ExprKind::Binary { operator, .. } = &mut expression.kind
            && operator == "+"
        {
            *operator = "-".into();
        }
    }
    let source = crate::ast::Source::from_syntax(&tree).unwrap();
    let assignments = source.modules()[0].assignments();
    let Expr::Resize { expr, width, .. } = assignments[0].rhs() else {
        panic!("{:?}", assignments[0].rhs())
    };
    assert_eq!(*width, 6);
    assert!(matches!(
        &**expr,
        Expr::Binary {
            op: BinaryOp::Sub,
            ..
        }
    ));
    let Expr::RepeatConcat { count, parts } = assignments[1].rhs() else {
        panic!()
    };
    assert!(matches!(
        count,
        ConstExpr::Binary {
            op: BinaryOp::Sub,
            ..
        }
    ));
    assert!(matches!(
        &parts[0],
        Expr::Binary {
            op: BinaryOp::Sub,
            ..
        }
    ));
}
#[test]
fn native_cast_target_name_disagreement_is_a_diagnostic() {
    let mut tree = parse_source(
        "module M(input logic a,output logic[7:0] y);typedef bit[7:0] T;assign y=T'(a);endmodule",
        Path::new("native-cast-target-guard.sv"),
    )
    .unwrap();
    for expression in &mut tree.declarations.expressions {
        if let celox_sv_syntax::ExprKind::Name(name) = &mut expression.kind
            && name.spelling == "T"
        {
            name.spelling = "Other".into();
        }
    }
    assert!((crate::ast::Source::from_syntax(&tree)).is_err());
}
