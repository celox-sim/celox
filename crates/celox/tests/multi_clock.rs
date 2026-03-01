use celox::Simulator;

/// Two independent clock domains: each FF only advances on its own clock.
#[test]
fn test_independent_clock_domains() {
    let code = r#"
        module Top (
            clk_a: input  clock,
            clk_b: input  clock,
            rst:   input  reset,
            da:    input  logic<8>,
            db:    input  logic<8>,
            qa:    output logic<8>,
            qb:    output logic<8>
        ) {
            var ra: logic<8>;
            var rb: logic<8>;
            always_ff (clk_a, rst) {
                if_reset {
                    ra = 8'd0;
                } else {
                    ra = da;
                }
            }
            always_ff (clk_b, rst) {
                if_reset {
                    rb = 8'd0;
                } else {
                    rb = db;
                }
            }
            assign qa = ra;
            assign qb = rb;
        }
    "#;

    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk_a = sim.event("clk_a");
    let clk_b = sim.event("clk_b");
    let rst = sim.signal("rst");
    let da = sim.signal("da");
    let db = sim.signal("db");
    let qa = sim.signal("qa");
    let qb = sim.signal("qb");

    // Reset both (AsyncLow: rst=0 means active)
    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(da, 0xAAu8);
        io.set(db, 0xBBu8);
    })
    .unwrap();
    sim.tick(clk_a).unwrap();
    sim.tick(clk_b).unwrap();
    assert_eq!(sim.get(qa), 0u8.into());
    assert_eq!(sim.get(qb), 0u8.into());

    // Release reset
    sim.modify(|io| io.set(rst, 1u8)).unwrap();

    // Only tick clk_a: qa should update, qb should stay 0
    sim.tick(clk_a).unwrap();
    assert_eq!(sim.get(qa), 0xAAu8.into());
    assert_eq!(sim.get(qb), 0u8.into());

    // Only tick clk_b: qb should update, qa stays
    sim.tick(clk_b).unwrap();
    assert_eq!(sim.get(qa), 0xAAu8.into());
    assert_eq!(sim.get(qb), 0xBBu8.into());
}

/// A counter in one clock domain feeding into another (CDC pattern).
/// Tests that domains are truly independent.
#[test]
fn test_clock_domain_crossing_pattern() {
    let code = r#"
        module Top (
            clk_fast: input  clock,
            clk_slow: input  clock,
            rst:      input  reset,
            count_out:  output logic<4>,
            sample_out: output logic<4>
        ) {
            var counter: logic<4>;
            var sample:  logic<4>;

            // Fast domain: counter increments
            always_ff (clk_fast, rst) {
                if_reset {
                    counter = 4'd0;
                } else {
                    counter = counter + 4'd1;
                }
            }

            // Slow domain: samples the counter
            always_ff (clk_slow, rst) {
                if_reset {
                    sample = 4'd0;
                } else {
                    sample = counter;
                }
            }

            assign count_out  = counter;
            assign sample_out = sample;
        }
    "#;

    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk_fast = sim.event("clk_fast");
    let clk_slow = sim.event("clk_slow");
    let rst = sim.signal("rst");
    let count_out = sim.signal("count_out");
    let sample_out = sim.signal("sample_out");

    // Reset (AsyncLow: rst=0 means active)
    sim.modify(|io| io.set(rst, 0u8)).unwrap();
    sim.tick(clk_fast).unwrap();
    sim.tick(clk_slow).unwrap();
    assert_eq!(sim.get(count_out), 0u8.into());
    assert_eq!(sim.get(sample_out), 0u8.into());

    // Release reset
    sim.modify(|io| io.set(rst, 1u8)).unwrap();

    // Tick fast 3 times (counter goes 1→2→3)
    sim.tick(clk_fast).unwrap();
    sim.tick(clk_fast).unwrap();
    sim.tick(clk_fast).unwrap();
    assert_eq!(sim.get(count_out), 3u8.into());
    // sample hasn't been clocked, still 0
    assert_eq!(sim.get(sample_out), 0u8.into());

    // Now tick slow once — captures counter=3
    sim.tick(clk_slow).unwrap();
    assert_eq!(sim.get(sample_out), 3u8.into());

    // Tick fast 2 more times (counter goes 4→5)
    sim.tick(clk_fast).unwrap();
    sim.tick(clk_fast).unwrap();
    assert_eq!(sim.get(count_out), 5u8.into());
    // sample still holds 3
    assert_eq!(sim.get(sample_out), 3u8.into());
}

/// FF with separate clocks and separate resets.
#[test]
fn test_separate_resets_per_domain() {
    let code = r#"
        module Top (
            clk_a: input  clock,
            clk_b: input  clock,
            rst_a: input  reset,
            rst_b: input  reset,
            da:    input  logic<8>,
            db:    input  logic<8>,
            qa:    output logic<8>,
            qb:    output logic<8>
        ) {
            var ra: logic<8>;
            var rb: logic<8>;
            always_ff (clk_a, rst_a) {
                if_reset {
                    ra = 8'hAA;
                } else {
                    ra = da;
                }
            }
            always_ff (clk_b, rst_b) {
                if_reset {
                    rb = 8'hBB;
                } else {
                    rb = db;
                }
            }
            assign qa = ra;
            assign qb = rb;
        }
    "#;

    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk_a = sim.event("clk_a");
    let clk_b = sim.event("clk_b");
    let rst_a = sim.signal("rst_a");
    let rst_b = sim.signal("rst_b");
    let da = sim.signal("da");
    let db = sim.signal("db");
    let qa = sim.signal("qa");
    let qb = sim.signal("qb");

    // Reset only domain A (AsyncLow: rst=0 means active)
    sim.modify(|io| {
        io.set(rst_a, 0u8);
        io.set(rst_b, 1u8);
        io.set(da, 0x11u8);
        io.set(db, 0x22u8);
    })
    .unwrap();
    sim.tick(clk_a).unwrap();
    sim.tick(clk_b).unwrap();
    assert_eq!(sim.get(qa), 0xAAu8.into()); // A in reset
    assert_eq!(sim.get(qb), 0x22u8.into()); // B captures data

    // Release A, assert B
    sim.modify(|io| {
        io.set(rst_a, 1u8);
        io.set(rst_b, 0u8);
    })
    .unwrap();
    sim.tick(clk_a).unwrap();
    sim.tick(clk_b).unwrap();
    assert_eq!(sim.get(qa), 0x11u8.into()); // A captures data
    assert_eq!(sim.get(qb), 0xBBu8.into()); // B in reset
}



