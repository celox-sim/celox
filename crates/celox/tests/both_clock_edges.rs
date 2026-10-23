//! `always_ff` processes sensed at both edges of one signal (#443).
//!
//! A signal carries one domain kind, so the frontend senses every further
//! edge or role of a signal on a hidden whole-variable copy of it, which runs
//! as a clock derived from the signal. These tests drive the timed
//! `Simulation` and check that each process runs at the edges IEEE 1800-2023
//! 9.4.2 defines, evaluating against the state before the edge (4.4.2,
//! 10.4.2), and that the copy is invisible.
//!
//! The event-based `Simulator`, whose `tick` is a clock period, is covered by
//! `tick_semantics.rs`.

#![cfg(feature = "systemverilog")]

use celox::{BigUint, OptLevel, Simulation};

fn build(source: &str, four_state: bool) -> Simulation {
    build_with(source, four_state, OptLevel::O1)
}

fn build_with(source: &str, four_state: bool, opt_level: OptLevel) -> Simulation {
    let path = std::path::Path::new("both_clock_edges.sv");
    Simulation::from_sv_sources(vec![(source, path)], "Top")
        .four_state(four_state)
        .opt_level(opt_level)
        .build()
        .unwrap()
}

fn get(sim: &mut Simulation, name: &str) -> u64 {
    let signal = sim.signal(name);
    sim.get(signal).try_into().unwrap()
}

/// Pulses `rst` high at time 0 with `clk` low, then applies `cycles` clock
/// periods of 10 (rising at 10, 20, ...; falling 5 later) and returns the
/// values of `outputs` after every edge, rising edge first.
fn run(sim: &mut Simulation, cycles: u64, outputs: &[&str]) -> Vec<Vec<u64>> {
    sim.schedule("clk", 0, 0).unwrap();
    sim.schedule("rst", 0, 1).unwrap();
    sim.step().unwrap();
    sim.schedule("rst", 1, 0).unwrap();
    sim.step().unwrap();
    let mut observed = Vec::new();
    for cycle in 0..cycles {
        for (offset, level) in [(10, 1), (15, 0)] {
            sim.schedule("clk", cycle * 10 + offset, level).unwrap();
            sim.step().unwrap();
            observed.push(outputs.iter().map(|name| get(sim, name)).collect());
        }
    }
    observed
}

/// `rise` and `fall` count the edges of `clk`; `at_fall` samples `rise` at
/// each falling edge and `at_rise` samples `fall` at each rising edge.
fn expected_counters(cycles: u64) -> Vec<Vec<u64>> {
    (1..=cycles)
        .flat_map(|k| [vec![k, k - 1, k - 1, k - 1], vec![k, k, k, k - 1]])
        .collect()
}

const COUNTERS: [&str; 4] = ["rise", "fall", "at_fall", "at_rise"];

fn counters_source(negedge_first: bool) -> String {
    let rise = "always_ff @(posedge clk or posedge rst)
              if (rst) begin rise <= 0; at_rise <= 0; end
              else begin rise <= rise + 1; at_rise <= fall; end";
    let fall = "always_ff @(negedge clk or posedge rst)
              if (rst) begin fall <= 0; at_fall <= 0; end
              else begin fall <= fall + 1; at_fall <= rise; end";
    let (first, second) = if negedge_first {
        (fall, rise)
    } else {
        (rise, fall)
    };
    format!(
        "module Top(input logic clk, input logic rst,
                    output logic [7:0] rise, output logic [7:0] fall,
                    output logic [7:0] at_fall, output logic [7:0] at_rise);
           {first}
           {second}
         endmodule"
    )
}

#[test]
fn processes_at_both_edges_of_one_clock() {
    for negedge_first in [false, true] {
        for four_state in [false, true] {
            for opt_level in [OptLevel::O0, OptLevel::O1, OptLevel::O2] {
                let mut sim = build_with(&counters_source(negedge_first), four_state, opt_level);
                assert_eq!(
                    run(&mut sim, 4, &COUNTERS),
                    expected_counters(4),
                    "negedge first: {negedge_first}, four-state: {four_state}, {opt_level:?}"
                );
            }
        }
    }
}

