//! Symbolic declaration consumers must use native execution ownership/values.
use super::*;
use crate::ast::{Callee, ConstExpr, Source};
use celox_sv_syntax::{ExprKind, StatementKind};
fn inline(tree: &ValidatedSyntax, input: i128) -> i128 {
    let source = Source::from_syntax(tree).unwrap();
    let expression = ConstExpr::Function {
        callee: Callee::Subroutine("f".into()),
        args: vec![ConstExpr::Literal(input.to_string())],
        site: None,
    };
    let expression = source.modules()[0].declaration_expr(
        &crate::ast::AnalysisState::default(),
        &expression,
        &HashMap::default(),
    );
    crate::typecheck::eval_const_expr(&expression.into(), &HashMap::default()).unwrap()
}
#[test]
fn native_function_operands_drive_symbolic_returns_and_local_continuations() {
    let mut tree=parse_source("module M;function automatic int f(input int c);int t;if(c)return 8+2;t=12+3;return t+1;endfunction endmodule",Path::new("native-symbolic-operators.sv")).unwrap();
    for expression in &mut tree.declarations.expressions {
        if let ExprKind::Binary { operator, .. } = &mut expression.kind
            && operator == "+"
        {
            *operator = "-".into();
        }
    }
    assert_eq!(inline(&tree, 1), 6);
    assert_eq!(inline(&tree, 0), 8);
}
#[test]
fn native_function_branch_membership_drives_symbolic_return_selection() {
    let mut tree=parse_source("module M;function automatic int f(input int c);if(c)return 1;else return 2;endfunction endmodule",Path::new("native-symbolic-branches.sv")).unwrap();
    let conditional = tree
        .declarations
        .statements
        .iter_mut()
        .find(|statement| matches!(statement.kind, StatementKind::Conditional { .. }))
        .unwrap();
    let StatementKind::Conditional {
        branches,
        otherwise: Some(otherwise),
        ..
    } = &mut conditional.kind
    else {
        unreachable!()
    };
    std::mem::swap(&mut branches[0].body, otherwise);
    assert_eq!(inline(&tree, 1), 2);
    assert_eq!(inline(&tree, 0), 1);
}
#[test]
fn missing_authored_function_membership_is_a_parse_error_not_a_signature_retry() {
    let mut tree = parse_source(
        "module M;function automatic int f(input int c);return c+1;endfunction endmodule",
        Path::new("native-symbolic-membership.sv"),
    )
    .unwrap();
    tree.declarations.subroutines[0].authored_statements.clear();
    assert!(matches!(
        Source::from_syntax(&tree),
        Err(AnalyzerError::Parse(_))
    ));
}
