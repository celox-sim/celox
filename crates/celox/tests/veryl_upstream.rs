//! Reconstructed Veryl upstream cases, executed through the shared corpus.
#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {
    fn constant_arrays_initialize_comb_and_ff_reads(sim) {
        // SV frontend issue #64: constant arrays and four-state FF event signals.
        @ignore_on(sv);
        @case "veryl_regressions::constant_arrays_initialize_comb_and_ff_reads";
    }

    fn wide_shift_count_preserves_unknowns_and_sign_fill(sim) {
        @case "veryl_regressions::wide_shift_count_preserves_unknowns_and_sign_fill";
    }

    fn inside_outside_range_endpoints(sim) {
        // SV frontend issue #64: unsupported set-membership assignment expressions.
        @ignore_on(sv);
        @case "veryl_language::inside_outside_range_endpoints";
    }
    fn parameter_expression_type_cast_widths(sim) {
        // SV frontend issue #64: unsupported cast expressions.
        @ignore_on(sv);
        @case "veryl_language::parameter_expression_type_cast_widths";
    }
    fn packed_union_members_alias(sim) {
        // SV frontend issue #64: unsupported packed struct/union types.
        @ignore_on(sv);
        @case "veryl_language::packed_union_members_alias";
    }
    fn wide_shift_amount_out_of_range(sim) {
        @case "veryl_regressions::wide_shift_amount_out_of_range";
    }
    fn unary_minus_as_shift_amount(sim) {
        @case "veryl_regressions::unary_minus_as_shift_amount";
    }
    fn struct_bit_field_rhs_no_spill(sim) {
        @case "veryl_regressions::struct_bit_field_rhs_no_spill";
    }
    fn wide_struct_bit_field_rhs_no_spill(sim) {
        @case "veryl_regressions::wide_struct_bit_field_rhs_no_spill";
    }
    fn wide_ternary_narrow_branch_no_spill(sim) {
        @case "veryl_regressions::wide_ternary_narrow_branch_no_spill";
    }
    fn nested_array_index_const_array(sim) {
        // SV frontend issue #64 rejects the constant-array assignment expression.
        @ignore_on(sv);
        @case "veryl_regressions::nested_array_index_const_array";
    }
    fn inst_port_default_value_connected_not_folded(sim) {
        @case "veryl_regressions::inst_port_default_value_connected_not_folded";
    }
    fn inlined_function_per_callsite_scratch_in_continuous_assign(sim) {
        // SV frontend issue #64: unsupported cast expressions.
        @ignore_on(sv);
        @case "veryl_regressions::inlined_function_per_callsite_scratch_in_continuous_assign";
    }
}

#[test]
fn zero_dimensions_report_analyzer_diagnostics() {
    for ty in ["logic<0>", "logic<8>[0]", "logic<2, 0>"] {
        let source = format!("module Top (value: output {ty}) {{ assign value = '0; }}");
        let error = celox::Simulator::builder(&source, "Top")
            .build_interpreter()
            .unwrap_err();
        let celox::SimulatorErrorKind::Analyzer(errors) = error.kind() else {
            panic!("{ty}: expected analyzer rejection, got {error:?}");
        };
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, veryl_analyzer::AnalyzerError::ZeroSize { .. })),
            "{ty}: expected ZeroSize, got {errors:?}"
        );
    }
}
