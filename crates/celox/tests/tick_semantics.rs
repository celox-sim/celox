//! The event-based `Simulator` API behaves as the SystemVerilog the execution
//! semantics guide defines it by. Each host operation is rewritten into those
//! statements of an `initial` process, which the timed `Simulation` runs, and
//! both must observe the same values after every operation:
//!
//! - `set(x, v)` is `x = v;`,
//! - `edge(e, lvl)` is `s <= lvl;`,
//! - `tick(e)` is `s <= A; #H; s <= I; #L;`,
//! - `tick_events(e1..ek)` makes the moves of all their signals together,
//!
//! where `s` is the event's signal, `A` its active level and `I` its inactive
//! one.

#![cfg(feature = "systemverilog")]

use std::path::Path;

use celox::{Edge, Simulation, Simulator};

const DUT: &str = r#"
`timescale 1ns/1ns
module Dut (
    input  logic       clk,
    input  logic       clk2,
    input  logic       en,
    input  logic       rst_n,
    output logic [7:0] p,
    output logic [7:0] n,
    output logic [7:0] g,
    output logic [7:0] ng,
    output logic [7:0] r,
    output logic [7:0] c2,
    output logic [7:0] s,
    output logic [7:0] d,
    output logic [7:0] e,
    output logic       lvl
);
    logic gclk;
    logic half = 1'b0;
    logic ren = 1'b0;
    logic rclk;
    assign gclk = clk & en;
    // A clock divided from `clk`, and a gate whose enable `clk`'s own domain
    // toggles.
    always_ff @(posedge clk) half <= ~half;
    always_ff @(posedge half) d <= d + 8'd1;
    always_ff @(posedge clk) ren <= ~ren;
    assign rclk = clk & ren;
    always_ff @(posedge rclk) e <= e + 8'd1;
    always_ff @(posedge clk) p <= p + 8'd1;
    always_ff @(negedge clk) n <= n + 8'd1;
    always_ff @(posedge gclk) g <= g + 8'd1;
    always_ff @(negedge gclk) ng <= ng + 8'd1;
    always_ff @(posedge clk or negedge rst_n)
        if (!rst_n) r <= 8'd0;
        else r <= r + 8'd1;
    always_ff @(posedge clk2) c2 <= c2 + 8'd1;
    always_ff @(posedge clk) s <= c2;
    assign lvl = clk;
endmodule
"#;

const OUTPUTS: [&str; 10] = ["p", "n", "g", "ng", "r", "c2", "s", "d", "e", "lvl"];

/// Event signals with their active levels.
const EVENTS: [(&str, u8); 3] = [("clk", 1), ("clk2", 1), ("rst_n", 0)];

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

/// The values of `OUTPUTS` after each operation, from the timed simulation of
/// the operations' statements. Operation `k` starts at time `4k + 1`.
fn reference(ops: &[Op]) -> Vec<Vec<u64>> {
    let mut body = String::new();
    for (k, &op) in ops.iter().enumerate() {
        body += &format!("        #({} - $time) {}\n", 4 * k + 1, statements(op));
    }
    let top = format!(
        r#"
`timescale 1ns/1ns
module Top;
    logic clk, clk2, en, rst_n;
    wire [7:0] p, n, g, ng, r, c2, s, d, e;
    wire lvl;
    Dut u (
        .clk, .clk2, .en, .rst_n, .p, .n, .g, .ng, .r, .c2, .s, .d, .e, .lvl
    );
    initial begin
{body}    end
endmodule
"#
    );
    let mut sim = Simulation::from_sv_sources(
        vec![(DUT, Path::new("dut.sv")), (&top, Path::new("top.sv"))],
        "Top",
    )
    .four_state(false)
    .build()
    .unwrap();
    (0..ops.len())
        .map(|k| {
            sim.run_until(4 * k as u64 + 4).unwrap();
            OUTPUTS
                .iter()
                .map(|name| {
                    let signal = sim.signal(name);
                    sim.get(signal).try_into().unwrap()
                })
                .collect()
        })
        .collect()
}

