//! Shared timing-control cases: `#delay`, `@(event)` and `wait` in
//! processes, driven through the timed `Simulation` on every backend.

sv_backends! {
    fn delays_resume_initial_blocks_at_later_times(sim) {
        @case "timing::delays_resume_initial_blocks_at_later_times";
    }

    fn always_with_a_delay_generates_a_clock(sim) {
        @case "timing::always_with_a_delay_generates_a_clock";
    }

    fn edge_waits_resume_before_the_registers_of_that_edge_update(sim) {
        @case "timing::edge_waits_resume_before_the_registers_of_that_edge_update";
    }

    fn any_change_waits_wake_on_every_value_change(sim) {
        @case "timing::any_change_waits_wake_on_every_value_change";
    }

    fn wait_statements_block_until_their_condition_holds(sim) {
        @case "timing::wait_statements_block_until_their_condition_holds";
    }

    fn level_sensitive_always_runs_as_a_process(sim) {
        @case "timing::level_sensitive_always_runs_as_a_process";
    }

    fn processes_wake_each_other_at_the_same_time(sim) {
        @case "timing::processes_wake_each_other_at_the_same_time";
    }

    fn tasks_with_timing_controls_run_in_processes(sim) {
        @case "timing::tasks_with_timing_controls_run_in_processes";
    }

    fn edge_waits_see_the_signals_the_script_drives(sim) {
        @case "timing::edge_waits_see_the_signals_the_script_drives";
    }

    fn finish_in_an_always_process_ends_the_simulation(sim) {
        @case "timing::finish_in_an_always_process_ends_the_simulation";
    }

    fn waiting_processes_do_not_keep_an_idle_simulation_running(sim) {
        @case "timing::waiting_processes_do_not_keep_an_idle_simulation_running";
    }

    fn four_state_edges_count_transitions_through_unknown(sim) {
        @case "timing::four_state_edges_count_transitions_through_unknown";
    }

    fn unknown_delays_and_conditions_count_as_zero_and_false(sim) {
        @case "timing::unknown_delays_and_conditions_count_as_zero_and_false";
    }
}
