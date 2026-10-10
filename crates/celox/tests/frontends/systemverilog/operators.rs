sv_backends! {
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
}
