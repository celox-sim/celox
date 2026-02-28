use celox::Simulation;

#[test]
fn test_cascade_race_condition() {
    // This design demonstrates a race condition in the current simulator implementation.
    // cnt1 is incremented by clk.
    // gclk is derived from clk (combinational cascade).
    // cnt2 is incremented by cnt1, triggered by gclk.
    // In a correct simulation, when clk rises, cnt2 should be incremented by the OLD value of cnt1.
    // In the current implementation, clk domain is evaluated and updates cnt1 BEFORE gclk domain is evaluated.
    let code = r#"
        module Top (
            clk: input clock,
            rst: input reset_async_high,
            cnt1_out: output logic<8>,
            cnt2_out: output logic<8>
        ) {
            var cnt1: logic<8>;
            var cnt2: logic<8>;
            var gclk: clock;

            assign gclk = clk; // Combinational cascade

            always_ff (clk, rst) {
                if_reset {
                    cnt1 = 8'd0;
                } else {
                    cnt1 = cnt1 + 8'd1;
                }
            }

            always_ff (gclk, rst) {
                if_reset {
                    cnt2 = 8'd0;
                } else {
                    cnt2 = cnt2 + cnt1;
                }
            }

            assign cnt1_out = cnt1;
            assign cnt2_out = cnt2;
        }
    "#;

    let mut sim = Simulation::builder(code, "Top").build().unwrap();
    let cnt1_out = sim.signal("cnt1_out");
    let cnt2_out = sim.signal("cnt2_out");

    // Reset
    sim.schedule("rst", 0, 1).unwrap();
    sim.schedule("clk", 0, 0).unwrap();
    sim.step().unwrap();
    assert_eq!(sim.get(cnt1_out), 0u8.into());
    assert_eq!(sim.get(cnt2_out), 0u8.into());

    // Release reset
    sim.schedule("rst", 10, 0).unwrap();
    sim.step().unwrap();

    // 1st tick:
    // clk rises.
    // cnt1: 0 -> 1
    // gclk rises (cascaded).
    // cnt2: 0 + cnt1(OLD=0) -> 0
    sim.schedule("clk", 20, 1).unwrap();
    sim.step().unwrap();
    println!(
        "Step 1: cnt1={}, cnt2={}",
        sim.get(cnt1_out),
        sim.get(cnt2_out)
    );

    // 2nd tick:
    // clk falls
    sim.schedule("clk", 30, 0).unwrap();
    sim.step().unwrap();

    // 3rd tick:
    // clk rises.
    // cnt1: 1 -> 2
    // gclk rises (cascaded).
    // cnt2: 0 + cnt1(OLD=1) -> 1
    sim.schedule("clk", 40, 1).unwrap();
    sim.step().unwrap();
    println!(
        "Step 2: cnt1={}, cnt2={}",
        sim.get(cnt1_out),
        sim.get(cnt2_out)
    );

    assert_eq!(sim.get(cnt1_out), 2u8.into());
    assert_eq!(
        sim.get(cnt2_out),
        1u8.into(),
        "Race condition detected: cnt2 should have used OLD value of cnt1"
    );
}

#[test]
fn test_sequential_cascade_race_condition() {
    // This design demonstrates a race condition with sequential cascade.
    // clk -> clk_div
    // clk_div -> cnt
    let code = r#"
        module Top (
            clk: input clock,
            rst: input reset_async_high,
            cnt_out: output logic<8>
        ) {
            var cnt1: logic<8>;
            var cnt2: logic<8>;
            var clk_div: clock;

            always_ff (clk, rst) {
                if_reset {
                    cnt1 = 8'd0;
                    clk_div = 1'b0;
                } else {
                    cnt1 = cnt1 + 8'd1;
                    clk_div = ~clk_div;
                }
            }

            // clk_div rises when clk rises (if clk_div was 0)
            always_ff (clk_div, rst) {
                if_reset {
                    cnt2 = 8'd0;
                } else {
                    cnt2 = cnt2 + cnt1;
                }
            }

            assign cnt_out = cnt2;
        }
    "#;

    let mut sim = Simulation::builder(code, "Top").build().unwrap();
    let cnt_out = sim.signal("cnt_out");

    // Reset
    sim.schedule("rst", 0, 1).unwrap();
    sim.schedule("clk", 0, 0).unwrap();
    sim.step().unwrap();

    // Release reset
    sim.schedule("rst", 10, 0).unwrap();
    sim.step().unwrap();

    // 1st tick: clk rises, clk_div: 0 -> 1.
    // cnt1: 0 -> 1
    // cnt2: 0 + cnt1(OLD=0) -> 0
    sim.schedule("clk", 20, 1).unwrap();
    sim.step().unwrap();
    println!("Seq Step 1: cnt={}", sim.get(cnt_out));

    // 2nd tick: clk falls
    sim.schedule("clk", 30, 0).unwrap();
    sim.step().unwrap();

    // 3rd tick: clk rises, clk_div: 1 -> 0.
    // cnt1: 1 -> 2
    // cnt2: remains 0
    sim.schedule("clk", 40, 1).unwrap();
    sim.step().unwrap();
    println!("Seq Step 2: cnt={}", sim.get(cnt_out));

    // 4th tick: clk falls
    sim.schedule("clk", 50, 0).unwrap();
    sim.step().unwrap();

    // 5th tick: clk rises, clk_div: 0 -> 1.
    // cnt1: 2 -> 3
    // cnt2: 0 + cnt1(OLD=2) -> 2
    sim.schedule("clk", 60, 1).unwrap();
    sim.step().unwrap();
    println!("Seq Step 3: cnt={}", sim.get(cnt_out));

    assert_eq!(
        sim.get(cnt_out),
        2u8.into(),
        "Race condition detected in sequential cascade"
    );
}
