use super::*;

sv_backends! {
    fn constant_functions_inside_selected_parameter_initializers(sim) {
        @case "procedural::constant_functions_inside_selected_parameter_initializers";
    }

    fn constant_functions_preserve_concat_operand_widths(sim) {
        @case "procedural::constant_functions_preserve_concat_operand_widths";
    }

    fn static_function_scratch_arrays_are_filled_before_reads(sim) {
        @case "procedural::static_function_scratch_arrays_are_filled_before_reads";
    }

    fn static_function_result_filled_by_constant_loops(sim) {
        @case "procedural::static_function_result_filled_by_constant_loops";
    }

    fn typedef_declaration_initializers(sim) {
        @case "procedural::typedef_declaration_initializers";
    }

    fn imported_typedef_declaration_initializers(sim) {
        @case "procedural::imported_typedef_declaration_initializers";
    }

    fn typedef_declaration_initializers_keep_unknown_bits(sim) {
        @case "procedural::typedef_declaration_initializers_keep_unknown_bits";
    }

    fn always_comb_case_compares_at_the_common_width(sim) {
        @case "procedural::always_comb_case_compares_at_the_common_width";
    }

    fn always_ff_blocking_assignments_and_run_time_loops(sim) {
        @case "procedural::always_ff_blocking_assignments_and_run_time_loops";
    }

    fn mixed_blocking_and_nonblocking_writes_keep_their_regions(sim) {
        @case "procedural::mixed_blocking_and_nonblocking_writes_keep_their_regions";
    }

    fn constant_functions_size_parameters_and_ports(sim) {
        @case "procedural::constant_functions_size_parameters_and_ports";
    }

    fn aggregate_parameters_are_constant_tables(sim) {
        @case "procedural::aggregate_parameters_are_constant_tables";
    }

    fn initial_blocks_define_the_initial_state(sim) {
        @case "procedural::initial_blocks_define_the_initial_state";
    }

    fn loop_initializer_widens_by_its_own_signedness(sim) {
        @case "procedural::loop_initializer_widens_by_its_own_signedness";
    }

    fn select_indices_call_functions(sim) {
        @case "procedural::select_indices_call_functions";
    }

    fn runtime_bits_of_a_constant_element(sim) {
        @case "procedural::runtime_bits_of_a_constant_element";
    }

    fn select_index_calls_run_as_written(sim) {
        @case "procedural::select_index_calls_run_as_written";
    }

    fn select_index_call_skipped_by_logical_and(sim) {
        @case "procedural::select_index_call_skipped_by_logical_and";
    }

    fn concatenated_targets_fix_their_positions_first(sim) {
        @case "procedural::concatenated_targets_fix_their_positions_first";
    }

    fn ff_select_index_calls_write_state_and_run_once(sim) {
        @case "procedural::ff_select_index_calls_write_state_and_run_once";
    }

    fn function_bodies_run_their_index_calls_each_time(sim) {
        @case "procedural::function_bodies_run_their_index_calls_each_time";
    }

    fn ff_processes_write_bits_of_separate_array_elements(sim) {
        @case "procedural::ff_processes_write_bits_of_separate_array_elements";
    }

    fn runtime_bits_stay_within_their_element(sim) {
        @case "procedural::runtime_bits_stay_within_their_element";
    }

    fn nonblocking_runtime_element_writes_apply_in_order(sim) {
        @case "procedural::nonblocking_runtime_element_writes_apply_in_order";
    }

    fn declaration_initializers_define_the_initial_state(sim) {
        @case "procedural::declaration_initializers_define_the_initial_state";
    }

    fn initial_blocks_run_after_declaration_initializers(sim) {
        @case "procedural::initial_blocks_run_after_declaration_initializers";
    }

    fn declaration_initializers_keep_unknown_bits(sim) {
        @case "procedural::declaration_initializers_keep_unknown_bits";
    }

    fn always_ff_at_both_edges_of_one_clock(sim) {
        @case "procedural::always_ff_at_both_edges_of_one_clock";
    }

    fn always_ff_at_both_edges_of_a_child_clock_port(sim) {
        @case "procedural::always_ff_at_both_edges_of_a_child_clock_port";
    }

    fn always_ff_reset_at_both_edges_and_as_a_clock(sim) {
        @case "procedural::always_ff_reset_at_both_edges_and_as_a_clock";
    }
}

sv_backends! {
    fn static_function_locals_share_storage_between_calls(sim) {
        @case "procedural::static_function_locals_share_storage_between_calls";
    }
    fn static_function_result_keeps_value_between_calls(sim) {
        @case "procedural::static_function_result_keeps_value_between_calls";
    }
    fn static_function_real_locals_and_results_keep_value(sim) {
        @case "procedural::static_function_real_locals_and_results_keep_value";
    }
    fn static_function_output_formals_keep_value(sim) {
        @case "procedural::static_function_output_formals_keep_value";
    }
}

sv_backends! {
    fn static_function_constant_calls_do_not_change_runtime_storage(sim) {
        @case "procedural::static_function_constant_calls_do_not_change_runtime_storage";
    }
    fn static_function_package_storage_is_shared_between_calls(sim) {
        @case "procedural::static_function_package_storage_is_shared_between_calls";
    }
}

sv_backends! {
    fn static_function_retained_arrays_and_instance_storage(sim) {
        @case "procedural::static_function_retained_arrays_and_instance_storage";
    }
    fn static_function_four_state_result_retains_unknown_and_known_values(sim) {
        @case "procedural::static_function_four_state_result_retains_unknown_and_known_values";
    }
    fn static_function_real_storage_restores_checkpoint(sim) {
        @setup { let source = r#"
            module Top(input bit clk, output logic [63:0] value);
                function real step();
                    static real saved = 0.25;
                    saved += 0.5;
                    return saved;
                endfunction
                always_ff @(posedge clk) value <= $realtobits(step());
            endmodule
        "#; }
        @build Simulator::from_sv_sources(vec![(source, Path::new("static_checkpoint.sv"))], "Top").four_state(true);
        let clk = sim.event("clk");
        let value = sim.signal("value");
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(value), num_bigint::BigUint::from(0.75f64.to_bits()));
        let checkpoint = sim.checkpoint().unwrap();
        sim.tick(clk).unwrap();
        let next = sim.get(value);
        assert_eq!(next, num_bigint::BigUint::from(1.25f64.to_bits()));
        sim.restore(&checkpoint).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(value), next);
    }
}

sv_backends! {
    fn static_function_wide_packed_storage_keeps_all_bits(sim) {
        @case "procedural::static_function_wide_packed_storage_keeps_all_bits";
    }
}
