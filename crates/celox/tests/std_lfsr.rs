use celox::Simulator;
use std::collections::HashSet;

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

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
    fn test_lfsr_basic_shift(sim) {
        @setup { let code = format!("{}\n{TOP}", test_utils::veryl_std::source(&["lfsr", "lfsr_galois.veryl"])); }
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
    fn test_lfsr_deterministic_cycle(sim) {
        @setup { let code = format!("{}\n{TOP}", test_utils::veryl_std::source(&["lfsr", "lfsr_galois.veryl"])); }
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

    // Collect values until the first seeded state repeats.
    let mut values = Vec::new();
    let first = {
        sim.tick(clk).unwrap();
        sim.get_as::<u8>(o_val)
    };
    values.push(first);

    let cycle_len = loop {
        assert!(
            values.len() < 512,
            "LFSR should repeat within 512 ticks, observed {} values without a repeat",
            values.len()
        );
        sim.tick(clk).unwrap();
        let val = sim.get_as::<u8>(o_val);
        if val == first {
            break values.len();
        }
        values.push(val);
    };

    // LFSR should produce non-zero values
    for (i, &v) in values.iter().enumerate() {
        assert_ne!(v, 0, "LFSR output should never be 0 (tick {i})");
    }

    assert_eq!(
        cycle_len, 255,
        "8-bit maximal LFSR should visit all non-zero states"
    );

    // Verify the cycle actually repeats for one more full period.
    for (i, expected) in values
        .iter()
        .copied()
        .skip(1)
        .chain(std::iter::once(first))
        .enumerate()
    {
        sim.tick(clk).unwrap();
        assert_eq!(
            sim.get_as::<u8>(o_val),
            expected,
            "cycle should repeat at offset {i}"
        );
    }

    }

    // LFSR disabled (i_en=0) should hold its value
    fn test_lfsr_enable_hold(sim) {
        @setup { let code = format!("{}\n{TOP}", test_utils::veryl_std::source(&["lfsr", "lfsr_galois.veryl"])); }
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
