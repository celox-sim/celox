//! A cast of a constant (IEEE 1800-2023 6.24.1) is a constant expression
//! (11.2.1), also of a member of an enumeration wider than 128 bits, which
//! is known by its value (6.19): it overrides a parameter of an instance
//! and initializes a local parameter, as OpenTitan's sparse state flops do
//! with `$bits(state_e)'(StReset)`.
use celox::Simulation;
use std::path::Path;

/// Runs `source`, whose `initial` blocks print lines, and returns them sorted.
fn display(source: &str, four_state: bool) -> Vec<String> {
    let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("wide_casts.sv"))], "Top")
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
fn casts_of_members_of_wide_enumerations_are_constants() {
    let source = r#"
    `timescale 1ns/1ns
    package States;
      parameter logic [15:0] B1 = 16'hdead;
      parameter logic [15:0] B0 = 16'hbeef;
      typedef enum logic [159:0] { StA = {10{B1}}, StB = {10{B0}} } state_e;
    endpackage
    module Flop #(parameter int Width = 1, parameter logic [Width-1:0] ResetValue = '0) ();
      initial #1 $display("%m w=%0d %h", Width, ResetValue);
    endmodule
    module Top import States::*; ;
      localparam logic [159:0] Mixed = 160'(StA ^ StB);
      Flop #(.Width($bits(state_e)), .ResetValue($bits(state_e)'(StB))) u_b();
      Flop #(.Width(160), .ResetValue(160'(StA))) u_a();
      Flop #(.Width(64), .ResetValue(64'(StA))) u_low();
      initial #2 $display("mixed=%h", Mixed);
    endmodule
    "#;
    for four_state in [false, true] {
        assert_eq!(
            display(source, four_state),
            [
                "Top.u_a w=160 deaddeaddeaddeaddeaddeaddeaddeaddeaddead",
                "Top.u_b w=160 beefbeefbeefbeefbeefbeefbeefbeefbeefbeef",
                "Top.u_low w=64 deaddeaddeaddead",
                "mixed=6042604260426042604260426042604260426042",
            ],
            "four-state {four_state}"
        );
    }
}
