//! The inactive edge of a tick stores only the level when nothing can run on
//! it, and still runs every domain that can. Each design is checked as
//! `tick_semantics` checks the host API: the timed `Simulation` of the
//! statements a host operation stands for must observe the same values,
//! unknown bits included, after every operation:
//!
//! - `set(x, v)` is `x = v;`,
//! - `edge(e, lvl)` is `s <= lvl;`,
//! - `tick(e)` is `s <= A; #H; s <= I; #L;`,
//! - `tick_events(e1..ek)` makes the moves of all their signals together.

#![cfg(feature = "systemverilog")]

use std::path::Path;

use celox::{Edge, EventHandle, Simulation, Simulator};
use num_bigint::BigUint;

/// Inputs of every design, all one bit wide.
const INPUTS: [&str; 4] = ["clk", "clk2", "en", "rst"];

/// Event signals with their active levels.
const EVENTS: [(&str, u8); 3] = [("clk", 1), ("clk2", 1), ("rst", 1)];

/// A gated clock clocking only rising edges, and combinational logic reading
/// the clock: the inactive edge of `clk` runs nothing.
const GATED: &str = r#"
`timescale 1ns/1ns
module Dut (
    input  logic       clk,
    input  logic       clk2,
    input  logic       en,
    input  logic       rst,
    output logic [7:0] p,
    output logic [7:0] g,
    output logic [7:0] c2,
    output logic [7:0] lvl
);
    logic gclk;
    assign gclk = clk & en;
    always_ff @(posedge clk or posedge rst)
        if (rst) p <= 8'd0;
        else p <= p + 8'd1;
    always_ff @(posedge gclk or posedge rst)
        if (rst) g <= 8'd0;
        else g <= g + 8'd1;
    always_ff @(posedge clk2 or posedge rst)
        if (rst) c2 <= 8'd0;
        else c2 <= c2 + 8'd1;
    assign lvl = {6'd0, gclk, clk};
endmodule
"#;

/// A domain on the falling edge of a gated clock runs on the inactive edge.
const GATED_NEGEDGE: &str = r#"
`timescale 1ns/1ns
module Dut (
    input  logic       clk,
    input  logic       clk2,
    input  logic       en,
    input  logic       rst,
    output logic [7:0] p,
    output logic [7:0] g,
    output logic [7:0] ng,
    output logic [7:0] lvl
);
    logic gclk;
    assign gclk = clk & en;
    always_ff @(posedge clk or posedge rst)
        if (rst) p <= 8'd0;
        else p <= p + 8'd1;
    always_ff @(posedge gclk or posedge rst)
        if (rst) g <= 8'd0;
        else g <= g + 8'd1;
    always_ff @(negedge gclk or posedge rst)
        if (rst) ng <= 8'd0;
        else ng <= ng + 8'd1;
    assign lvl = {7'd0, clk};
endmodule
"#;

/// The rising edge of `~clk` is the falling edge of `clk`.
const INVERTED: &str = r#"
`timescale 1ns/1ns
module Dut (
    input  logic       clk,
    input  logic       clk2,
    input  logic       en,
    input  logic       rst,
    output logic [7:0] p,
    output logic [7:0] q,
    output logic [7:0] lvl
);
    logic nclk;
    assign nclk = ~clk;
    always_ff @(posedge clk or posedge rst)
        if (rst) p <= 8'd0;
        else p <= p + 8'd1;
    always_ff @(posedge nclk or posedge rst)
        if (rst) q <= 8'd0;
        else q <= q + 8'd1;
    assign lvl = {7'd0, clk};
endmodule
"#;

/// A register divider; with `en` low the divided clock stays where it is.
const DIVIDER: &str = r#"
`timescale 1ns/1ns
module Dut (
    input  logic       clk,
    input  logic       clk2,
    input  logic       en,
    input  logic       rst,
    output logic [7:0] p,
    output logic [7:0] d,
    output logic [7:0] lvl
);
    logic div;
    always_ff @(posedge clk or posedge rst)
        if (rst) begin
            p <= 8'd0;
            div <= 1'b0;
        end else begin
            p <= p + 8'd1;
            if (en) div <= ~div;
        end
    always_ff @(posedge div or posedge rst)
        if (rst) d <= 8'd0;
        else d <= d + 8'd1;
    assign lvl = {6'd0, div, clk};
endmodule
"#;

/// A plain clock nothing reads as data, and one a flip-flop samples.
const PLAIN: &str = r#"
`timescale 1ns/1ns
module Dut (
    input  logic       clk,
    input  logic       clk2,
    input  logic       en,
    input  logic       rst,
    output logic [7:0] p,
    output logic [7:0] q
);
    always_ff @(posedge clk or posedge rst)
        if (rst) p <= 8'd0;
        else p <= p + 8'd1;
    always_ff @(posedge clk2 or posedge rst)
        if (rst) q <= 8'd0;
        else q <= {6'd0, clk2, en};
endmodule
"#;

#[derive(Clone, Copy, Debug)]
enum Op {
    Set(&'static str, u8),
    Edge(&'static str, u8),
    Tick(&'static [&'static str]),
}

fn active(name: &str) -> u8 {
    EVENTS.iter().find(|(event, _)| *event == name).unwrap().1
}

/// The statements `op` stands for, with the half periods of a tick at 1.
fn statements(op: Op) -> String {
    match op {
        Op::Set(name, value) => format!("{name} = {value};"),
        Op::Edge(name, level) => format!("{name} <= {level};"),
        Op::Tick(names) => {
            let moves = |level: fn(u8) -> u8| {
                names
                    .iter()
                    .map(|name| format!("{name} <= {}; ", level(active(name))))
                    .collect::<String>()
            };
            format!("{}#1; {}#1;", moves(|a| a), moves(|a| 1 - a))
        }
    }
}

type Values = Vec<(BigUint, BigUint)>;

/// The outputs after each operation, from the timed simulation of the
/// operations' statements. Operation `k` starts at time `4k + 1`.
fn reference(dut: &str, outputs: &[&str], four_state: bool, ops: &[Op]) -> Vec<Values> {
    let mut body = String::new();
    for (k, &op) in ops.iter().enumerate() {
        body += &format!("        #({} - $time) {}\n", 4 * k + 1, statements(op));
    }
    let top = format!(
        r#"
`timescale 1ns/1ns
module Top;
    logic {inputs};
    wire [7:0] {outputs};
    Dut u (.{ports});
    initial begin
{body}    end
endmodule
"#,
        inputs = INPUTS.join(", "),
        outputs = outputs.join(", "),
        ports = INPUTS
            .iter()
            .chain(outputs)
            .copied()
            .collect::<Vec<_>>()
            .join(", ."),
    );
    let mut sim = Simulation::from_sv_sources(
        vec![(dut, Path::new("dut.sv")), (&top, Path::new("top.sv"))],
        "Top",
    )
    .four_state(four_state)
    .build()
    .unwrap();
    (0..ops.len())
        .map(|k| {
            sim.run_until(4 * k as u64 + 4).unwrap();
            outputs
                .iter()
                .map(|name| {
                    let signal = sim.signal(name);
                    sim.get_four_state(signal)
                })
                .collect()
        })
        .collect()
}

