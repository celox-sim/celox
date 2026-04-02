use celox::SimulatorBuilder;

#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

all_backends! {

fn test_context_determined_width_subtraction(sim) {
    @setup { let code = r#"
        module Top (
            o1: output logic<1>,
            o2: output logic<1>
        ) {
            always_comb {
                o1 = (2'd0 - 2'd1) == 3'd7;
                o2 = (2'd0 - 2'd1) == 2'd3;
            }
        }
    "#; }
    @build SimulatorBuilder::new(code, "Top");

    let o1 = sim.signal("o1");
    let o2 = sim.signal("o2");

    assert_eq!(
        sim.get(o1),
        1u8.into(),
        "(2'd0 - 2'd1) == 3'd7 should be true"
    );
    assert_eq!(
        sim.get(o2),
        1u8.into(),
        "(2'd0 - 2'd1) == 2'd3 should be true"
    );
}

fn test_unsized_constant_width_subtraction(sim) {
    @setup { let code = r#"
        module Top (
            o: output logic<1>
        ) {
            always_comb {
                o = 2'd0 - 2'd1 == 3;
            }
        }
    "#; }
    @build SimulatorBuilder::new(code, "Top");

    let o = sim.signal("o");

    assert_eq!(
        sim.get(o),
        0u8.into(),
        "2'd0 - 2'd1 == 3 should be false because unsized value is extended to 32 bits"
    );
}

fn test_runtime_variable_width3_subtraction(sim) {
    @setup { let code = r#"
        module Top (
            i: input logic<2>,
            o: output logic<3>,
            c: output logic<1>
        ) {
            always_comb {
                o = i - 2'd1;
                c = (i - 2'd1) == 3'd7;
            }
        }
    "#; }
    @build SimulatorBuilder::new(code, "Top");

    let i = sim.signal("i");
    let o = sim.signal("o");
    let c = sim.signal("c");

    // Case 1: i = 0 -> o = (0 - 1) & 0x7 = 7
    sim.modify(|io| io.set(i, 0u8)).unwrap();
    assert_eq!(
        sim.get(o),
        7u8.into(),
        "0 - 1 with 3-bit output should be 7"
    );
    assert_eq!(sim.get(c), 1u8.into(), "(0 - 1) == 3'd7 should be true");

    // Case 2: i = 1 -> o = (1 - 1) & 0x7 = 0
    sim.modify(|io| io.set(i, 1u8)).unwrap();
    assert_eq!(
        sim.get(o),
        0u8.into(),
        "1 - 1 with 3-bit output should be 0"
    );
    assert_eq!(sim.get(c), 0u8.into(), "(1 - 1) == 3'd7 should be true");
}

fn test_runtime_variable_width2_subtraction(sim) {
    @setup { let code = r#"
        module Top (
            i: input logic<2>,
            o: output logic<2>
        ) {
            always_comb {
                o = i - 2'd1;
            }
        }
    "#; }
    @build SimulatorBuilder::new(code, "Top");

    let i = sim.signal("i");
    let o = sim.signal("o");

    // Case 1: i = 0 -> o = (0 - 1) & 0x7 = 7
    sim.modify(|io| io.set(i, 0u8)).unwrap();
    assert_eq!(
        sim.get(o),
        3u8.into(),
        "0 - 1 with 2-bit output should be 3"
    );

    // Case 2: i = 1 -> o = (1 - 1) & 0x7 = 0
    sim.modify(|io| io.set(i, 1u8)).unwrap();
    assert_eq!(
        sim.get(o),
        0u8.into(),
        "1 - 1 with 2-bit output should be 0"
    );
}

fn test_comparison_different_widths(sim) {
    @setup { let code = r#"
        module Top (
            o: output logic<1>
        ) {
            always_comb {
                o = 2'd3 == 3'd3;
            }
        }
    "#; }
    @build SimulatorBuilder::new(code, "Top");
    let o = sim.signal("o");
    assert_eq!(sim.get(o), 1u8.into(), "2'd3 == 3'd3 should be true");
}

fn test_addition_different_widths(sim) {
    @setup { let code = r#"
        module Top (
            o: output logic<4>
        ) {
            always_comb {
                o = 2'd2 + 3'd5;
            }
        }
    "#; }
    @build SimulatorBuilder::new(code, "Top");
    let o = sim.signal("o");
    assert_eq!(sim.get(o), 7u8.into(), "2'd2 + 3'd5 should be 7");
}

fn test_ff_width_propagation(sim) {
    @ignore_on(veryl);
    @setup { let code = r#"
        module Top (
            clk: input clock,
            rst: input reset,
            i: input logic<2>,
            o: output logic<3>,
        ) {
            always_ff {
                if_reset {
                    o = 3'd0;
                } else {
                    o = i + 2'd2;
                }
            }
        }
    "#; }
    @build SimulatorBuilder::new(code, "Top");
    let i = sim.signal("i");
    let o = sim.signal("o");
    let rst = sim.signal("rst");
    let clk = sim.event("clk");
    sim.modify(|io| io.set(rst, 0u8)).unwrap(); // AsyncLow: active-low reset
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o), 0u8.into(), "Reset should set o to 0");
    sim.modify(|io| io.set(rst, 1u8)).unwrap(); // Deactivate reset
    sim.modify(|io| io.set(i, 2u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o), 4u8.into(), "i=2, o=2+2=4");
}

fn test_zero_extend(sim) {
    @setup { let code = r#"
        module Top (
            o: output logic<4>
        ) {
            always_comb {
                o = 2'd1;
            }
        }
    "#; }
    @build SimulatorBuilder::new(code, "Top");
    let o = sim.signal("o");
    assert_eq!(sim.get(o), 1u8.into(), "2'd1 zero-extended to 4 bits");
}

fn test_nested_width_propagation(sim) {
    @setup { let code = r#"
        module Top (
            o: output logic<5>
        ) {
            always_comb {
                o = (2'd1 + 3'd2) * 2'd2;
            }
        }
    "#; }
    @build SimulatorBuilder::new(code, "Top");
    let o = sim.signal("o");
    assert_eq!(sim.get(o), 6u8.into(), "(1+2)*2 = 6, width propagation");
}

fn test_runtime_shift_width_behavior(sim) {
    @setup { let code = r#"
        module Top (
            i: input  logic<4>,
            s: input  logic<2>,
            o1: output logic<8>,
        ) {
            always_comb {
                // context width assumed to be 8 because o1 is logic<8>.
                o1 = i << s;
            }
        }
    "#; }
    @build SimulatorBuilder::new(code, "Top");

    let i = sim.signal("i");
    let s = sim.signal("s");
    let o1 = sim.signal("o1");

    sim.modify(|io| {
        io.set(i, 12u8);
        io.set(s, 1u8);
    })
    .unwrap();

    assert_eq!(
        sim.get(o1),
        24u8.into(),
        "Upper bit should be preserved because context width is 8"
    );

    sim.modify(|io| {
        io.set(i, 8u8);
        io.set(s, 2u8);
    })
    .unwrap();

    assert_eq!(sim.get(o1), 32u8.into());
}

fn test_runtime_arithmetic_shift_behavior(sim) {
    @setup { let code = r#"
        module Top (
            i_u: input  logic<4>,
            i_s: input  signed logic<4>,
            s:   input  logic<2>,
            o_l: output logic<4>,
            o_a: output logic<4>
        ) {
            always_comb {
                // Logical right shift (zero-filling)
                o_l = i_u >> s;
                // Arithmetic right shift (sign-filling)
                o_a = i_s >>> s;
            }
        }
    "#; }
    @build SimulatorBuilder::new(code, "Top");

    let i_u = sim.signal("i_u");
    let i_s = sim.signal("i_s");
    let s = sim.signal("s");
    let o_l = sim.signal("o_l");
    let o_a = sim.signal("o_a");

    // Input: 4'b1000 (unsigned 8, signed -8), Shift: 2
    sim.modify(|io| {
        io.set(i_u, 8u8);
        io.set(i_s, 8u8); // 8u8 as bit pattern 1000
        io.set(s, 2u8);
    })
    .unwrap();

    // Logical: 4'b1000 >> 2 = 4'b0010 (2)
    assert_eq!(sim.get(o_l), 2u8.into());
    // Arithmetic: 4'sb1000 >>> 2 = 4'sb1110 (14 or -2)
    assert_eq!(sim.get(o_a), 14u8.into());
}

}
