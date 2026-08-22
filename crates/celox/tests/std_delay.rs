use celox::Simulator;

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    // DELAY=0: passthrough (no delay)
    fn test_delay_zero(sim) {
        @setup { let top = r#"
module Top (
clk: input  clock,
rst: input  reset,
i_d: input  logic<8>,
o_d: output logic<8>,
) {
inst u: delay #(DELAY: 0, WIDTH: 8) (
i_clk: clk,
i_rst: rst,
i_d,
o_d,
);
}
"#;
let code = format!("{}\n{top}", test_utils::veryl_std::source(&["delay", "delay.veryl"])); }
        @build Simulator::builder(&code, "Top");
    let i_d = sim.signal("i_d");
    let o_d = sim.signal("o_d");

    // Combinational passthrough - no clock needed
    sim.modify(|io| io.set(i_d, 0xABu8)).unwrap();
    assert_eq!(sim.get_as::<u8>(o_d), 0xAB, "DELAY=0 should pass through");

    sim.modify(|io| io.set(i_d, 0xCDu8)).unwrap();
    assert_eq!(sim.get_as::<u8>(o_d), 0xCD);

    }

    // DELAY=1: one cycle delay
    fn test_delay_one(sim) {
        @setup { let top = r#"
module Top (
clk: input  clock,
rst: input  reset,
i_d: input  logic<8>,
o_d: output logic<8>,
) {
inst u: delay #(DELAY: 1, WIDTH: 8) (
i_clk: clk,
i_rst: rst,
i_d,
o_d,
);
}
"#;
let code = format!("{}\n{top}", test_utils::veryl_std::source(&["delay", "delay.veryl"])); }
        @build Simulator::builder(&code, "Top");
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let i_d = sim.signal("i_d");
    let o_d = sim.signal("o_d");

    // Reset
    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(i_d, 0u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(rst, 1u8)).unwrap();

    assert_eq!(sim.get_as::<u8>(o_d), 0, "output should be 0 after reset");

    // Set input to 0xAA
    sim.modify(|io| io.set(i_d, 0xAAu8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get_as::<u8>(o_d), 0xAA, "should appear after 1 tick");

    // Change input to 0xBB
    sim.modify(|io| io.set(i_d, 0xBBu8)).unwrap();
    // Before tick, output still 0xAA
    sim.tick(clk).unwrap();
    assert_eq!(sim.get_as::<u8>(o_d), 0xBB, "should appear after 1 tick");

    }

    // DELAY=3: three cycle pipeline
    fn test_delay_three(sim) {
        @setup { let top = r#"
module Top (
clk: input  clock,
rst: input  reset,
i_d: input  logic<8>,
o_d: output logic<8>,
) {
inst u: delay #(DELAY: 3, WIDTH: 8) (
i_clk: clk,
i_rst: rst,
i_d,
o_d,
);
}
"#;
let code = format!("{}\n{top}", test_utils::veryl_std::source(&["delay", "delay.veryl"])); }
        @build Simulator::builder(&code, "Top");
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let i_d = sim.signal("i_d");
    let o_d = sim.signal("o_d");

    // Reset
    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(i_d, 0u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(rst, 1u8)).unwrap();

    // Send 0xAA, then track when it appears at output
    sim.modify(|io| io.set(i_d, 0xAAu8)).unwrap();

    // Tick 1: pipeline stage 0 has 0xAA
    sim.tick(clk).unwrap();
    assert_eq!(sim.get_as::<u8>(o_d), 0, "not yet after 1 tick");

    // Tick 2: pipeline stage 1 has 0xAA
    sim.modify(|io| io.set(i_d, 0xBBu8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get_as::<u8>(o_d), 0, "not yet after 2 ticks");

    // Tick 3: pipeline stage 2 (output) has 0xAA
    sim.modify(|io| io.set(i_d, 0xCCu8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.get_as::<u8>(o_d),
        0xAA,
        "0xAA should appear after 3 ticks"
    );

    // Tick 4: 0xBB arrives
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.get_as::<u8>(o_d),
        0xBB,
        "0xBB should appear after 4 ticks"
    );

    // Tick 5: 0xCC arrives
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.get_as::<u8>(o_d),
        0xCC,
        "0xCC should appear after 5 ticks"
    );

    }
}
