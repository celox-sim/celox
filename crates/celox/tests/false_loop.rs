use celox::{Simulator, SimulatorBuilder};

fn setup_and_trace_with_loops(
    code: &str,
    top: &str,
    false_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
    )],
    true_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
        usize,
    )],
) -> celox::CompilationTrace {
    let mut builder = SimulatorBuilder::new(code, top)
        .optimize(true)
        .trace_sim_modules()
        .trace_post_optimized_sir();

    for (from, to) in false_loops {
        builder = builder.false_loop(from.clone(), to.clone());
    }
    for (from, to, limit) in true_loops {
        builder = builder.true_loop(from.clone(), to.clone(), *limit);
    }

    let result = builder.build_with_trace();
    result.trace
}

#[test]
fn test_false_loop_limitations() {
    // This code contains a functional "false loop".
    // v[0] depends on v[1] when sel is 0.
    // v[1] depends on v[0] when sel is 1.
    // Even though they never loop at the same time, static analysis sees a cycle.
    let code = r#"
        module Top (
            sel: input logic,
            i: input logic<2>,
            o: output logic<2>,
        ) {
            var v: logic<2>;
            always_comb {
                if sel {
                    v[1] = v[0];
                    v[0] = i[0];
                } else {
                    v[0] = v[1];
                    v[1] = i[1];
                }
            }
            assign o = v;
        }
    "#;

    // We expect the simulator construction to fail during the scheduling phase.
    // The scheduler currently uses static topological sort based on bit-level dependencies.
    // It cannot "see" through the Mux/If condition to prove the loop is false.
    let result = Simulator::builder(code, "Top").build();

    assert!(
        result.is_err(),
        "Simulator should fail to schedule due to static combinational loop detection"
    );

    // NOTE: In the future, if we want to support this, we would need
    // path-sensitive analysis (e.g., using an SMT solver) or
    // a dynamic scheduling approach. For now, we "give up" and
    // treat this as an illegal combinational loop.
    let code = r#"
        module Top (
            sel: input logic,
            i: input logic<2>,
            o: output logic<2>,
        ) {
            var v: logic<2>;
            always_comb {
                if sel {
                    v[1] = v[0];
                    v[0] = i[0];
                } else {
                    v[0] = v[1];
                    v[1] = i[1];
                }
            }
            assign o = v;
        }
    "#;

    // We expect the simulator construction to fail during the scheduling phase.
    // The scheduler currently uses static topological sort based on bit-level dependencies.
    // It cannot "see" through the Mux/If condition to prove the loop is false.
    // use ignore_loop to ignore the false loop
    // [Internal Behavior]
    // 1. Cycle Detection: The scheduler identifies Strongly Connected Components (SCCs).
    // 2. Authorization: It checks if the cycle is explicitly allowed via `ignore_loop`.
    // 3. Static Unrolling: If authorized, the scheduler double-emits (2-pass) the nodes
    //    within the SCC into the static execution sequence.
    // 4. Convergence: This 2-pass execution ensures that even in the worst-case
    //    topological order, values propagate through the false loop to reach a
    //    fixed point (stable state) within a single simulation step.
    let result = SimulatorBuilder::new(code, "Top")
        .false_loop(
            (vec![], vec!["v".to_owned()]),
            (vec![], vec!["v".to_owned()]),
        )
        .build();

    assert!(result.is_ok());
}

use proptest::prelude::*;

