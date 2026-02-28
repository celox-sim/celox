use celox::Simulation;

#[test]
fn test_ff_comb_clock_cascade() {
    let code = r#"
        module Top (
            clk: input clock,
            rst: input reset_async_high,
            en: input logic<1>,
            cnt_out: output logic<8>
        ) {
            var ff_clk: logic<1>;
            var gated_clk: clock;
            var cnt: logic<8>;

            always_ff (clk, rst) {
                if_reset {
                    ff_clk = 1'b0;
                } else {
                    ff_clk = ~ff_clk;
                }
            }

            // Combinational logic driven by FF, acting as a clock
            assign gated_clk = ff_clk & en;

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

    let en = sim.signal("en");
    sim.schedule("rst", 0, 1).unwrap();
    sim.schedule("clk", 0, 0).unwrap();
    sim.modify(|io| io.set(en, 1u8)).unwrap();
    sim.step().unwrap();

    sim.schedule("rst", 10, 0).unwrap();
    sim.step().unwrap();

    // clk rises. ff_clk -> 1. gated_clk -> 1.
    // cnt should increment from 0 -> 1.
    sim.schedule("clk", 20, 1).unwrap();
    sim.step().unwrap();

    assert_eq!(
        sim.get(cnt_out),
        1u8.into(),
        "FF -> Comb -> Clock cascade failed"
    );
}
