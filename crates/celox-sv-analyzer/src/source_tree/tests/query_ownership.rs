//! Query call IDs come from native semantic owners, without a source index.
use super::*;
use crate::ast::{Expr, Source};
use celox_sv_syntax::ExprKind;
fn reduce(tree: &mut SourceTree) {
    for expression in &mut tree.declarations.expressions {
        if let ExprKind::Unary { operator, .. } = &mut expression.kind
            && operator == "+"
        {
            *operator = "&".into();
        }
    }
}
#[test]
fn native_parameter_query_operand_controls_elaboration() {
    let mut tree=parse_source("module M(input logic[7:0]a,output logic[N:0]y);parameter int N=$bits(+a);assign y=0;endmodule",Path::new("native-parameter-query.sv")).unwrap();
    reduce(&mut tree);
    let ir = crate::analyze::analyze_source(Source::from_syntax(&tree).unwrap()).unwrap();
    assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(1));
    assert_eq!(
        ir.modules()[0].ports()[1].r#type().resolved_width(),
        Some(2)
    );
}
#[test]
fn native_alias_bound_query_operand_controls_width() {
    let mut tree=parse_source("module M(input logic[7:0]a,output T y);typedef logic[$bits(+a)-1:0] T;assign y=0;endmodule",Path::new("native-alias-query.sv")).unwrap();
    reduce(&mut tree);
    let ir = crate::analyze::analyze_source(Source::from_syntax(&tree).unwrap()).unwrap();
    assert_eq!(
        ir.modules()[0].ports()[1].r#type().resolved_width(),
        Some(1)
    );
}
#[test]
fn native_query_handles_expansions_directives_comments_and_split_numbers() {
    let tree=parse_source("`define F(x) $bits(x)\n`timescale 1ns/1ps\nmodule M(output int y);assign y=`F(8'shff /*end*/);endmodule\n`resetall\n",Path::new("native-query-expansions.sv")).unwrap();
    let source = Source::from_syntax(&tree).unwrap();
    assert!(
        matches!(source.modules()[0].assignments()[0].rhs(),Expr::Literal(value) if value=="8")
    );
}
#[test]
fn selected_native_query_replacement_controls_parameter_value() {
    let mut tree = parse_source(
        "module M;parameter int N=$bits(1);endmodule",
        Path::new("native-query-missing.sv"),
    )
    .unwrap();
    for expression in &mut tree.declarations.expressions {
        if matches!(expression.kind, ExprKind::Call { .. }) {
            expression.kind = ExprKind::Literal("0".into());
        }
    }
    let ir = crate::analyze::analyze_source(Source::from_syntax(&tree).unwrap()).unwrap();
    assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(0));
}
#[test]
fn discarded_query_statement_keeps_native_call_ownership() {
    let tree=parse_source("module M(output int y);logic[7:0]a;always_comb begin $bits(a);void'($size(a));y=1;end endmodule",Path::new("native-query-discard.sv")).unwrap();
    crate::analyze::analyze_source(Source::from_syntax(&tree).unwrap()).unwrap();
}

#[test]
fn attribute_queries_keep_native_validation_without_executing_metadata() {
    let tree = parse_source(
        "(* width=$bits(1) *) module M;endmodule",
        Path::new("native-query-attribute.sv"),
    )
    .unwrap();
    Source::from_syntax(&tree).unwrap();
}

#[test]
fn opaque_assertion_queries_retain_the_explicit_rejection() {
    let tree = parse_source(
        "module M(input logic[3:0]a);assert property(@(posedge a[0]) $bits(a)==4);endmodule",
        Path::new("native-query-opaque.sv"),
    )
    .unwrap();
    assert!(
        matches!(Source::from_syntax(&tree), Err(AnalyzerError::Unsupported(detail)) if detail == "concurrent assertion")
    );
}

#[test]
fn indexed_query_bases_and_widths_keep_the_complete_shape_context() {
    let tree = parse_source(
        "module M#(parameter logic[1:0][3:0]P=8'hab)(input logic[15:0]data,output logic[3:0]y);assign y=data[$bits(P[0]) +: $size(P[0])];endmodule",
        Path::new("native-indexed-query-context.sv"),
    ).unwrap();
    crate::analyze::analyze_source(Source::from_syntax(&tree).unwrap()).unwrap();
}

#[test]
fn interface_projection_validates_selected_native_query_membership() {
    crate::analyze_source(
        "interface I#(parameter N=1);endinterface module M(output int y);typedef struct packed{logic[16:0]later;}T;I#($bits(T))first();I later();assign y=first.N;endmodule",
        Path::new("native-interface-query-membership.sv"),
    ).unwrap();
}

#[test]
fn native_size_dimension_operand_controls_the_result() {
    let mut tree = parse_source(
        "module M(input logic[1:0][3:0]a,output int y);assign y=$size(a,1+1);endmodule",
        Path::new("native-size-dimension.sv"),
    )
    .unwrap();
    let root = tree.declarations.assign_statements[0].assignments[0].right;
    let ExprKind::Call { arguments, .. } = &tree.declarations.expression(root).kind else {
        panic!()
    };
    let dimension = tree
        .declarations
        .expression(arguments[1].value.unwrap())
        .tokens;
    for expression in &mut tree.declarations.expressions {
        if expression.tokens == dimension {
            let ExprKind::Binary { operator, .. } = &mut expression.kind else {
                panic!()
            };
            *operator = "*".into();
        }
    }
    let source = Source::from_syntax(&tree).unwrap();
    assert!(
        matches!(source.modules()[0].assignments()[0].rhs(), Expr::Literal(value) if value == "2")
    );
}

#[test]
fn attribute_query_operands_retain_their_diagnostics() {
    let tree = parse_source(
        "(* width=$bits(missing) *) module M;endmodule",
        Path::new("native-invalid-attribute-query.sv"),
    )
    .unwrap();
    assert!(Source::from_syntax(&tree).is_err());
}
