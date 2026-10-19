//! Observable native numeric-call operand ownership and compatibility.
use super::*;
use crate::ast::{BinaryOp, Expr, Source, SystemFunction};
use celox_sv_syntax::{Declarations, ExprKind};
fn subtract(tree: &mut ValidatedSyntax) {
    for expression in &mut tree.declarations.expressions {
        if let ExprKind::Binary { operator, .. } = &mut expression.kind
            && operator == "+"
        {
            *operator = "-".into();
        }
    }
}
#[test]
fn native_numeric_system_actuals_drive_runtime_expression_operands() {
    for name in [
        "$signed",
        "$unsigned",
        "$clog2",
        "$countones",
        "$onehot",
        "$onehot0",
        "$isunknown",
        "$countbits",
    ] {
        let args = if name == "$countbits" {
            "a+1,1'b1"
        } else {
            "a+1"
        };
        let mut tree = parse_source(
            &format!(
                "module M(input logic[7:0]a,output logic[31:0]y);assign y={name}({args});endmodule"
            ),
            Path::new("native-numeric-runtime.sv"),
        )
        .unwrap();
        subtract(&mut tree);
        let source = Source::from_syntax(&tree).unwrap();
        let value = source.modules()[0].assignments()[0].rhs();
        let argument = match value {
            Expr::Resize { expr, signed, .. } => {
                assert_eq!(*signed, name == "$signed");
                expr.as_ref()
            }
            Expr::Call {
                callee: crate::ast::Callee::System(function),
                args,
                ..
            } => {
                assert_eq!(Some(*function), SystemFunction::from_name(name));
                args[0].as_ref().unwrap()
            }
            _ => panic!("{name}: {value:?}"),
        };
        assert!(
            matches!(
                argument,
                Expr::Binary {
                    op: BinaryOp::Sub,
                    ..
                }
            ),
            "{name}: {argument:?}"
        );
        crate::analyze::analyze_source(source).unwrap();
    }
}
#[test]
fn native_numeric_system_actuals_drive_constant_parameter_values() {
    for (name, args, expected) in [
        ("$signed", "8+2", 6),
        ("$unsigned", "8+2", 6),
        ("$clog2", "8 + (* note=1 *) 2", 3),
        ("$countones", "8+1", 3),
        ("$onehot", "2+2", 0),
        ("$onehot0", "3+2", 1),
        ("$isunknown", "8+2", 0),
        ("$countbits", "8+1,1'b1", 3),
    ] {
        let mut tree = parse_source(
            &format!("module M;parameter N={name}({args});endmodule"),
            Path::new("native-numeric-constant.sv"),
        )
        .unwrap();
        subtract(&mut tree);
        let source = Source::from_syntax(&tree).unwrap();
        let ir = crate::analyze::analyze_source(source).unwrap();
        assert_eq!(
            ir.modules()[0].parameters()[0].resolved_value(),
            Some(expected),
            "{name}"
        );
    }
}
#[test]
fn native_nested_numeric_constants_retain_symbolic_override_dependencies() {
    let mut tree=parse_source("module M#(parameter int W=8,parameter int N=$clog2($unsigned(W+2)))(output logic[N:0]y);assign y='0;endmodule",Path::new("native-numeric-symbolic.sv")).unwrap();
    subtract(&mut tree);
    for (override_, expected) in [(None, 3), (Some(64), 6)] {
        let overrides = override_
            .map(|value| HashMap::from_iter([("W".into(), value)]))
            .unwrap_or_default();
        let source =
            Source::from_syntax_with_module_parameter_overrides(&tree, "M", &overrides).unwrap();
        let ir = crate::analyze::analyze_source(source).unwrap();
        assert_eq!(
            ir.modules()[0].parameters()[1].resolved_value(),
            Some(expected)
        );
        assert_eq!(
            ir.modules()[0].ports()[0].r#type().resolved_width(),
            Some(expected as usize + 1)
        );
    }
}
#[test]
fn native_numeric_call_identity_and_argument_shape_errors_do_not_retry() {
    for name in [false, true] {
        let mut tree = parse_source(
            "module M;parameter N=$clog2(8);endmodule",
            Path::new("native-numeric-identity.sv"),
        )
        .unwrap();
        if name {
            let range = tree
                .declarations
                .expressions
                .iter()
                .find_map(|value| match value.kind {
                    ExprKind::Call { expression, .. } => {
                        Some(tree.declarations().expression(expression).tokens)
                    }
                    _ => None,
                })
                .unwrap();
            let value = tree
                .declarations
                .expressions
                .iter_mut()
                .find(|value| value.tokens == range)
                .unwrap();
            let ExprKind::Name(name) = &mut value.kind else {
                unreachable!()
            };
            name.spelling = "$unknown".into();
        } else {
            for expression in &mut tree.declarations.expressions {
                if let ExprKind::Call { arguments, .. } = &mut expression.kind {
                    arguments.clear();
                }
            }
        }
        assert!((Source::from_syntax(&tree)).is_err());
    }
}
#[test]
fn native_numeric_call_catalog_and_typed_operand_diagnostics_are_preserved() {
    for call in ["$clog2()", "$clog2(1,2)", "$countbits(1)", "$signed(,)"] {
        let tree = parse_source(
            &format!("module M(output int y);assign y={call};endmodule"),
            Path::new("native-numeric-diagnostic.sv"),
        )
        .unwrap();
        assert!(
            matches!(
                Source::from_syntax(&tree),
                Err(AnalyzerError::InvalidSystemTfCall { .. })
            ),
            "{call}"
        );
    }
    for (call, detail) in [
        ("$onehot(.x(1))", "named arguments"),
        ("$clog2(logic)", "operand of"),
    ] {
        let tree = parse_source(
            &format!("module M(output int y);assign y={call};endmodule"),
            Path::new("native-numeric-typed-diagnostic.sv"),
        )
        .unwrap();
        assert!(
            matches!(Source::from_syntax(&tree),Err(AnalyzerError::Unsupported(message)) if message.contains(detail)),
            "{call}"
        );
    }
}
#[test]
fn deep_native_numeric_actual_groups_bind_on_the_default_stack() {
    let path = Path::new("deep-native-numeric.sv");
    let mut tree = parse_source("module M;parameter N=$clog2((8+2));endmodule", path).unwrap();
    *tree.declarations = Declarations::parse(
        &format!(
            "module M;parameter N=$clog2({}8+2{});endmodule",
            "(".repeat(10_000),
            ")".repeat(10_000)
        ),
        path,
    )
    .unwrap();
    subtract(&mut tree);
    let source = Source::from_syntax(&tree).unwrap();
    let ir = crate::analyze::analyze_source(source).unwrap();
    assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(3));
}
