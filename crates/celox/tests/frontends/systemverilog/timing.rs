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
    fn imported_timed_tasks_do_not_replay_setup_edges(sim) { @case "timing::imported_timed_tasks_do_not_replay_setup_edges"; }
    fn untimed_local_tasks_shadow_timed_tasks_in_other_scopes(sim) { @case "timing::untimed_local_tasks_shadow_timed_tasks_in_other_scopes"; }
    fn automatic_module_ff_staging_keeps_its_state(sim) { @case "timing::automatic_module_ff_staging_keeps_its_state"; }
    fn control_members_do_not_pollute_user_package_imports(sim) { @case "timing::control_members_do_not_pollute_user_package_imports"; }
    fn blocking_ff_stores_are_visible_before_inactive_work(sim) { @case "timing::blocking_ff_stores_are_visible_before_inactive_work"; }
    fn nonblocking_ff_stores_remain_pending_during_inactive_work(sim) { @case "timing::nonblocking_ff_stores_remain_pending_during_inactive_work"; }
    fn mixed_ff_stores_keep_their_active_and_nba_values(sim) { @case "timing::mixed_ff_stores_keep_their_active_and_nba_values"; }
    fn single_item_generate_ff_rewrites_keep_their_else(sim) { @case "timing::single_item_generate_ff_rewrites_keep_their_else"; }
}

sv_backends! {
 fn immediate_assertion_ff_keeps_both_actions(sim) { @case "timing::immediate_assertion_ff_keeps_both_actions"; }
 fn printed_finish_notice_is_design_output(sim) { @case "timing::printed_finish_notice_is_design_output"; }
 fn combinational_finish_ends_timed_execution(sim) { @case "timing::combinational_finish_ends_timed_execution"; }
}

sv_backends! {
 fn local_function_shadows_unrelated_timed_task(sim) { @case "timing::local_function_shadows_unrelated_timed_task"; }
 fn explicit_timed_import_overrides_earlier_wildcard(sim) { @case "timing::explicit_timed_import_overrides_earlier_wildcard"; }
}

sv_backends! {
 fn else_prefixed_instance_is_not_an_else_action(sim) { @case "timing::else_prefixed_instance_is_not_an_else_action"; }
 fn nested_imports_do_not_leak_into_module_edge_processes(sim) { @case "timing::nested_imports_do_not_leak_into_module_edge_processes"; }
}

sv_backends! {
 fn single_item_case_generate_ff_is_staged_together(sim){@case "timing::single_item_case_generate_ff_is_staged_together";}
 fn timed_expansion_preserves_adapter_macros(sim){@case "timing::timed_expansion_preserves_adapter_macros";}
}

sv_backends! {
 fn comment_semicolons_in_function_headers_preserve_setup_edges(sim){@case "timing::comment_semicolons_in_function_headers_preserve_setup_edges";}
}

sv_backends! {
 fn import_comment_commas_do_not_change_visible_callables(sim){@case "timing::import_comment_commas_do_not_change_visible_callables";}
}

sv_backends! {
 fn escaped_import_commas_keep_explicit_callable_selection(sim){@case "timing::escaped_import_commas_keep_explicit_callable_selection";}
}

sv_backends! {
 fn parameterized_interface_methods_keep_setup_edges(sim){@case "timing::parameterized_interface_methods_keep_setup_edges";}
}

sv_backends! {
    fn blocking_concatenation_publishes_one_event_update(sim) {
        @case "timing::blocking_concatenation_publishes_one_event_update";
    }
    fn finite_active_feedback_preserves_convergence(sim) {
        @case "timing::finite_active_feedback_preserves_convergence";
    }
}

sv_backends! { fn interface_modport_methods_keep_setup_edges(sim) { @case "timing::interface_modport_methods_keep_setup_edges"; } }

sv_backends! { fn generic_interface_methods_keep_setup_edges(sim) { @case "timing::generic_interface_methods_keep_setup_edges"; } }

sv_backends! { fn inherited_interface_methods_keep_setup_edges(sim) { @case "timing::inherited_interface_methods_keep_setup_edges"; } }

sv_backends! { fn pure_function_rhs_preserves_distinct_write_times(sim) { @case "timing::pure_function_rhs_preserves_distinct_write_times"; } }

sv_backends! { fn conditional_writes_preserve_distinct_activation_times(sim) { @case "timing::conditional_writes_preserve_distinct_activation_times"; } }

sv_backends! { fn case_writes_preserve_distinct_activation_times(sim) { @case "timing::case_writes_preserve_distinct_activation_times"; } }

sv_backends! { fn staged_wildcard_finish_waits_for_startup(sim) { @case "timing::staged_wildcard_finish_waits_for_startup"; } }

sv_backends! { fn constant_repeat_writes_preserve_distinct_activation_times(sim) { @case "timing::constant_repeat_writes_preserve_distinct_activation_times"; } }

sv_backends! { fn separate_comb_assignments_preserve_selected_dependencies(sim) { @case "timing::separate_comb_assignments_preserve_selected_dependencies"; } }

sv_backends! { fn staged_always_comb_finish_ignores_initial_inputs(sim) { @case "timing::staged_always_comb_finish_ignores_initial_inputs"; } }

sv_backends! { fn constant_for_writes_preserve_distinct_activation_times(sim) { @case "timing::constant_for_writes_preserve_distinct_activation_times"; } }

sv_backends! { fn untimed_tasks_preserve_distinct_activation_times(sim) { @case "timing::untimed_tasks_preserve_distinct_activation_times"; } }

sv_backends! { fn untimed_task_copyback_preserves_distinct_activation_times(sim) { @case "timing::untimed_task_copyback_preserves_distinct_activation_times"; } }

sv_backends! { fn statically_satisfied_waits_preserve_distinct_activation_times(sim) { @case "timing::statically_satisfied_waits_preserve_distinct_activation_times"; } }
sv_backends! { fn nba_woken_waiters_sample_before_continuations(sim) { @case "timing::nba_woken_waiters_sample_before_continuations"; } }
sv_backends! { fn staged_always_comb_side_effects_wait_for_setup(sim) { @case "timing::staged_always_comb_side_effects_wait_for_setup"; } }
