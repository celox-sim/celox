//! Native complex `$bits` operand jobs through runtime and parameter consumers.
use super::*;
use crate::ast::{Expr, Source};
use celox_sv_syntax::ExprKind;
fn parsed(expression: &str, parameter: bool) -> SourceTree {
    let statement = if parameter {
        format!("localparam N={expression};assign y=N;")
    } else {
        format!("assign y={expression};")
    };
    let mut tree = parse_source(
        &format!("module M(input logic[31:0]a,output int y);{statement}endmodule"),
        Path::new("native-complex-bits.sv"),
    )
    .unwrap();
    for node in &mut tree.declarations.expressions {
        match &mut node.kind {
            ExprKind::Binary { operator, .. } if operator == "+" => *operator = "-".into(),
            ExprKind::Unary { operator, .. } if operator == "+" => *operator = "&".into(),
            _ => {}
        }
    }
    tree
}
fn number(tree: &SourceTree, parameter: bool) -> i128 {
    let source = Source::from_syntax(tree).unwrap();
    if parameter {
        let ir = crate::analyze::analyze_source(source).unwrap();
        return ir.modules()[0].parameters()[0].resolved_value().unwrap();
    }
    let Expr::Literal(text) = source.modules()[0].assignments()[0].rhs() else {
        panic!()
    };
    text.parse().unwrap()
}
#[test]
fn native_bits_cast_target_operators_drive_runtime_and_parameter_queries() {
    for parameter in [false, true] {
        let tree = parsed("$bits((8+2)'(a))", parameter);
        assert_eq!(number(&tree, parameter), 6);
    }
}
#[test]
fn native_bits_replication_counts_and_part_types_drive_runtime_and_parameter_queries() {
    for parameter in [false, true] {
        for (expression, expected) in [("$bits({(3+1){+a}})", 2), ("$bits({3{+a}})", 3)] {
            let tree = parsed(expression, parameter);
            assert_eq!(
                number(&tree, parameter),
                expected,
                "{expression} parameter={parameter}"
            );
        }
    }
}
#[test]
fn native_bits_range_and_indexed_width_operators_drive_queries() {
    for parameter in [false, true] {
        for (expression, expected) in [("$bits(a[7+1:0])", 7), ("$bits(a[2+1 +: (6+2)])", 4)] {
            let tree = parsed(expression, parameter);
            assert_eq!(
                number(&tree, parameter),
                expected,
                "{expression} parameter={parameter}"
            );
        }
    }
}
#[test]
fn native_bits_complex_operands_keep_alias_scope_and_effectful_call_type_queries() {
    let tree = parse_source(
        "package p;typedef bit[6:0]T;endpackage module M(output int y);int counter=0;function automatic logic[4:0]f(input int x);counter++;return x;endfunction assign y=$bits({3{p::T'(f(7))}});endmodule",
        Path::new("native-complex-bits-scope.sv"),
    ).unwrap();
    assert_eq!(number(&tree, false), 21);
    assert_eq!(
        Source::from_syntax(&tree).unwrap().modules()[0]
            .assignments()
            .len(),
        1
    );
}
#[test]
fn native_bits_complex_operand_literals_are_authoritative() {
    for expression in ["$bits(8'(a))", "$bits(a[7:0])"] {
        let mut tree = parsed(expression, false);
        for node in &mut tree.declarations.expressions {
            if let ExprKind::Literal(text) = &mut node.kind
                && matches!(text.as_str(), "8" | "7")
            {
                *text = "9".into();
            }
        }
        assert_eq!(
            number(&tree, false),
            if expression.contains("'(a)") { 9 } else { 10 },
            "{expression}"
        );
    }
}
