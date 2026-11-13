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

/// An unpacked array parameter may be overridden by a call of a constant
/// function that returns an unpacked array, built in a local variable from
/// an array parameter of the instantiating module (IEEE 1800-2023 13.4.1,
/// 13.4.3). An element the function does not write keeps the initial value
/// of its type: 0 for `int`, X for `logic`. The call may name its actuals in
/// another order. Verilator 5.054 prints the same lines.
#[test]
fn array_parameter_overrides_call_functions_that_return_arrays() {
    let source = r#"`timescale 1ns/1ns
module child #(parameter int v_p [0:3] = '{default:7}, parameter logic [3:0] l_p [0:1] = '{default:4'd5}) ();
  initial #1 $display("%m %0d %0d %0d %0d %b %b", v_p[0], v_p[1], v_p[2], v_p[3], l_p[0], l_p[1]);
endmodule
module top #(parameter int vec_p [0:7] = '{0, 1, 2, 3, 4, 5, 6, 7});
  typedef int sub_t [0:3];
  typedef logic [3:0] nib_t [0:1];
  function sub_t get(int x);
    sub_t vec;
    for (int i = 0; i < 4; i++) vec[i] = vec_p[x * 4 + i] * 10;
    return vec;
  endfunction
  function sub_t pick(int base, int scale);
    sub_t vec;
    for (int i = 0; i < 4; i++) vec[i] = vec_p[base + i] * scale;
    return vec;
  endfunction
  function sub_t sparse(int x);
    sub_t vec;
    vec[1] = x + 1;
    return vec;
  endfunction
  function nib_t nib(int x);
    nib_t n;
    n[0] = x;
    return n;
  endfunction
  for (genvar x = 0; x < 2; x++) begin : g
    child #(.v_p(get(x))) c ();
  end
  child #(.v_p(sparse(4)), .l_p(nib(3))) s ();
  child #(.v_p(pick(.scale(2), .base(3)))) named ();
  initial #2 $finish;
endmodule"#;
    for four_state in [false, true] {
        let x = if four_state { "xxxx" } else { "0000" };
        assert_eq!(
            sorted_lines(source, four_state),
            [
                "top.g[0].c 0 10 20 30 0101 0101".to_string(),
                "top.g[1].c 40 50 60 70 0101 0101".to_string(),
                "top.named 6 8 10 12 0101 0101".to_string(),
                format!("top.s 0 5 0 0 0011 {x}"),
            ],
            "four_state={four_state}"
        );
    }
}
