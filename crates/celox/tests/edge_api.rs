//! Explicit edges: `posedge`, `negedge`, `active_edge`, `inactive_edge`,
//! `tick` and coincident `edges`, on every backend.

use celox::{ClockType, Edge, SimBackend, Simulation, Simulator, SimulatorBuilder};

/// A simulator under test, independent of its backend.
trait Probe {
    fn edge(&mut self, event: &str, edge: Edge);
    fn active_edge(&mut self, event: &str);
    fn inactive_edge(&mut self, event: &str);
    fn tick(&mut self, event: &str);
    fn edges(&mut self, moves: &[(&str, Edge)]);
    fn set(&mut self, name: &str, value: u8);
    fn get(&mut self, name: &str) -> u64;
}

impl<B: SimBackend> Probe for Simulator<B> {
    fn edge(&mut self, event: &str, edge: Edge) {
        let event = self.event(event);
        Simulator::edge(self, event, edge).unwrap();
    }
    fn active_edge(&mut self, event: &str) {
        let event = self.event(event);
        Simulator::active_edge(self, event).unwrap();
    }
    fn inactive_edge(&mut self, event: &str) {
        let event = self.event(event);
        Simulator::inactive_edge(self, event).unwrap();
    }
    fn tick(&mut self, event: &str) {
        let event = self.event(event);
        Simulator::tick(self, event).unwrap();
    }
    fn edges(&mut self, moves: &[(&str, Edge)]) {
        let moves: Vec<_> = moves
            .iter()
            .map(|&(event, edge)| (self.event(event), edge))
            .collect();
        Simulator::edges(self, &moves).unwrap();
    }
    fn set(&mut self, name: &str, value: u8) {
        let signal = self.signal(name);
        self.modify(|io| io.set(signal, value)).unwrap();
    }
    fn get(&mut self, name: &str) -> u64 {
        let signal = self.signal(name);
        Simulator::get(self, signal).try_into().unwrap()
    }
}

