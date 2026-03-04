use celox::Simulator;

/// Test NBA semantics across separate always_ff blocks with the same clock.
/// In RTL, two always_ff blocks on the same clock should both read OLD values
/// (pre-edge) and write NEW values (post-edge), regardless of textual order.
#[test]
fn test_nba_separate_blocks_swap() {
    let code = r#"
        module Top (clk: input clock, rst: input reset, a: output logic<8>, b: output logic<8>) {
            var r1: logic<8>;
            var r2: logic<8>;

            // Block 1: r1 <= r2 (reads OLD r2)
            always_ff (clk, rst) {
                if_reset {
                    r1 = 8'hAA;
                } else {
                    r1 = r2;
                }
            }

            // Block 2: r2 <= r1 (reads OLD r1)
            always_ff (clk, rst) {
                if_reset {
                    r2 = 8'h55;
                } else {
                    r2 = r1;
                }
            }

            assign a = r1;
            assign b = r2;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let a = sim.signal("a");
    let b = sim.signal("b");

    // Reset
    sim.modify(|io| io.set(rst, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(a), 0xAAu32.into(), "r1 should be 0xAA after reset");
    assert_eq!(sim.get(b), 0x55u32.into(), "r2 should be 0x55 after reset");

    // Deactivate reset and tick once — should swap
    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.get(a),
        0x55u32.into(),
        "r1 should be 0x55 (old r2) after swap"
    );
    assert_eq!(
        sim.get(b),
        0xAAu32.into(),
        "r2 should be 0xAA (old r1) after swap"
    );

    // Tick again — should swap back
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.get(a),
        0xAAu32.into(),
        "r1 should be 0xAA after 2nd swap"
    );
    assert_eq!(
        sim.get(b),
        0x55u32.into(),
        "r2 should be 0x55 after 2nd swap"
    );
}

/// Test pipeline pattern across 3 separate always_ff blocks.
/// d → stage1 → stage2 → stage3 should take 3 clock cycles.
#[test]
fn test_nba_separate_blocks_pipeline() {
    let code = r#"
        module Top (clk: input clock, d: input logic<8>, q: output logic<8>) {
            var stage1: logic<8>;
            var stage2: logic<8>;

            always_ff (clk) { stage1 = d; }
            always_ff (clk) { stage2 = stage1; }
            always_ff (clk) { q = stage2; }
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let q = sim.signal("q");

    sim.modify(|io| io.set(d, 0x42u8)).unwrap();

    // Tick 1: stage1=0x42, stage2=0, q=0
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0x0u32.into(), "q should be 0 after 1st tick");

    // Tick 2: stage1=0x42, stage2=0x42, q=0
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0x0u32.into(), "q should be 0 after 2nd tick");

    // Tick 3: q=0x42
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.get(q),
        0x42u32.into(),
        "q should be 0x42 after 3rd tick"
    );
}

/// Test that the order of always_ff blocks in source code does not matter.
/// Reverse the pipeline order (q first, stage1 last) — same behavior expected.
#[test]
fn test_nba_separate_blocks_pipeline_reversed() {
    let code = r#"
        module Top (clk: input clock, d: input logic<8>, q: output logic<8>) {
            var stage1: logic<8>;
            var stage2: logic<8>;

            // Intentionally reversed order in source
            always_ff (clk) { q = stage2; }
            always_ff (clk) { stage2 = stage1; }
            always_ff (clk) { stage1 = d; }
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let q = sim.signal("q");

    sim.modify(|io| io.set(d, 0x42u8)).unwrap();

    // Tick 1: stage1=0x42, stage2=0, q=0
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0x0u32.into(), "q should be 0 after 1st tick");

    // Tick 2: stage1=0x42, stage2=0x42, q=0
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0x0u32.into(), "q should be 0 after 2nd tick");

    // Tick 3: q=0x42
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.get(q),
        0x42u32.into(),
        "q should be 0x42 after 3rd tick"
    );
}
