//! Runtime query ownership/context regressions, independently of constant proofs.
use super::*;
use crate::ast::{Expr, Source};
use celox_sv_syntax::ExprKind;
fn value(tree: &SourceTree) -> i128 {
    let source = Source::from_syntax(tree).unwrap();
    let Expr::Literal(value) = source.modules()[0].assignments()[0].rhs() else {
        panic!("{:?}", source.modules()[0].assignments()[0].rhs())
    };
    value.parse().unwrap()
}
fn reduce(tree: &mut SourceTree) {
    for value in &mut tree.declarations.expressions {
        if let ExprKind::Unary { operator, .. } = &mut value.kind
            && operator == "+"
        {
            *operator = "&".into();
        }
    }
}
#[test]
fn native_runtime_bits_operands_own_operator_and_numeric_subtrees() {
    for expression in [
        "$bits(+a)",
        "$bits($unsigned(+a))",
        "$bits({+a,1'b1})",
        "$bits(1?(+a):1'b0)",
    ] {
        let mut tree = parse_source(
            &format!("module M(input logic[7:0]a,output int y);assign y={expression};endmodule"),
            Path::new("native-runtime-bits.sv"),
        )
        .unwrap();
        reduce(&mut tree);
        assert_eq!(
            value(&tree),
            if expression.contains('{') { 2 } else { 1 },
            "{expression}"
        );
    }
}
#[test]
fn native_runtime_bits_nested_queries_keep_expression_result_types() {
    let mut tree = parse_source(
        "module M(input logic[7:0]a,output int y);assign y=$bits($bits($unsigned(+a)));endmodule",
        Path::new("native-runtime-bits-nested.sv"),
    )
    .unwrap();
    reduce(&mut tree);
    assert_eq!(value(&tree), 32);
}
#[test]
fn native_runtime_bits_function_type_queries_do_not_inline_effectful_bodies() {
    let tree=parse_source("module M(output int y);int counter=0;function automatic logic[4:0]f(input int x);counter++;return x;endfunction assign y=$bits(f(7));endmodule",Path::new("native-runtime-bits-call.sv")).unwrap();
    assert_eq!(value(&tree), 5);
    let source = Source::from_syntax(&tree).unwrap();
    assert_eq!(source.modules()[0].assignments().len(), 1);
}
#[test]
fn native_runtime_bits_type_aliases_and_explicit_datatype_owner_stay_supported() {
    for (expression, expected) in [("$bits((T))", 7), ("$bits(int)", 32)] {
        let tree = parse_source(
            &format!("module M(output int y);typedef logic[6:0]T;assign y={expression};endmodule"),
            Path::new("native-runtime-bits-types.sv"),
        )
        .unwrap();
        assert_eq!(value(&tree), expected);
    }
}
#[test]
fn native_runtime_query_callee_is_authoritative_and_missing_arguments_reject() {
    for argument in [false, true] {
        let mut tree = parse_source(
            "module M(input logic[7:0]a,output int y);assign y=$bits(a);endmodule",
            Path::new("native-runtime-bits-guard.sv"),
        )
        .unwrap();
        for value in &mut tree.declarations.expressions {
            match &mut value.kind {
                ExprKind::Name(name) if !argument && name.spelling == "$bits" => {
                    name.spelling = "$signed".into();
                    break;
                }
                ExprKind::Call { arguments, .. } if argument => {
                    arguments[0].value = None;
                    break;
                }
                _ => {}
            }
        }
        if argument {
            assert!(Source::from_syntax(&tree).is_err());
        } else {
            let source = Source::from_syntax(&tree).unwrap();
            assert!(matches!(
                source.modules()[0].assignments()[0].rhs(),
                Expr::Resize {
                    width: 8,
                    signed: true,
                    ..
                }
            ));
        }
    }
}
#[test]
fn native_runtime_bits_unbounded_parameter_queries_keep_their_diagnostic() {
    let tree = parse_source(
        "module M#(parameter P=$)(output int y);assign y=$bits(P);endmodule",
        Path::new("native-runtime-bits-unbounded.sv"),
    )
    .unwrap();
    let error = Source::from_syntax(&tree).unwrap_err();
    assert!(error.to_string().contains("unbounded"), "{error}");
}

#[test]
fn native_runtime_bits_query_context_checks_output_actual_writability() {
    let tree=parse_source("module M(output int y);function automatic logic[4:0]f(output int x);x=0;return 0;endfunction assign y=$bits(f(7));endmodule",Path::new("native-runtime-bits-output.sv")).unwrap();
    let error = Source::from_syntax(&tree).unwrap_err();
    assert!(
        error.to_string().contains("non-writable argument 1 of `f`"),
        "{error}"
    );
}