proptest! {
    // Increase cases to cover all input space (1-bit sel + 2-bit i = 8 patterns)
    #![proptest_config(ProptestConfig::with_cases(8))]
    #[test]
    fn test_false_loop_convergence_prop(sel_val in 0u8..2, i_val in 0u8..4) {
        let code = r#"
            module Top (
                sel: input logic,
                i: input logic<2>,
                o: output logic<2>,
            ) {
                var v: logic<2>;
                always_comb {
                    if sel {
                        v[1] = v[0];
                        v[0] = i[0];
                    } else {
                        v[0] = v[1];
                        v[1] = i[1];
                    }
                }
                assign o = v;
            }
        "#;

        // Build simulator with the ignore_loop setting.
        // The scheduler will detect SCC and unroll it to 2-pass execution.
        let result = SimulatorBuilder::new(code, "Top")
            .false_loop(
                (vec![], vec!["v".to_owned()]),
                (vec![], vec!["v".to_owned()]),
            )
            .build();

        let mut sim = result.expect("Simulator build failed");
        let id_sel = sim.signal("sel");
        let id_i = sim.signal("i");
        let id_o = sim.signal("o");

        // Set random inputs
        sim.modify(|io| {
            io.set(id_sel, sel_val);
            io.set(id_i, i_val);
        }).unwrap();

        // Mathematical model of the stable state (Fixed Point)
        // sel == 1: v[0] = i[0], v[1] = v[0] -> both bits are i[0]
        // sel == 0: v[1] = i[1], v[0] = v[1] -> both bits are i[1]
        let expected = if sel_val == 1 {
            if (i_val & 0b01) != 0 { 0b11u8 } else { 0b00u8 }
        } else {
            if (i_val & 0b10) != 0 { 0b11u8 } else { 0b00u8 }
        };

        let actual: u8 = sim.get(id_o).try_into().unwrap();

        assert_eq!(
            actual,
            expected,
            "Failed convergence for sel={}, i={}. Expected {:02b}, got {:02b}",
            sel_val, i_val, expected, actual
        );
    }
}

#[test]
fn test_struct_member_dynamic_access_false_loop() {
    // This test demonstrates a false loop where dynamic accesses to disjoint struct members
    // are treated as overlapping because they share the same bounding box.
    let code = r#"
    module Top (
        i: input logic<2>,
        in_data: input logic<8>,
    ) {
        struct S {
            a: logic<8>,
            b: logic<8>,
        }
        var v: S [4];
        always_comb{
            v[i].a = in_data;
        }
        always_comb{
            v[i].b = v[i].a;
        }
    }
    "#;

    // This is expected to panic with CombinationalLoop because of the false dependency.
    let result = Simulator::builder(code, "Top").build();
    assert!(result.is_err());

    let code = r#"
    module Top (
        i: input logic<2>,
        in_data: input logic<8>,
    ) {
        struct S {
            a: logic<8>,
            b: logic<8>,
        }
        var v: S [4];
        
        always_comb{
            v[i].a = in_data;
            v[i].b = in_data;
        }
    }
    "#;

    let result = Simulator::builder(code, "Top").build();

    assert!(result.is_ok());
}