/// The values of `OUTPUTS` after each operation through the `Simulator`
/// API, with derived clock domains run by their generated edge units, or by
/// walking their plans when `walk`.
fn hosted(ops: &[Op], walk: bool) -> Vec<Vec<u64>> {
    let builder =
        Simulator::from_sv_sources(vec![(DUT, Path::new("dut.sv"))], "Dut").four_state(false);
    let builder = if walk {
        builder.disable_pass(celox::SirPass::DerivedEdgeUnits)
    } else {
        builder
    };
    let mut sim = builder.build().unwrap();
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
            OUTPUTS
                .iter()
                .map(|name| {
                    let signal = sim.signal(name);
                    sim.get(signal).try_into().unwrap()
                })
                .collect()
        })
        .collect()
}

fn check(ops: &[Op]) {
    check_except(ops, &[]);
}

/// [`check`] of every output but `skipped`.
fn check_except(ops: &[Op], skipped: &[&str]) {
    let kept: Vec<usize> = (0..OUTPUTS.len())
        .filter(|&index| !skipped.contains(&OUTPUTS[index]))
        .collect();
    let select = |values: &Vec<u64>| kept.iter().map(|&index| values[index]).collect::<Vec<_>>();
    let expected = reference(ops);
    for walk in [false, true] {
        let actual = hosted(ops, walk);
        for (k, (expected, actual)) in expected.iter().zip(&actual).enumerate() {
            assert_eq!(
                select(actual),
                select(expected),
                "after {:?} (operation {k}, walk {walk}), outputs {OUTPUTS:?} without {skipped:?}",
                ops[k]
            );
        }
    }
}

/// A tick runs the domains of both edges of its signal and of a clock gated
/// from it, and leaves the signal at its inactive level.
#[test]
fn a_tick_is_a_clock_period() {
    check(&[
        Op::Edge("rst_n", 1),
        Op::Set("en", 1),
        Op::Tick(&["clk"]),
        Op::Tick(&["clk"]),
        Op::Set("en", 0),
        Op::Tick(&["clk"]),
    ]);
}

/// A signal already at its active level takes no active edge: a tick of an
/// active-low reset that starts low only releases it.
#[test]
fn a_tick_from_the_active_level_takes_only_the_inactive_edge() {
    check(&[
        Op::Tick(&["rst_n"]),
        Op::Tick(&["clk"]),
        Op::Edge("rst_n", 0),
        Op::Tick(&["clk"]),
        Op::Edge("rst_n", 1),
        Op::Edge("clk", 1),
        Op::Tick(&["clk"]),
        Op::Tick(&["clk"]),
    ]);
}

const COINCIDENT: [Op; 5] = [
    Op::Edge("rst_n", 1),
    Op::Tick(&["clk2"]),
    Op::Tick(&["clk", "clk2"]),
    Op::Tick(&["clk", "clk2"]),
    Op::Tick(&["clk"]),
];

/// Coincident ticks sample each other's values from before the edge. The
/// commit-raised gate's domain (`e`) is checked against Icarus below until
/// the timed reference sees its edge in a coincident batch (#1860).
#[test]
fn ticks_of_several_events_are_coincident() {
    check_except(&COINCIDENT, &["e"]);
}

/// At the second coincident tick `clk` rises while `ren` is already high, so
/// `rclk` rises in the active region and `e` counts: Icarus 13.0 gives
/// `e` = 0 0 1 2 3 after the operations. The timed reference gives 0 0 1 1 2
/// (#1860); both `Simulator` paths agree with Icarus.
#[test]
#[ignore = "#1860"]
fn coincident_ticks_run_the_domain_of_a_gate_the_commit_raised() {
    let e = OUTPUTS.iter().position(|name| *name == "e").unwrap();
    let values = |rows: Vec<Vec<u64>>| rows.iter().map(|row| row[e]).collect::<Vec<_>>();
    let icarus = vec![0, 0, 1, 2, 3];
    assert_eq!(
        values(reference(&COINCIDENT)),
        icarus,
        "the timed reference"
    );
    for walk in [false, true] {
        assert_eq!(values(hosted(&COINCIDENT, walk)), icarus, "walk {walk}");
    }
}
