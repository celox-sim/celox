use celox::Simulator;

// A `for` loop with compile-time-constant bounds inside `always_ff` is
// unrolled by the Veryl analyzer before Celox processes the IR.
// These tests verify that the unrolled shift-register pattern produces
// correct non-blocking FF semantics.

#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

all_backends! {

fn test_for_loop_unroll_shift_register(sim) {
    @setup { let code = r#"
        module Delay #(param DELAY: u32 = 3, param WIDTH: u32 = 8) (
            i_clk: input clock,
            i_rst: input reset,
            i_d:   input  logic<WIDTH>,
            o_d:   output logic<WIDTH>,
        ) {
            var delay: logic<WIDTH> [DELAY];
            assign o_d = delay[DELAY - 1];
            always_ff (i_clk, i_rst) {
                if_reset {
                    delay = '{default: 8'h0};
                } else {
                    delay[0] = i_d;
                    for i in 1..DELAY {
                        delay[i] = delay[i - 1];
                    }
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Delay");
    let clk = sim.event("i_clk");
    let i_rst = sim.signal("i_rst");
    let i_d = sim.signal("i_d");
    let o_d = sim.signal("o_d");

    // Apply reset (AsyncLow default: rst=0 is active)
    sim.modify(|io| io.set(i_rst, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(i_rst, 1u8)).unwrap();

    // Shift 0xAA, 0xBB, 0xCC through the 3-stage delay
    sim.modify(|io| io.set(i_d, 0xAAu8)).unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(i_d, 0xBBu8)).unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(i_d, 0xCCu8)).unwrap();
    sim.tick(clk).unwrap();

    // After 3 cycles the first value propagates to the output
    assert_eq!(sim.get(o_d), 0xAAu8.into());
}

fn test_for_loop_unroll_break_in_always_ff(sim) {
    @setup { let code = r#"
        module Top (
            i_clk: input clock,
            i_rst: input reset,
            o0:    output logic,
            o1:    output logic,
            o2:    output logic,
            o3:    output logic,
        ) {
            always_ff (i_clk, i_rst) {
                if_reset {
                    o0 = 0;
                    o1 = 0;
                    o2 = 0;
                    o3 = 0;
                } else {
                    for i in 0..8 {
                        if i == 3 {
                            break;
                        }
                        if i == 0 { o0 = 1; }
                        if i == 1 { o1 = 1; }
                        if i == 2 { o2 = 1; }
                        if i == 3 { o3 = 1; }
                    }
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("i_clk");
    let i_rst = sim.signal("i_rst");
    let o0 = sim.signal("o0");
    let o1 = sim.signal("o1");
    let o2 = sim.signal("o2");
    let o3 = sim.signal("o3");

    sim.modify(|io| io.set(i_rst, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(i_rst, 1u8)).unwrap();

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o0), 1u8.into());
    assert_eq!(sim.get(o1), 1u8.into());
    assert_eq!(sim.get(o2), 1u8.into());
    assert_eq!(sim.get(o3), 0u8.into());
}

// The std::delay module pattern: `'{0}` reset combined with a `for` loop.
#[ignore = "blocked by upstream Veryl IR: UnsupportedByIr at conv/utils.rs:231"]
fn test_for_loop_unroll_with_brace_zero_reset(sim) {
    @setup { let code = r#"
        module Delay #(param DELAY: u32 = 3, param WIDTH: u32 = 8) (
            i_clk: input clock,
            i_rst: input reset,
            i_d:   input  logic<WIDTH>,
            o_d:   output logic<WIDTH>,
        ) {
            var delay: logic<WIDTH> [DELAY];
            assign o_d = delay[DELAY - 1];
            always_ff (i_clk, i_rst) {
                if_reset {
                    delay = '{0};
                } else {
                    delay[0] = i_d;
                    for i in 1..DELAY {
                        delay[i] = delay[i - 1];
                    }
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Delay");
}

}
