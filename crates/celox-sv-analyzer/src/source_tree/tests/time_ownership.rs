//! Native time syntax and query identity own elaboration without legacy joins.
use super::*;
use crate::ast::Source;

fn value(tree: &SourceTree) -> Option<i128> {
    let source = Source::from_syntax(tree).unwrap();
    let ir = crate::analyze::analyze_source(source).unwrap();
    ir.modules()[0].parameters()[0].resolved_value()
}

#[test]
fn native_time_declaration_controls_the_scope() {
    let mut tree = parse_source(
        "module M; timeunit 1ns / 1ps; localparam U=$timeunit(); endmodule",
        Path::new("native-time-declaration.sv"),
    )
    .unwrap();
    tree.declarations.time_declarations[0]
        .unit
        .as_mut()
        .unwrap()
        .unit = "us".into();
    assert_eq!(value(&tree), Some(-6));
}

#[test]
fn native_time_directive_controls_inheritance() {
    let mut tree = parse_source(
        "`timescale 1ns / 1ps\nmodule M; localparam U=$timeunit(); endmodule",
        Path::new("native-time-directive.sv"),
    )
    .unwrap();
    let celox_sv_syntax::TimeDirectiveKind::Scale { unit, .. } =
        &mut tree.declarations.time_directives[0].kind
    else {
        panic!("expected scale");
    };
    unit.unit = "us".into();
    assert_eq!(value(&tree), Some(-6));
}

#[test]
fn native_time_query_callee_controls_the_value() {
    let mut tree = parse_source(
        "module M; timeunit 1ns / 1ps; localparam U=$timeunit(); endmodule",
        Path::new("native-time-query.sv"),
    )
    .unwrap();
    for expression in &mut tree.declarations.expressions {
        if let celox_sv_syntax::ExprKind::Name(name) = &mut expression.kind
            && name.spelling == "$timeunit"
        {
            name.spelling = "$timeprecision".into();
        }
    }
    assert_eq!(value(&tree), Some(-12));
}

#[test]
fn native_time_queries_remain_valid_inside_numeric_and_type_queries() {
    let tree = parse_source("module M; timeunit 1ns / 1ps; localparam U=$bits($timeunit()); localparam P=$clog2(-$timeprecision()); endmodule", Path::new("native-nested-time-query.sv")).unwrap();
    let source = Source::from_syntax(&tree).unwrap();
    let ir = crate::analyze::analyze_source(source).unwrap();
    assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(32));
    assert_eq!(ir.modules()[0].parameters()[1].resolved_value(), Some(4));
}

#[test]
fn discarded_time_queries_use_the_native_call_below_void_casts() {
    let tree = parse_source(
        "module M(output int y); timeunit 1ns / 1ps; always_comb begin $timeunit(); void'($timeprecision()); y=1; end endmodule",
        Path::new("native-discarded-time-query.sv"),
    ).unwrap();
    let source = Source::from_syntax(&tree).unwrap();
    crate::analyze::analyze_source(source).unwrap();
}