/// A child module senses both edges of its clock port, the falling edge in
/// its first process. The child's rising-edge register `c` samples the
/// parent's register `p` before the parent's rising edge commits, and the
/// falling edge runs on its own.
#[test]
fn both_edges_of_a_child_clock_port() {
    let source = "module Child(input logic clk, input logic rst, input logic [7:0] p,
                               output logic [7:0] c, output logic [7:0] f);
           always_ff @(negedge clk or posedge rst) if (rst) f <= 0; else f <= f + 1;
           always_ff @(posedge clk or posedge rst) if (rst) c <= 0; else c <= p;
         endmodule
         module Top(input logic clk, input logic rst,
                    output logic [7:0] p, output logic [7:0] c, output logic [7:0] f);
           always_ff @(posedge clk or posedge rst) if (rst) p <= 0; else p <= p + 1;
           Child u(.clk(clk), .rst(rst), .p(p), .c(c), .f(f));
         endmodule";
    let expected: Vec<Vec<u64>> = (1..=4)
        .flat_map(|k| [vec![k, k - 1, k - 1], vec![k, k - 1, k]])
        .collect();
    for four_state in [false, true] {
        let mut sim = build(source, four_state);
        assert_eq!(
            run(&mut sim, 4, &["p", "c", "f"]),
            expected,
            "four-state: {four_state}"
        );
    }
}

/// Both edges reached through whole-variable copies of `clk`, and both edges
/// of one copy.
#[test]
fn both_edges_through_clock_copies() {
    let source = "module Top(input logic clk, input logic rst,
                    output logic [7:0] rise, output logic [7:0] fall,
                    output logic [7:0] at_fall, output logic [7:0] at_rise);
           logic c1, c2;
           assign c1 = clk;
           assign c2 = c1;
           always_ff @(posedge c2 or posedge rst) if (rst) rise <= 0; else rise <= rise + 1;
           always_ff @(negedge c2 or posedge rst) if (rst) fall <= 0; else fall <= fall + 1;
           always_ff @(negedge c1 or posedge rst) if (rst) at_fall <= 0; else at_fall <= rise;
           always_ff @(posedge clk or posedge rst) if (rst) at_rise <= 0; else at_rise <= fall;
         endmodule";
    for four_state in [false, true] {
        let mut sim = build(source, four_state);
        assert_eq!(
            run(&mut sim, 4, &COUNTERS),
            expected_counters(4),
            "four-state: {four_state}"
        );
    }
}

/// One asynchronous reset sensed active high by one process and active low by
/// another.
#[test]
fn a_reset_at_both_edges() {
    let source = "module Top(input logic clk, input logic rst,
                    output logic [7:0] high, output logic [7:0] low);
           always_ff @(posedge clk or posedge rst) if (rst) high <= 0; else high <= high + 1;
           always_ff @(posedge clk or negedge rst) if (!rst) low <= 0; else low <= low + 1;
         endmodule";
    for four_state in [false, true] {
        let mut sim = build(source, four_state);
        let initial_low = get(&mut sim, "low");
        let mut observed = Vec::new();
        let mut time = 0;
        let mut edge = |sim: &mut Simulation, port: &str, level: u64| {
            sim.schedule(port, time, level).unwrap();
            sim.step().unwrap();
            time += 5;
            (get(sim, "high"), get(sim, "low"))
        };
        // `rst` high clears `high`; its fall clears `low`.
        edge(&mut sim, "clk", 0);
        observed.push(edge(&mut sim, "rst", 1));
        for _ in 0..2 {
            observed.push(edge(&mut sim, "clk", 1));
            edge(&mut sim, "clk", 0);
        }
        observed.push(edge(&mut sim, "rst", 0));
        for _ in 0..2 {
            observed.push(edge(&mut sim, "clk", 1));
            edge(&mut sim, "clk", 0);
        }
        observed.push(edge(&mut sim, "rst", 1));
        observed.push(edge(&mut sim, "clk", 1));
        // Before `rst` first falls, `low` is unknown, and stays unknown when
        // incremented. In four-state, `rst` rises from its initial unknown
        // level, a posedge but no negedge (IEEE 1800-2023 Table 9-2), so
        // `low` keeps its initial value until the first clock edge.
        let low = |count: u64| if four_state { 0xff } else { count };
        assert_eq!(
            observed,
            vec![
                (0, if four_state { initial_low } else { 0 }),
                (0, low(1)),
                (0, low(2)),
                (0, 0),
                (1, 0),
                (2, 0),
                (0, 0),
                (0, 1),
            ],
            "four-state: {four_state}"
        );
    }
}

