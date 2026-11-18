//! A parameter wider than its value takes the value padded to its width: an
//! unsigned value by zeros, a signed one by its sign (IEEE 1800-2023 10.7,
//! 11.6.1). That holds past 128 bits too, where an unsigned value with bit
//! 127 set is no negative number.
use celox::Simulation;
use std::path::Path;

/// Runs `source`, whose `initial` blocks print lines, and returns them.
fn display(source: &str, four_state: bool) -> Vec<String> {
    let mut sim =
        Simulation::from_sv_sources(vec![(source, Path::new("wide_parameters.sv"))], "Top")
            .four_state(four_state)
            .build()
            .unwrap();
    sim.run_until(100).unwrap();
    sim.drain_runtime_events()
        .into_iter()
        .filter_map(|event| match event {
            celox::RuntimeEvent::Display { message } => {
                Some(String::from_utf8_lossy(&message).into_owned())
            }
            _ => None,
        })
        .collect()
}

#[test]
fn unsigned_values_with_bit_127_set_are_zero_extended() {
    let source = r#"
    `timescale 1ns/1ns
    typedef struct packed { bit [71:0] hi; bit [127:0] lo; } wide_t;
    module Sink #(parameter logic [199:0] P = '0)(output logic [199:0] y);
      assign y = P;
    endmodule
    module Top;
      localparam logic [127:0] BASE = 128'h8000_0000_0000_0000_0000_0000_0000_0001;
      localparam logic [199:0] U = 128'h8000_0000_0000_0000_0000_0000_0000_0001;
      localparam logic [199:0] S = 128'sh8000_0000_0000_0000_0000_0000_0000_0001;
      localparam logic [199:0] N = -1;
      // Through an unsigned parameter, and an expression of one.
      localparam logic [199:0] C = BASE;
      localparam logic [199:0] E = BASE | 128'h2;
      localparam wide_t P = 128'h8000_0000_0000_0000_0000_0000_0000_0001;
      logic [199:0] v, through;
      assign v = U;
      Sink #(.P(128'h8000_0000_0000_0000_0000_0000_0000_0001)) sink(.y(through));
      initial begin
        #1;
        $display("U=%h", U);
        $display("S=%h", S);
        $display("N=%h", N);
        $display("C=%h", C);
        $display("E=%h", E);
        $display("P=%h %h", P.hi, P.lo);
        $display("v=%h", v);
        $display("y=%h", through);
        $display("compare=%b%b%b%b%b", U == 200'h8000_0000_0000_0000_0000_0000_0000_0001,
                 U < S, U[199:128] == 72'd0, v == C, P.hi == 72'd0);
      end
    endmodule
    "#;
    let unsigned = "00000000000000000080000000000000000000000000000001";
    for four_state in [false, true] {
        assert_eq!(
            display(source, four_state),
            [
                format!("U={unsigned}"),
                "S=ffffffffffffffffff80000000000000000000000000000001".to_string(),
                "N=ffffffffffffffffffffffffffffffffffffffffffffffffff".to_string(),
                format!("C={unsigned}"),
                "E=00000000000000000080000000000000000000000000000003".to_string(),
                "P=000000000000000000 80000000000000000000000000000001".to_string(),
                format!("v={unsigned}"),
                format!("y={unsigned}"),
                "compare=11111".to_string(),
            ],
            "four-state {four_state}"
        );
    }
}

/// The value of a constant function is extended by the sign of its return
/// type, like any other value.
#[test]
fn function_results_are_extended_by_their_sign() {
    let source = r#"
    `timescale 1ns/1ns
    module Top;
      function automatic logic [127:0] unsigned_bits();
        return 128'h8000_0000_0000_0000_0000_0000_0000_0001;
      endfunction
      function automatic logic signed [127:0] signed_bits();
        return 128'sh8000_0000_0000_0000_0000_0000_0000_0001;
      endfunction
      function automatic int minus_one();
        return -1;
      endfunction
      localparam logic [199:0] U = unsigned_bits();
      localparam logic [199:0] S = signed_bits();
      localparam logic [199:0] N = minus_one();
      localparam logic [199:0] E = unsigned_bits() | 128'h2;
      initial begin
        #1;
        $display("U=%h", U);
        $display("S=%h", S);
        $display("N=%h", N);
        $display("E=%h", E);
      end
    endmodule
    "#;
    for four_state in [false, true] {
        assert_eq!(
            display(source, four_state),
            [
                "U=00000000000000000080000000000000000000000000000001",
                "S=ffffffffffffffffff80000000000000000000000000000001",
                "N=ffffffffffffffffffffffffffffffffffffffffffffffffff",
                "E=00000000000000000080000000000000000000000000000003",
            ],
            "four-state {four_state}"
        );
    }
}
