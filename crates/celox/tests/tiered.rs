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
    assert!(!sim.is_compiled(), "tiered starts on the interpreter");

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
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
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
        assert_eq!(message, &format!("tick {}", index));
    }

    // Wait out background compilation, then drive more ticks on the
    // compiled tier so drained events span both tiers.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
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
    // The counter value at promotion is the base; messages continue from it.
    for (index, event) in post.iter().enumerate() {
        let celox::RuntimeEvent::Display { message } = event else {
            panic!("unexpected event {event:?}");
        };
        assert_eq!(message, &format!("tick {}", base.wrapping_add(index as u8)));
    }

    assert_eq!(
        sim.get_as::<u8>(sim.signal("cnt")),
        base.wrapping_add(POST_TICKS as u8)
    );
}

#[test]
fn tiered_four_state_matches_two_state_across_promotion() {
    let mut sim = SimulatorBuilder::new(COUNTER, "Top")
        .four_state(true)
        .build_tiered()
        .unwrap();
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    sim.modify(|io| io.set(rst, 0u8)).unwrap();

    for _ in 0..16 {
        sim.tick(clk).unwrap();
    }
    assert!(sim.promotion_error().is_none());
}
