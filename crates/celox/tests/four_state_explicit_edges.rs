#![cfg(feature = "systemverilog")]
//! Explicit edges of four-state clocks that are x or z follow IEEE
//! 1800-2023 9.4.2, Table 9-2: a rising edge from x or z is a posedge, a
//! falling one a negedge, and a gated clock is judged from the clock's real
//! level before the edge. On plain events, gated clocks and register
//! clocks, on every backend.

use celox::{BigUint, Edge, SimBackend, Simulator};

/// `clkp` and `clkn` clock plain posedge and negedge counters; `clkg` gates
/// `gc = clkg & en`; `clkd` clocks a register `r` whose posedges are
/// counted.
const SOURCE: &str = "module Top(input logic clkp, input logic clkn, input logic clkg,
    input logic clkd, input logic en, input logic d,
    output logic [7:0] pos, output logic [7:0] neg, output logic [7:0] gpos,
    output logic [7:0] dpos);
  logic [7:0] p = 8'd0;
  logic [7:0] n = 8'd0;
  logic [7:0] g = 8'd0;
  logic [7:0] t = 8'd0;
  logic [7:0] q = 8'd0;
  logic r = 1'b0;
  logic gc;
  assign gc = clkg & en;
  always_ff @(posedge clkp) p <= p + 8'd1;
  always_ff @(negedge clkn) n <= n + 8'd1;
  always_ff @(posedge clkg) t <= t + 8'd1;
  always_ff @(posedge gc) g <= g + 8'd1;
  always_ff @(posedge clkd) r <= d;
  always_ff @(posedge r) q <= q + 8'd1;
  assign pos = p;
  assign neg = n;
  assign gpos = g;
  assign dpos = q;
endmodule";

const X: (u8, u8) = (1, 1);
const Z: (u8, u8) = (0, 1);
const ZERO: (u8, u8) = (0, 0);
const ONE: (u8, u8) = (1, 0);

fn set<B: SimBackend>(sim: &mut Simulator<B>, name: &str, (value, mask): (u8, u8)) {
    let signal = sim.signal(name);
    sim.modify(|io| io.set_four_state(signal, BigUint::from(value), BigUint::from(mask)))
        .unwrap();
}

fn get<B: SimBackend>(sim: &mut Simulator<B>, name: &str) -> u64 {
    let signal = sim.signal(name);
    sim.get(signal).try_into().unwrap()
}

fn edge<B: SimBackend>(sim: &mut Simulator<B>, event: &str, edge: Edge) {
    let event = sim.event(event);
    sim.edge(event, edge).unwrap();
}

fn check<B: SimBackend>(mut sim: Simulator<B>, backend: &str) {
    let counts = |sim: &mut Simulator<B>| ["pos", "neg", "gpos", "dpos"].map(|name| get(sim, name));

    // Plain events: x -> 1 is a posedge, z -> 0 a negedge, and the
    // inactive x -> 0 runs nothing.
    set(&mut sim, "clkp", X);
    edge(&mut sim, "clkp", Edge::Rising);
    set(&mut sim, "clkn", Z);
    edge(&mut sim, "clkn", Edge::Falling);
    assert_eq!(counts(&mut sim), [1, 1, 0, 0], "{backend}: plain from x/z");
    set(&mut sim, "clkp", X);
    edge(&mut sim, "clkp", Edge::Falling);
    set(&mut sim, "clkn", X);
    edge(&mut sim, "clkn", Edge::Rising);
    assert_eq!(counts(&mut sim), [1, 1, 0, 0], "{backend}: inactive from x");
    sim.tick(sim.event("clkp")).unwrap();
    assert_eq!(counts(&mut sim), [2, 1, 0, 0], "{backend}: tick after x");

    // A gated clock: `x & 1` -> `1 & 1` rises; `x & x` -> `1 & x` stays x.
    set(&mut sim, "en", ONE);
    set(&mut sim, "clkg", X);
    edge(&mut sim, "clkg", Edge::Rising);
    assert_eq!(counts(&mut sim)[2], 1, "{backend}: gated x -> 1");
    set(&mut sim, "en", X);
    set(&mut sim, "clkg", X);
    edge(&mut sim, "clkg", Edge::Rising);
    assert_eq!(counts(&mut sim)[2], 1, "{backend}: gated x -> x is no edge");
    // `z & 1` is x: z -> 1 of the clock is x -> 1 of the gated clock.
    set(&mut sim, "en", ONE);
    set(&mut sim, "clkg", Z);
    edge(&mut sim, "clkg", Edge::Rising);
    assert_eq!(counts(&mut sim)[2], 2, "{backend}: gated z -> 1");
    // `0 & 1` -> `1 & 1`, with the clock coming from 0.
    edge(&mut sim, "clkg", Edge::Falling);
    edge(&mut sim, "clkg", Edge::Rising);
    assert_eq!(counts(&mut sim)[2], 3, "{backend}: gated 0 -> 1");

    // A register clock: `r` goes 0 -> x -> 1, two posedges, through edges
    // of a clock that starts at z.
    set(&mut sim, "clkd", Z);
    set(&mut sim, "d", X);
    edge(&mut sim, "clkd", Edge::Rising);
    assert_eq!(counts(&mut sim)[3], 1, "{backend}: register 0 -> x");
    set(&mut sim, "d", ONE);
    edge(&mut sim, "clkd", Edge::Falling);
    edge(&mut sim, "clkd", Edge::Rising);
    assert_eq!(counts(&mut sim)[3], 2, "{backend}: register x -> 1");
    set(&mut sim, "d", ZERO);
    sim.tick(sim.event("clkd")).unwrap();
    assert_eq!(counts(&mut sim)[3], 2, "{backend}: register 1 -> 0");

    // Coincident edges from x judge the gated clock from x too.
    set(&mut sim, "en", X);
    set(&mut sim, "clkg", X);
    set(&mut sim, "clkp", X);
    let moves = [
        (sim.event("clkg"), Edge::Rising),
        (sim.event("clkp"), Edge::Rising),
    ];
    sim.edges(&moves).unwrap();
    assert_eq!(counts(&mut sim)[..3], [3, 1, 3], "{backend}: coincident");
}

#[test]
fn explicit_edges_from_unknown_levels_follow_table_9_2() {
    let build = || {
        Simulator::from_sv_sources(
            vec![(SOURCE, std::path::Path::new("four_state_explicit_edges.sv"))],
            "Top",
        )
        .four_state(true)
    };
    check(build().build_interpreter().unwrap(), "interp");
    check(build().build_cranelift().unwrap(), "cranelift");
    check(build().build_wasm().unwrap(), "wasm");
    check(build().build_tiered().unwrap(), "tiered");
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    check(build().build_native().unwrap(), "native");
}
