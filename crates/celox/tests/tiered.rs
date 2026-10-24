//! Tiered execution: start on the interpreter, promote to compiled code.
//!
//! These tests drive `build_tiered` through enough ticks that background
//! compilation finishes mid-simulation, then verify the results and runtime
//! events match the compiled backends bit-for-bit across the promotion.

use celox::{Simulator, SimulatorBuilder, TieredBackend};

#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros, unused_imports)]
mod test_utils;

const COUNTER: &str = r#"
module Top (
    clk: input clock,
    rst: input reset,
    d: input logic<8>,
    q: output logic<8>,
) {
    var stage1: logic<8>;
    var stage2: logic<8>;

    always_ff (clk, rst) {
        if_reset {
            stage1 = 0;
            stage2 = 0;
        } else {
            stage1 = d;
            stage2 = stage1;
        }
    }
    assign q = stage2;
}
"#;

type TieredSimulator = Simulator<TieredBackend>;

fn build_tiered_counter() -> TieredSimulator {
    SimulatorBuilder::new(COUNTER, "Top")
        .build_tiered()
        .unwrap()
}

#[test]
fn tiered_promotes_and_matches_expected_results() {
    let mut sim = build_tiered_counter();
    // NOTE: the initial tier is intentionally not asserted here — a tiny
    // design can finish background compilation before the builder's final
    // stabilization safe point, so the simulator may legitimately already be
    // compiled when this test starts. Deterministic initial-tier coverage
    // lives in the gated in-crate unit tests; this test verifies that both
    // tiers produce identical results whenever promotion lands.

    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let d = sim.signal("d");
    let q = sim.signal("q");

    // Active-low reset with a known input.
    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(d, 9u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(rst, 1u8)).unwrap();

    // The two-stage pipeline delivers the input two ticks later.
    sim.tick(clk).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get_as::<u8>(q), 9u8);

    sim.modify(|io| io.set(d, 77u8)).unwrap();
    sim.tick(clk).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get_as::<u8>(q), 77u8);

    // Keep ticking through the promotion safe point.
    for i in 0..64u8 {
        sim.modify(|io| io.set(d, i)).unwrap();
        sim.tick(clk).unwrap();
    }
    sim.tick(clk).unwrap();
    sim.tick(clk).unwrap();
    // The loop's last input was d=63; after the two trailing ticks the
    // pipeline has fully absorbed it.
    assert_eq!(sim.get_as::<u8>(q), 63u8);

    // Wait for background compilation, ticking so safe points poll the
    // completion channel.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while !sim.is_compiled() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
        sim.tick(clk).unwrap();
    }
    assert!(
        sim.is_compiled(),
        "background compilation should complete during simulation"
    );
    assert!(sim.promotion_error().is_none());

    // The compiled tier must keep producing identical results.
    sim.modify(|io| io.set(d, 200u8)).unwrap();
    sim.tick(clk).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get_as::<u8>(q), 200u8);
}

#[test]
fn tiered_runtime_events_survive_promotion() {
    let code = r#"
module Top (
    clk: input clock,
    cnt: output logic<4>,
) {
    var c: logic<4>;
    always_ff (clk) {
        c = c + 1;
        $display("tick %0d", c);
    }
    assign cnt = c;
}
"#;
    let mut sim: Simulator<TieredBackend> =
        SimulatorBuilder::new(code, "Top").build_tiered().unwrap();
    let clk = sim.event("clk");

    const TICKS: u32 = 16;
    for _ in 0..TICKS {
        sim.tick(clk).unwrap();
    }

    // The interpreted tier emits exactly one display per tick.
    let pre = sim.drain_runtime_events();
    assert_eq!(pre.len(), TICKS as usize);
    for (index, event) in pre.iter().enumerate() {
        let celox::RuntimeEvent::Display { message } = event else {
            panic!("unexpected event {event:?}");
        };
        // The display observes the pre-increment value under FF scheduling.
        assert_eq!(message, &format!("tick {}", index).into_bytes());
    }

    // Wait out background compilation, then drive more ticks on the
    // compiled tier so drained events span both tiers.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while !sim.is_compiled() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
        sim.tick(clk).unwrap();
    }
    assert!(sim.is_compiled());
    // The counter value at promotion becomes the base for post messages;
    // the wait-loop tick count is intentionally unobservable.
    let base = sim.get_as::<u8>(sim.signal("cnt"));
    // Discard events from the wait-loop ticks.
    let _ = sim.drain_runtime_events();

    const POST_TICKS: u32 = 8;
    for _ in 0..POST_TICKS {
        sim.tick(clk).unwrap();
    }
    let post = sim.drain_runtime_events();
    assert_eq!(
        post.len(),
        POST_TICKS as usize,
        "the compiled tier keeps emitting one display per tick"
    );
    // The counter value at promotion is the base; messages continue from it,
    // wrapping at the signal's four-bit modulus.
    for (index, event) in post.iter().enumerate() {
        let celox::RuntimeEvent::Display { message } = event else {
            panic!("unexpected event {event:?}");
        };
        let expected = (u32::from(base) + index as u32) % 16;
        assert_eq!(message, &format!("tick {expected}").into_bytes());
    }

    let expected_final = (u32::from(base) + POST_TICKS) % 16;
    assert_eq!(sim.get_as::<u8>(sim.signal("cnt")), expected_final as u8);
}

