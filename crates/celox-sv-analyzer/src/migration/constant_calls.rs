//! Production ownership regressions for native constant-call actuals.
use super::*;
use crate::ast::Source;
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
fn native_constant_call_arguments_drive_parameters_ranges_and_generate() {
    let mut tree = parse_source("module M;function automatic int f(input int x);return x;endfunction parameter int N=f(f((8+2)));wire[f(10+2):0]a;assign a=0;if(f(1+1))begin:g wire z=0;end endmodule", Path::new("native-constant-calls.sv")).unwrap();
    subtract(&mut tree);
    let source = Source::from_syntax(&tree).unwrap();
    let module = &source.modules()[0];
    assert!(module.signals().iter().all(|signal| signal.name() != "g.z"));
    let ir = crate::analyze::analyze_source(source).unwrap();
    assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(6));
    let a = ir.modules()[0]
        .signals()
        .iter()
        .find(|signal| signal.name() == "a")
        .unwrap();
    assert_eq!(a.r#type().resolved_width(), Some(9));
}
#[test]
fn native_constant_named_omitted_and_default_calls_keep_parameter_binding() {
    let mut tree = parse_source("module M;function automatic int f(input int left=1,right=2);return left*10+right;endfunction parameter int N=f(.right(8+2),.left(4+1)),D=f(.right(8+2),.left()),E=f();endmodule",Path::new("native-constant-default-calls.sv")).unwrap();
    // Change actual operators only, keeping the function's arithmetic intact.
    let ids = tree
        .declarations
        .expressions
        .iter()
        .filter_map(|expression| match &expression.kind {
            ExprKind::Call { arguments, .. } => Some(
                arguments
                    .iter()
                    .filter_map(|argument| argument.value)
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        })
        .flatten()
        .collect::<Vec<_>>();
    for id in ids {
        let range = tree.declarations().expression(id).tokens;
        if let ExprKind::Binary { operator, .. } = &mut tree
            .declarations
            .expressions
            .iter_mut()
            .find(|expression| expression.tokens == range)
            .unwrap()
            .kind
        {
            *operator = "-".into();
        }
    }
    let source = Source::from_syntax(&tree).unwrap();
    let ir = crate::analyze::analyze_source(source).unwrap();
    assert_eq!(
        ir.modules()[0]
            .parameters()
            .iter()
            .map(|parameter| parameter.resolved_value())
            .collect::<Vec<_>>(),
        [Some(36), Some(16), Some(12)]
    );
}
#[test]
fn native_package_constant_calls_consume_native_actuals() {
    let mut tree = parse_source("package p;function automatic int f(input int x);return x;endfunction endpackage module M;parameter int N=p::f(8+2);endmodule",Path::new("native-package-constant-call.sv")).unwrap();
    subtract(&mut tree);
    let source = Source::from_syntax(&tree).unwrap();
    let ir = crate::analyze::analyze_source(source).unwrap();
    assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(6));
}
#[test]
fn native_constant_calls_reject_unknown_callees_and_missing_actuals() {
    for name in [false, true] {
        let mut tree = parse_source("module M;function automatic int f(input int x);return x;endfunction parameter int N=f(7);endmodule",Path::new("native-constant-call-identity.sv")).unwrap();
        if name {
            let id = tree
                .declarations
                .expressions
                .iter()
                .find_map(|value| match value.kind {
                    ExprKind::Call { expression, .. } => Some(expression),
                    _ => None,
                })
                .unwrap();
            let range = tree.declarations().expression(id).tokens;
            let value = tree
                .declarations
                .expressions
                .iter_mut()
                .find(|value| value.tokens == range)
                .unwrap();
            let ExprKind::Name(name) = &mut value.kind else {
                unreachable!()
            };
            name.spelling = "other".into();
        } else {
            for value in &mut tree.declarations.expressions {
                if let ExprKind::Call { arguments, .. } = &mut value.kind {
                    arguments.clear();
                }
            }
        }
        let ir = crate::analyze::analyze_source(Source::from_syntax(&tree).unwrap()).unwrap();
        assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), None);
    }
}
#[test]
fn deep_native_constant_call_actuals_bind_on_the_default_stack() {
    let path = Path::new("deep-native-constant-call.sv");
    let prefix =
        "module M;function automatic int f(input int x);return x;endfunction parameter int N=f(";
    let mut tree = parse_source(&format!("{prefix}(8+2));endmodule"), path).unwrap();
    // Isolate native binding from the remaining full legacy parser's depth.
    *tree.declarations = Declarations::parse(
        &format!(
            "{prefix}{}8+2{});endmodule",
            "(".repeat(10_000),
            ")".repeat(10_000)
        ),
        path,
    )
    .unwrap();
    subtract(&mut tree);
    let source = Source::from_syntax(&tree).unwrap();
    let ir = crate::analyze::analyze_source(source).unwrap();
    assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(6));
}
#[test]
fn native_constant_call_actuals_drive_instance_parameter_values() {
    let mut tree = parse_source("module C#(parameter int N=0)();endmodule module M;function automatic int f(input int x);return x;endfunction C#(.N(f(8+2)))child();endmodule",Path::new("native-override-constant-call.sv")).unwrap();
    subtract(&mut tree);
    let source = Source::from_syntax(&tree).unwrap();
    let module = source
        .modules()
        .iter()
        .find(|module| module.name() == "M")
        .unwrap();
    let value = module.instances()[0].parameter_overrides()[0]
        .value()
        .unwrap();
    let crate::ast::ConstExpr::Function { args, .. } = value else {
        panic!("{value:?}")
    };
    assert!(matches!(
        &args[0],
        crate::ast::ConstExpr::Binary {
            op: crate::ast::BinaryOp::Sub,
            ..
        }
    ));
    crate::analyze::analyze_source(source).unwrap();
}
#[test]
fn native_nested_named_constant_calls_defer_until_formals_are_registered() {
    let mut tree=parse_source("module M;function automatic int f(input int x);return x;endfunction function automatic int g(input int x=1);return x;endfunction parameter int N=f(g(.x(8+2))),F=f(.x(g(.x(8+2))));endmodule",Path::new("native-nested-named-constant-call.sv")).unwrap();
    subtract(&mut tree);
    let source = Source::from_syntax(&tree).unwrap();
    let ir = crate::analyze::analyze_source(source).unwrap();
    // Native actual binding resolves nested named calls after formals are installed.
    assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(6));
    assert_eq!(ir.modules()[0].parameters()[1].resolved_value(), Some(6));
}