#[test]
fn test_large_scc_dynamic_loop_convergence() {
    let chain_size = 20;
    let mut assignments = String::new();

    for k in 0..chain_size {
        let prev = if k == 0 { chain_size - 1 } else { k - 1 };
        let next = (k + 1) % chain_size;

        assignments.push_str(&format!(
            "assign v[{}] = (v[{}] & 0) ^ (v[{}] & 0) ^ i;\n",
            k, prev, next
        ));
    }

    let code = format!(
        r#"
    module LargeLoop (
        i: input logic,
        o: output logic<{}>,
    ) {{
        var v: logic<{}>;
        {}
        assign o = v;
    }}
    "#,
        chain_size, chain_size, assignments
    );

    // Initialize simulator (SCC extraction and Strategy B application are performed internally)
    let mut sim = SimulatorBuilder::new(&code, "LargeLoop")
        .false_loop(
            (vec![], vec!["v".to_owned()]),
            (vec![], vec!["v".to_owned()]),
        )
        .build()
        .unwrap();
    let i_port = sim.signal("i");
    let o_port = sim.signal("o");

    // 1. Set input to 1
    // Each bit should converge to v[k] = 0 ^ 0 ^ 1 = 1
    sim.modify(|io| io.set(i_port, 1u8)).unwrap();

    // Expected value: all 20 bits are 1 (0xF_FFFF)
    let expected_all_ones = (1u32 << chain_size) - 1;
    assert_eq!(sim.get(o_port), expected_all_ones.into());

    // 2. Reset input to 0
    // Verify that all bits converge to 0
    sim.modify(|io| io.set(i_port, 0u8)).unwrap();
    assert_eq!(sim.get(o_port), 0u32.into());
}
#[test]
fn test_range_limited_dependency_success() {
    // [Structure]
    // 1. Array v has 16 elements (0..15).
    // 2. idx_ext is 3 bits, so its reach is 2^3 = 8 elements (0..7).
    //
    // [Judgment]
    // Writing to v[8] is outside the source range of v[idx_ext] (0..7).
    // If the scheduler understands this, no Combinational Loop occurs and it succeeds.
    let code = r#"
        module Top (
            idx_ext: input logic<3>, // 0..7
            i:       input logic<32>,
            o:       output logic<32>
        ) {
            var v: logic<32>[16];

            always_comb {
                // Initialize v[0] (source of o)
                v[0] = i;
                // Update v[8]. Since index is 0..7,
                // it's impossible to read v[8] itself.
                v[8] = v[idx_ext]; 
            }
            assign o = v[0];
        }
    "#;
    let result = SimulatorBuilder::new(code, "Top").build();
    assert!(result.is_err());
    let result = SimulatorBuilder::new(code, "Top")
        .false_loop(
            (vec![], vec!["v".to_owned()]),
            (vec![], vec!["v".to_owned()]),
        )
        .build();

    assert!(result.is_ok(),);

    let mut sim = result.unwrap();
    let i = sim.signal("i");
    let o = sim.signal("o");

    sim.modify(|io| io.set(i, 0x12345678u32)).unwrap();
    assert_eq!(sim.get(o), 0x12345678u32.into());
}
#[test]
fn test_cross_bit_dependency_false_loop() {
    let code = r#"
        module Top (
            i: input logic<2>,
            o: output logic<2>,
        ) {
            var a: logic<2>;
            var b: logic<2>;
            always_comb {
                a[0] = i[0];
                b[0] = a[0]; // a[0] -> b[0]
                
                b[1] = i[1];
                a[1] = b[1]; // b[1] -> a[1]
            }
            assign o = {a[1], b[0]};
        }
    "#;
    let result = SimulatorBuilder::new(code, "Top").build();
    // our scheduler correctly detects that there is no loop
    assert!(result.is_ok());
}
#[test]
fn test_read_then_overwrite_convergence() {
    let code = r#"
        module Top (
            i: input logic,
            o: output logic,
        ) {
            var a: logic<2>;
            always_comb {
                a[0] = a[1]; 
                a[1] = i;    
            }
            assign o = a[0];
        }
    "#;

    let mut sim = SimulatorBuilder::new(code, "Top")
        .build()
        .expect("Failed to build simulator with SCC unrolling");

    let i_port = sim.signal("i");
    let o_port = sim.signal("o");

    sim.modify(|io| io.set(i_port, 1u8)).unwrap();

    let result = sim.get(o_port);

    assert_eq!(
        result,
        1u8.into(),
        "The false loop failed to converge to the stable state (Fixed Point)"
    );
}

#[test]
fn test_hierarchical_conditional_false_loop() {
    let code = r#"
        module Pass (i: input logic, o: output logic) {
            assign o = i;
        }
        module Top (sel: input logic, i: input logic, o: output logic) {
            var a: logic;
            var b: logic;
            inst p1: Pass (i: if sel ? i : b, o: a);
            inst p2: Pass (i: if sel ? a : i, o: b);
            assign o = a;
        }
    "#;

    let result = SimulatorBuilder::new(code, "Top")
        .false_loop(
            (vec![], vec!["a".to_owned()]),                     // Source: Top.a
            (vec![("p2".to_owned(), 0)], vec!["i".to_owned()]), // Target: p2.i
        )
        .build();
    assert!(result.is_ok());
}

