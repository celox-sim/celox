sv_backends! {
    fn always_comb_case_compares_at_the_common_width(sim) {
        @case "procedural::always_comb_case_compares_at_the_common_width";
    }

    fn always_ff_blocking_assignments_and_run_time_loops(sim) {
        @case "procedural::always_ff_blocking_assignments_and_run_time_loops";
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
}
