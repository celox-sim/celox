//! A clock derived from part of a register is edge-sensed on that part.
//!
//! `let divided: '_ clock = a[0];` selects one bit of an 8-bit register. The
//! derived domain must fire on the rising edges of that bit, not whenever the
//! whole register changes from zero to non-zero. Only a copy of an entire
//! variable may share its source's clock domain.

use celox::Simulation;

/// A counter `a` advanced by `STEP` on every `clk` edge drives the clock
/// `divided = a[SELECT]`, and `b` counts the rising edges of `divided`.
fn counter_source(step: u8, select: &str) -> String {
    format!(
        r#"
module Top (
    clk: input '_ clock,
    rst: input '_ reset_async_low,
    cnt1: output logic<8>,
    cnt2: output logic<8>,
) {{
    var a: logic<8>;
    var b: logic<8>;
    always_ff (clk, rst) {{
        if_reset {{ a = 0; }} else {{ a = a + 8'd{step}; }}
    }}
    let divided: '_ clock = a[{select}];
    always_ff (divided, rst) {{
        if_reset {{ b = 0; }} else {{ b = b + 8'd1; }}
    }}
    assign cnt1 = a;
    assign cnt2 = b;
}}
"#
    )
}

fn simulation(source: &str) -> Simulation {
    Simulation::builder(source, "Top")
        .build()
        .unwrap_or_else(|error| panic!("{error:?}"))
}

/// Resets the design, then applies `edges` rising `clk` edges and returns
/// `(cnt1, cnt2)` after each edge.
fn run_edges(sim: &mut Simulation, edges: u64) -> Vec<(u64, u64)> {
    let cnt1 = sim.signal("cnt1");
    let cnt2 = sim.signal("cnt2");
    sim.schedule("rst", 0, 0).unwrap();
    sim.schedule("clk", 0, 0).unwrap();
    sim.step().unwrap();
    sim.schedule("rst", 5, 1).unwrap();
    sim.step().unwrap();
    let mut observed = Vec::new();
    for edge in 0..edges {
        sim.schedule("clk", 10 + edge * 20, 1).unwrap();
        sim.step().unwrap();
        observed.push((
            sim.get(cnt1).try_into().unwrap(),
            sim.get(cnt2).try_into().unwrap(),
        ));
        sim.schedule("clk", 20 + edge * 20, 0).unwrap();
        sim.step().unwrap();
    }
    observed
}

/// The expected `(a, b)` after each edge: `b` counts the 0 -> 1 transitions
/// of `a[bit]` while `a` advances by `step`.
fn expected(step: u64, bit: u32, edges: u64) -> Vec<(u64, u64)> {
    let mut b = 0;
    (1..=edges)
        .map(|edge| {
            let previous = ((edge - 1) * step) & 0xff;
            let a = (edge * step) & 0xff;
            if (previous >> bit) & 1 == 0 && (a >> bit) & 1 == 1 {
                b += 1;
            }
            (a, b)
        })
        .collect()
}

#[test]
fn bit_zero_of_an_incrementing_register_divides_by_two() {
    let mut sim = simulation(&counter_source(1, "0"));
    assert_eq!(run_edges(&mut sim, 8), expected(1, 0, 8));
}

/// `a` advances by 2, so `a[0]` stays low and `divided` never rises, although
/// the register leaves zero on the first edge.
#[test]
fn a_constant_low_bit_never_clocks_although_the_register_changes() {
    let mut sim = simulation(&counter_source(2, "0"));
    let observed = run_edges(&mut sim, 6);
    assert!(observed.iter().all(|&(a, _)| a != 0));
    assert_eq!(observed, expected(2, 0, 6));
}

#[test]
fn bit_one_divides_by_four() {
    let mut sim = simulation(&counter_source(1, "1"));
    let observed = run_edges(&mut sim, 9);
    assert_eq!(observed[0], (1, 0), "a = 1 leaves a[1] low");
    assert_eq!(observed, expected(1, 1, 9));
}

/// A one-bit field at a non-zero offset of a wider part-select.
#[test]
fn a_high_bit_divides_by_its_weight() {
    let mut sim = simulation(&counter_source(1, "2"));
    assert_eq!(run_edges(&mut sim, 12), expected(1, 2, 12));
}

/// A multi-bit register drives a clock through a single-bit part-select at a
/// non-zero offset, `a[3:3]`.
#[test]
fn a_part_select_at_an_offset_clocks_on_its_own_bit() {
    let mut sim = simulation(&counter_source(1, "3:3"));
    assert_eq!(run_edges(&mut sim, 20), expected(1, 3, 20));
}

/// A one-bit register copied whole is still the same clock domain as the
/// register: a classic `clk_div = ~clk_div` divider.
#[test]
fn a_toggling_register_divides_by_two() {
    let mut sim = simulation(
        r#"
module Top (
    clk: input '_ clock,
    rst: input '_ reset_async_low,
    cnt1: output logic<8>,
    cnt2: output logic<8>,
) {
    var half: logic;
    var a: logic<8>;
    var b: logic<8>;
    always_ff (clk, rst) {
        if_reset { half = 0; a = 0; } else { half = ~half; a = a + 8'd1; }
    }
    let divided: '_ clock = half;
    always_ff (divided, rst) {
        if_reset { b = 0; } else { b = b + 8'd1; }
    }
    assign cnt1 = a;
    assign cnt2 = b;
}
"#,
    );
    assert_eq!(run_edges(&mut sim, 8), expected(1, 0, 8));
}

/// The derived clock reaches a child's clock port, whose domain is the
/// derived clock's, not the register's.
#[test]
fn a_register_bit_connected_to_a_child_clock_port() {
    let mut sim = simulation(
        r#"
module Counter (
    clk: input '_ clock,
    rst: input '_ reset_async_low,
    q: output logic<8>,
) {
    always_ff (clk, rst) {
        if_reset { q = 0; } else { q = q + 8'd1; }
    }
}

module Top (
    clk: input '_ clock,
    rst: input '_ reset_async_low,
    cnt1: output logic<8>,
    cnt2: output logic<8>,
) {
    var a: logic<8>;
    always_ff (clk, rst) {
        if_reset { a = 0; } else { a = a + 8'd1; }
    }
    let divided: '_ clock = a[1];
    inst u: Counter (clk: divided, rst, q: cnt2);
    assign cnt1 = a;
}
"#,
    );
    assert_eq!(run_edges(&mut sim, 9), expected(1, 1, 9));
}

/// A clock gated by a register still fires only while the gate is open.
#[test]
fn a_gated_clock_follows_its_enable() {
    let mut sim = simulation(
        r#"
module Top (
    clk: input '_ clock,
    eclk: input '_ clock,
    rst: input '_ reset_async_low,
    en_in: input logic,
    q: output logic<8>,
) {
    var en: logic;
    var r: logic<8>;
    always_ff (eclk, rst) {
        if_reset { en = 0; } else { en = en_in; }
    }
    let gated: '_ clock = clk & en;
    always_ff (gated, rst) {
        if_reset { r = 0; } else { r = r + 1; }
    }
    assign q = r;
}
"#,
    );
    let q = sim.signal("q");
    let en_in = sim.signal("en_in");
    sim.modify(|io| io.set(en_in, 1u8)).unwrap();
    for (port, time, value) in [("rst", 0, 0), ("clk", 0, 0), ("eclk", 0, 0), ("rst", 10, 1)] {
        sim.schedule(port, time, value).unwrap();
        sim.step().unwrap();
    }
    for (time, value) in [(20, 1), (30, 0)] {
        sim.schedule("eclk", time, value).unwrap();
        sim.step().unwrap();
    }
    assert_eq!(sim.get(q), 0u8.into(), "capturing the enable is no edge");
    for (time, expected) in [(40, 1u8), (60, 2)] {
        sim.schedule("clk", time, 1).unwrap();
        sim.step().unwrap();
        assert_eq!(sim.get(q), expected.into());
        sim.schedule("clk", time + 10, 0).unwrap();
        sim.step().unwrap();
    }
    sim.modify(|io| io.set(en_in, 0u8)).unwrap();
    for (time, value) in [(80, 1), (90, 0)] {
        sim.schedule("eclk", time, value).unwrap();
        sim.step().unwrap();
    }
    sim.schedule("clk", 100, 1).unwrap();
    sim.step().unwrap();
    assert_eq!(sim.get(q), 2u8.into(), "a closed gate passes no edge");
}

#[cfg(feature = "systemverilog")]
fn sv_simulation(source: &str) -> Simulation {
    Simulation::from_sv_sources(
        vec![(source, std::path::Path::new("derived_register_clock.sv"))],
        "Top",
    )
    .build()
    .unwrap_or_else(|error| panic!("{error:?}"))
}

#[cfg(feature = "systemverilog")]
#[test]
fn a_systemverilog_register_bit_clock_divides_by_two() {
    let mut sim = sv_simulation(
        "module Top(input logic clk, input logic rst, output logic [7:0] cnt1, cnt2);
           logic [7:0] a, b;
           logic divided;
           assign divided = a[0];
           always_ff @(posedge clk or negedge rst) if (!rst) a <= 0; else a <= a + 1;
           always_ff @(posedge divided or negedge rst) if (!rst) b <= 0; else b <= b + 1;
           assign cnt1 = a;
           assign cnt2 = b;
         endmodule",
    );
    assert_eq!(run_edges(&mut sim, 8), expected(1, 0, 8));
}

/// A register bit connected directly to a child's clock port.
#[cfg(feature = "systemverilog")]
#[test]
fn a_systemverilog_register_bit_on_a_child_clock_port() {
    let mut sim = sv_simulation(
        "module Counter(input logic clk, input logic rst, output logic [7:0] q);
           always_ff @(posedge clk or negedge rst) if (!rst) q <= 0; else q <= q + 1;
         endmodule
         module Top(input logic clk, input logic rst, output logic [7:0] cnt1, cnt2);
           logic [7:0] a;
           always_ff @(posedge clk or negedge rst) if (!rst) a <= 0; else a <= a + 1;
           Counter u(.clk(a[2]), .rst(rst), .q(cnt2));
           assign cnt1 = a;
         endmodule",
    );
    assert_eq!(run_edges(&mut sim, 12), expected(1, 2, 12));
}

/// A child's clock output drives one bit of a parent vector. The vector is
/// not that clock: a clock taken from its other, constant bit never rises,
/// while one taken from the driven bit follows the child's clock.
#[cfg(feature = "systemverilog")]
#[test]
fn a_systemverilog_child_clock_into_a_vector_bit() {
    for (bit, step) in [(0, 0), (1, 1)] {
        let mut sim = sv_simulation(&format!(
            "module Divider(input logic clk, input logic rst, output logic half);
               always_ff @(posedge clk or negedge rst) if (!rst) half <= 0; else half <= ~half;
             endmodule
             module Top(input logic clk, input logic rst, output logic [7:0] cnt1, cnt2);
               logic [7:0] a, b;
               logic [1:0] clocks;
               logic selected;
               assign clocks[0] = 1'b0;
               Divider u(.clk(clk), .rst(rst), .half(clocks[1]));
               assign selected = clocks[{bit}];
               always_ff @(posedge clk or negedge rst) if (!rst) a <= 0; else a <= a + {step};
               always_ff @(posedge selected or negedge rst) if (!rst) b <= 0; else b <= b + 1;
               assign cnt1 = a;
               assign cnt2 = b;
             endmodule",
        ));
        // `a` mirrors the divider's phase in bit 0 when it counts edges.
        let observed = run_edges(&mut sim, 8);
        let b: Vec<_> = observed.iter().map(|&(_, b)| b).collect();
        let want: Vec<_> = expected(1, 0, 8).iter().map(|&(_, b)| b * bit).collect();
        assert_eq!(b, want, "clocks[{bit}]");
    }
}
