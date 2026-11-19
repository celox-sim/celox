//! The copies of a clock that modules sense at the opposite edge of their
//! source, through ports or `assign c = clk`, share one domain of that edge,
//! and each still runs at its own edge: a design whose top senses the clock
//! falling and whose units sense their port copies rising counts both edges.
use celox::{RuntimeEvent, Simulation};
use std::path::Path;

pub const UNITS: usize = 30;

pub fn source() -> String {
    let mut source = String::from(
        "`timescale 1ns/1ns
module Unit(input logic clk, output logic [15:0] q);
  initial q = 0;
  always_ff @(posedge clk) q <= q + 1;
endmodule
module Top;
  logic clk = 0;
  always #1 clk = ~clk;
  logic [15:0] falls = 0, copy_falls = 0;
  always @(negedge clk) falls <= falls + 1;
  logic c;
  assign c = clk;
  always_ff @(negedge c) copy_falls <= copy_falls + 1;
",
    );
    for index in 0..UNITS {
        source += &format!("  logic [15:0] q{index};\n  Unit u{index}(.clk(clk), .q(q{index}));\n");
    }
    source += "  logic [15:0] rises;\n  always_comb begin\n    rises = 0;\n";
    for index in 0..UNITS {
        source += &format!("    rises = rises + q{index};\n");
    }
    source += "  end\n  initial begin\n    #41 $display(\"%0d %0d %0d %0d\", rises, q3, falls, copy_falls);\n    $finish;\n  end\nendmodule\n";
    source
}

fn lines<B: celox::SimBackend>(mut sim: Simulation<B>) -> Vec<String> {
    sim.run_until(100).unwrap();
    sim.drain_runtime_events()
        .into_iter()
        .filter_map(|event| match event {
            RuntimeEvent::Display { message } => {
                Some(String::from_utf8_lossy(&message).into_owned())
            }
            _ => None,
        })
        .collect()
}

#[test]
fn opposite_edge_clock_copies_share_a_domain_and_keep_their_edges() {
    let source = source();
    // Rising edges at 1, 3, ..., 39 and falling ones at 2, 4, ..., 40.
    let expected = vec![format!("{} 20 20 20", 20 * UNITS)];
    for four_state in [false, true] {
        let builder = || {
            Simulation::from_sv_sources(vec![(source.as_str(), Path::new("copies.sv"))], "Top")
                .four_state(four_state)
        };
        assert_eq!(
            lines(builder().build_interpreter().unwrap()),
            expected,
            "interpreter, four_state={four_state}"
        );
        assert_eq!(
            lines(builder().build_native().unwrap()),
            expected,
            "native, four_state={four_state}"
        );
    }
}
