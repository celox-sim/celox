//! Operands and nets of types the operator or the net does not take.
use super::*;

#[test]
fn rejects_nets_of_strings_and_class_handles() {
    for (body, name, data_type) in [
        ("wire string x;", "x", "type `string`"),
        ("class C; endclass wire C x;", "x", "a handle type"),
        ("wire event e;", "e", "a handle type"),
    ] {
        let source = format!("module test; {body} initial $display(\"x\"); endmodule");
        let error = analyze_source(&source, Path::new("nets.sv")).unwrap_err();
        assert!(
            matches!(
                error.source_error(),
                AnalyzerError::IllegalNetDataType { name: n, data_type: t } if n == name && t == &data_type
            ),
            "{body}: {error}"
        );
    }
    for body in [
        "wire logic [3:0] x;",
        "wire integer x;",
        "typedef struct packed { logic a; } S; wire S x;",
        "wire logic x [2];",
    ] {
        let source = format!("module test; {body} endmodule");
        analyze_source(&source, Path::new("nets.sv"))
            .unwrap_or_else(|error| panic!("{body}: {error}"));
    }
}

/// Wildcard equality and concatenation take integral operands (IEEE
/// 1800-2023 11.3, Table 11-1).
#[test]
fn rejects_real_operands_of_integral_operators() {
    for (body, operator) in [
        ("parameter weq1 = 2'b01 ==? 0.0;", "==?"),
        ("parameter wneq2 = 0.0 !=? 2'b01;", "!=?"),
        (
            "parameter real r = 1.0; initial if (2'b01 ==? r) $display(\"x\");",
            "==?",
        ),
        (
            "parameter real rpar1 = 1.0; parameter real rpar2 = 2.0; parameter real rparb = {rpar1, rpar2};",
            "{}",
        ),
        ("parameter real rpar = {2.0, 1.0};", "{}"),
    ] {
        let source = format!("module top; {body} endmodule");
        let error = analyze_source(&source, Path::new("operands.sv")).unwrap_err();
        assert!(
            matches!(
                error.source_error(),
                AnalyzerError::IllegalOperand { operator: o, operand: "real" } if o == operator
            ),
            "{body}: {error}"
        );
    }
    for body in [
        "parameter weq = 2'b01 ==? 2'b0x;",
        "parameter real r = 1.5; parameter int i = r > 1.0;",
        "parameter c = {2'b01, 3'd2};",
        "real ra [2] = {1.0, 2.0};",
        "parameter string s = {\"a\", \"b\"};",
        "initial #1.5 $display(\"x\");",
    ] {
        let source = format!("module top; {body} endmodule");
        analyze_source(&source, Path::new("operands.sv"))
            .unwrap_or_else(|error| panic!("{body}: {error}"));
    }
}

/// A replication of zero copies has no size: it appears only in a
/// concatenation with an operand of positive size (IEEE 1800-2023
/// 11.4.12.1).
#[test]
fn rejects_zero_replications_alone_in_a_concatenation() {
    for body in [
        "initial lat = {{2{{0{sign_bit}}}}, 16'h0001};",
        "parameter P = 32; initial lat = {{{32-P{1'b1}}}, 1'b0};",
    ] {
        let source = format!("module test; reg [31:0] lat; wire sign_bit = 1'b1; {body} endmodule");
        let error = analyze_source(&source, Path::new("replications.sv")).unwrap_err();
        assert!(
            matches!(error.source_error(), AnalyzerError::InvalidReplication(_)),
            "{body}: {error}"
        );
    }
    for body in [
        "parameter P = 32; initial lat = {{32-P{1'b1}}, 32'h1};",
        "initial lat = {{0{sign_bit}}, 32'h1};",
        "initial lat = {2{16'h1}};",
    ] {
        let source = format!("module test; reg [31:0] lat; wire sign_bit = 1'b1; {body} endmodule");
        analyze_source(&source, Path::new("replications.sv"))
            .unwrap_or_else(|error| panic!("{body}: {error}"));
    }
}
