//! A call of a constant function (IEEE 1800-2023 13.4.3) is a constant
//! expression (11.2.1), so it may give the value of a parameter assignment
//! of an instantiation (23.10). It is evaluated in the instantiating scope,
//! with the parameters that scope has in its specialization, and the result
//! has the function's return type.
use celox::Simulation;
use std::path::Path;

/// Runs `source`, whose `initial` blocks print lines, and returns them sorted.
fn display(source: &str, four_state: bool) -> Vec<String> {
    let mut sim =
        Simulation::from_sv_sources(vec![(source, Path::new("override_calls.sv"))], "Top")
            .four_state(four_state)
            .build()
            .unwrap();
    sim.run_until(100).unwrap();
    let mut lines: Vec<String> = sim
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

#[test]
fn constant_function_calls_give_parameter_overrides() {
    let source = r#"
    `timescale 1ns/1ns
    package Util;
      function automatic integer vbits(integer value);
        return (value == 1) ? 1 : $clog2(value);
      endfunction
      function automatic logic [69:0] wide(int shift);
        return 70'h1 << shift;
      endfunction
      function automatic int negate(int value);
        return -value;
      endfunction
    endpackage
    module Leaf #(parameter int Width = 8, parameter logic [69:0] W = '0,
                  parameter int S = 0) (output logic [Width-1:0] q);
      assign q = '1;
      initial #1 $display("%m width=%0d w=%h s=%0d", Width, W, S);
    endmodule
    module Mid #(parameter int N = 3) ();
      import Util::vbits;
      function automatic int twice(int value);
        return 2 * value;
      endfunction
      // A call sizes the port it is connected to as well.
      logic [vbits(N)-1:0] q;
      Leaf #(.Width(vbits(N)), .W(Util::wide(N + 60)), .S(Util::negate(N))) u_leaf(.q);
      for (genvar k = 1; k < 3; k++) begin : gen
        Leaf #(.Width(twice(k) + vbits(N))) u_k(.q());
      end
      initial #2 $display("%m q=%b", q);
    endmodule
    module Top;
      Mid u_default();
      Mid #(.N(16)) u_wide();
    endmodule
    "#;
    for four_state in [false, true] {
        assert_eq!(
            display(source, four_state),
            [
                "Top.u_default q=11",
                "Top.u_default.gen[1].u_k width=4 w=000000000000000000 s=0",
                "Top.u_default.gen[2].u_k width=6 w=000000000000000000 s=0",
                "Top.u_default.u_leaf width=2 w=008000000000000000 s=-3",
                "Top.u_wide q=1111",
                "Top.u_wide.gen[1].u_k width=6 w=000000000000000000 s=0",
                "Top.u_wide.gen[2].u_k width=8 w=000000000000000000 s=0",
                "Top.u_wide.u_leaf width=4 w=000000000000000000 s=-16",
            ],
            "four-state {four_state}"
        );
    }
}
