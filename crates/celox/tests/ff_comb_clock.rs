use celox::Simulation;

#[test]
fn test_ff_comb_clock_cascade() {
    // Test: FF-driven clock divider feeds a gated clock.
    // clk_div is an external half-rate clock. en gates it combinationally.
    // cnt increments on gated_clk rising edge.
    let code = r#"
        module Top (
            clk: input '_ clock,
            clk_div: input '_ clock,
            rst: input '_ reset_async_high,
            en: input logic<1>,
            cnt_out: output logic<8>
        ) {
            var cnt: logic<8>;

            // Combinational gated clock from clock input
            let gated_clk: '_ clock = clk_div & en;

            always_ff (gated_clk, rst) {
                if_reset {
                    cnt = 8'd0;
                } else {
                    cnt = cnt + 8'd1;
                }
            }

            assign cnt_out = cnt;
        }
    "#;

    let mut sim = Simulation::builder(code, "Top").build().unwrap();
    let cnt_out = sim.signal("cnt_out");

    // Reset
    sim.schedule("rst", 0, 1).unwrap();
    sim.schedule("clk", 0, 0).unwrap();
    sim.schedule("clk_div", 0, 0).unwrap();
    let en = sim.signal("en");
    sim.modify(|io| io.set(en, 1u8)).unwrap();
    sim.step().unwrap();

    // Release reset
    sim.schedule("rst", 10, 0).unwrap();
    sim.step().unwrap();

    // clk and clk_div both rise. en=1 => gated_clk rises.
    // cnt: 0 -> 1
    sim.schedule("clk", 20, 1).unwrap();
    sim.schedule("clk_div", 20, 1).unwrap();
    sim.step().unwrap();

    assert_eq!(
        sim.get(cnt_out),
        1u8.into(),
        "FF -> Comb -> Clock cascade failed"
    );
}
