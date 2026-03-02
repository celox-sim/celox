use celox::Simulator;

// Test 1: '{0} on param-sized array should fill all elements with 0
#[test]
fn test_array_literal_single_fills_param_array() {
    let code = r#"
        module Top #(param N: u32 = 3) (
            i_clk: input clock,
            i_rst: input reset,
            o0: output logic<8>,
        ) {
            var arr: logic<8> [N];
            assign o0 = arr[0];
            always_ff (i_clk, i_rst) {
                if_reset {
                    arr = '{0};
                } else {
                    arr[0] = 8'hAB;
                }
            }
        }
    "#;
    Simulator::builder(code, "Top").build().expect("'{0} should compile for param-sized array");
}

// Test 2: for loop inside always_ff should compile and correctly implement a delay
#[test]
fn test_for_loop_in_always_ff() {
    let code = r#"
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
                    for i: u32 in 1..DELAY {
                        delay[i] = delay[i - 1];
                    }
                }
            }
        }
    "#;

    let mut sim = Simulator::builder(code, "Delay")
        .build()
        .expect("for loop in always_ff should compile");
    let clk = sim.event("i_clk");
    let i_rst = sim.signal("i_rst");
    let i_d   = sim.signal("i_d");
    let o_d   = sim.signal("o_d");

    // Apply reset (AsyncLow default: rst=0 is active)
    sim.modify(|io| io.set(i_rst, 0u8)).unwrap();
    sim.tick(clk).unwrap();

    // Deactivate reset
    sim.modify(|io| io.set(i_rst, 1u8)).unwrap();

    // Shift 0xAA, 0xBB, 0xCC through the 3-stage delay
    sim.modify(|io| io.set(i_d, 0xAAu8)).unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(i_d, 0xBBu8)).unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(i_d, 0xCCu8)).unwrap();
    sim.tick(clk).unwrap();

    // After 3 cycles, the first value (0xAA) should appear at output
    assert_eq!(sim.get(o_d), 0xAAu8.into());
}

// Test 3: full delay module with '{0} reset (the exact std::delay pattern)
#[test]
fn test_delay_module_with_brace_zero_reset() {
    let code = r#"
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
                    for i: u32 in 1..DELAY {
                        delay[i] = delay[i - 1];
                    }
                }
            }
        }
    "#;
    Simulator::builder(code, "Delay")
        .build()
        .expect("delay module with '{0} reset and for loop should compile");
}
