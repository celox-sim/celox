use celox::Simulator;

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    // Simple counter: increment on each tick, reset to 0
    fn test_counter_n4_basic(sim) {
        @ignore_on(veryl);
        @setup { let code = r#"
module Top (
clk: input clock,
rst: input reset,
cnt0: output logic<32>,
cnt1: output logic<32>,
cnt3: output logic<32>,
) {
var cnt: logic<32>[4];
assign cnt0 = cnt[0];
assign cnt1 = cnt[1];
assign cnt3 = cnt[3];
for i in 0..4: g {
always_ff (clk, rst) {
if_reset { cnt[i] = 0; }
else { cnt[i] += 1; }
}
}
}
"#; }
        @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let cnt0 = sim.signal("cnt0");
    let cnt1 = sim.signal("cnt1");
    let cnt3 = sim.signal("cnt3");

    // Assert reset (active low: rst=0)
    sim.modify(|io| io.set(rst, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(cnt0), 0u32.into());
    assert_eq!(sim.get(cnt1), 0u32.into());

    // Deassert reset (rst=1), start counting
    sim.modify(|io| io.set(rst, 1u8)).unwrap();

    // Tick 10 times
    for _ in 0..10 {
        sim.tick(clk).unwrap();
    }
    assert_eq!(sim.get(cnt0), 10u32.into());
    assert_eq!(sim.get(cnt1), 10u32.into());
    assert_eq!(sim.get(cnt3), 10u32.into());

    // 100 more ticks
    for _ in 0..100 {
        sim.tick(clk).unwrap();
    }
    assert_eq!(sim.get(cnt0), 110u32.into());
    assert_eq!(sim.get(cnt3), 110u32.into());

    }

    // Large counter array (similar to bench)
    fn test_counter_n100_wrap(sim) {
        @ignore_on(veryl);
        @setup { let code = r#"
module Top #(param N: u32 = 100) (
clk: input clock,
rst: input reset,
cnt0_out: output logic<8>,
cnt99_out: output logic<8>,
) {
var cnt: logic<8>[N];
assign cnt0_out = cnt[0];
assign cnt99_out = cnt[99];
for i in 0..N: g {
always_ff (clk, rst) {
if_reset { cnt[i] = 0; }
else { cnt[i] += 1; }
}
}
}
"#; }
        @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let cnt0 = sim.signal("cnt0_out");
    let cnt99 = sim.signal("cnt99_out");

    // Reset
    sim.modify(|io| io.set(rst, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(cnt0), 0u8.into());

    // Deassert reset
    sim.modify(|io| io.set(rst, 1u8)).unwrap();

    // 256 ticks: 8-bit counter wraps at 255
    for _ in 0..256 {
        sim.tick(clk).unwrap();
    }
    // 256 ticks = 256 mod 256 = 0
    assert_eq!(sim.get(cnt0), 0u8.into(), "8-bit counter should wrap");
    assert_eq!(sim.get(cnt99), 0u8.into());

    // 1 more tick
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(cnt0), 1u8.into());
    assert_eq!(sim.get(cnt99), 1u8.into());

    }
}
