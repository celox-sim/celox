use celox::Simulator;

#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

// The SV frontend currently rejects output/inout function arguments.
all_backends! {

// Keep direct-syntax regressions ready for an upstream fix: Veryl 0.21.0's
// conv_function matches only Input/Output for scalar formals and panics on Inout.
#[ignore = "Veryl 0.21.0 conv_function panics on scalar inout arguments"]
fn test_comb_inout_statement_copies_input_before_mutating_formal(sim) {
    @case "function_arguments::test_comb_inout_statement_copies_input_before_mutating_formal";
}

#[ignore = "Veryl 0.21.0 conv_function panics on scalar inout arguments"]
fn test_ff_inout_expression_copyout_commits_with_nonblocking_assignments(sim) {
    @case "function_arguments::test_ff_inout_expression_copyout_commits_with_nonblocking_assignments";
}

fn test_comb_output_copyout_freezes_aliased_inputs_and_return(sim) {
    @ignore_on(sv);
    @case "function_arguments::test_comb_output_copyout_freezes_aliased_inputs_and_return";
}

fn test_comb_statement_output_copyout_obeys_named_argument_order(sim) {
    @ignore_on(sv);
    @case "function_arguments::test_comb_statement_output_copyout_obeys_named_argument_order";
}

fn test_comb_nested_output_copyout_stops_at_early_return(sim) {
    @ignore_on(veryl, sv);
    @case "function_arguments::test_comb_nested_output_copyout_stops_at_early_return";
}

fn test_comb_output_copyout_to_concat_preserves_unselected_bits_and_elements(sim) {
    @ignore_on(veryl, sv);
    @case "function_arguments::test_comb_output_copyout_to_concat_preserves_unselected_bits_and_elements";
}

fn test_output_copyout_converts_formal_width_and_signedness_in_comb_and_ff(sim) {
    @ignore_on(veryl, sv);
    @case "function_arguments::test_output_copyout_converts_formal_width_and_signedness_in_comb_and_ff";
}

fn test_comb_expression_output_copyout_uses_unsigned_formal_for_signed_body(sim) {
    @ignore_on(sv);
    @case "function_arguments::test_comb_expression_output_copyout_uses_unsigned_formal_for_signed_body";
}

fn test_comb_output_copyout_observer_sees_formal_sign_extension(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @build Simulator::builder(r#"
        module Top (
            d: input logic<8>, copied: output logic<16>,
            bits: output logic<2>, returned: output logic,
        ) {
            function observe (x: input logic<16>) -> logic {
                $display("copied=%0d", x);
                return x[15];
            }
            function produce (x: input logic<8>, dst: output signed logic<8>) -> logic {
                dst = x;
                return 1'b1;
            }
            always_comb {
                copied = 16'd0;
                bits = 2'b00;
                returned = produce(d, {bits[observe(copied)], copied});
            }
        }
    "#, "Top");
    let d = sim.signal("d");
    let copied = sim.signal("copied");
    let bits = sim.signal("bits");
    let returned = sim.signal("returned");
    sim.drain_runtime_events();

    for value in [0x80u8, 0x7f, 0xff, 0] {
        sim.modify(|io| io.set(d, value)).unwrap();
        let expected = value as i8 as i16 as u16;
        assert_eq!(sim.get(copied), expected.into());
        let expected_bits = if value & 0x80 != 0 { 2u8 } else { 0 };
        assert_eq!(sim.get(bits), expected_bits.into());
        assert_eq!(sim.get(returned), 1u8.into());
        assert_eq!(
            sim.drain_runtime_events(),
            vec![celox::RuntimeEvent::Display {
                message: format!("copied={expected}"),
            }],
        );
    }
}

fn test_ff_expression_output_copyout_extends_before_splitting_concat(sim) {
    @ignore_on(veryl, sv);
    @case "function_arguments::test_ff_expression_output_copyout_extends_before_splitting_concat";
}

fn test_ff_statement_output_copyout_freezes_all_inputs(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @case "function_arguments::test_ff_statement_output_copyout_freezes_all_inputs";
}

fn test_ff_output_copyout_to_dynamic_slice_preserves_other_bits(sim) {
    @ignore_on(veryl, sv);
    @case "function_arguments::test_ff_output_copyout_to_dynamic_slice_preserves_other_bits";
}

fn test_ff_nested_output_copyout_is_visible_before_outer_copyout(sim) {
    @omit_veryl;
    @ignore_on(sv);
    @case "function_arguments::test_ff_nested_output_copyout_is_visible_before_outer_copyout";
}

}
