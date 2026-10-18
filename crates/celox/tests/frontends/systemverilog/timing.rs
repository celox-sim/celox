//! Shared timing-control cases: `#delay`, `@(event)` and `wait` in
//! processes, driven through the timed `Simulation`.
//!
//! Every exclusion names what Celox lacks for the case, so the change that
//! adds it removes the exclusion. Supported cases run on every backend.

macro_rules! timed_cases {
    () => {};
    (
        $(#[$attr:meta])*
        fn $name:ident = $case:literal;
        $($rest:tt)*
    ) => {
        mod $name {
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            #[test]
            $(#[$attr])*
            fn native() {
                crate::suite::run_case($case, "native");
            }

            #[test]
            $(#[$attr])*
            fn cranelift() {
                crate::suite::run_case($case, "cranelift");
            }

            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            #[test]
            $(#[$attr])*
            fn native_parallel() {
                crate::suite::run_case($case, "native-parallel");
            }

            #[test]
            $(#[$attr])*
            fn cranelift_parallel() {
                crate::suite::run_case($case, "cranelift-parallel");
            }

            #[test]
            $(#[$attr])*
            fn wasm() {
                crate::suite::run_case($case, "wasm");
            }
        }
        timed_cases! { $($rest)* }
    };
}

timed_cases! {
    fn time_calls_in_child_port_subroutines = "timing::time_calls_in_child_port_subroutines";
    fn time_calls_in_child_port_actuals = "timing::time_calls_in_child_port_actuals";
    fn delays_resume_initial_blocks_at_later_times = "timing::delays_resume_initial_blocks_at_later_times";
    fn always_with_a_delay_generates_a_clock = "timing::always_with_a_delay_generates_a_clock";
    fn edge_waits_resume_before_the_registers_of_that_edge_update = "timing::edge_waits_resume_before_the_registers_of_that_edge_update";
    fn any_change_waits_wake_on_every_value_change = "timing::any_change_waits_wake_on_every_value_change";
    fn wait_statements_block_until_their_condition_holds = "timing::wait_statements_block_until_their_condition_holds";
    fn level_sensitive_always_runs_as_a_process = "timing::level_sensitive_always_runs_as_a_process";
    fn processes_wake_each_other_at_the_same_time = "timing::processes_wake_each_other_at_the_same_time";
    #[ignore = "the SV frontend does not lower tasks with timing controls yet (#1123)"]
    fn tasks_with_timing_controls_run_in_processes = "timing::tasks_with_timing_controls_run_in_processes";
    fn edge_waits_see_the_signals_the_script_drives = "timing::edge_waits_see_the_signals_the_script_drives";
    fn time_functions_report_the_time_in_the_caller_unit = "timing::time_functions_report_the_time_in_the_caller_unit";
    fn finish_in_an_always_process_ends_the_simulation = "timing::finish_in_an_always_process_ends_the_simulation";
    fn waiting_processes_do_not_keep_an_idle_simulation_running = "timing::waiting_processes_do_not_keep_an_idle_simulation_running";
    fn four_state_edges_count_transitions_through_unknown = "timing::four_state_edges_count_transitions_through_unknown";
    fn unknown_delays_and_conditions_count_as_zero_and_false = "timing::unknown_delays_and_conditions_count_as_zero_and_false";
    fn direct_reads_settle_waiting_processes = "timing::direct_reads_settle_waiting_processes";
    fn output_reads_settle_waiting_processes = "timing::output_reads_settle_waiting_processes";
    fn run_until_drains_target_time_before_later_writes = "timing::run_until_drains_target_time_before_later_writes";
    fn repeated_settles_do_not_advance_design_time = "timing::repeated_settles_do_not_advance_design_time";
    fn run_to_finish_has_no_simulated_time_deadline = "timing::run_to_finish_has_no_simulated_time_deadline";
    fn blocking_ff_stores_are_visible_before_inactive_work = "timing::blocking_ff_stores_are_visible_before_inactive_work";
    fn blocking_ff_clock_edges_trigger_their_domains_before_inactive_work = "timing::blocking_ff_clock_edges_trigger_their_domains_before_inactive_work";
    fn nonblocking_assignments_in_processes_update_in_the_nba_region = "timing::nonblocking_assignments_in_processes_update_in_the_nba_region";
    fn nonblocking_ff_stores_remain_pending_during_inactive_work = "timing::nonblocking_ff_stores_remain_pending_during_inactive_work";
    fn mixed_ff_stores_keep_their_active_and_nba_values = "timing::mixed_ff_stores_keep_their_active_and_nba_values";
    fn printed_finish_notice_is_design_output = "timing::printed_finish_notice_is_design_output";
    fn combinational_finish_ends_timed_execution = "timing::combinational_finish_ends_timed_execution";
    fn inactive_time_directives_do_not_change_scheduler_units = "timing::inactive_time_directives_do_not_change_scheduler_units";
    #[ignore = "the SV frontend lowers only variables as event expressions (#1123)"]
    fn blocking_concatenation_publishes_one_event_update = "timing::blocking_concatenation_publishes_one_event_update";
    fn finite_active_feedback_preserves_convergence = "timing::finite_active_feedback_preserves_convergence";
    #[ignore = "the SV frontend lowers only variables as event expressions (#1123)"]
    fn pure_function_rhs_preserves_distinct_write_times = "timing::pure_function_rhs_preserves_distinct_write_times";
    #[ignore = "the SV frontend lowers only variables as event expressions (#1123)"]
    fn conditional_writes_preserve_distinct_activation_times = "timing::conditional_writes_preserve_distinct_activation_times";
    #[ignore = "the SV frontend lowers only variables as event expressions (#1123)"]
    fn case_writes_preserve_distinct_activation_times = "timing::case_writes_preserve_distinct_activation_times";
    #[ignore = "the SV frontend lowers only variables as event expressions (#1123)"]
    fn constant_repeat_writes_preserve_distinct_activation_times = "timing::constant_repeat_writes_preserve_distinct_activation_times";
    #[ignore = "the SV frontend lowers only variables as event expressions (#1123)"]
    fn separate_comb_assignments_preserve_selected_dependencies = "timing::separate_comb_assignments_preserve_selected_dependencies";
    #[ignore = "the SV frontend lowers only variables as event expressions (#1123)"]
    fn constant_for_writes_preserve_distinct_activation_times = "timing::constant_for_writes_preserve_distinct_activation_times";
    #[ignore = "the SV frontend lowers only variables as event expressions (#1123)"]
    fn untimed_tasks_preserve_distinct_activation_times = "timing::untimed_tasks_preserve_distinct_activation_times";
    #[ignore = "the SV frontend lowers only variables as event expressions (#1123)"]
    fn untimed_task_copyback_preserves_distinct_activation_times = "timing::untimed_task_copyback_preserves_distinct_activation_times";
    #[ignore = "the SV frontend lowers only variables as event expressions (#1123)"]
    fn statically_satisfied_waits_preserve_distinct_activation_times = "timing::statically_satisfied_waits_preserve_distinct_activation_times";
    #[ignore = "the SV frontend lowers only variables as event expressions (#1123)"]
    fn nba_woken_waiters_sample_before_continuations = "timing::nba_woken_waiters_sample_before_continuations";
    fn negative_repeat_counts_do_not_run_the_body = "timing::negative_repeat_counts_do_not_run_the_body";
    #[ignore = "the SV frontend lowers only variables as event expressions (#1123)"]
    fn blocking_ff_woken_waiters_sample_before_continuations = "timing::blocking_ff_woken_waiters_sample_before_continuations";
    #[ignore = "the SV frontend lowers only variables as event expressions (#1123)"]
    fn direct_blocking_ff_woken_waiters_sample_before_continuations = "timing::direct_blocking_ff_woken_waiters_sample_before_continuations";
    fn constant_variable_initializers_do_not_invent_clock_edges = "timing::constant_variable_initializers_do_not_invent_clock_edges";
    fn repeated_active_edges_share_one_nba_batch = "timing::repeated_active_edges_share_one_nba_batch";
    #[ignore = "the SV frontend lowers only variables as event expressions (#1123)"]
    fn false_wait_preserves_separate_hierarchical_write_times = "timing::false_wait_preserves_separate_hierarchical_write_times";
    #[ignore = "the SV frontend does not lower tasks with timing controls yet (#1123)"]
    fn always_timed_task_without_inline_timing_executes = "timing::always_timed_task_without_inline_timing_executes";
    fn constant_comb_if_discards_unreachable_pulse_branch = "timing::constant_comb_if_discards_unreachable_pulse_branch";
    fn constant_child_ff_case_discards_unreachable_pulse_arm = "timing::constant_child_ff_case_discards_unreachable_pulse_arm";
    fn constant_child_loop_exit_keeps_one_write = "timing::constant_child_loop_exit_keeps_one_write";
    fn constant_child_case_exit_keeps_one_write = "timing::constant_child_case_exit_keeps_one_write";
    fn four_state_child_if_keeps_one_write = "timing::four_state_child_if_keeps_one_write";
    fn selected_child_dependency_ignores_unreachable_reads = "timing::selected_child_dependency_ignores_unreachable_reads";
    fn selected_child_loop_exit_ignores_unreachable_suffix = "timing::selected_child_loop_exit_ignores_unreachable_suffix";
    fn finite_process_loop_preserves_continuation_values = "timing::finite_process_loop_preserves_continuation_values";
    fn zero_trip_child_dependencies_ignore_body_and_step = "timing::zero_trip_child_dependencies_ignore_body_and_step";
    fn cross_file_package_initializers_keep_static_clock_baseline = "timing::cross_file_package_initializers_keep_static_clock_baseline";
    fn edge_waits_wake_on_rising_and_falling_edges = "timing::edge_waits_wake_on_rising_and_falling_edges";
    fn min_typ_max_delays_wait_the_typical_value = "timing::min_typ_max_delays_wait_the_typical_value";
    fn long_process_loops_finish_within_one_time_slot = "timing::long_process_loops_finish_within_one_time_slot";
    fn initial_blocks_run_at_time_zero = "timing::initial_blocks_run_at_time_zero";
    fn host_edges_after_time_zero_clock_flip_flops = "timing::host_edges_after_time_zero_clock_flip_flops";
}