fn hosted(
    dut: &str,
    outputs: &[&str],
    four_state: bool,
    inactive_runs: bool,
    ops: &[Op],
) -> Vec<Values> {
    let mut sim = Simulator::from_sv_sources(vec![(dut, Path::new("dut.sv"))], "Dut")
        .four_state(four_state)
        .build()
        .unwrap();
    let clk = sim.event("clk");
    assert_eq!(
        sim.event_edges()[clk.id()].inactive_runs,
        inactive_runs,
        "whether the inactive edge of clk can run a domain"
    );
    ops.iter()
        .map(|&op| {
            match op {
                Op::Set(name, value) => {
                    let signal = sim.signal(name);
                    sim.set(signal, value);
                }
                Op::Edge(name, level) => {
                    let event = sim.event(name);
                    let edge = if level == 1 {
                        Edge::Rising
                    } else {
                        Edge::Falling
                    };
                    sim.edge(event, edge).unwrap();
                }
                Op::Tick([name]) => {
                    let event = sim.event(name);
                    sim.tick(event).unwrap();
                }
                Op::Tick(names) => {
                    let events: Vec<_> = names.iter().map(|name| sim.event(name)).collect();
                    sim.tick_events(&events).unwrap();
                }
            }
            outputs
                .iter()
                .map(|name| {
                    let signal = sim.signal(name);
                    sim.get_four_state(signal)
                })
                .collect()
        })
        .collect()
}