#[test]
fn test_scc_bit_cross_dependency_sir_snapshot() {
    // v[0] and v[1] depend on each other through different branches.
    // This forms an SCC at the bit-level or variable-level depending on the IR.
    let code = r#"
module Top (
    sel: input logic,
    i: input logic<2>,
    o: output logic<2>,
) {
    var v: logic<2>;
    always_comb {
        if sel {
            v[1] = v[0];
            v[0] = i[0];
        } else {
            v[0] = v[1];
            v[1] = i[1];
        }
    }
    assign o = v;
}
"#;

    let result = SimulatorBuilder::new(code, "Top")
        .trace_sim_modules()
        .trace_post_optimized_sir()
        .false_loop(
            (vec![], vec!["v".to_owned()]),
            (vec![], vec!["v".to_owned()]),
        )
        .build_with_trace();
    let trace = result.trace;
    let sir_output = trace.format_program().unwrap();

    // Snapshot will catch:
    // 1. Redundant emits of the mux/selection logic.
    // 2. Incorrect Store Forwarding where the 'old' value of v[x] is lost.
    // 3. Dependency order violations within the SCC.
    insta::assert_snapshot!("scc_bit_cross_dependency", sir_output);
}

#[test]
fn test_large_scc_dynamic_loop_sir_snapshot() {
    // Construct a large SCC that exceeds the UNROLL_THRESHOLD (128).
    // We create a ring dependency of size 20 (bits).
    // v[0] <- v[19]
    // v[1] <- v[0]
    // ...
    // v[19] <- v[18]
    //
    // SCC Size = 20 nodes.
    // Required Iterations to resolve the ring = 20.
    // Total Estimated Ops = 20 * 20 = 400 > 128.
    // This should trigger the "Strategy B: Dynamic Loop Generation".

    let chain_size = 20;
    let mut assignments = String::new();

    // v[0] depends on v[last] (closing the loop) with some input mix
    assignments.push_str(&format!("assign v[0] = v[{}] ^ i;\n", chain_size - 1));

    // v[k] depends on v[k-1]
    for k in 1..chain_size {
        assignments.push_str(&format!("assign v[{}] = v[{}];\n", k, k - 1));
    }

    let code = format!(
        r#"
module LargeLoop (
    i: input logic,
    o: output logic<{}>,
) {{
    var v: logic<{}>;
    {}
    assign o = v;
}}
"#,
        chain_size, chain_size, assignments
    );

    let trace = setup_and_trace_with_loops(
        &code,
        "LargeLoop",
        &[(
            (vec![], vec!["v".to_owned()]),
            (vec![], vec!["v".to_owned()]),
        )],
        &[],
    );
    let sir_output = trace.format_program().unwrap();

    // The snapshot should contain:
    // 1. "Jump(bX [rY])" - passing the loop counter to the header.
    // 2. "Branch(rA ? bB : bC)" - the loop condition check.
    // 3. The logic body appearing ONLY ONCE (inside the loop body block),
    //    not repeated 20 times.
    insta::assert_snapshot!("large_scc_dynamic_loop", sir_output);
}

#[test]
fn test_large_scc_dynamic_loop_bidirectional_sir_snapshot() {
    let chain_size = 20;
    let mut assignments = String::new();

    for k in 0..chain_size {
        let prev = if k == 0 { chain_size - 1 } else { k - 1 };
        let next = (k + 1) % chain_size;
        assignments.push_str(&format!(
            "assign v[{}] = (v[{}] & 0) ^ (v[{}] & 0) ^ i;\n",
            k, prev, next
        ));
    }

    let code = format!(
        r#"
module LargeLoop (
    i: input logic,
    o: output logic<{}>,
) {{
    var v: logic<{}>;
    {}
    assign o = v;
}}
"#,
        chain_size, chain_size, assignments
    );
    let trace = setup_and_trace_with_loops(
        &code,
        "LargeLoop",
        &[(
            (vec![], vec!["v".to_owned()]),
            (vec![], vec!["v".to_owned()]),
        )],
        &[],
    );
    let sir_output = trace.format_program().unwrap();

    insta::assert_snapshot!("large_scc_dynamic_loop_bidirectional", sir_output);
}
