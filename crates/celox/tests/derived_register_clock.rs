//! Clocks derived from other clocks or from registers.
//!
//! `let divided: '_ clock = a[0];` selects one bit of an 8-bit register. The
//! derived domain must fire on the rising edges of that bit, not whenever the
//! whole register changes from zero to non-zero. Only a copy of an entire
//! variable may share its source's clock domain.
//!
//! Every case runs twice: on the timed `Simulation`, and on the event-based
//! `Simulator`, whose `tick` must fire the derived domains a tick reaches
//! with the same results.

use celox::{Simulation, Simulator, SimulatorBuilder};

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

/// Resets the design, then applies `edges` rising `clk` edges on the timed
/// simulation and returns `(cnt1, cnt2)` after each edge.
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

/// The same stimulus with `Simulator::tick`: one reset tick, then `edges`
/// ticks of `clk`.
fn run_ticks(sim: &mut Simulator, edges: u64) -> Vec<(u64, u64)> {
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let cnt1 = sim.signal("cnt1");
    let cnt2 = sim.signal("cnt2");
    sim.modify(|io| io.set(rst, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    (0..edges)
        .map(|_| {
            sim.tick(clk).unwrap();
            (
                sim.get(cnt1).try_into().unwrap(),
                sim.get(cnt2).try_into().unwrap(),
            )
        })
        .collect()
}

/// Run `edges` clock edges on both APIs and compare them with `expected`.
fn check(source: &str, edges: u64, expected: &[(u64, u64)]) {
    check_with(
        Simulation::builder(source, "Top"),
        Simulator::builder(source, "Top"),
        edges,
        expected,
    );
}

fn check_with(
    timed: SimulatorBuilder<'_, Simulation>,
    untimed: SimulatorBuilder<'_, Simulator>,
    edges: u64,
    expected: &[(u64, u64)],
) {
    let mut timed = timed.build().unwrap_or_else(|error| panic!("{error:?}"));
    assert_eq!(run_edges(&mut timed, edges), expected, "timed simulation");
    let mut untimed = untimed.build().unwrap_or_else(|error| panic!("{error:?}"));
    assert_eq!(run_ticks(&mut untimed, edges), expected, "Simulator::tick");
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
    check(&counter_source(1, "0"), 8, &expected(1, 0, 8));
}

/// `a` advances by 2, so `a[0]` stays low and `divided` never rises, although
/// the register leaves zero on the first edge.
#[test]
fn a_constant_low_bit_never_clocks_although_the_register_changes() {
    let want = expected(2, 0, 6);
    assert!(want.iter().all(|&(a, b)| a != 0 && b == 0));
    check(&counter_source(2, "0"), 6, &want);
}

#[test]
fn bit_one_divides_by_four() {
    let want = expected(1, 1, 9);
    assert_eq!(want[0], (1, 0), "a = 1 leaves a[1] low");
    check(&counter_source(1, "1"), 9, &want);
}

/// A one-bit field at a non-zero offset of a wider part-select.
#[test]
fn a_high_bit_divides_by_its_weight() {
    check(&counter_source(1, "2"), 12, &expected(1, 2, 12));
}

/// A multi-bit register drives a clock through a single-bit part-select at a
/// non-zero offset, `a[3:3]`.
#[test]
fn a_part_select_at_an_offset_clocks_on_its_own_bit() {
    check(&counter_source(1, "3:3"), 20, &expected(1, 3, 20));
}

/// A one-bit register copied whole is still the same clock domain as the
/// register: a classic `clk_div = ~clk_div` divider.
#[test]
fn a_toggling_register_divides_by_two() {
    check(
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
        8,
        &expected(1, 0, 8),
    );
}

/// A divider of a divider: `quarter` toggles on `half`, and `b` counts the
/// rising edges of `quarter`, one every fourth `clk` edge.
#[test]
fn a_divider_of_a_divider_divides_by_four() {
    check(
        r#"
module Top (
    clk: input '_ clock,
    rst: input '_ reset_async_low,
    cnt1: output logic<8>,
    cnt2: output logic<8>,
) {
    var half: logic;
    var quarter: logic;
    var a: logic<8>;
    var b: logic<8>;
    always_ff (clk, rst) {
        if_reset { half = 0; a = 0; } else { half = ~half; a = a + 8'd1; }
    }
    let half_clk: '_ clock = half;
    always_ff (half_clk, rst) {
        if_reset { quarter = 0; } else { quarter = ~quarter; }
    }
    let quarter_clk: '_ clock = quarter;
    always_ff (quarter_clk, rst) {
        if_reset { b = 0; } else { b = b + 8'd1; }
    }
    assign cnt1 = a;
    assign cnt2 = b;
}
"#,
        12,
        // `half` rises on edges 1, 3, 5, ... and `quarter` on edges 1, 5, 9.
        &(1..=12u64)
            .map(|edge| (edge, edge.div_ceil(4)))
            .collect::<Vec<_>>(),
    );
}

/// The derived clock reaches a child's clock port, whose domain is the
/// derived clock's, not the register's.
#[test]
fn a_register_bit_connected_to_a_child_clock_port() {
    check(
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
        9,
        &expected(1, 1, 9),
    );
}

/// `clk & en` with `en` an input: `b` counts the edges while `en` is high.
const INPUT_GATED: &str = r#"
module Top (
    clk: input '_ clock,
    rst: input '_ reset_async_low,
    en: input logic,
    cnt1: output logic<8>,
    cnt2: output logic<8>,
) {
    var a: logic<8>;
    var b: logic<8>;
    always_ff (clk, rst) {
        if_reset { a = 0; } else { a = a + 8'd1; }
    }
    let gated: '_ clock = clk & en;
    always_ff (gated, rst) {
        if_reset { b = 0; } else { b = b + 8'd1; }
    }
    assign cnt1 = a;
    assign cnt2 = b;
}
"#;

/// The enable for each of eight edges, and the expected `(a, b)` after it.
fn gated_stimulus() -> (Vec<u8>, Vec<(u64, u64)>) {
    let enables = vec![1, 1, 0, 0, 1, 0, 1, 1];
    let mut b = 0;
    let expected = enables
        .iter()
        .enumerate()
        .map(|(edge, &enable)| {
            b += u64::from(enable);
            (edge as u64 + 1, b)
        })
        .collect();
    (enables, expected)
}

#[test]
fn an_input_gated_clock_fires_only_with_its_enable() {
    let (enables, expected) = gated_stimulus();

    let mut timed = Simulation::builder(INPUT_GATED, "Top").build().unwrap();
    let (en, cnt1, cnt2) = (
        timed.signal("en"),
        timed.signal("cnt1"),
        timed.signal("cnt2"),
    );
    for (port, time, value) in [("rst", 0, 0), ("clk", 0, 0), ("rst", 5, 1)] {
        timed.schedule(port, time, value).unwrap();
        timed.step().unwrap();
    }
    let mut observed = Vec::new();
    for (edge, &enable) in enables.iter().enumerate() {
        let time = 10 + edge as u64 * 20;
        timed.modify(|io| io.set(en, enable)).unwrap();
        timed.schedule("clk", time, 1).unwrap();
        timed.step().unwrap();
        observed.push((
            timed.get(cnt1).try_into().unwrap(),
            timed.get(cnt2).try_into().unwrap(),
        ));
        timed.schedule("clk", time + 10, 0).unwrap();
        timed.step().unwrap();
    }
    assert_eq!(observed, expected, "timed simulation");

    let mut sim = Simulator::builder(INPUT_GATED, "Top").build().unwrap();
    let (clk, rst, en) = (sim.event("clk"), sim.signal("rst"), sim.signal("en"));
    let (cnt1, cnt2) = (sim.signal("cnt1"), sim.signal("cnt2"));
    sim.modify(|io| io.set(rst, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    let mut observed = Vec::new();
    for &enable in &enables {
        sim.modify(|io| io.set(en, enable)).unwrap();
        sim.tick(clk).unwrap();
        observed.push((
            sim.get(cnt1).try_into().unwrap(),
            sim.get(cnt2).try_into().unwrap(),
        ));
    }
    assert_eq!(observed, expected, "Simulator::tick");
    assert_eq!(
        sim.get(sim.signal("clk")),
        0u8.into(),
        "tick leaves clk low"
    );
}

/// The source domain reads a register of the gated domain: both share the
/// edge, so `clk`'s domain reads the gated register from before it.
#[test]
fn the_source_domain_reads_the_gated_domain_before_the_edge() {
    const CODE: &str = r#"
module Top (
    clk: input '_ clock,
    rst: input '_ reset_async_low,
    en: input logic,
    cnt1: output logic<8>,
    cnt2: output logic<8>,
) {
    var a: logic<8>;
    var b: logic<8>;
    always_ff (clk, rst) {
        if_reset { a = 0; } else { a = b; }
    }
    let gated: '_ clock = clk & en;
    always_ff (gated, rst) {
        if_reset { b = 0; } else { b = b + 8'd1; }
    }
    assign cnt1 = a;
    assign cnt2 = b;
}
"#;
    // `a` follows `b` one edge behind.
    let expected: Vec<(u64, u64)> = (1..=6).map(|edge| (edge - 1, edge)).collect();
    let mut timed = Simulation::builder(CODE, "Top").build().unwrap();
    let en = timed.signal("en");
    timed.modify(|io| io.set(en, 1u8)).unwrap();
    assert_eq!(run_edges(&mut timed, 6), expected, "timed simulation");
    let mut untimed = Simulator::builder(CODE, "Top").build().unwrap();
    let en = untimed.signal("en");
    untimed.modify(|io| io.set(en, 1u8)).unwrap();
    assert_eq!(run_ticks(&mut untimed, 6), expected, "Simulator::tick");
}

/// `~clk` rises when `clk` falls: the timed simulation, which drives both
/// levels of `clk`, runs its domain once per cycle, while a tick is the
/// active edge of `clk` only and does not run it.
#[test]
fn an_inverted_clock_runs_on_the_falling_edge_only() {
    let code = r#"
module Top (
    clk: input '_ clock,
    rst: input '_ reset_async_low,
    cnt2: output logic<8>,
) {
    var b: logic<8>;
    let inverted: '_ clock = ~clk;
    always_ff (inverted, rst) {
        if_reset { b = 0; } else { b = b + 8'd1; }
    }
    assign cnt2 = b;
}
"#;
    let mut timed = Simulation::builder(code, "Top").build().unwrap();
    let cnt2 = timed.signal("cnt2");
    for (port, time, value) in [("rst", 0, 1), ("clk", 0, 1), ("rst", 1, 0), ("rst", 2, 1)] {
        timed.schedule(port, time, value).unwrap();
        timed.step().unwrap();
    }
    for cycle in 0..4u64 {
        timed.schedule("clk", 10 + cycle * 20, 0).unwrap();
        timed.step().unwrap();
        assert_eq!(timed.get(cnt2), (cycle + 1).into(), "falling edge {cycle}");
        timed.schedule("clk", 20 + cycle * 20, 1).unwrap();
        timed.step().unwrap();
        assert_eq!(timed.get(cnt2), (cycle + 1).into(), "rising edge {cycle}");
    }

    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let (clk, rst, cnt2) = (sim.event("clk"), sim.signal("rst"), sim.signal("cnt2"));
    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    for _ in 0..4 {
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(cnt2), 0u8.into(), "a tick is not a falling edge");
    }
}

/// A clock gated by a register of another domain fires only while the gate
/// is open, and a closed gate passes no edge.
const REGISTER_GATED: &str = r#"
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
"#;

#[test]
fn a_register_gated_clock_follows_its_enable() {
    let mut sim = Simulation::builder(REGISTER_GATED, "Top").build().unwrap();
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

#[test]
fn a_register_gated_clock_follows_its_enable_on_tick() {
    let mut sim = Simulator::builder(REGISTER_GATED, "Top").build().unwrap();
    let (clk, eclk) = (sim.event("clk"), sim.event("eclk"));
    let (rst, en_in, q) = (sim.signal("rst"), sim.signal("en_in"), sim.signal("q"));
    sim.modify(|io| io.set(rst, 0u8)).unwrap();
    sim.tick(eclk).unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0u8.into(), "the gate is still closed");
    sim.modify(|io| io.set(en_in, 1u8)).unwrap();
    sim.tick(eclk).unwrap();
    assert_eq!(sim.get(q), 0u8.into(), "capturing the enable is no edge");
    for expected in 1..=2u8 {
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(q), expected.into());
    }
    sim.modify(|io| io.set(en_in, 0u8)).unwrap();
    sim.tick(eclk).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 2u8.into(), "a closed gate passes no edge");
    // Both clocks at once: the edge finds the gate closed, then `eclk`'s
    // commit opens it while `clk` is high, which is an edge of `gated`.
    sim.modify(|io| io.set(en_in, 1u8)).unwrap();
    sim.tick_events(&[clk, eclk]).unwrap();
    assert_eq!(
        sim.get(q),
        3u8.into(),
        "the opened gate passes the high clock"
    );
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 4u8.into(), "the next edge passes the open gate");
}

/// A native testbench with one `initial` block drives its clock with
/// `Simulator::tick`; the divided domain must still run.
#[test]
fn a_testbench_counts_the_register_bit_clock() {
    let code = r#"
module Divider (
    clk: input '_ clock,
    rst: input '_ reset,
    cnt1: output logic<8>,
    cnt2: output logic<8>,
) {
    var a: logic<8>;
    var b: logic<8>;
    always_ff (clk, rst) {
        if_reset { a = 0; } else { a = a + 8'd1; }
    }
    let divided: '_ clock = a[1];
    always_ff (divided, rst) {
        if_reset { b = 0; } else { b = b + 8'd1; }
    }
    assign cnt1 = a;
    assign cnt2 = b;
}

#[test(t)]
module t {
    inst clk: $tb::clock_gen;
    inst rst: $tb::reset_gen(clk);
    var cnt1: logic<8>;
    var cnt2: logic<8>;
    inst dut: Divider (clk, rst, cnt1, cnt2);
    initial {
        rst.assert();
        clk.next(1);
        $assert(cnt1 == 1 && cnt2 == 0, "a=1 leaves a[1] low: a=%d b=%d", cnt1, cnt2);
        clk.next(1);
        $assert(cnt1 == 2 && cnt2 == 1, "a=2 raises a[1]: a=%d b=%d", cnt1, cnt2);
        clk.next(8);
        $assert(cnt1 == 10 && cnt2 == 3, "a=10: a=%d b=%d", cnt1, cnt2);
        $finish();
    }
}
"#;
    assert_eq!(
        Simulator::builder(code, "t").run_test().unwrap(),
        celox::TestResult::Pass
    );
}

#[cfg(feature = "systemverilog")]
fn check_sv(source: &str, edges: u64, expected: &[(u64, u64)]) {
    let path = std::path::Path::new("derived_register_clock.sv");
    check_with(
        Simulation::from_sv_sources(vec![(source, path)], "Top"),
        Simulator::from_sv_sources(vec![(source, path)], "Top"),
        edges,
        expected,
    );
}

#[cfg(feature = "systemverilog")]
#[test]
fn a_systemverilog_register_bit_clock_divides_by_two() {
    check_sv(
        "module Top(input logic clk, input logic rst, output logic [7:0] cnt1, cnt2);
           logic [7:0] a, b;
           logic divided;
           assign divided = a[0];
           always_ff @(posedge clk or negedge rst) if (!rst) a <= 0; else a <= a + 1;
           always_ff @(posedge divided or negedge rst) if (!rst) b <= 0; else b <= b + 1;
           assign cnt1 = a;
           assign cnt2 = b;
         endmodule",
        8,
        &expected(1, 0, 8),
    );
}

/// A register bit connected directly to a child's clock port.
#[cfg(feature = "systemverilog")]
#[test]
fn a_systemverilog_register_bit_on_a_child_clock_port() {
    check_sv(
        "module Counter(input logic clk, input logic rst, output logic [7:0] q);
           always_ff @(posedge clk or negedge rst) if (!rst) q <= 0; else q <= q + 1;
         endmodule
         module Top(input logic clk, input logic rst, output logic [7:0] cnt1, cnt2);
           logic [7:0] a;
           always_ff @(posedge clk or negedge rst) if (!rst) a <= 0; else a <= a + 1;
           Counter u(.clk(a[2]), .rst(rst), .q(cnt2));
           assign cnt1 = a;
         endmodule",
        12,
        &expected(1, 2, 12),
    );
}

/// `clk & en` on a child's clock port: the child counts the gated edges.
#[cfg(feature = "systemverilog")]
#[test]
fn a_systemverilog_gated_child_clock_port() {
    let source = "module Counter(input logic clk, input logic rst, output logic [7:0] q);
           always_ff @(posedge clk or negedge rst) if (!rst) q <= 0; else q <= q + 1;
         endmodule
         module Top(input logic clk, input logic rst, input logic en,
                    output logic [7:0] cnt1, cnt2);
           logic [7:0] a;
           always_ff @(posedge clk or negedge rst) if (!rst) a <= 0; else a <= a + 1;
           Counter u(.clk(clk & en), .rst(rst), .q(cnt2));
           assign cnt1 = a;
         endmodule";
    let (enables, expected) = gated_stimulus();
    let mut sim =
        Simulator::from_sv_sources(vec![(source, std::path::Path::new("gated_port.sv"))], "Top")
            .build()
            .unwrap_or_else(|error| panic!("{error:?}"));
    let (clk, rst, en) = (sim.event("clk"), sim.signal("rst"), sim.signal("en"));
    let (cnt1, cnt2) = (sim.signal("cnt1"), sim.signal("cnt2"));
    sim.modify(|io| io.set(rst, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    let mut observed = Vec::new();
    for &enable in &enables {
        sim.modify(|io| io.set(en, enable)).unwrap();
        sim.tick(clk).unwrap();
        observed.push((
            sim.get(cnt1).try_into().unwrap(),
            sim.get(cnt2).try_into().unwrap(),
        ));
    }
    assert_eq!(observed, expected);
}

/// A child's clock output drives one bit of a parent vector. The vector is
/// not that clock: a clock taken from its other, constant bit never rises,
/// while one taken from the driven bit follows the child's clock.
#[cfg(feature = "systemverilog")]
#[test]
fn a_systemverilog_child_clock_into_a_vector_bit() {
    for (bit, step) in [(0u64, 0u64), (1, 1)] {
        let source = format!(
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
        );
        // `b` follows the divider's phase when it counts its edges.
        let want: Vec<_> = expected(1, 0, 8)
            .into_iter()
            .enumerate()
            .map(|(edge, (_, b))| ((edge as u64 + 1) * step, b * bit))
            .collect();
        check_sv(&source, 8, &want);
    }
}

/// Drives `clk` low and high four times and checks that `cnt` counts the
/// falling edges only.
fn check_falling_edges(mut timed: Simulation) {
    let cnt = timed.signal("cnt");
    for (port, time, value) in [("rst", 0, 1), ("clk", 0, 1), ("rst", 1, 0), ("rst", 2, 1)] {
        timed.schedule(port, time, value).unwrap();
        timed.step().unwrap();
    }
    for cycle in 0..4u64 {
        timed.schedule("clk", 10 + cycle * 20, 0).unwrap();
        timed.step().unwrap();
        assert_eq!(timed.get(cnt), (cycle + 1).into(), "falling edge {cycle}");
        timed.schedule("clk", 20 + cycle * 20, 1).unwrap();
        timed.step().unwrap();
        assert_eq!(timed.get(cnt), (cycle + 1).into(), "rising edge {cycle}");
    }
}

/// A whole copy of `clk` sensed at its falling edge does not join `clk`'s
/// rising domain.
#[test]
fn a_clock_copy_sensed_at_the_falling_edge_runs_on_the_falling_edge() {
    check_falling_edges(
        Simulation::builder(
            r#"
module Top (
    clk: input '_ clock,
    rst: input '_ reset_async_low,
    cnt: output logic<8>,
) {
    var c: '_ clock_negedge;
    var b: logic<8>;
    assign c = clk;
    always_ff (c, rst) {
        if_reset { b = 0; } else { b = b + 8'd1; }
    }
    assign cnt = b;
}
"#,
            "Top",
        )
        .build()
        .unwrap(),
    );
}

/// The same for a child clock port sensed at the falling edge.
#[test]
fn a_falling_edge_child_clock_port_runs_on_the_falling_edge() {
    check_falling_edges(
        Simulation::builder(
            r#"
module Child (
    clk: input '_ clock_negedge,
    rst: input '_ reset_async_low,
    cnt: output logic<8>,
) {
    var b: logic<8>;
    always_ff (clk, rst) {
        if_reset { b = 0; } else { b = b + 8'd1; }
    }
    assign cnt = b;
}

module Top (
    clk: input '_ clock,
    rst: input '_ reset_async_low,
    cnt: output logic<8>,
) {
    var a: logic<8>;
    always_ff (clk, rst) {
        if_reset { a = 0; } else { a = a + 8'd1; }
    }
    inst u: Child (clk, rst, cnt);
}
"#,
            "Top",
        )
        .build()
        .unwrap(),
    );
}

#[cfg(feature = "systemverilog")]
#[test]
fn a_systemverilog_clock_copy_sensed_at_the_falling_edge() {
    let source = "module Top(input logic clk, input logic rst, output logic [7:0] cnt);
           logic c;
           logic [7:0] a;
           assign c = clk;
           always_ff @(posedge clk or negedge rst) if (!rst) a <= 0; else a <= a + 1;
           always_ff @(negedge c or negedge rst) if (!rst) cnt <= 0; else cnt <= cnt + 1;
         endmodule";
    let path = std::path::Path::new("derived_register_clock.sv");
    check_falling_edges(
        Simulation::from_sv_sources(vec![(source, path)], "Top")
            .build()
            .unwrap(),
    );
}
