use celox::{ParserError, SchedulerError, Simulator, SimulatorError};

#[test]
fn test_scheduler_loop_detection() {
    let code = r#"
        module Top (a: input logic, o: output logic) {
            var x: logic;
            var y: logic;
            var z: logic;
            assign x = y;
            assign y = z;
            assign z = x;
            assign o = x;
        }
    "#;

    let result = Simulator::builder(code, "Top").build();
    assert!(
        result.is_err(),
        "Combinational loop should be detected as an Err"
    );

    if let Err(e) = result {
        match e {
            SimulatorError::SIRParser(ParserError::Scheduler(
                SchedulerError::CombinationalLoop { blocks },
            )) => {
                assert_eq!(blocks.len(), 3, "Loop should involve exactly 3 blocks");
            }
            _ => panic!("Expected CombinationalLoop error, but got: {:?}", e),
        }
    }
}

#[test]
fn test_combinational_loop() {
    let code = r#"
        module Top () {
            var y: logic;
            var x: logic;
            always_comb {
                x = y;
            }
            always_comb {
                y = x;
            }
        }
    "#;

    let result = Simulator::builder(code, "Top").build();
    match result {
        Err(SimulatorError::SIRParser(ParserError::Scheduler(
            SchedulerError::CombinationalLoop { blocks },
        ))) => {
            assert_eq!(blocks.len(), 2);
        }
        Err(e) => panic!("Expected CombinationalLoop error, but got: {:?}", e),
        Ok(_) => panic!("Should have failed with CombinationalLoop"),
    }
}

#[test]
fn test_combinational_loop_in_single_block() {
    let code = r#"
        module Top () {
            var y: logic;
            var x: logic;
            always_comb {
                x = y;
                y = x;
            }
        }
    "#;

    let result = Simulator::builder(code, "Top").build();
    match result {
        Err(SimulatorError::SIRParser(ParserError::Scheduler(
            SchedulerError::CombinationalLoop { blocks },
        ))) => {
            assert_eq!(blocks.len(), 1);
        }
        Err(e) => panic!("Expected CombinationalLoop, but got: {:?}", e),
        Ok(_) => {
            panic!("Should have failed because 'y' is read before being written in the same block")
        }
    }
}

#[test]
fn test_dynamic_index_bit_disparity_bullying() {
    let code = r#"
        module Top (
            i: input logic,
            j: input logic
        ) {
            var y: logic;
            var x: logic[2,4];
            always_comb{ x[j][0] = x[j][1]; }
            always_comb{ x[j][1] = x[j][0]; }
        }
    "#;

    let result = Simulator::builder(code, "Top").build();
    match result {
        Err(SimulatorError::SIRParser(ParserError::Scheduler(
            SchedulerError::MultipleDriver { blocks: _ },
        ))) => {}
        Err(e) => panic!("Expected MultipleDriver error, but got: {:?}", e),
        Ok(_) => panic!("Should have failed"),
    }
}

#[test]
fn test_dynamic_access_with_static_precedence_is_ok() {
    let code = r#"
        module Top (j: input logic) {
            var a: logic;
            var x: logic[2,4];
            always_comb {
                x[0][0] = a;
                x[j][1] = x[0][0];
            }
        }
    "#;

    let result = Simulator::builder(code, "Top").build();
    assert!(
        result.is_ok(),
        "Should be OK because x[0][0] is defined before being read. but {:?}",
        result.err()
    );
}

#[test]
fn test_dynamic_access_self_loop_is_err() {
    let code = r#"
        module Top (j: input logic) {
            var x: logic[2,4];
            always_comb {
                x[j] = x[j] + 1;
            }
        }
    "#;

    let result = Simulator::builder(code, "Top").build();
    match result {
        Err(SimulatorError::SIRParser(ParserError::Scheduler(
            SchedulerError::CombinationalLoop { blocks },
        ))) => {
            assert!(blocks.len() >= 1, "Should detect a loop: {:?}", blocks);
        }
        Err(e) => panic!("Expected CombinationalLoop, but got: {:?}", e),
        Ok(_) => panic!("Should have failed: x[j] depends on its previous value (latch/loop)"),
    }
}

#[test]
fn test_if_without_else_latch_loop() {
    let code = r#"
        module Top (sel: input logic, a: input logic) {
            var x: logic;
            var y: logic;
            always_comb {
                if sel {
                    x = a;
                }
                y = x;
            }
        }
    "#;

    let result = Simulator::builder(code, "Top").build();
    assert!(
        result.is_err(),
        "Should fail: x is not guaranteed to be defined before being read (latch/loop)"
    );
}

#[test]
fn test_default_assignment_with_if_is_ok() {
    let code = r#"
        module Top (sel: input logic, a: input logic<8>) {
            var x: logic<8>;
            always_comb {
                x = 8'h00;
                if sel {
                    x = a;
                }
            }
        }
    "#;
    let result = Simulator::builder(code, "Top").build();
    assert!(result.is_ok());
}

#[test]
fn test_dynamic_write_then_read_is_ok() {
    let code = r#"
        module Top (i: input logic<2>, j: input logic<2>, a: input logic<8>) {
            var x: logic<8> [4];
            var y: logic<8>;
            always_comb {
                x[i] = a;
                y = x[j];
            }
        }
    "#;
    let result = Simulator::builder(code, "Top").build();
    assert!(
        result.is_ok(),
        "Dynamic sequence allowed due to analysis complexity"
    );
}