/// Operations before [`RESET`] leave the counters unknown, and the timed
/// simulation's start-up settle may already clock them; they are not
/// compared.
const RESET: usize = 4;

fn check(dut: &str, outputs: &[&str], inactive_runs: bool, ops: &[Op]) {
    for four_state in [false, true] {
        let expected = reference(dut, outputs, four_state, ops);
        let actual = hosted(dut, outputs, four_state, inactive_runs, ops);
        for (k, (expected, actual)) in expected.iter().zip(&actual).enumerate().skip(RESET) {
            assert_eq!(
                actual, expected,
                "four_state {four_state}: after {:?} (operation {k}), outputs {outputs:?}",
                ops[k]
            );
        }
    }
}

/// Reset, then periods with the gate open, closed and open again. The clock
/// starts unknown in a four-state design.
const PERIODS: &[Op] = &[
    Op::Set("en", 0),
    Op::Set("clk2", 0),
    Op::Edge("rst", 1),
    Op::Edge("rst", 0),
    Op::Tick(&["clk"]),
    Op::Set("en", 1),
    Op::Tick(&["clk"]),
    Op::Tick(&["clk"]),
    Op::Set("en", 0),
    Op::Tick(&["clk"]),
    Op::Set("en", 1),
    Op::Tick(&["clk"]),
];

/// A gated clock that clocks only rising edges: combinational logic reading
/// the clock and the gated clock sees both low after every tick, and ticks
/// coincident with another clock still gate.
#[test]
fn a_gated_rising_edge_domain_leaves_the_inactive_edge_quiet() {
    let mut ops = PERIODS.to_vec();
    ops.extend([
        Op::Tick(&["clk", "clk2"]),
        Op::Set("en", 0),
        Op::Tick(&["clk", "clk2"]),
        Op::Set("en", 1),
        Op::Edge("clk", 1),
        Op::Tick(&["clk"]),
        Op::Tick(&["clk"]),
    ]);
    check(GATED, &["p", "g", "c2", "lvl"], false, &ops);
}

/// A domain on the falling edge of a gated clock runs on every tick that
/// opens the gate.
#[test]
fn a_gated_falling_edge_domain_runs_on_every_tick() {
    check(GATED_NEGEDGE, &["p", "g", "ng", "lvl"], true, PERIODS);
}

/// The rising edge of an inverted clock runs on the inactive edge.
#[test]
fn an_inverted_clock_domain_runs_on_the_inactive_edge() {
    check(INVERTED, &["p", "q", "lvl"], true, PERIODS);
}

/// A divided domain runs when the divider rises and not while the divider
/// holds.
#[test]
fn a_divided_domain_runs_only_when_the_divider_rises() {
    check(DIVIDER, &["p", "d", "lvl"], false, PERIODS);
}

/// A plain tick whose level stores nothing observes is one run of the
/// domain; a domain that samples its clock still sees it high. A tick from
/// an unknown or active level still moves the signal.
#[test]
fn a_plain_tick_skips_only_unobservable_level_stores() {
    for four_state in [false, true] {
        let sim = Simulator::from_sv_sources(vec![(PLAIN, Path::new("dut.sv"))], "Dut")
            .four_state(four_state)
            .build()
            .unwrap();
        let level_read = |name| sim.event_edges()[sim.event(name).id()].level_read;
        assert!(!level_read("clk"), "nothing reads the level of clk");
        assert!(level_read("clk2"), "a flip-flop samples clk2");
    }
    let mut ops = PERIODS.to_vec();
    ops.extend([
        Op::Tick(&["clk2"]),
        Op::Set("en", 1),
        Op::Tick(&["clk2"]),
        Op::Edge("clk", 1),
        Op::Tick(&["clk"]),
        Op::Tick(&["clk"]),
        Op::Edge("clk2", 1),
        Op::Tick(&["clk2"]),
    ]);
    check(PLAIN, &["p", "q"], false, &ops);
}
