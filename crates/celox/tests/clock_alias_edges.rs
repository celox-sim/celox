#![cfg(feature = "systemverilog")]
//! An aliased clock or reset keeps the edge its event control names.
//!
//! `assign g = clk;` makes `g` and `clk` one clock domain, so the domain that
//! the scheduler drives is the port. The domain must still be the
//! edge-triggered domain the `always_ff` control names: a buffered clock is
//! not a double-edge clock, and releasing an asynchronous reset is not a
//! clock edge.

use celox::Simulation;

fn simulation(source: &'static str) -> Simulation {
    Simulation::from_sv_sources(
        vec![(source, std::path::Path::new("clock_alias.sv"))],
        "Top",
    )
    .build()
    .unwrap_or_else(|error| panic!("{error:?}"))
}

/// A register on a buffered clock counts rising edges only, while the clock
/// also has falling edges. Icarus and Verilator both count 4 here.
#[test]
fn a_buffered_clock_counts_rising_edges_only() {
    let mut sim = simulation(
        "module Top(input logic clk, output logic [31:0] cnt);
           logic g1;
           assign g1 = clk;
           logic [31:0] r;
           always_ff @(posedge g1) r <= r + 1;
           assign cnt = r;
         endmodule",
    );
    sim.add_clock("clk", 10, 3); // rising 3, 13, 23, 33; falling 8, 18, 28
    let cnt = sim.signal("cnt");
    sim.run_until(35).unwrap();
    assert_eq!(sim.get(cnt), 4u8.into());
}

/// The same buffered clock with a `negedge` register counts falling edges
/// only: Icarus and Verilator both count 3 here.
#[test]
fn a_buffered_clock_counts_falling_edges_only() {
    let mut sim = simulation(
        "module Top(input logic clk, output logic [31:0] cnt);
           logic g1;
           assign g1 = clk;
           logic [31:0] r;
           always_ff @(negedge g1) r <= r + 1;
           assign cnt = r;
         endmodule",
    );
    sim.add_clock("clk", 10, 3); // rising 3, 13, 23, 33; falling 8, 18, 28
    let cnt = sim.signal("cnt");
    sim.run_until(35).unwrap();
    assert_eq!(sim.get(cnt), 3u8.into());
}

/// An asynchronous reset routed through a buffer keeps its level sensitivity:
/// asserting it acts between clock edges, and releasing it must not run the
/// register's clocked body.
#[test]
fn an_aliased_async_reset_does_not_clock_on_release() {
    let mut sim = simulation(
        "module Top(input logic clk, input logic rst_p, output logic [31:0] q);
           logic rn;
           assign rn = rst_p;
           logic [31:0] r;
           always_ff @(posedge clk or negedge rn) if (!rn) r <= 0; else r <= r + 1;
           assign q = r;
         endmodule",
    );
    sim.add_clock("clk", 10, 5); // rising 5, 15, 25, 35, 45
    sim.schedule("rst_p", 0, 1).unwrap(); // released: not an edge either
    sim.schedule("rst_p", 17, 0).unwrap(); // asserted between edges
    sim.schedule("rst_p", 22, 1).unwrap(); // released between edges
    let q = sim.signal("q");

    sim.run_until(20).unwrap();
    assert_eq!(sim.get(q), 0u8.into(), "t=15 counts, t=17 clears");
    sim.run_until(22).unwrap();
    assert_eq!(sim.get(q), 0u8.into(), "releasing the reset is not a clock");
    sim.run_until(45).unwrap();
    assert_eq!(sim.get(q), 3u8.into(), "rising edges at 25, 35 and 45");
}

#[test]
fn generated_clock_instances_keep_their_own_edge_kind() {
    let mut sim = simulation(
        r#"
module Generator(input logic clk, output logic g);
    initial g = 0;
    always_ff @(posedge clk) g <= ~g;
endmodule
module Top(input logic cp, cn, output logic [7:0] p, n);
    logic gp, gn;
    Generator up(.clk(cp), .g(gp));
    Generator un(.clk(cn), .g(gn));
    initial begin p = 0; n = 0; end
    always_ff @(posedge gp) p <= p + 1;
    always_ff @(negedge gn) n <= n + 1;
endmodule
"#,
    );
    sim.add_clock("cp", 10, 0);
    sim.add_clock("cn", 10, 5);
    sim.run_until(25).unwrap();
    assert_eq!(sim.get(sim.signal("p")), 2u8.into());
    assert_eq!(sim.get(sim.signal("n")), 1u8.into());
}
