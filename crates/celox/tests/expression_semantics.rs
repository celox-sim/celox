#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {
    fn cast_binary_semantics_match_between_comb_and_ff(sim) {
        @omit_veryl;
        @ignore_on(sv);
        @case "expression_semantics::cast_binary_semantics_match_between_comb_and_ff";
    }

    fn parent_context_and_self_determined_boundaries_match_between_comb_and_ff(sim) {
        @ignore_on(sv);
        @case "expression_semantics::parent_context_and_self_determined_boundaries_match_between_comb_and_ff";
    }

    #[ignore = "Veryl 0.20.2 folds signed numeric casts before Celox receives AIR"]
    fn constant_and_runtime_casts_use_the_same_resize_rule(sim) {
        @omit_veryl;
        @case "expression_semantics::constant_and_runtime_casts_use_the_same_resize_rule";
    }

    fn folded_and_runtime_builtin_selects_are_unsigned(sim) {
        @case "expression_semantics::folded_and_runtime_builtin_selects_are_unsigned";
    }

    fn wildcard_predicates_remain_one_bit_in_ternaries_and_concats(sim) {
        @case "expression_semantics::wildcard_predicates_remain_one_bit_in_ternaries_and_concats";
    }

    fn function_actuals_are_converted_at_the_formal_boundary(sim) {
        @case "expression_semantics::function_actuals_are_converted_at_the_formal_boundary";
    }

    fn unary_expression_context_signedness_matches_veryl(sim) {
        @case "expression_semantics::unary_expression_context_signedness_matches_veryl";
    }

    fn signed_type_cast_keeps_comparison_operands_signed(sim) {
        @ignore_on(veryl, sv);
        @case "expression_semantics::signed_type_cast_keeps_comparison_operands_signed";
    }

    fn aggregate_results_consume_the_unary_parent_context(sim) {
        @ignore_on(sv);
        @case "expression_semantics::aggregate_results_consume_the_unary_parent_context";
    }

    #[ignore = "Deferred $bits fix: Celox returns 0xff; Veryl simulator panics; SV frontend unsupported. See celox-test-suite-veryl/MISMATCH_REVIEW.md section 2"]
    fn system_function_results_obey_ternary_width_contexts(sim) {
        @case "expression_semantics::system_function_results_obey_ternary_width_contexts";
    }

    fn short_circuit_operators_skip_effectful_operands(sim) {
        @omit_veryl;
        @ignore_on(sv);
        @case "expression_semantics::short_circuit_operators_skip_effectful_operands";
    }

    fn numeric_cast_preserves_four_state_sign_extension(sim) {
        @ignore_on(sv);
        @case "expression_semantics::numeric_cast_preserves_four_state_sign_extension";
    }

    fn narrow_signed_comparison_sign_extends_both_operands(sim) {
        @case "expression_semantics::narrow_signed_comparison_sign_extends_both_operands";
    }
}
