//! The actual of an input port may be a hierarchical name (IEEE 1800-2023
//! 23.3.2.2, 23.6): a variable, a select of one or a net of an instance
//! below the instantiating module, such as a clock of a chip that a
//! testbench counter runs on.
use celox::Simulation;
use std::path::Path;

/// Runs `source`, whose `initial` blocks print lines, and returns them.
fn display(source: &str, four_state: bool) -> Vec<String> {
    let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("port_actuals.sv"))], "Top")
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
fn input_actuals_are_hierarchical_names() {
    let source = r#"
    `timescale 1ns/1ns
    module Chip(input logic clk_i, input logic [7:0] value_i);
      logic clk_muxed;
      logic [7:0] doubled;
      wire  [7:0] net;
      assign clk_muxed = clk_i;
      assign doubled = value_i * 2;
      assign net = value_i + 8'd1;
    endmodule
    module Counter(input logic clk, input logic [7:0] step, input logic [7:0] seen, output logic [7:0] n);
      always_ff @(posedge clk) n <= n + step;
      initial n = 0;
    endmodule
    module Top;
      logic clk = 0;
      logic [7:0] value = 8'd3;
      logic [7:0] n;
      if (1) begin : system
        Chip chip(.clk_i(clk), .value_i(value));
      end
      // Input actuals that are hierarchical names: a variable, a slice of one
      // and a net of an instance inside a generate block.
      Counter c(.clk(system.chip.clk_muxed), .step(system.chip.doubled[7:0]), .seen(system.chip.net), .n(n));
      initial begin
        #1 clk = 1; #1 clk = 0;
        #1 clk = 1; #1 clk = 0;
        value = 8'd5;
        #1 clk = 1; #1;
        $display("n=%0d seen=%0d", n, c.seen);
      end
    endmodule
    "#;
    for four_state in [false, true] {
        assert_eq!(
            display(source, four_state),
            ["n=22 seen=6"],
            "four-state {four_state}"
        );
    }
}