/// Build `source` on every backend, in both state modes.
fn backends(
    source: &'static str,
    configure: impl Fn(SimulatorBuilder<'static, Simulator>) -> SimulatorBuilder<'static, Simulator>,
) -> Vec<(String, Box<dyn Probe>)> {
    let mut built: Vec<(String, Box<dyn Probe>)> = Vec::new();
    for four_state in [false, true] {
        let build = || configure(Simulator::builder(source, "Top").four_state(four_state));
        let name = |backend: &str| format!("{backend}, four_state={four_state}");
        built.push((
            name("interp"),
            Box::new(build().build_interpreter().unwrap()),
        ));
        built.push((
            name("cranelift"),
            Box::new(build().build_cranelift().unwrap()),
        ));
        built.push((name("wasm"), Box::new(build().build_wasm().unwrap())));
        built.push((name("tiered"), Box::new(build().build_tiered().unwrap())));
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        built.push((name("native"), Box::new(build().build_native().unwrap())));
    }
    built
}

/// Assert the reset, then release it, with explicit edges.
fn reset(sim: &mut dyn Probe) {
    sim.edge("rst", Edge::Rising);
    sim.edge("rst", Edge::Falling);
    sim.edge("rst", Edge::Rising);
}

const COUNTERS: &str = r#"
module Top (
    clk: input '_ clock,
    rst: input '_ reset_async_low,
    en: input logic,
    rising: output logic<8>,
    falling: output logic<8>,
    gated_falling: output logic<8>,
) {
    var r: logic<8>;
    var f: logic<8>;
    var g: logic<8>;
    always_ff (clk, rst) { if_reset { r = 0; } else { r = r + 8'd1; } }
    let inverted: '_ clock = ~clk;
    always_ff (inverted, rst) { if_reset { f = 0; } else { f = f + 8'd1; } }
    let gated: '_ clock = ~(clk & en);
    always_ff (gated, rst) { if_reset { g = 0; } else { g = g + 8'd1; } }
    assign rising = r;
    assign falling = f;
    assign gated_falling = g;
}
"#;

fn counters(sim: &mut dyn Probe) -> [u64; 3] {
    [
        sim.get("rising"),
        sim.get("falling"),
        sim.get("gated_falling"),
    ]
}

/// `posedge` runs the rising domain, `negedge` the domains of `~clk` and of
/// a falling `clk & en`; an edge to the level the clock already has does
/// nothing.
#[test]
fn edges_run_the_domains_of_their_direction() {
    for (backend, mut sim) in backends(COUNTERS, |builder| builder) {
        let sim = sim.as_mut();
        sim.set("en", 1);
        sim.edge("clk", Edge::Falling);
        reset(sim);
        assert_eq!(counters(sim), [0, 0, 0], "{backend}: reset");
        sim.edge("clk", Edge::Rising);
        assert_eq!(counters(sim), [1, 0, 0], "{backend}: posedge");
        sim.edge("clk", Edge::Rising);
        assert_eq!(counters(sim), [1, 0, 0], "{backend}: no second posedge");
        sim.edge("clk", Edge::Falling);
        assert_eq!(counters(sim), [1, 1, 1], "{backend}: negedge");
        sim.set("en", 0);
        sim.tick("clk");
        assert_eq!(counters(sim), [2, 2, 1], "{backend}: closed gate");
        sim.set("en", 1);
        sim.tick("clk");
        assert_eq!(counters(sim), [3, 3, 2], "{backend}: tick");
        assert_eq!(sim.get("clk"), 0, "{backend}: tick leaves clk low");
    }
}

/// With the abstract `clock` type, the active edge follows the build's clock
/// type.
#[test]
fn abstract_clocks_follow_the_clock_type() {
    const CODE: &str = r#"
module Top (clk: input clock, rst: input reset_async_low, q: output logic<8>) {
    var r: logic<8>;
    always_ff (clk, rst) { if_reset { r = 0; } else { r = r + 8'd1; } }
    assign q = r;
}
"#;
    for (clock_type, active, inactive) in [
        (ClockType::PosEdge, Edge::Rising, Edge::Falling),
        (ClockType::NegEdge, Edge::Falling, Edge::Rising),
    ] {
        for (backend, mut sim) in backends(CODE, |builder| builder.clock_type(clock_type)) {
            let sim = sim.as_mut();
            sim.edge("clk", inactive);
            reset(sim);
            assert_eq!(sim.get("q"), 0, "{backend}: {clock_type:?} reset");
            sim.active_edge("clk");
            assert_eq!(sim.get("q"), 1, "{backend}: {clock_type:?} active");
            sim.inactive_edge("clk");
            assert_eq!(sim.get("q"), 1, "{backend}: {clock_type:?} inactive");
            sim.edge("clk", active);
            assert_eq!(sim.get("q"), 2, "{backend}: {clock_type:?} explicit");
            // A clock at its active level takes no active edge.
            sim.tick("clk");
            assert_eq!(sim.get("q"), 2, "{backend}: {clock_type:?} stays active");
            sim.tick("clk");
            assert_eq!(sim.get("q"), 3, "{backend}: {clock_type:?} tick");
        }
    }
}

/// `tick` runs a clock period exactly as the timed simulation's two steps
/// do: gated clocks with pre-edge sampling, a gate the commit raises, `~clk`
/// and a register divider of a divider.
#[test]
fn a_tick_matches_the_timed_simulation() {
    const CODE: &str = r#"
module Top (
    clk: input '_ clock,
    rst: input '_ reset_async_low,
    en_in: input logic,
    observed: output logic<64>,
) {
    var a: logic<8>;
    var en: logic;
    var b: logic<8>;
    var d: logic<8>;
    var half: logic;
    var quarter: logic;
    var q: logic<8>;
    always_ff (clk, rst) {
        if_reset { a = 0; en = 1; half = 0; } else { a = a + 8'd1; en = en_in; half = ~half; }
    }
    let gated: '_ clock = clk & en;
    always_ff (gated, rst) { if_reset { b = 0; } else { b = b + a; } }
    let inverted: '_ clock = ~clk;
    always_ff (inverted, rst) { if_reset { d = 0; } else { d = a ^ b; } }
    let half_clk: '_ clock = half;
    always_ff (half_clk, rst) { if_reset { quarter = 0; } else { quarter = ~quarter; } }
    let quarter_clk: '_ clock = quarter;
    always_ff (quarter_clk, rst) { if_reset { q = 0; } else { q = q + d; } }
    assign observed = {a, b, d, q, 7'd0, en, 7'd0, half, 7'd0, quarter, 8'd0};
}
"#;
    const ENABLES: [u8; 12] = [1, 0, 1, 1, 0, 0, 1, 0, 1, 1, 1, 0];
    let mut timed = Simulation::builder(CODE, "Top").build().unwrap();
    let observed = timed.signal("observed");
    let en_in = timed.signal("en_in");
    for (port, time, value) in [("clk", 0, 0), ("rst", 0, 1), ("rst", 1, 0), ("rst", 2, 1)] {
        timed.schedule(port, time, value).unwrap();
        timed.step().unwrap();
    }
    let mut expected = Vec::new();
    for (cycle, &enable) in ENABLES.iter().enumerate() {
        let time = 10 + cycle as u64 * 20;
        timed.modify(|io| io.set(en_in, enable)).unwrap();
        timed.schedule("clk", time, 1).unwrap();
        timed.step().unwrap();
        timed.schedule("clk", time + 10, 0).unwrap();
        timed.step().unwrap();
        expected.push(u64::try_from(timed.get(observed)).unwrap());
    }

    for (backend, mut sim) in backends(CODE, |builder| builder) {
        let sim = sim.as_mut();
        sim.edge("clk", Edge::Falling);
        reset(sim);
        let observed: Vec<u64> = ENABLES
            .iter()
            .map(|&enable| {
                sim.set("en_in", enable);
                sim.tick("clk");
                sim.get("observed")
            })
            .collect();
        assert_eq!(observed, expected, "{backend}");
    }
}

/// Coincident edges evaluate every sensitive domain against the state
/// before them.
#[test]
fn coincident_edges_sample_the_state_before_them() {
    const CODE: &str = r#"
module Top (a_clk: input '_ clock, b_clk: input '_ clock, rst: input '_ reset_async_low,
            a: output logic<8>, b: output logic<8>) {
    var ra: logic<8>;
    var rb: logic<8>;
    always_ff (a_clk, rst) { if_reset { ra = 3; } else { ra = rb + 8'd1; } }
    always_ff (b_clk, rst) { if_reset { rb = 5; } else { rb = ra + 8'd2; } }
    assign a = ra;
    assign b = rb;
}
"#;
    for (backend, mut sim) in backends(CODE, |builder| builder) {
        let sim = sim.as_mut();
        sim.edges(&[("a_clk", Edge::Falling), ("b_clk", Edge::Falling)]);
        reset(sim);
        sim.edges(&[("a_clk", Edge::Rising), ("b_clk", Edge::Rising)]);
        assert_eq!(sim.get("a"), 6, "{backend}: a reads b before the edge");
        assert_eq!(sim.get("b"), 5, "{backend}: b reads a before the edge");
    }
}

/// SystemVerilog: a register on `posedge clk` and one on `negedge` of a copy
/// of `clk` run on the matching edges, in a tick-free and a timed run.
#[cfg(feature = "systemverilog")]
#[test]
fn systemverilog_posedge_and_negedge_on_one_clock() {
    let source = "module Top(input logic clk, input logic rst_n, output logic [7:0] p, n);
        logic c2;
        assign c2 = clk;
        always_ff @(posedge clk or negedge rst_n) if (!rst_n) p <= 0; else p <= p + 1;
        always_ff @(negedge c2 or negedge rst_n) if (!rst_n) n <= 0; else n <= n + 1;
      endmodule";
    let path = std::path::Path::new("edges.sv");
    for four_state in [false, true] {
        let mut sim = Simulator::from_sv_sources(vec![(source, path)], "Top")
            .four_state(four_state)
            .build()
            .unwrap();
        let sim: &mut dyn Probe = &mut sim;
        sim.edge("clk", Edge::Falling);
        sim.edge("rst_n", Edge::Rising);
        sim.edge("rst_n", Edge::Falling);
        sim.edge("rst_n", Edge::Rising);
        sim.edge("clk", Edge::Rising);
        assert_eq!([sim.get("p"), sim.get("n")], [1, 0]);
        sim.edge("clk", Edge::Falling);
        assert_eq!([sim.get("p"), sim.get("n")], [1, 1]);
    }

    let mut timed = Simulation::from_sv_sources(vec![(source, path)], "Top")
        .build()
        .unwrap();
    timed.schedule("rst_n", 0, 0).unwrap();
    timed.schedule("clk", 0, 0).unwrap();
    timed.step().unwrap();
    timed.schedule("rst_n", 1, 1).unwrap();
    timed.step().unwrap();
    timed.add_clock("clk", 10, 5);
    timed.run_until(42).unwrap();
    let p = timed.signal("p");
    let n = timed.signal("n");
    assert_eq!(timed.get(p), 4u8.into(), "rising edges at 5, 15, 25, 35");
    assert_eq!(timed.get(n), 4u8.into(), "falling edges at 10, 20, 30, 40");
}

/// The edge plans that WASM hosts run name every derived domain of an event
/// by its kernel's event name.
#[test]
fn edge_plans_describe_derived_domains_by_event_name() {
    let path = std::path::Path::new("counters.veryl");
    let (program, _) = celox::compile_to_sir(
        &[(COUNTERS, path)],
        "Top",
        &[],
        &[],
        false,
        &Default::default(),
        None,
        None,
        None,
        None,
        &[],
        &Default::default(),
    )
    .unwrap();
    let plans = celox::edge_plans(&program.into_laid_out(false));
    let clk = &plans.events["clk"];
    assert_eq!(clk.edge, "posedge");
    let mut gated: Vec<_> = clk
        .gated
        .iter()
        .map(|domain| domain.event.as_str())
        .collect();
    gated.sort_unstable();
    assert_eq!(gated, ["gated", "inverted"]);
    assert!(
        clk.gated
            .iter()
            .all(|domain| plans.events.contains_key(&domain.event))
    );
    assert!(clk.comb_read);
    let json = serde_json::to_value(&plans).unwrap();
    assert_eq!(json["events"]["clk"]["combRead"], true);
}
