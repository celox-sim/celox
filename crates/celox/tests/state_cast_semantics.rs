use celox::Simulator;

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {
    fn type_cast_clears_unknown_bits_in_comb_and_ff(sim) {
        @omit_veryl;
        @ignore_on(sv);
        @case "state_cast_semantics::type_cast_clears_unknown_bits_in_comb_and_ff";
    }

    fn constant_type_cast_clears_unknown_bits(sim) {
        @omit_veryl;
        @ignore_on(sv);
        @case "state_cast_semantics::constant_type_cast_clears_unknown_bits";
    }

    fn function_formal_type_clears_unknown_bits(sim) {
        @omit_veryl;
        @ignore_on(sv);
        @case "state_cast_semantics::function_formal_type_clears_unknown_bits";
    }

    fn signed_four_state_function_formal_preserves_unknown_bits(sim) {
        @ignore_on(sv);
        @case "state_cast_semantics::signed_four_state_function_formal_preserves_unknown_bits";
    }

    fn implicit_assignment_to_bit_clears_unknowns_without_mutating_source(sim) {
        @ignore_on(veryl, sv);
        @case "state_cast_semantics::implicit_assignment_to_bit_clears_unknowns_without_mutating_source";
    }
}

#[test]
fn state_cast_is_explicit_in_sir() {
    let result = Simulator::builder(
        r#"
module Top (
    a: input logic<130>,
    y: output logic<130>,
) {
    type U130 = bit<130>;
    assign y = a as U130;
}
"#,
        "Top",
    )
    .four_state(true)
    .trace_post_optimized_sir()
    .build_with_trace();
    let sir = result
        .trace
        .format_post_optimized_sir()
        .expect("post-optimized SIR");
    assert!(
        sir.contains("ToTwoState"),
        "the state-conversion boundary disappeared from SIR:\n{sir}"
    );
}
