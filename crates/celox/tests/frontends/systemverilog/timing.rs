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

    fn host_inputs_are_staged_before_time_zero_initial_blocks(sim) {
        @case "timing::host_inputs_are_staged_before_time_zero_initial_blocks";
    }

    fn eval_starts_due_time_zero_processes(sim) {
        @case "timing::eval_starts_due_time_zero_processes";
    }

    fn direct_reads_settle_waiting_processes(sim) {
        @case "timing::direct_reads_settle_waiting_processes";
    }

    fn output_reads_settle_waiting_processes(sim) {
        @case "timing::output_reads_settle_waiting_processes";
    }

    fn run_until_drains_target_time_before_later_writes(sim) {
        @case "timing::run_until_drains_target_time_before_later_writes";
    }

    fn repeated_settles_do_not_advance_design_time(sim) {
        @case "timing::repeated_settles_do_not_advance_design_time";
    }

    fn run_to_finish_has_no_simulated_time_deadline(sim) {
        @case "timing::run_to_finish_has_no_simulated_time_deadline";
    }
}

sv_backends! {
    fn plain_always_processes_start_after_host_inputs(sim) {
        @case "timing::plain_always_processes_start_after_host_inputs";
    }
    fn timed_control_names_do_not_collide_with_design_names(sim) {
        @case "timing::timed_control_names_do_not_collide_with_design_names";
    }
}

sv_backends! {
    fn event_always_processes_start_after_host_inputs(sim) {
        @case "timing::event_always_processes_start_after_host_inputs";
    }
}

sv_backends! {
    fn ff_actions_wait_for_staged_startup(sim) {
        @case "timing::ff_actions_wait_for_staged_startup";
    }
    fn ff_startup_does_not_invent_an_unstaged_edge(sim) {
        @case "timing::ff_startup_does_not_invent_an_unstaged_edge";
    }
}

sv_backends! {
    fn ff_startup_preserves_nested_if_case_actions(sim) {
        @case "timing::ff_startup_preserves_nested_if_case_actions";
    }
}

sv_backends! {
    fn qualified_ff_startup_actions(sim) { @case "timing::qualified_ff_startup_actions"; }
    fn ordinary_edge_always_retains_staged_startup_edges(sim) { @case "timing::ordinary_edge_always_retains_staged_startup_edges"; }
    fn macro_ff_startup_actions(sim) { @case "timing::macro_ff_startup_actions"; }
    fn macro_argument_process_keywords_are_expanded_before_staging(sim) { @case "timing::macro_argument_process_keywords_are_expanded_before_staging"; }
    fn do_while_ff_bodies_are_staged_in_full(sim) { @case "timing::do_while_ff_bodies_are_staged_in_full"; }
    fn wildcard_always_blocks_settle_staged_inputs(sim) { @case "timing::wildcard_always_blocks_settle_staged_inputs"; }
    fn timed_edge_always_bodies_do_not_replay_setup_edges(sim) { @case "timing::timed_edge_always_bodies_do_not_replay_setup_edges"; }
    fn timed_edge_always_task_calls_do_not_replay_setup_edges(sim) { @case "timing::timed_edge_always_task_calls_do_not_replay_setup_edges"; }
}
