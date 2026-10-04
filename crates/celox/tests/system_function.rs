use celox::Simulator;

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {
    #[ignore = "direct $onehot in always_comb currently evaluates incorrectly before Celox system-function lowering"]
    fn test_direct_comb_onehot_system_function(sim) {
        @case "system_function::test_direct_comb_onehot_system_function";
    }

    fn test_comb_function_body_onehot_system_function(sim) {
        @ignore_on(veryl);
        @case "system_function::test_comb_function_body_onehot_system_function";
    }

    fn test_direct_comb_bits_system_function(sim) {
        @ignore_on(sv);
        @case "system_function::test_direct_comb_bits_system_function";
    }

    fn test_direct_comb_size_system_function(sim) {
        @ignore_on(sv);
        @case "system_function::test_direct_comb_size_system_function";
    }

    #[ignore = "direct $clog2 in always_comb is folded from X payload by Veryl analyzer before Celox comb lowering"]
    fn test_direct_comb_clog2_system_function(sim) {
        @case "system_function::test_direct_comb_clog2_system_function";
    }

    fn test_comb_function_body_clog2_system_function(sim) {
        @ignore_on(veryl, sv);
        @case "system_function::test_comb_function_body_clog2_system_function";
    }

    fn test_comb_function_body_bits_size_system_functions(sim) {
        @ignore_on(sv);
        @case "system_function::test_comb_function_body_bits_size_system_functions";
    }

    fn test_direct_comb_signed_system_function_sign_extends_to_context(sim) {
        @case "system_function::test_direct_comb_signed_system_function_sign_extends_to_context";
    }

    fn test_direct_comb_unsigned_system_function_zero_extends_to_context(sim) {
        @case "system_function::test_direct_comb_unsigned_system_function_zero_extends_to_context";
    }

    fn test_direct_comb_signed_unsigned_system_functions_affect_comparison(sim) {
        @case "system_function::test_direct_comb_signed_unsigned_system_functions_affect_comparison";
    }

    fn test_comb_function_body_signed_unsigned_system_functions(sim) {
        @ignore_on(sv);
        @case "system_function::test_comb_function_body_signed_unsigned_system_functions";
    }

    fn test_direct_comb_bits_type_system_function(sim) {
        @ignore_on(sv);
        @case "system_function::test_direct_comb_bits_type_system_function";
    }

    fn test_direct_ff_bits_system_function(sim) {
        @ignore_on(sv);
        @case "system_function::test_direct_ff_bits_system_function";
    }

    fn test_direct_ff_bits_type_system_function(sim) {
        @ignore_on(sv);
        @case "system_function::test_direct_ff_bits_type_system_function";
    }

    fn test_direct_ff_bits_array_system_function(sim) {
        @ignore_on(sv);
        @case "system_function::test_direct_ff_bits_array_system_function";
    }

    fn test_direct_ff_size_system_function(sim) {
        @ignore_on(sv);
        @case "system_function::test_direct_ff_size_system_function";
    }

    fn test_direct_ff_size_type_system_function(sim) {
        @ignore_on(sv);
        @case "system_function::test_direct_ff_size_type_system_function";
    }

    fn test_direct_ff_size_packed_multidimensional_system_function(sim) {
        @ignore_on(sv);
        @case "system_function::test_direct_ff_size_packed_multidimensional_system_function";
    }

    fn test_direct_ff_size_packed_multidimensional_type_system_function(sim) {
        @ignore_on(sv);
        @case "system_function::test_direct_ff_size_packed_multidimensional_type_system_function";
    }

    #[ignore = "direct $clog2 in always_ff is folded from X payload by Veryl analyzer before Celox FF lowering"]
    fn test_direct_ff_clog2_system_function(sim) {
        @case "system_function::test_direct_ff_clog2_system_function";
    }

    fn test_ff_function_body_clog2_system_function(sim) {
        @ignore_on(veryl, sv);
        @case "system_function::test_ff_function_body_clog2_system_function";
    }

    fn test_direct_ff_signed_system_function(sim) {
        @case "system_function::test_direct_ff_signed_system_function";
    }

    fn test_direct_ff_signed_system_function_sign_extends_to_context(sim) {
        @case "system_function::test_direct_ff_signed_system_function_sign_extends_to_context";
    }

    fn test_direct_ff_unsigned_system_function(sim) {
        @case "system_function::test_direct_ff_unsigned_system_function";
    }

    fn test_direct_ff_unsigned_system_function_zero_extends_to_context(sim) {
        @case "system_function::test_direct_ff_unsigned_system_function_zero_extends_to_context";
    }

    #[ignore = "direct $onehot in always_ff is folded to 1'h0 by Veryl analyzer before Celox FF lowering"]
    fn test_direct_ff_onehot_system_function(sim) {
        @case "system_function::test_direct_ff_onehot_system_function";
    }

    fn test_ff_function_body_onehot_system_function(sim) {
        @ignore_on(veryl);
        @case "system_function::test_ff_function_body_onehot_system_function";
    }
}

#[test]
fn test_ff_statement_runtime_event_system_functions_are_supported() {
    let code = r#"
module Top (clk: input clock, d: input logic) {
    always_ff (clk) {
        $display("display d=%0d", d);
        $write("write d=%0d", d);
        $assert(d, "assert d=%0d", d);
    }
}
"#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let d = sim.signal("d");

    sim.modify(|io| io.set(d, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "display d=1".to_string(),
            },
            celox::RuntimeEvent::Write {
                message: "write d=1".to_string(),
            },
        ]
    );
}

#[test]
fn test_ff_statement_system_tasks_are_lowered() {
    // `$readmemh` reads its file at compile time, so a missing file is an
    // input error rather than an unsupported construct.
    let readmemh = r#"
module Top (clk: input clock) {
    var mem: logic<8>[4];
    always_ff (clk) {
        $readmemh("celox-missing-memory-file.hex", mem);
    }
}
"#;
    let err = Simulator::builder(readmemh, "Top")
        .build()
        .expect_err("a missing memory file must be reported");
    assert!(
        matches!(
            err.kind(),
            celox::SimulatorErrorKind::SIRParser(celox::ParserError::MemoryFile { .. })
        ),
        "expected a memory file error, got: {err:?}"
    );

    let finish = r#"
module Top (clk: input clock) {
    always_ff (clk) {
        $finish();
    }
}
"#;
    let mut sim = Simulator::builder(finish, "Top").build().unwrap();
    let clk = sim.event("clk");
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Finish]
    );
}