/// One signal as the asynchronous reset of one process and the clock of
/// others, at the same and at the opposite edge.
#[test]
fn a_signal_as_reset_and_as_clock() {
    let source = "module Top(input logic clk, input logic rst,
                    output logic [7:0] cnt, output logic [7:0] rises, output logic [7:0] falls);
           always_ff @(posedge clk or posedge rst) if (rst) cnt <= 0; else cnt <= cnt + 1;
           always_ff @(posedge rst) rises <= rises + 1;
           always_ff @(negedge rst) falls <= falls + 1;
           initial begin rises = 0; falls = 0; end
         endmodule";
    for four_state in [false, true] {
        let mut sim = build(source, four_state);
        let mut time = 0;
        let mut edge = |sim: &mut Simulation, port: &str, level: u64| {
            sim.schedule(port, time, level).unwrap();
            sim.step().unwrap();
            time += 5;
            (get(sim, "cnt"), get(sim, "rises"), get(sim, "falls"))
        };
        edge(&mut sim, "clk", 0);
        // A four-state `rst` starts unknown, and its move to 0 is a negedge
        // (IEEE 1800-2023 Table 9-2); a two-state one starts at 0.
        let (_, _, initial_falls) = edge(&mut sim, "rst", 0);
        assert_eq!(
            initial_falls,
            u64::from(four_state),
            "four-state: {four_state}"
        );
        let mut observed = Vec::new();
        for _ in 0..2 {
            observed.push(edge(&mut sim, "rst", 1));
            observed.push(edge(&mut sim, "clk", 1));
            edge(&mut sim, "clk", 0);
            observed.push(edge(&mut sim, "rst", 0));
            observed.push(edge(&mut sim, "clk", 1));
            edge(&mut sim, "clk", 0);
        }
        let f = initial_falls;
        assert_eq!(
            observed,
            vec![
                (0, 1, f),
                (0, 1, f),
                (0, 1, f + 1),
                (1, 1, f + 1),
                (0, 2, f + 1),
                (0, 2, f + 1),
                (0, 2, f + 2),
                (1, 2, f + 2),
            ],
            "four-state: {four_state}"
        );
    }
}

/// Drives `d` through `levels` (value, unknown mask) and pulses `clk` after
/// each, so that the register `q` takes each level in turn, starting from
/// unknown. Returns the outputs `pos` and `neg` after each level.
fn register_edges(source: &str, levels: &[(u8, u8)]) -> Vec<(u64, u64)> {
    let mut sim = build(source, true);
    let d = sim.signal("d");
    sim.schedule("clk", 0, 0).unwrap();
    sim.schedule("rst", 0, 1).unwrap();
    sim.step().unwrap();
    sim.schedule("rst", 1, 0).unwrap();
    sim.step().unwrap();
    let mut time = 10;
    let mut observed = Vec::new();
    for &(value, mask) in levels {
        sim.modify(|io| io.set_four_state(d, BigUint::from(value), BigUint::from(mask)))
            .unwrap();
        sim.schedule("clk", time, 1).unwrap();
        sim.step().unwrap();
        sim.schedule("clk", time + 5, 0).unwrap();
        sim.step().unwrap();
        time += 10;
        observed.push((get(&mut sim, "pos"), get(&mut sim, "neg")));
    }
    observed
}