#[test]
fn tiered_four_state_matches_two_state_across_promotion() {
    let code = r#"
module Top (
    clk: input clock,
    rst: input reset,
    d: input logic<8>,
    q: output logic<8>,
) {
    var stage1: logic<8>;
    var stage2: logic<8>;

    always_ff (clk, rst) {
        if_reset {
            stage1 = 0;
            stage2 = 0;
        } else {
            stage1 = d;
            stage2 = stage1;
        }
    }
    assign q = stage2;
}
"#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .four_state(true)
        .build_tiered()
        .unwrap();
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let d = sim.signal("d");
    let q = sim.signal("q");

    // Active-low reset with a known input, then absorb the pipeline.
    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(d, 9u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(rst, 1u8)).unwrap();

    // Drive inputs while interpreted and record the observed outputs.
    let mut observed = Vec::new();
    for value in 0..16u8 {
        sim.modify(|io| io.set(d, value)).unwrap();
        sim.tick(clk).unwrap();
        sim.tick(clk).unwrap();
        observed.push(sim.get_as::<u8>(q));
    }

    // Wait for background compilation so the promotion actually happens
    // inside this test instead of silently staying on the interpreter.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while !sim.is_compiled() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
        sim.tick(clk).unwrap();
    }
    assert!(
        sim.is_compiled(),
        "background compilation should complete during simulation"
    );
    assert!(sim.promotion_error().is_none());

    // The compiled tier must keep producing the same results: the two-stage
    // pipeline delivers each input exactly two ticks later, four-state X
    // handling notwithstanding.
    for value in 20..36u8 {
        sim.modify(|io| io.set(d, value)).unwrap();
        sim.tick(clk).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get_as::<u8>(q), value, "input {value}");
    }

    // Sanity on the recorded interpreted-tier window: each sampled output
    // equals the input driven in that iteration.
    for (index, observed_value) in observed.iter().enumerate() {
        assert_eq!(*observed_value, index as u8);
    }
}

const LANES: &str = r#"
module Lane (
    clk: input clock,
    rst: input reset,
    seed: input logic<32>,
    out: output logic<32>,
) {
    var s0: logic<32>;
    var s1: logic<32>;
    let m0: logic<32> = (s0 * 32'h9e3779b9) ^ (s1 >> 3);
    always_ff (clk, rst) {
        if_reset {
            s0 = 0;
            s1 = 1;
        } else {
            s0 = m0 ^ seed;
            s1 = m0 + s1;
        }
    }
    assign out = s0 ^ s1;
}

module Top (
    clk: input clock,
    rst: input reset,
    seed: input logic<32>,
    out: output logic<32>[16],
) {
    for i in 0..16: g {
        inst l: Lane (clk, rst, seed: seed + i, out: out[i]);
    }
}
"#;

macro_rules! lanes_reset {
    ($sim:expr) => {{
        let clk = $sim.event("clk");
        let rst = $sim.signal("rst");
        let seed = $sim.signal("seed");
        $sim.modify(|io| {
            io.set(rst, 0u8);
            io.set(seed, 0x1234u32);
        })
        .unwrap();
        $sim.tick(clk).unwrap();
        $sim.modify(|io| io.set(rst, 1u8)).unwrap();
    }};
}

macro_rules! lanes_tick {
    ($sim:expr) => {{
        let clk = $sim.event("clk");
        $sim.tick(clk).unwrap();
    }};
}

macro_rules! lanes_out {
    ($sim:expr) => {{
        let out = $sim.signal("out");
        $sim.get(out)
    }};
}

/// Lane-partitioned kernels need one arena per lane; promotion must still fit
/// in the reserved image and the results must match sequential execution.
#[test]
fn tiered_promotion_with_lane_partitioned_kernels() {
    let mut sim = SimulatorBuilder::new(LANES, "Top")
        .threads(4)
        .parallel_partition(celox::ParallelPartition::Always)
        .build_tiered()
        .unwrap();
    let mut reference = SimulatorBuilder::new(LANES, "Top").build().unwrap();
    lanes_reset!(sim);
    lanes_reset!(reference);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while !sim.is_compiled() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
        lanes_tick!(sim);
        lanes_tick!(reference);
    }
    assert!(sim.is_compiled(), "tiered simulation was not promoted");
    assert!(sim.promotion_error().is_none());
    for _ in 0..64 {
        lanes_tick!(sim);
        lanes_tick!(reference);
        assert_eq!(lanes_out!(sim), lanes_out!(reference));
    }
}

