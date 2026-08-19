use celox::Simulator;

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    // Simple counter: increment on each tick, reset to 0
    fn test_counter_n4_basic(sim) {
        @ignore_on(veryl, sv);
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
        @ignore_on(veryl, sv);
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

    fn test_phase_state_ssa_preserves_eval_before_apply_and_four_state(sim) {
        @ignore_on(veryl, sv);
        @setup { let code = r#"
module Top (
    clk: input clock,
    rst: input reset,
    data: input logic<8>,
    next_value: output logic<8>,
    captured: output logic<8>,
    previous: output logic<8>,
) {
    assign next_value = data ^ 8'ha5;

    always_ff (clk, rst) {
        if_reset {
            captured = 0;
        } else {
            captured = next_value;
        }
    }

    always_ff (clk, rst) {
        if_reset {
            previous = 0;
        } else {
            previous = captured;
        }
    }
}
"#; }
        @build Simulator::builder(code, "Top").four_state(true);

        let clk = sim.event("clk");
        let rst = sim.signal("rst");
        let data = sim.signal("data");
        let captured = sim.signal("captured");
        let previous = sim.signal("previous");

        sim.modify(|io| io.set(rst, 0u8)).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(
            sim.get_four_state(captured),
            (num_bigint::BigUint::from(0u8), num_bigint::BigUint::from(0u8))
        );
        assert_eq!(
            sim.get_four_state(previous),
            (num_bigint::BigUint::from(0u8), num_bigint::BigUint::from(0u8))
        );

        let first_value = num_bigint::BigUint::from(0x3cu8);
        let first_mask = num_bigint::BigUint::from(0x10u8);
        sim.modify(|io| {
            io.set(rst, 1u8);
            io.set_four_state(data, first_value.clone(), first_mask.clone());
        })
        .unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(
            sim.get_four_state(captured),
            (
                &first_value ^ num_bigint::BigUint::from(0xa5u8),
                first_mask.clone(),
            )
        );
        assert_eq!(
            sim.get_four_state(previous),
            (num_bigint::BigUint::from(0u8), num_bigint::BigUint::from(0u8))
        );

        let second_value = num_bigint::BigUint::from(0x52u8);
        let second_mask = num_bigint::BigUint::from(0x03u8);
        sim.modify(|io| {
            io.set_four_state(data, second_value.clone(), second_mask.clone());
        })
        .unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(
            sim.get_four_state(captured),
            (
                &second_value ^ num_bigint::BigUint::from(0xa5u8),
                second_mask,
            )
        );
        assert_eq!(
            sim.get_four_state(previous),
            (
                &first_value ^ num_bigint::BigUint::from(0xa5u8),
                first_mask,
            )
        );
    }
}