#[test]
fn test_multiple_driver_error() {
    let code = r#"
        module Top (a: input logic, b: input logic, o: output logic) {
            assign o = a;
            assign o = b;
        }
    "#;
    let result = Simulator::builder(code, "Top").build();
    match result {
        Err(SimulatorError::SIRParser(ParserError::Scheduler(
            SchedulerError::MultipleDriver { .. },
        ))) => {}
        _ => panic!("Should have failed with MultipleDriver error"),
    }
}

#[test]
fn test_bit_level_false_loop_is_ok() {
    let code = r#"
        module Top (a: input logic, o: output logic) {
            var x: logic<2>;
            assign x[0] = x[1];
            assign x[1] = a;
            assign o = x[0];
        }
    "#;
    let result = Simulator::builder(code, "Top").build();
    assert!(
        result.is_ok(),
        "Variable-level analysis would fail, but bit-level should pass"
    );
}

#[test]
fn pass_comb_block_scheduling_combinational_loop() {
    let code = r#"
        module Top (a: input logic, o: output logic) {
            var x: logic<2>;
            var y: logic;
            always_comb {
                x[0] = a;
                o    = x[1];
            }
            assign y    = x[0];
            assign x[1] = y;
        }
    "#;
    let result = Simulator::builder(code, "Top").build();
    assert!(
        result.is_ok(),
        "Block-level analysis would fail, but bit-level should pass"
    );
}

#[test]
fn detect_hierarchical_true_concat_feedback_loop() {
    let code = r#"
        module Child (
            a: input logic<3>,
            lo: output logic,
        ) {
            assign lo = a[2];
        }

        module Top (
            out: output logic,
        ) {
            var v: logic<3>;
            var lo: logic;

            inst c: Child (
                a: v,
                lo: lo,
            );

            // True loop at bit level: lo -> v[2] -> lo
            assign v = {lo, 1'b0, 1'b1};
            assign out = lo;
        }
    "#;

    let result = Simulator::builder(code, "Top").build();
    assert!(
        result.is_err(),
        "Expected combinational loop to be rejected across hierarchy"
    );

    match result {
        Err(SimulatorError::SIRParser(ParserError::Scheduler(
            SchedulerError::CombinationalLoop { .. },
        ))) => {}
        Err(e) => panic!("expected CombinationalLoop error, got {e:?}"),
        Ok(_) => panic!("expected CombinationalLoop error, got Ok"),
    }
}

#[test]
fn test_hierarchical_read_slice_feedback_should_not_form_loop() {
    let code = r#"
        module Child (
            a: input logic<2>,
            lo: output logic,
        ) {
            assign lo = a[1];
        }

        module Top (
            inp: input logic,
            out: output logic,
        ) {
            var v: logic<2>;
            var lo: logic;

            inst c: Child (
                a: v,
                lo: lo,
            );

            // Explicitly acyclic at bit level:
            // v[1] = inp, lo = c(v)[=v[1]], v[0] = lo
            assign v[1] = inp;
            assign v[0] = lo;
            assign out = v[0];
        }
    "#;

    let result = Simulator::builder(code, "Top").build();
    assert!(
        result.is_ok(),
        "Bit-level dependency is acyclic, but got error: {:?}",
        result.err()
    );

    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let inp = sim.signal("inp");
    let out = sim.signal("out");

    sim.modify(|io| io.set(inp, 0u8)).unwrap();
    assert_eq!(sim.get(out), 0u8.into());

    sim.modify(|io| io.set(inp, 1u8)).unwrap();
    assert_eq!(sim.get(out), 1u8.into());

    sim.modify(|io| io.set(inp, 0u8)).unwrap();
    assert_eq!(sim.get(out), 0u8.into());
}

#[test]
fn test_interface_design_is_currently_accepted() {
    let code = r#"
        interface BusIf {
            var data: logic<8>;
            modport mp {
                data: inout,
            }
        }

        module Top () {
            inst bus: BusIf;
        }
    "#;

    let result = Simulator::builder(code, "Top").build();
    assert!(
        result.is_ok(),
        "interface-only design is accepted by analyzer/simulator pipeline"
    );
}

#[test]
fn test_sv_module_instance_returns_unsupported_parser_error() {
    let code = r#"
        module Top (
            i_clk  : input  logic,
            i_rst_n: input  logic,
            i_d    : input  logic,
            o_d    : output logic,
        ) {
            inst u0: $sv::delay (
                i_clk,
                i_rst_n,
                i_d,
                o_d,
            );
        }
    "#;

    let result = Simulator::builder(code, "Top").build();

    match result {
        Err(SimulatorError::SIRParser(ParserError::UnsupportedSimulatorParser {
            feature, ..
        })) => {
            assert_eq!(feature, "systemverilog module instantiation")
        }
        Err(e) => panic!("expected UnsupportedSimulatorParser for $sv module, got {e:?}"),
        Ok(_) => panic!("expected UnsupportedSimulatorParser for $sv module, got Ok"),
    }
}
