use celox::{
    LoweringPhase, ParserError, RuntimeErrorCode, SchedulerError, Simulator, SimulatorErrorKind,
};

/// Helper: assert the error is either Analyzer or a specific SIRParser variant.
/// The updated Veryl analyzer may catch issues before the SIR scheduler does.
fn assert_analyzer_or_sir(
    result: Result<Simulator, celox::SimulatorError>,
    sir_check: impl FnOnce(&celox::SimulatorError),
) {
    let err = result.expect_err("Expected an error");
    match err.kind() {
        SimulatorErrorKind::Analyzer(_) => {} // OK: analyzer caught it first
        SimulatorErrorKind::SIRParser(_) => sir_check(&err),
        other => panic!("Expected Analyzer or SIRParser error, got: {other:?}"),
    }
}

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
        match e.kind() {
            SimulatorErrorKind::SIRParser(ParserError::Scheduler(
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
    match result.as_ref().map_err(|e| e.kind()) {
        Err(SimulatorErrorKind::SIRParser(ParserError::Scheduler(
            SchedulerError::CombinationalLoop { blocks },
        ))) => {
            assert_eq!(blocks.len(), 2);
        }
        Err(k) => panic!("Expected CombinationalLoop error, but got: {:?}", k),
        Ok(_) => panic!("Should have failed with CombinationalLoop"),
    }
}

#[test]
fn test_combinational_loop_in_single_block() {
    // The Veryl analyzer now catches this as UnassignVariable (y is read before
    // being written in the same always_comb block), so we expect an Analyzer error.
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
    assert_analyzer_or_sir(result, |e| {
        let msg = format!("{e:?}");
        assert!(
            msg.contains("CombinationalLoop") || msg.contains("unassign"),
            "Expected loop or unassign error, got: {e:?}"
        );
    });
}

#[test]
fn test_dynamic_index_bit_disparity_bullying() {
    let code = r#"
        module Top (
            j: input logic
        ) {
            var x: logic[2,4];
            always_comb{ x[j][0] = x[j][1]; }
            always_comb{ x[j][1] = x[j][0]; }
        }
    "#;

    let result = Simulator::builder(code, "Top").build();
    assert_analyzer_or_sir(result, |e| {
        let msg = format!("{e:?}");
        assert!(
            msg.contains("CombinationalLoop") || msg.contains("MultipleDriver"),
            "Expected loop or multiple-driver error, got: {e:?}"
        );
    });
}

#[test]
fn test_dynamic_access_with_static_precedence_is_ok() {
    let code = r#"
        module Top (j: input logic, a: input logic) {
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
    // The Veryl analyzer catches InvalidOperand (can't apply + to an array).
    // Use logic<8>[2] so x[j] is a scalar and + is valid.
    let code = r#"
        module Top (j: input logic) {
            var x: logic<8> [2];
            always_comb {
                x[j] = x[j] + 1;
            }
        }
    "#;

    let result = Simulator::builder(code, "Top").build();
    assert_analyzer_or_sir(result, |e| {
        let msg = format!("{e:?}");
        assert!(
            msg.contains("CombinationalLoop") || msg.contains("MultipleDriver"),
            "Expected loop or multiple-driver error, got: {e:?}"
        );
    });
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
    assert_analyzer_or_sir(result, |e| {
        let msg = format!("{e:?}");
        assert!(
            msg.contains("CombinationalLoop") || msg.contains("unassign"),
            "Expected loop or unassign error, got: {e:?}"
        );
    });
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
fn test_runtime_for_loop_state_does_not_trigger_scheduler_loop() {
    let code = r#"
        module Top (
            count: input logic<8>,
            o: output logic<8>
        ) {
            var acc: logic<8>;
            always_comb {
                acc = 0;
                for i: u32 in 0..count {
                    acc += i as 8;
                }
            }
            always_comb {
                o = acc + 1;
            }
        }
    "#;

    let result = Simulator::builder(code, "Top").build();
    assert!(
        result.is_ok(),
        "runtime ForFold self-state must not be treated as a scheduler loop: {:?}",
        result.err()
    );
}

#[test]
fn test_runtime_for_loop_external_feedback_is_still_scheduler_loop() {
    let code = r#"
        module Top (
            count: input logic<8>,
            o: output logic<8>
        ) {
            var acc: logic<8>;
            var y: logic<8>;
            always_comb {
                acc = 0;
                for i: u32 in 0..count {
                    acc += y;
                }
            }
            always_comb {
                y = acc;
                o = y;
            }
        }
    "#;

    let result = Simulator::builder(code, "Top").build();
    match result.as_ref().map_err(|e| e.kind()) {
        Err(SimulatorErrorKind::SIRParser(ParserError::Scheduler(
            SchedulerError::CombinationalLoop { blocks },
        ))) => {
            assert_eq!(blocks.len(), 2);
        }
        Err(k) => panic!("Expected CombinationalLoop error, but got: {:?}", k),
        Ok(_) => panic!("Should have failed with CombinationalLoop"),
    }
}

#[test]
fn test_always_ff_runtime_for_non_progress_is_rejected() {
    let code = r#"
        module Top (
            clk: input clock,
            count: input logic<8>,
            q: output logic<8>
        ) {
            always_ff (clk) {
                q = 0;
                for i: u32 in 1..count step *= 1 {
                    q = i as 8;
                }
            }
        }
    "#;

    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let count = sim.signal("count");
    sim.modify(|io| io.set(count, 4u8)).unwrap();
    assert_eq!(
        sim.tick(clk).unwrap_err(),
        RuntimeErrorCode::DetectedTrueLoop
    );
}

#[test]
fn test_always_ff_runtime_for_zero_start_mul_non_progress_is_rejected() {
    let code = r#"
        module Top (
            clk: input clock,
            count: input logic<8>,
            q: output logic<8>
        ) {
            always_ff (clk) {
                q = 0;
                for i: u32 in 0..count step *= 2 {
                    q = i as 8;
                }
            }
        }
    "#;

    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let count = sim.signal("count");
    sim.modify(|io| io.set(count, 4u8)).unwrap();
    assert_eq!(
        sim.tick(clk).unwrap_err(),
        RuntimeErrorCode::DetectedTrueLoop
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
    assert_analyzer_or_sir(result, |e| match e.kind() {
        SimulatorErrorKind::SIRParser(ParserError::Scheduler(SchedulerError::MultipleDriver {
            ..
        })) => {}
        _ => panic!("Expected MultipleDriver error, got: {e:?}"),
    });
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

    match result.as_ref().map_err(|e| e.kind()) {
        Err(SimulatorErrorKind::SIRParser(ParserError::Scheduler(
            SchedulerError::CombinationalLoop { .. },
        ))) => {}
        Err(k) => panic!("expected CombinationalLoop error, got {k:?}"),
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
    // The updated Veryl analyzer may reject interface-only designs as an error
    // or emit a warning. Either outcome is acceptable.
    match &result {
        Err(_) => {} // Analyzer or SIR error — OK
        Ok(sim) => {
            // Compilation succeeded; the analyzer flagged it as a warning
            assert!(
                !sim.warnings().is_empty(),
                "Expected at least a warning for interface-only design"
            );
        }
    }
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

    match result.as_ref().map_err(|e| e.kind()) {
        Err(SimulatorErrorKind::SIRParser(ParserError::Unsupported {
            phase: LoweringPhase::SimulatorParser,
            feature,
            ..
        })) => {
            assert_eq!(*feature, "systemverilog module instantiation")
        }
        Err(k) => panic!("expected Unsupported(SimulatorParser) for $sv module, got {k:?}"),
        Ok(_) => panic!("expected Unsupported(SimulatorParser) for $sv module, got Ok"),
    }
}

#[test]
fn test_top_not_found_returns_error() {
    let code = r#"
        module Foo (a: input logic, b: output logic) {
            assign b = a;
        }
    "#;

    let result = Simulator::builder(code, "NonExistentTop").build();

    match result.as_ref().map_err(|e| e.kind()) {
        Err(SimulatorErrorKind::SIRParser(ParserError::TopNotFound { name })) => {
            assert_eq!(name, "NonExistentTop");
        }
        Err(k) => panic!("expected TopNotFound, got {k:?}"),
        Ok(_) => panic!("expected TopNotFound, got Ok"),
    }
}

#[test]
fn test_generic_top_returns_error() {
    let code = r#"
        module GenericPass::<T: type> (
            a: input  T,
            b: output T,
        ) {
            assign b = a;
        }
    "#;

    let result = Simulator::builder(code, "GenericPass").build();

    match result.as_ref().map_err(|e| e.kind()) {
        Err(SimulatorErrorKind::SIRParser(ParserError::GenericTop { name })) => {
            assert_eq!(name, "GenericPass");
        }
        Err(k) => panic!("expected GenericTop, got {k:?}"),
        Ok(_) => panic!("expected GenericTop, got Ok"),
    }
}
