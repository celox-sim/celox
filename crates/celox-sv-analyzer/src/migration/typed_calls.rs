//! Typed initializer calls consume native actual identities and value structure.
use super::*;
use crate::ast::Source;
use celox_sv_syntax::{Declarations, DimensionKind, ExprKind};
fn subtract(tree: &mut ValidatedSyntax) {
    for value in &mut tree.declarations.expressions {
        if let ExprKind::Binary { operator, .. } = &mut value.kind
            && operator == "+"
        {
            *operator = "-".into();
        }
    }
}
fn number(source: Source) -> Option<i128> {
    let ir = crate::analyze::analyze_source(source).unwrap();
    ir.modules()[0]
        .parameters()
        .iter()
        .find(|value| value.name() == "N")
        .unwrap()
        .resolved_value()
}
const PREFIX: &str = "module M;function automatic int f(input int x=7);return x;endfunction parameter logic[1:0][3:0]A=8'h42;";
#[test]
fn native_typed_call_actual_indices_operators_and_conditionals_drive_values() {
    for (argument, expected) in [
        ("(8+2)-A[1+1][1+0]", 5),
        ("1?A[1+1][1+0]:0", 1),
        ("int'(A[1+1][1+0])", 1),
        ("$unsigned(A[1+1][1+0])", 1),
    ] {
        let mut tree = parse_source(
            &format!("{PREFIX}parameter int N=$unsigned({argument});endmodule"),
            Path::new("native-typed-call.sv"),
        )
        .unwrap();
        subtract(&mut tree);
        assert_eq!(
            number(Source::from_syntax(&tree).unwrap()),
            Some(expected),
            "{argument}"
        );
    }
}
#[test]
fn native_typed_numeric_system_actuals_consume_declared_indices() {
    for (call, expected) in [
        ("$countones(A[1+1][1+0])", 1),
        ("$unsigned(A[1+1][1+0])", 1),
        ("$clog2(A[1+1][1+0]+(8+2))", 32),
        ("$onehot(A[1+1][1+0])", 1),
    ] {
        let mut tree = parse_source(
            &format!("{PREFIX}parameter int N={call};endmodule"),
            Path::new("native-typed-system.sv"),
        )
        .unwrap();
        subtract(&mut tree);
        assert_eq!(
            number(Source::from_syntax(&tree).unwrap()),
            Some(expected),
            "{call}"
        );
    }
}
#[test]
fn native_typed_calls_specialize_actual_indices_after_overrides() {
    let mut tree=parse_source("module M#(parameter int I=1)(output logic[N:0]y);function automatic int f(input int x);return x;endfunction parameter logic[1:0][3:0]A=8'h42;parameter int N=$unsigned(A[I+1][1+0]);assign y=0;endmodule",Path::new("native-typed-call-override.sv")).unwrap();
    subtract(&mut tree);
    for (i, expected) in [(1, 1), (2, 0)] {
        let source = Source::from_syntax_with_module_parameter_overrides(
            &tree,
            "M",
            &HashMap::from_iter([("I".into(), i)]),
        )
        .unwrap();
        let ir = crate::analyze::analyze_source(source).unwrap();
        assert_eq!(
            ir.modules()[0]
                .parameters()
                .iter()
                .find(|v| v.name() == "N")
                .unwrap()
                .resolved_value(),
            Some(expected)
        );
        assert_eq!(
            ir.modules()[0].ports()[0].r#type().resolved_width(),
            Some(expected as usize + 1)
        );
    }
}
#[test]
fn native_typed_call_signedness_is_authoritative_and_invalid_actuals_reject() {
    for change in 0..3 {
        let mut tree = parse_source(
            &format!("{PREFIX}parameter int N=$unsigned(A[0][1]);endmodule"),
            Path::new("native-typed-call-guard.sv"),
        )
        .unwrap();
        for value in &mut tree.declarations.expressions {
            match &mut value.kind {
                ExprKind::Name(name) if change == 0 && name.spelling == "$unsigned" => {
                    name.spelling = "$signed".into();
                    break;
                }
                ExprKind::Call { arguments, .. } if change == 1 => {
                    arguments[0].value = None;
                    break;
                }
                ExprKind::Select { dimension, .. } if change == 2 => {
                    let DimensionKind::Index(index) = dimension.kind else {
                        continue;
                    };
                    dimension.kind = DimensionKind::Range {
                        left: index,
                        right: index,
                    };
                    break;
                }
                _ => {}
            }
        }
        let result = Source::from_syntax(&tree);
        if change == 0 {
            assert_eq!(number(result.unwrap()), Some(-1));
        } else {
            assert!(result.is_err());
        }
    }
}
#[test]
fn deep_native_typed_call_actual_groups_use_the_shared_worklist() {
    let path = Path::new("deep-native-typed-call.sv");
    let mut tree = parse_source(
        &format!("{PREFIX}parameter int N=$unsigned(A[0][(2+1)]);endmodule"),
        path,
    )
    .unwrap();
    let open = "(".repeat(10_000);
    let close = ")".repeat(10_000);
    *tree.declarations = Declarations::parse(
        &format!(
            "{PREFIX}parameter int N=$unsigned({open}A[0][{open}2+1{close}]{close});endmodule"
        ),
        path,
    )
    .unwrap();
    subtract(&mut tree);
    assert_eq!(number(Source::from_syntax(&tree).unwrap()), Some(1));
}
