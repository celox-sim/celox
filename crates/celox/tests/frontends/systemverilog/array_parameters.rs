//! Parameters of unpacked array type (IEEE 1800-2023 6.20.2).
use crate::*;

fn sorted_lines(source: &str, four_state: bool) -> Vec<String> {
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("array_parameters.sv"))], "top")
            .four_state(four_state)
            .build()
            .unwrap();
    sim.run_until(10).unwrap();
    let mut lines: Vec<_> = sim
        .drain_runtime_events()
        .into_iter()
        .filter_map(|event| match event {
            celox::RuntimeEvent::Display { message } => {
                Some(String::from_utf8_lossy(&message).into_owned())
            }
            _ => None,
        })
        .collect();
    lines.sort();
    lines
}

/// The value of an array parameter is an expression of the array type: a
/// conditional whose condition is a constant selects one of its operands
/// whole (11.4.11), and another array parameter gives its value.
#[test]
fn array_parameter_values_are_array_expressions() {
    let source = r#"`timescale 1ns/1ns
module leaf #(parameter int h_p = 0) ();
  localparam int els_lp[4:0] = (h_p == 0) ? '{2, 2, 2, 2, 3} : '{4, 4, 4, 4, 4};
  localparam int copy_lp[4:0] = els_lp;
  localparam int nested_lp[1:0] = (h_p == 2) ? '{7, 8} : ((h_p == 1) ? '{5, 6} : '{1, 0});
  initial #1 $display("%0d: %0d %0d / %0d %0d / %0d %0d", h_p, els_lp[0], els_lp[4],
                      copy_lp[0], copy_lp[4], nested_lp[1], nested_lp[0]);
endmodule
module top;
  leaf #(.h_p(0)) a ();
  leaf #(.h_p(1)) b ();
  leaf #(.h_p(2)) c ();
  initial #2 $finish;
endmodule"#;
    for four_state in [false, true] {
        assert_eq!(
            sorted_lines(source, four_state),
            [
                "0: 3 2 / 3 2 / 1 0",
                "1: 4 4 / 4 4 / 5 6",
                "2: 4 4 / 4 4 / 7 8"
            ],
            "four_state={four_state}"
        );
    }
}

/// An element of an array parameter is an elaboration constant (11.2.1): it
/// sizes a declaration and a member of a structure, gives a parameter its
/// value and selects a generate branch, with the value of each instance: the
/// default, one an instantiation gives as a pattern, and one it forwards.
/// Verilator 5.054 prints the same lines.
#[test]
fn array_parameter_elements_are_elaboration_constants() {
    let source = r#"`timescale 1ns/1ns
module leaf #(parameter int dims_p = 2, parameter int pos_p [dims_p:0] = '{5, 4, 0}, parameter int idx_p = 0) ();
  typedef struct packed {
    logic [3:0] len;
    logic [pos_p[dims_p]-1:0] cord;
  } hdr_s;
  hdr_s hdr;
  localparam int top_lp = pos_p[1];
  if (pos_p[dims_p] == 5) begin : five initial #1 $display("leaf %0d: five bits=%0d top=%0d", idx_p, $bits(hdr), top_lp); end
  else begin : other initial #1 $display("leaf %0d: other bits=%0d top=%0d", idx_p, $bits(hdr), top_lp); end
  for (genvar i = 0; i <= dims_p; i++) begin : g
    localparam int e_lp = pos_p[i];
    logic [pos_p[i]:0] w;
    initial #1 $display("leaf %0d elem %0d = %0d bits=%0d", idx_p, i, e_lp, $bits(w));
  end
endmodule
module top;
  localparam int tbl_lp [2:0] = '{9, 6, 1};
  leaf #(.idx_p(0)) l0 ();
  leaf #(.pos_p('{7, 3, 2}), .idx_p(1)) l1 ();
  leaf #(.pos_p(tbl_lp), .idx_p(2)) l2 ();
  initial #2 $finish;
endmodule"#;
    for four_state in [false, true] {
        assert_eq!(
            sorted_lines(source, four_state),
            [
                "leaf 0 elem 0 = 0 bits=1",
                "leaf 0 elem 1 = 4 bits=5",
                "leaf 0 elem 2 = 5 bits=6",
                "leaf 0: five bits=9 top=4",
                "leaf 1 elem 0 = 2 bits=3",
                "leaf 1 elem 1 = 3 bits=4",
                "leaf 1 elem 2 = 7 bits=8",
                "leaf 1: other bits=11 top=3",
                "leaf 2 elem 0 = 1 bits=2",
                "leaf 2 elem 1 = 6 bits=7",
                "leaf 2 elem 2 = 9 bits=10",
                "leaf 2: other bits=13 top=6",
            ],
            "four_state={four_state}"
        );
    }
}

