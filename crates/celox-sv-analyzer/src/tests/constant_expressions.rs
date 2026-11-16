//! Constant expressions: part-select bounds (IEEE 1800-2023 11.5.1) and
//! parameter values (6.20.2).
use super::*;

fn module(body: &str) -> Result<Ir, AnalyzerError> {
    let source = format!(
        "module top(input wire [2:0] N, input wire [7:0] In, output reg [7:0] Out); \
         {body} endmodule"
    );
    analyze_source(&source, Path::new("selects.sv"))
}

#[test]
fn rejects_part_selects_with_variable_bounds() {
    for body in [
        "wire [7:0] A; assign A[7:1] = In[7:N];",
        "wire [7:0] A; assign A[7:1] = In[N:1];",
        "initial Out[7:N] = In[7:1];",
        "initial Out[N:0] = In[7:1];",
        "reg [15:0] m, a; always @(posedge In[0]) begin m = 1; a[15*m:0] = 1; end",
    ] {
        let error = module(body).unwrap_err();
        assert!(
            matches!(
                error.source_error(),
                AnalyzerError::NonConstantExpression { construct, .. }
                    if construct == "a bound of a part-select"
            ),
            "{body}: {error}"
        );
    }
}

#[test]
fn accepts_part_selects_with_constant_bounds() {
    for body in [
        "localparam L = 3; initial Out[L:0] = In[7:L+1];",
        "parameter P = 2; initial Out[P*2+1:P] = In[3:0];",
        "for (genvar i = 0; i < 2; i++) begin : g assign Out[i*4+3:i*4] = In[i*4+3:i*4]; end",
        "initial Out[N +: 2] = In[1:0];",
        "initial Out[N] = In[N];",
        "function automatic [3:0] f(input [7:0] v); localparam W = 4; return v[W-1:0]; endfunction \
         initial Out[3:0] = f(In);",
    ] {
        module(body).unwrap_or_else(|error| panic!("{body}: {error}"));
    }
}

/// A parameter value is a constant expression (IEEE 1800-2023 6.20.2,
/// 11.2.1), also through the defaults of a constant function call (13.4.3).
#[test]
fn rejects_parameter_values_that_read_variables() {
    for (body, construct, name) in [
        (
            "integer Value1; parameter Value2 = Value1;",
            "the value of parameter `value2`",
            "value1",
        ),
        (
            "integer a; integer b; function integer k(integer i, integer j = a+b); k = i + j; endfunction \
             localparam integer result = k(3);",
            "the default of argument `j` of function `k`",
            "a",
        ),
    ] {
        let source = format!("module test(); {body} endmodule");
        let error = analyze_source(&source, Path::new("parameters.sv")).unwrap_err();
        assert!(
            matches!(
                error.source_error(),
                AnalyzerError::NonConstantExpression { construct: c, name: n }
                    if c.to_lowercase().starts_with(construct) && n.to_lowercase() == name
            ),
            "{body}: {error}"
        );
    }
    for body in [
        "integer v; localparam W = $bits(v);",
        "localparam A = 2; parameter B = A * 3;",
        "integer a; function automatic [3:0] f(integer j = a); return j; endfunction \
         localparam W = $bits(f());",
        // A pattern key names a member, not the variable of that name.
        "typedef struct packed { logic [7:0] opcode; } e_t; logic [7:0] opcode; \
         localparam e_t ENTRY = '{opcode: 8'h40};",
        "integer a; function integer k(integer i, integer j = a); k = i + j; endfunction \
         localparam integer result = k(3, 4);",
        "integer a; function integer k(integer i, integer j = 1); k = i + j; endfunction \
         localparam integer result = k(3); initial a = k(a);",
    ] {
        let source = format!("module test(); {body} endmodule");
        analyze_source(&source, Path::new("parameters.sv"))
            .unwrap_or_else(|error| panic!("{body}: {error}"));
    }
}

/// No constant value satisfies a parameter that depends on itself.
#[test]
fn rejects_parameters_that_depend_on_their_own_value() {
    for body in [
        "parameter PARAMB = PARAMB + 6;",
        "parameter PARAMB = PARAMA; parameter PARAMA = PARAMB;",
        "parameter real PARAMB = PARAMB + 1.0;",
        "parameter A = C; localparam B = A; localparam C = B + 1;",
    ] {
        let source = format!("module top; {body} initial $display(\"x\"); endmodule");
        let error = analyze_source(&source, Path::new("parameters.sv")).unwrap_err();
        assert!(
            matches!(
                error.source_error(),
                AnalyzerError::CircularParameter { .. }
            ),
            "{body}: {error}"
        );
    }
    for body in [
        "parameter A = 1; parameter B = A + A;",
        "parameter B = A; parameter A = 3;",
        // A data query does not evaluate its operand.
        "parameter int A = $bits(B), B = $bits(A);",
        // A pattern key names a member, not the parameter of that name.
        "typedef struct packed { logic [3:0] A; } T; parameter T X = '{A: 1}; parameter A = X.A;",
        "parameter A = 1; if (1) begin : g localparam A2 = A; end",
    ] {
        let source = format!("module top; {body} endmodule");
        analyze_source(&source, Path::new("parameters.sv"))
            .unwrap_or_else(|error| panic!("{body}: {error}"));
    }
}