/// Wildcard comparison results must remain X when the native tier takes over.
#[cfg(feature = "systemverilog")]
#[test]
fn tiered_wildcard_unknown_stays_x_across_promotion() {
    let code = r#"
module Top(input logic [3:0] x, y, output logic neq);
    assign neq = (x !=? y);
endmodule
"#;
    let mut sim =
        celox::Simulator::from_sv_sources(vec![(code, std::path::Path::new("wildcard.sv"))], "Top")
            .four_state(true)
            .tier_promotion(celox::TierPromotion::AfterSteps(32))
            .build_tiered()
            .unwrap();
    let x = sim.signal("x");
    let y = sim.signal("y");
    let neq = sim.signal("neq");
    sim.modify(|io| {
        io.set_four_state(x, 4u8.into(), 3u8.into());
        io.set(y, 4u8);
    })
    .unwrap();
    let expected = (1u8.into(), 1u8.into());
    assert!(!sim.is_compiled());
    assert_eq!(sim.get_four_state(neq), expected);

    // The step threshold holds the initial tier; this deadline only bounds
    // waiting for the background compiler and does not select the tier.
    for _ in 0..32 {
        sim.eval_comb().unwrap();
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while !sim.is_compiled() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
        sim.eval_comb().unwrap();
    }
    assert!(sim.is_compiled(), "background compilation did not finish");
    assert!(sim.promotion_error().is_none());
    assert_eq!(sim.get_four_state(neq), expected);
}

const DERIVED: &str = r#"
module Top (
    clk: input clock,
    rst: input reset,
    en: input logic,
    cnt: output logic<8>,
    extra: output logic<8>,
    more: output logic<8>,
) {
    var c: logic<8>;
    var e: logic<8>;
    var m: logic<8>;
    var half: logic;
    let gated: '_ clock = clk & en;
    let divided: '_ clock = half;
    always_ff (clk, rst) {
        if_reset {
            c = 0;
            half = 0;
        } else {
            c += 1;
            half = ~half;
        }
    }
    always_ff (gated) {
        e += c;
    }
    always_ff (divided) {
        m += 1;
    }
    assign cnt = c;
    assign extra = e;
    assign more = m;
}
"#;

/// Derived clock edges walk their plans until the tier with the generated
/// derived-edge units runs (the optimizing image of a two-stage build), then
/// run the units, matching the plan walk throughout.
#[test]
fn tiered_derived_edges_switch_to_generated_units() {
    use celox::SimBackend as _;
    let mut tiered: Simulator<TieredBackend> = SimulatorBuilder::new(DERIVED, "Top")
        .build_tiered()
        .unwrap();
    let mut walked = Simulator::builder(DERIVED, "Top")
        .disable_pass(celox::SirPass::DerivedEdgeUnits)
        .build()
        .unwrap();
    let outputs = ["cnt", "extra", "more"];
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let mut live_ticks = 0u32;
    let mut step = 0u32;
    while live_ticks < 32 {
        assert!(
            std::time::Instant::now() < deadline,
            "the generated derived-edge units never became live"
        );
        let (rst, en) = (u8::from(step > 1), u8::from(!step.is_multiple_of(5)));
        let reset = tiered.signal("rst");
        let enable = tiered.signal("en");
        tiered
            .modify(|io| {
                io.set(reset, rst);
                io.set(enable, en);
            })
            .unwrap();
        let reset = walked.signal("rst");
        let enable = walked.signal("en");
        walked
            .modify(|io| {
                io.set(reset, rst);
                io.set(enable, en);
            })
            .unwrap();
        let clk = tiered.event("clk");
        tiered.tick(clk).unwrap();
        let clk = walked.event("clk");
        walked.tick(clk).unwrap();
        for name in outputs {
            let left = tiered.get_as::<u8>(tiered.signal(name));
            let right = walked.get_as::<u8>(walked.signal(name));
            assert_eq!(left, right, "step {step}: {name}");
        }
        if tiered.backend_ref().runs_derived_edge_units() {
            live_ticks += 1;
        } else {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        step += 1;
    }
}
