use super::*;

sv_backends! {
    // Compare cached runtime calls with elaboration-time constants through
    // Celox's four-state API, including the payload and mask of unknown bits.
    fn constant_function_inside_unknowns(sim) {
        @setup { let source = r#"
    module Top(output logic [4:0] q, output logic [4:0] p);
      function automatic logic unknown_match();
        logic x;
        return x inside {1'b1};
      endfunction
      function automatic logic definite_match(); return 1'bx inside {1'b0, 1'b?}; endfunction
      function automatic logic definite_mismatch(); return 2'bx0 inside {2'b11}; endfunction
      function automatic logic unknown_range(); return 1'bx inside {[1'b0:1'b1]}; endfunction
      function automatic logic empty_range(); return 2'b11 inside {[2'bxx:2'b01]}; endfunction
      localparam logic [4:0] P = {empty_range(), unknown_range(), definite_mismatch(), definite_match(), unknown_match()};
      assign p = P;
      assign q = {empty_range(), unknown_range(), definite_mismatch(), definite_match(), unknown_match()};
    endmodule
  "#; }
        @build Simulator::from_sv_sources(vec![(source, Path::new("constant_inside.sv"))], "Top").four_state(true);
        for name in ["q", "p"] {
            assert_eq!(sim.get_four_state(sim.signal(name)), (BigUint::from(0x0bu8), BigUint::from(0x09u8)), "{name}");
        }
    }

    fn constant_function_case_context(sim) { @case "operators::constant_function_case_context"; }
    fn constant_function_case_unknowns(sim) { @case "operators::constant_function_case_unknowns"; }
    fn logical_equivalence(sim) { @case "operators::logical_equivalence"; }
    fn logical_equivalence_unknown(sim) { @case "operators::logical_equivalence_unknown"; }
    fn logical_equivalence_evaluates_operands_once(sim) { @case "operators::logical_equivalence_evaluates_operands_once"; }
    fn logical_implication_short_circuit(sim) { @case "operators::logical_implication_short_circuit"; }
    fn logical_implication_unknown(sim) { @case "operators::logical_implication_unknown"; }
    fn logical_implication(sim) { @case "operators::logical_implication"; }
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
    fn two_state_case_equality_preserves_unknown_literals(sim) {
        @case "operators::two_state_case_equality_preserves_unknown_literals";
    }
    fn two_state_case_equality_compares_literal_structure(sim) {
        @case "operators::two_state_case_equality_compares_literal_structure";
    }
    fn two_state_case_equality_preserves_conditional_unknowns(sim) {
        @case "operators::two_state_case_equality_preserves_conditional_unknowns";
    }
    fn two_state_case_equality_keeps_call_effects_and_loaded_bytes(sim) {
        @case "operators::two_state_case_equality_keeps_call_effects_and_loaded_bytes";
    }
    fn two_state_case_equality_keeps_unknown_expression_bits(sim) {
        @case "operators::two_state_case_equality_keeps_unknown_expression_bits";
    }
}
