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
        @ignore_on(veryl, sv);
        @case "system_function::test_comb_function_body_onehot_system_function";
    }

    fn test_direct_comb_bits_system_function(sim) {
        @ignore_on(sv);
        @case "system_function::test_direct_comb_bits_system_function";
    }

    fn test_direct_comb_size_system_function(sim) {
        @ignore_on(veryl, sv);
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
        @ignore_on(veryl, sv);
        @case "system_function::test_comb_function_body_bits_size_system_functions";
    }

    fn test_direct_comb_signed_system_function_sign_extends_to_context(sim) {
        @ignore_on(sv);
        @case "system_function::test_direct_comb_signed_system_function_sign_extends_to_context";
    }

    fn test_direct_comb_unsigned_system_function_zero_extends_to_context(sim) {
        @ignore_on(sv);
        @case "system_function::test_direct_comb_unsigned_system_function_zero_extends_to_context";
    }

    fn test_direct_comb_signed_unsigned_system_functions_affect_comparison(sim) {
        @ignore_on(veryl, sv);
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
        @ignore_on(veryl, sv);
        @case "system_function::test_direct_ff_bits_array_system_function";
    }

    fn test_direct_ff_size_system_function(sim) {
        @ignore_on(veryl, sv);
        @case "system_function::test_direct_ff_size_system_function";
    }

    fn test_direct_ff_size_type_system_function(sim) {
        @ignore_on(sv);
        @case "system_function::test_direct_ff_size_type_system_function";
    }

    #[ignore = "$size on packed multidimensional types is folded to total width by Veryl analyzer before Celox FF lowering"]
    fn test_direct_ff_size_packed_multidimensional_system_function(sim) {
        @case "system_function::test_direct_ff_size_packed_multidimensional_system_function";
    }

    #[ignore = "$size on packed multidimensional type arguments is folded to total width by Veryl analyzer before Celox FF lowering"]
    fn test_direct_ff_size_packed_multidimensional_type_system_function(sim) {
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
        @ignore_on(sv);
        @case "system_function::test_direct_ff_signed_system_function";
    }

    fn test_direct_ff_signed_system_function_sign_extends_to_context(sim) {
        @ignore_on(sv);
        @case "system_function::test_direct_ff_signed_system_function_sign_extends_to_context";
    }

    fn test_direct_ff_unsigned_system_function(sim) {
        @ignore_on(sv);
        @case "system_function::test_direct_ff_unsigned_system_function";
    }

    fn test_direct_ff_unsigned_system_function_zero_extends_to_context(sim) {
        @ignore_on(sv);
        @case "system_function::test_direct_ff_unsigned_system_function_zero_extends_to_context";
    }

    #[ignore = "direct $onehot in always_ff is folded to 1'h0 by Veryl analyzer before Celox FF lowering"]
    fn test_direct_ff_onehot_system_function(sim) {
        @case "system_function::test_direct_ff_onehot_system_function";
    }

    fn test_ff_function_body_onehot_system_function(sim) {
        @ignore_on(veryl, sv);
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
fn test_unsupported_ff_statement_system_functions_are_reported() {
    let cases = [
        (
            "readmemh",
            r#"
module Top (clk: input clock) {
    var mem: logic<8>[4];
    always_ff (clk) {
        $readmemh("mem.hex", mem);
    }
}
"#,
        ),
        (
            "finish",
            r#"
module Top (clk: input clock) {
    always_ff (clk) {
        $finish();
    }
}
"#,
        ),
    ];

    for (name, code) in cases {
        let err = Simulator::builder(code, "Top")
            .build()
            .expect_err("statement system function should be unsupported in FF lowering");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("system function call"),
            "expected system function unsupported error for {name}, got: {err:?}"
        );
    }
}