/// The elements of an array that takes its value from an earlier array, of
/// an array whose element type an element sizes, and a parameter whose type
/// an element sizes are constants too. Verilator 5.054 prints the same line.
#[test]
fn array_parameter_elements_reach_later_declarations() {
    let source = r#"`timescale 1ns/1ns
module top #(parameter int table_p[1:0] = '{6, 3});
  localparam int copy_lp[1:0] = table_p;
  logic [copy_lp[1]-1:0] w;
  localparam logic [table_p[0]-1:0] sized_lp[0:0] = '{3'd5};
  localparam int element_lp = sized_lp[0];
  localparam logic [table_p[0]-1:0] q_lp = '1;
  initial #1 $display("%0d %0d %0d %0d %b", $bits(w), $bits(sized_lp[0]), element_lp,
                      $bits(q_lp), q_lp);
  initial #2 $finish;
endmodule"#;
    for four_state in [false, true] {
        assert_eq!(
            sorted_lines(source, four_state),
            ["6 3 5 3 111"],
            "four_state={four_state}"
        );
    }
}

/// A select of the packed dimensions of an element is a constant too, and an
/// element type may depend on an element of an array whose own element type
/// depends on another. Verilator 5.054 prints the same line.
#[test]
fn array_parameter_element_selects_and_chains_are_elaboration_constants() {
    let source = r#"`timescale 1ns/1ns
module top #(parameter int table_p[1:0] = '{6, 5},
             parameter logic [1:0][3:0] nib_p[0:1] = '{8'h3A, 8'h51});
  localparam int a_lp[0:0] = '{4};
  localparam logic [a_lp[0]-1:0] b_lp[0:0] = '{4'd7};
  localparam logic [b_lp[0]-1:0] c_lp[0:0] = '{7'd9};
  localparam logic [c_lp[0]-1:0] d_lp = '1;
  logic [nib_p[0][1]:0] w;
  if (table_p[0][2] && !table_p[0][1]) begin : yes
    initial #1 $display("yes %0d %0d %0d %0d", $bits(w), nib_p[1][0], $bits(c_lp[0]), $bits(d_lp));
  end else begin : no
    initial #1 $display("no");
  end
  initial #2 $finish;
endmodule"#;
    for four_state in [false, true] {
        assert_eq!(
            sorted_lines(source, four_state),
            ["yes 4 1 7 9"],
            "four_state={four_state}"
        );
    }
}

