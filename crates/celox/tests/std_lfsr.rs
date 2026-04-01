use celox::Simulator;
use std::collections::HashSet;

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

const LFSR_SRC: &str =
    include_str!("../../../deps/veryl/crates/std/veryl/src/lfsr/lfsr_galois.veryl");

/// LFSR Galois has no reset port -- uses i_set for initialization.
/// The Top module wraps it with a clock but no reset connection.
const TOP: &str = r#"
module Top (
    clk     : input  clock,
    rst     : input  reset,
    i_en    : input  logic,
    i_set   : input  logic,
    i_setval: input  logic<8>,
    o_val   : output logic<8>,
) {
    inst u: lfsr_galois #(SIZE: 8) (
        i_clk   : clk,
        i_en,
        i_set,
        i_setval,
        o_val,
    );
}
"#;

all_backends! {

    // Basic LFSR: seed then shift, verify output changes
    //
    // NOTE: Veryl analyzer has a bug where generate-if `TAPVEC[K]` bit-index
    // conditions are always evaluated as true, so all bit positions get XOR taps
    // instead of only the ones specified by the tap vector. This makes the LFSR
    // cycle length incorrect (9 instead of 255 for SIZE=8). These tests verify
    // the simulation mechanics (seed, shift, hold) but not LFSR correctness.
    #[ignore = "Veryl analyzer bug: generate-if TAPVEC[K] bit-index always evaluates true"]
    fn test_lfsr_basic_shift(sim) {
        @setup { let code = format!("{LFSR_SRC}\n{TOP}"); }
        @build Simulator::builder(&code, "Top");
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let i_en = sim.signal("i_en");
    let i_set = sim.signal("i_set");
    let i_setval = sim.signal("i_setval");
    let o_val = sim.signal("o_val");

    // Reset (for Top module convention, LFSR itself ignores reset)
    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(i_en, 0u8);
        io.set(i_set, 0u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(rst, 1u8)).unwrap();

    // Seed LFSR with 1
    sim.modify(|io| {
        io.set(i_en, 1u8);
        io.set(i_set, 1u8);
        io.set(i_setval, 1u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();

    // Disable set, enable shifting
    sim.modify(|io| io.set(i_set, 0u8)).unwrap();

    // Collect 10 values and verify they change
    let mut values = Vec::new();
    for _ in 0..10 {
        sim.tick(clk).unwrap();
        let val = sim.get_as::<u8>(o_val);
        values.push(val);
    }

    // All values should be non-zero (maximal LFSR never reaches 0)
    for (i, &v) in values.iter().enumerate() {
        assert_ne!(v, 0, "LFSR output should never be 0 (tick {i})");
    }

    // Values should not all be the same
    let unique: HashSet<u8> = values.iter().copied().collect();
    assert!(unique.len() > 1, "LFSR should produce varying outputs");

    }

    // Cycle detection: LFSR should produce a deterministic repeating cycle
    #[ignore = "Veryl analyzer bug: generate-if TAPVEC[K] bit-index always evaluates true"]
    fn test_lfsr_deterministic_cycle(sim) {
        @setup { let code = format!("{LFSR_SRC}\n{TOP}"); }
        @build Simulator::builder(&code, "Top");
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let i_en = sim.signal("i_en");
    let i_set = sim.signal("i_set");
    let i_setval = sim.signal("i_setval");
    let o_val = sim.signal("o_val");

    // Reset
    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(i_en, 0u8);
        io.set(i_set, 0u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(rst, 1u8)).unwrap();

    // Seed with 1
    sim.modify(|io| {
        io.set(i_en, 1u8);
        io.set(i_set, 1u8);
        io.set(i_setval, 1u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(i_set, 0u8)).unwrap();

    // Collect first 300 values to find cycle
    let mut values = Vec::new();
    for _ in 0..300 {
        sim.tick(clk).unwrap();
        let val = sim.get_as::<u8>(o_val);
        values.push(val);
    }

    // LFSR should produce non-zero values
    for (i, &v) in values.iter().enumerate() {
        assert_ne!(v, 0, "LFSR output should never be 0 (tick {i})");
    }

    // Find cycle length: detect when the first value repeats
    let first = values[0];
    let cycle_len = values[1..].iter().position(|&v| v == first).map(|p| p + 1);
    assert!(
        cycle_len.is_some(),
        "LFSR should have a repeating cycle within 300 ticks"
    );
    let cycle_len = cycle_len.unwrap();
    assert!(cycle_len > 1, "cycle length should be > 1, got {cycle_len}");

    // Verify the cycle actually repeats
    for i in 0..cycle_len {
        assert_eq!(
            values[i],
            values[i + cycle_len],
            "cycle should repeat at offset {i}"
        );
    }

    }

    // LFSR disabled (i_en=0) should hold its value
    #[ignore = "Veryl analyzer bug: generate-if TAPVEC[K] bit-index always evaluates true"]
    fn test_lfsr_enable_hold(sim) {
        @setup { let code = format!("{LFSR_SRC}\n{TOP}"); }
        @build Simulator::builder(&code, "Top");
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let i_en = sim.signal("i_en");
    let i_set = sim.signal("i_set");
    let i_setval = sim.signal("i_setval");
    let o_val = sim.signal("o_val");

    // Reset
    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(i_en, 0u8);
        io.set(i_set, 0u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(rst, 1u8)).unwrap();

    // Seed with 0x42
    sim.modify(|io| {
        io.set(i_en, 1u8);
        io.set(i_set, 1u8);
        io.set(i_setval, 0x42u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();

    // Shift a few times
    sim.modify(|io| io.set(i_set, 0u8)).unwrap();
    for _ in 0..5 {
        sim.tick(clk).unwrap();
    }
    let val_before = sim.get_as::<u8>(o_val);

    // Disable: value should hold
    sim.modify(|io| io.set(i_en, 0u8)).unwrap();
    for _ in 0..5 {
        sim.tick(clk).unwrap();
        assert_eq!(
            sim.get_as::<u8>(o_val),
            val_before,
            "LFSR should hold when disabled"
        );
    }

    }
}
