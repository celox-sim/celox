sv_backends! {
    fn casts_preserve_guarded_division_signedness(sim) {
        @case "operators::casts_preserve_guarded_division_signedness";
    }

    fn casts_preserve_guarded_division_unknowns(sim) {
        @case "operators::casts_preserve_guarded_division_unknowns";
    }

    fn preserves_power_signedness_in_arithmetic(sim) {
        @case "operators::preserves_power_signedness_in_arithmetic";
    }

    fn preserves_power_signedness_in_four_state_arithmetic(sim) {
        @case "operators::preserves_power_signedness_in_four_state_arithmetic";
    }

    fn size_casts_resolve_generated_locals_and_parameter_expressions(sim) {
        @case "operators::size_casts_resolve_generated_locals_and_parameter_expressions";
    }

    fn parameter_size_casts_in_generate_conditions(sim) {
        @case "operators::parameter_size_casts_in_generate_conditions";
    }
    fn forwards_replicated_parameter_values(sim) {
        @case "operators::forwards_replicated_parameter_values";
    }
    fn zero_count_replications_preserve_unknown_bits(sim) {
        @case "operators::zero_count_replications_preserve_unknown_bits";
    }
    fn zero_count_replications_preserve_concat_bits(sim) {
        @case "operators::zero_count_replications_preserve_concat_bits";
    }
    fn multidimensional_packed_declared_ranges_four_state(sim) { @case "operators::multidimensional_packed_declared_ranges_four_state"; }
    fn multidimensional_packed_declared_ranges(sim) { @case "operators::multidimensional_packed_declared_ranges"; }
    fn multidimensional_packed_declared_range_writes(sim) { @case "operators::multidimensional_packed_declared_range_writes"; }

    fn nested_operands_extend_before_context_operations(sim) { @case "operators::nested_operands_extend_before_context_operations"; }

    fn wide_arithmetic_shift_preserves_unknown_sign(sim) { @case "operators::wide_arithmetic_shift_preserves_unknown_sign"; }

    fn partial_word_shift_sign_z_65(sim) { @case "operators::partial_word_shift_sign_z_65"; }
    fn partial_word_shift_sign_x_65(sim) { @case "operators::partial_word_shift_sign_x_65"; }

    fn simulates_systemverilog_top_comb_assign(sim) {
        @case "operators::simulates_systemverilog_top_comb_assign";
    }

    fn simulates_systemverilog_binary_operators(sim) {
        @case "operators::simulates_systemverilog_binary_operators";
    }

    fn simulates_systemverilog_case_equality_operators(sim) {
        @case "operators::simulates_systemverilog_case_equality_operators";
    }

    fn simulates_systemverilog_unary_operators(sim) {
        @case "operators::simulates_systemverilog_unary_operators";
    }

    fn simulates_systemverilog_reduction_unary_operators(sim) {
        @case "operators::simulates_systemverilog_reduction_unary_operators";
    }

    fn simulates_systemverilog_select_and_concat(sim) {
        @case "operators::simulates_systemverilog_select_and_concat";
    }

    fn simulates_systemverilog_selected_lvalue_assignments(sim) {
        @case "operators::simulates_systemverilog_selected_lvalue_assignments";
    }

    fn simulates_systemverilog_selected_lvalue_ff_intermediate(sim) {
        @case "operators::simulates_systemverilog_selected_lvalue_ff_intermediate";
    }

    fn simulates_systemverilog_hierarchical_selected_lvalue_ff_intermediate(sim) {
        @case "operators::simulates_systemverilog_hierarchical_selected_lvalue_ff_intermediate";
    }

    fn simulates_systemverilog_hierarchical_parameter_selected_lvalue_ff_intermediate(sim) {
        @case "operators::simulates_systemverilog_hierarchical_parameter_selected_lvalue_ff_intermediate";
    }

    fn simulates_systemverilog_contiguous_selected_lvalue_ff_intermediate(sim) {
        @case "operators::simulates_systemverilog_contiguous_selected_lvalue_ff_intermediate";
    }

    fn simulates_systemverilog_genvar_loop_comb_assignments(sim) {
        @case "operators::simulates_systemverilog_genvar_loop_comb_assignments";
    }

    fn simulates_systemverilog_genvar_loop_with_localparam_and_if(sim) {
        @case "operators::simulates_systemverilog_genvar_loop_with_localparam_and_if";
    }

    fn simulates_systemverilog_packed_multidimensional_selects(sim) {
        @case "operators::simulates_systemverilog_packed_multidimensional_selects";
    }

    fn simulates_systemverilog_simple_always_ff(sim) {
        @case "operators::simulates_systemverilog_simple_always_ff";
    }

    fn simulates_systemverilog_always_ff_if_else_chain(sim) {
        @case "operators::simulates_systemverilog_always_ff_if_else_chain";
    }

    fn simulates_systemverilog_always_ff_case(sim) {
        @case "operators::simulates_systemverilog_always_ff_case";
    }

    fn simulates_systemverilog_always_ff_case_context_values(sim) {
        @case "operators::simulates_systemverilog_always_ff_case_context_values";
    }

    fn simulates_systemverilog_always_ff_case_calls_and_xz_parameters(sim) {
        @case "operators::simulates_systemverilog_always_ff_case_calls_and_xz_parameters";
    }

    fn simulates_systemverilog_repeat_concat(sim) {
        @case "operators::simulates_systemverilog_repeat_concat";
    }

    fn preserves_unknown_complement_and_converts_bit_function_arguments(sim) {
        @case "operators::preserves_unknown_complement_and_converts_bit_function_arguments";
    }

    fn applies_procedural_truth_and_bit_conversion_to_function_results(sim) {
        @case "operators::applies_procedural_truth_and_bit_conversion_to_function_results";
    }

    fn tracks_nested_function_slice_dependencies(sim) {
        @case "operators::tracks_nested_function_slice_dependencies";
    }

    fn preserves_function_locals_assigned_by_conditionals(sim) {
        @case "operators::preserves_function_locals_assigned_by_conditionals";
    }

    fn preserves_function_locals_assigned_by_case_items(sim) {
        @case "operators::preserves_function_locals_assigned_by_case_items";
    }

    fn preserves_disjoint_slice_writes_in_merged_always_ff_processes(sim) {
        @case "operators::preserves_disjoint_slice_writes_in_merged_always_ff_processes";
    }

    fn infers_async_reset_from_always_ff_assignment_rhs(sim) {
        @case "operators::infers_async_reset_from_always_ff_assignment_rhs";
    }

    fn preserves_multidimensional_packed_prefix_in_part_selects(sim) {
        @case "operators::preserves_multidimensional_packed_prefix_in_part_selects";
    }

    fn guards_division_by_zero_nested_under_muxes(sim) {
        @case "operators::guards_division_by_zero_nested_under_muxes";
    }

    fn zero_extends_mixed_signed_arithmetic_operands(sim) {
        @case "operators::zero_extends_mixed_signed_arithmetic_operands";
    }

    fn propagates_child_input_width_into_connection_expressions(sim) {
        @case "operators::propagates_child_input_width_into_connection_expressions";
    }

    fn preserves_signed_parameter_types_in_hierarchy_connections(sim) {
        @case "operators::preserves_signed_parameter_types_in_hierarchy_connections";
    }

    fn converts_function_arguments_using_actual_signedness(sim) {
        @case "operators::converts_function_arguments_using_actual_signedness";
    }

    fn negates_wide_systemverilog_values(sim) {
        @case "operators::negates_wide_systemverilog_values";
    }

    fn unknown_logical_operand_evaluates_effectful_right_operand(sim) {
        @case "operators::unknown_logical_operand_evaluates_effectful_right_operand";
    }

    fn wildcard_comparisons_normalize_unknown_to_x(sim) {
        @case "operators::wildcard_comparisons_normalize_unknown_to_x";
    }

    fn wide_wildcard_comparisons_normalize_unknown_to_x(sim) {
        @case "operators::wide_wildcard_comparisons_normalize_unknown_to_x";
    }

    fn masked_shift_fields_keep_expression_width(sim) {
        @case "operators::masked_shift_fields_keep_expression_width";
    }
}

sv_backends! {
    fn wide_reduction_and_respects_partial_storage_words(sim) {
        @case "operators::wide_reduction_and_respects_partial_storage_words";
    }
}