/// The declarations of a scope bound before its array parameters are
/// collected see their elements too: a later parameter of the header sized
/// by an element of an array that takes its value from another, and the
/// base type of an enumeration. Verilator 5.054 prints the same line.
#[test]
fn array_parameter_elements_reach_parameters_and_enumerations_of_the_scope() {
    let source = r#"`timescale 1ns/1ns
module top #(parameter int a[0:0] = '{4}, parameter int b[0:0] = a, parameter logic [b[0]-1:0] q = '1,
             parameter int width_p[0:0] = '{3});
  typedef enum logic [width_p[0]-1:0] { ALL = '1 } e_t;
  localparam int v = ALL;
  initial #1 $display("%0d %b %0d %0d", $bits(q), q, v, $bits(e_t));
endmodule"#;
    for four_state in [false, true] {
        assert_eq!(
            sorted_lines(source, four_state),
            ["4 1111 7 3"],
            "four_state={four_state}"
        );
    }
}

/// A constant index out of range reads the value of a nonexistent entry
/// (Table 7-1): zero for two-state elements, X for four-state ones.
#[test]
fn array_parameter_element_out_of_range_reads_the_default_of_its_type() {
    let source = r#"`timescale 1ns/1ns
module top #(parameter int two_p[1:0] = '{6, 3}, parameter logic [3:0] four_p[1:0] = '{4'd6, 4'd3});
  localparam logic [3:0] unknown_lp = four_p[5];
  if (two_p[7] == 0) begin : zero
    initial #1 $display("zero %b", unknown_lp);
  end else begin : other
    initial #1 $display("other %b", unknown_lp);
  end
  initial #2 $finish;
endmodule"#;
    assert_eq!(sorted_lines(source, true), ["zero xxxx"]);
    assert_eq!(sorted_lines(source, false), ["zero 0000"]);
}

/// A variable of a generate block hides an array parameter of the same
/// name, whose elements are no constants there.
#[test]
fn generate_local_variable_hides_the_elements_of_an_array_parameter() {
    let source = r#"`timescale 1ns/1ns
module top #(parameter int pos_p[1:0] = '{6, 2}) ();
  localparam int outer_lp = pos_p[1];
  if (1) begin : g
    logic [3:0] pos_p;
    initial begin
      pos_p = 4'b0101;
      #1 $display("%0d %b %b", outer_lp, pos_p[1], pos_p[0]);
    end
  end
  initial #2 $finish;
endmodule"#;
    for four_state in [false, true] {
        assert_eq!(
            sorted_lines(source, four_state),
            ["6 0 1"],
            "four_state={four_state}"
        );
    }
}

/// An enumeration member or a genvar of a generate block hides an array
/// parameter of the same name: a select of the name in the block's
/// constants selects the bits of the nearer constant. Verilator 5.054
/// prints the same lines.
#[test]
fn generate_local_constants_hide_the_elements_of_an_array_parameter() {
    let source = r#"`timescale 1ns/1ns
module top;
  parameter logic [3:0] a [0:1] = '{4'd1, 4'd3};
  parameter logic [3:0] i [0:1] = '{4'd1, 4'd1};
  if (1) begin : g
    typedef enum logic [3:0] { a = 4'd8 } e_t;
    localparam logic b0 = a[0];
    localparam logic b3 = a[3];
    logic [1:0] o;
    if (a[0]) begin : outer
      assign o = 2'd1;
    end else begin : inner
      assign o = 2'd2;
    end
    initial #1 $display("enum %b %b %0d", b0, b3, o);
  end
  for (genvar i = 2; i < 3; i++) begin : l
    localparam logic b1 = i[1];
    initial #1 $display("genvar %b", b1);
  end
  initial #2 $finish;
endmodule"#;
    for four_state in [false, true] {
        assert_eq!(
            sorted_lines(source, four_state),
            ["enum 0 1 2", "genvar 1"],
            "four_state={four_state}"
        );
    }
}

/// Every operand of a conditional array value is a value of the parameter,
/// also the one its condition does not select (11.4.11).
#[test]
fn conditional_array_parameter_value_checks_the_operand_it_does_not_select() {
    for value in ["(1) ? '{1, 2} : '{1, 2, 3}", "(0) ? wide_lp : '{1, 2}"] {
        let source = format!(
            "module top; localparam int wide_lp[2:0] = '{{1, 2, 3}}; \
             localparam int v_lp[1:0] = {value}; endmodule"
        );
        assert!(
            celox::Simulation::from_sv_sources(vec![(&source, Path::new("arms.sv"))], "top")
                .build()
                .is_err(),
            "{value}"
        );
    }
}
