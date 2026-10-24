use super::*;

sv_backends! {
    fn real_parameter_overrides_preserve_fractional_values(sim) {
        @case "floating_point::real_parameter_overrides_preserve_fractional_values";
    }
    fn real_parameter_overrides_keep_declared_conversion_contexts(sim) {
        @case "floating_point::real_parameter_overrides_keep_declared_conversion_contexts";
    }
    fn real_parameter_overrides_from_math_and_generate_scopes(sim) {
        @case "floating_point::real_parameter_overrides_from_math_and_generate_scopes";
    }

    fn real_math_runtime_functions(sim) { @case "real_math::runtime_functions"; }
    fn real_math_constant_functions(sim) { @case "real_math::constant_functions"; }
    fn real_arithmetic_and_rounding(sim) { @case "floating_point::real_arithmetic_and_rounding"; }
    fn real_bits_and_comparisons(sim) { @case "floating_point::real_bits_and_comparisons"; }
    fn shortreal_storage_precision(sim) { @case "floating_point::shortreal_storage_precision"; }
    fn sequential_real(sim) { @case "floating_point::sequential_real"; }
    fn real_truth_and_unknown_integer_conversion(sim) { @case "floating_point::real_truth_and_unknown_integer_conversion"; }
    fn real_parameter_and_default_storage(sim) { @case "floating_point::real_parameter_and_default_storage"; }
    fn real_narrow_integer_conversion(sim) { @case "floating_point::real_narrow_integer_conversion"; }
    fn real_ports_and_subroutines(sim) { @case "floating_point::real_ports_and_subroutines"; }
    fn real_conversion_functions(sim) { @case "floating_point::real_conversion_functions"; }
    fn real_zero_divisor(sim) { @case "floating_point::real_zero_divisor"; }
    fn real_display_formats(sim) { @case "floating_point::real_display_formats"; }
    fn real_display_negative_zero(sim) { @case "floating_point::real_display_negative_zero"; }
    fn real_file_display_formats(sim) { @case "floating_point::real_file_display_formats"; }
}

sv_backends! {
    fn combinational_real_display_and_assertion_messages(sim) {
        @setup { let source = r#"
            module Top(input logic enabled, input logic [63:0] bits);
                real x;
                assign x = $bitstoreal(bits);
                always_comb if (enabled) begin
                    $display("comb=%.2f expression=%.1f", x, x * 2.0);
                    assert (x < 0.0) else $error("assert=%.2f", x);
                end
            endmodule
        "#; }
        @build Simulator::from_sv_sources(vec![(source, Path::new("real_comb_display.sv"))], "Top").four_state(true);
        sim.drain_runtime_events();
        let enabled = sim.signal("enabled");
        let bits = sim.signal("bits");
        sim.modify(|io| { io.set(bits, 0x3ff8000000000000u64); io.set(enabled, 1u8); }).unwrap();
        assert_eq!(sim.get(sim.signal("x")), BigUint::from(0x3ff8000000000000u64));
        let events = sim.drain_runtime_events();
        assert_eq!(events.len(), 2, "{events:?}");
        assert!(events.contains(&celox::RuntimeEvent::Display { message: b"comb=1.50 expression=3.0".to_vec() }), "{events:?}");
        assert!(events.contains(&celox::RuntimeEvent::AssertContinue { message: b"assert=1.50".to_vec() }), "{events:?}");
    }
}
