use celox::SimulatorBuilder;

#[test]
#[ignore = "analyzer constant folding returns invalid results"]
fn test_context_determined_width_subtraction() {
    let code = r#"
        module Top (
            o1: output logic<1>,
            o2: output logic<1>
        ) {
            always_comb {
                o1 = (2'd0 - 2'd1) == 3'd7;
                o2 = (2'd0 - 2'd1) == 2'd3;
            }
        }
    "#;
    let result = SimulatorBuilder::new(code, "Top")
        .trace_flattened_comb_blocks()
        .trace_analyzer_ir()
        .build_with_trace();

    let res = result.res;
    let trace = result.trace;

    let mut sim = res.expect("Build should succeed");
    if let Some(air) = trace.analyzer_ir {
        println!("AIR:\n{}", air);
    }
    if let Some((blocks, arena)) = trace.flattened_comb_blocks {
        for path in blocks {
            println!("Target: {}", path.target);
            println!("SLT:\n{}", arena.display(path.expr));
        }
    }

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

#[test]
#[ignore = "analyzer constant folding returns invalid results"]
fn test_unsized_constant_width_subtraction() {
    let code = r#"
        module Top (
            o: output logic<1>
        ) {
            always_comb {
                o = 2'd0 - 2'd1 == 3;
            }
        }
    "#;
    let result = SimulatorBuilder::new(code, "Top")
        .trace_flattened_comb_blocks()
        .build_with_trace();

    let res = result.res;
    let trace = result.trace;

    let mut sim = res.expect("Build should succeed");

    if let Some((blocks, arena)) = trace.flattened_comb_blocks {
        for path in blocks {
            println!("Target: {}", path.target);
            println!("SLT:\n{}", arena.display(path.expr));
        }
    }

    let o = sim.signal("o");

    assert_eq!(
        sim.get(o),
        0u8.into(),
        "2'd0 - 2'd1 == 3 should be false because unsized value is extended to 32 bits"
    );
}
#[test]
fn test_runtime_variable_width3_subtraction() {
    let code = r#"
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
    "#;
    let result = SimulatorBuilder::new(code, "Top")
        .trace_flattened_comb_blocks()
        .trace_analyzer_ir()
        .build_with_trace();

    let res = result.res;
    let trace = result.trace;

    let mut sim = res.expect("Build should succeed");
    if let Some(air) = trace.analyzer_ir {
        println!("AIR:\n{}", air);
    }
    if let Some((blocks, arena)) = trace.flattened_comb_blocks {
        for path in blocks {
            println!("Target: {}", path.target);
            println!("SLT:\n{}", arena.display(path.expr));
        }
    }

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
#[test]
fn test_runtime_variable_width2_subtraction() {
    let code = r#"
        module Top (
            i: input logic<2>,
            o: output logic<2>
        ) {
            always_comb {
                o = i - 2'd1;
            }
        }
    "#;
    let result = SimulatorBuilder::new(code, "Top")
        .trace_flattened_comb_blocks()
        .build_with_trace();

    let res = result.res;
    let trace = result.trace;

    let mut sim = res.expect("Build should succeed");

    if let Some((blocks, arena)) = trace.flattened_comb_blocks {
        for path in blocks {
            println!("Target: {}", path.target);
            println!("SLT:\n{}", arena.display(path.expr));
        }
    }

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

#[test]
fn test_comparison_different_widths() {
    let code = r#"
        module Top (
            o: output logic<1>
        ) {
            always_comb {
                o = 2'd3 == 3'd3;
            }
        }
    "#;
    let sim = SimulatorBuilder::new(code, "Top").build();
    let mut sim = sim.expect("Build should succeed");
    let o = sim.signal("o");
    assert_eq!(sim.get(o), 1u8.into(), "2'd3 == 3'd3 should be true");
}

#[test]
fn test_addition_different_widths() {
    let code = r#"
        module Top (
            o: output logic<4>
        ) {
            always_comb {
                o = 2'd2 + 3'd5;
            }
        }
    "#;
    let sim = SimulatorBuilder::new(code, "Top").build();
    let mut sim = sim.expect("Build should succeed");
    let o = sim.signal("o");
    assert_eq!(sim.get(o), 7u8.into(), "2'd2 + 3'd5 should be 7");
}

#[test]
fn test_ff_width_propagation() {
    let code = r#"
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
    "#;
    let sim = SimulatorBuilder::new(code, "Top").build();
    let mut sim = sim.expect("Build should succeed");
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

#[test]
fn test_zero_extend() {
    let code = r#"
        module Top (
            o: output logic<4>
        ) {
            always_comb {
                o = 2'd1;
            }
        }
    "#;
    let sim = SimulatorBuilder::new(code, "Top").build();
    let mut sim = sim.expect("Build should succeed");
    let o = sim.signal("o");
    assert_eq!(sim.get(o), 1u8.into(), "2'd1 zero-extended to 4 bits");
}

#[test]
fn test_nested_width_propagation() {
    let code = r#"
        module Top (
            o: output logic<5>
        ) {
            always_comb {
                o = (2'd1 + 3'd2) * 2'd2;
            }
        }
    "#;
    let sim = SimulatorBuilder::new(code, "Top").build();
    let mut sim = sim.expect("Build should succeed");
    let o = sim.signal("o");
    assert_eq!(sim.get(o), 6u8.into(), "(1+2)*2 = 6, width propagation");
}
#[test]
fn test_runtime_shift_width_behavior() {
    let code = r#"
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
    "#;
    let result = SimulatorBuilder::new(code, "Top")
        .trace_sim_modules()
        .trace_post_optimized_sir()
        .build_with_trace();
    let res = result.res;
    let trace = result.trace;
    let mut sim = res.expect("Build should succeed");
    println!("{}", trace.format_slt().unwrap());
    println!("{}", trace.format_post_optimized_sir().unwrap());

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

#[test]
fn test_runtime_arithmetic_shift_behavior() {
    let code = r#"
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
    "#;
    let result = SimulatorBuilder::new(code, "Top")
        .trace_sim_modules()
        .trace_post_optimized_sir()
        .build_with_trace();
    let res = result.res;
    let trace = result.trace;
    let mut sim = res.expect("Build should succeed");
    println!("{}", trace.format_slt().unwrap());
    println!("{}", trace.format_post_optimized_sir().unwrap());

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