fn register_source(posedge: bool, negedge: bool) -> String {
    let pos = if posedge {
        "always_ff @(posedge q or posedge rst) if (rst) pos <= 0; else pos <= pos + 1;"
    } else {
        "assign pos = 0;"
    };
    let neg = if negedge {
        "always_ff @(negedge q or posedge rst) if (rst) neg <= 0; else neg <= neg + 1;"
    } else {
        "assign neg = 0;"
    };
    format!(
        "module Top(input logic clk, input logic rst, input logic d,
                    output logic [7:0] pos, output logic [7:0] neg);
           logic q;
           always_ff @(posedge clk) q <= d;
           {pos}
           {neg}
         endmodule"
    )
}

const LOW: (u8, u8) = (0, 0);
const HIGH: (u8, u8) = (1, 0);
const X: (u8, u8) = (1, 1);
const Z: (u8, u8) = (0, 1);

/// Both edges of a register that takes unknown and high-impedance values.
#[test]
fn both_edges_of_a_register_through_unknown_values() {
    // `q` starts unknown.
    let levels = [LOW, HIGH, LOW, X, LOW];
    assert_eq!(
        register_edges(&register_source(true, true), &levels),
        vec![
            (0, 1), // x -> 0: negedge
            (1, 1), // 0 -> 1: posedge
            (1, 2), // 1 -> 0: negedge
            (2, 2), // 0 -> x: posedge
            (2, 3), // x -> 0: negedge
        ]
    );
    // Through every transition between 0, 1, x, and z, each process runs
    // exactly when it runs in a design that senses only its edge of `q`.
    let levels = [HIGH, X, HIGH, Z, HIGH, LOW, Z, LOW, X, Z, X, LOW, HIGH];
    let both = register_edges(&register_source(true, true), &levels);
    let pos = register_edges(&register_source(true, false), &levels);
    let neg = register_edges(&register_source(false, true), &levels);
    assert_eq!(
        both,
        pos.iter()
            .zip(&neg)
            .map(|(&(pos, _), &(_, neg))| (pos, neg))
            .collect::<Vec<_>>()
    );
}

/// The hidden copy is no signal, event, or trace variable of the design.
#[test]
fn the_edge_copy_is_hidden() {
    let dir = tempfile::tempdir().unwrap();
    let vcd = dir.path().join("wave.vcd");
    let path = std::path::Path::new("both_clock_edges.sv");
    let source = counters_source(false);
    let mut sim = Simulation::from_sv_sources(vec![(source.as_str(), path)], "Top")
        .vcd(&vcd)
        .build()
        .unwrap();
    let names: Vec<_> = sim
        .named_signals()
        .into_iter()
        .map(|signal| signal.name)
        .collect();
    assert!(names.iter().all(|name| !name.contains('@')), "{names:?}");
    assert!(names.iter().any(|name| name == "clk"), "{names:?}");
    let events: Vec<_> = sim
        .named_events()
        .into_iter()
        .map(|event| event.name)
        .collect();
    assert_eq!(
        events.iter().filter(|name| name.contains("clk")).count(),
        1,
        "{events:?}"
    );
    assert!(events.iter().all(|name| !name.contains('@')), "{events:?}");
    let hierarchy = sim.named_hierarchy();
    assert!(
        hierarchy
            .signals
            .iter()
            .all(|signal| !signal.name.contains('@')),
        "hierarchy lists a hidden copy"
    );
    run(&mut sim, 2, &COUNTERS);
    sim.dump(100);
    sim.flush_vcd().unwrap();
    let wave = std::fs::read_to_string(&vcd).unwrap();
    assert!(!wave.contains('@'), "{wave}");
    assert!(wave.contains(" clk "), "{wave}");
    let addr = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sim.signal("clk@negedge")));
    assert!(addr.is_err(), "the copy resolves by name");
}
