//! Shared timing-control cases: `#delay`, `@(event)` and `wait` in
//! processes, driven through the timed `Simulation`.
//!
//! Every exclusion names what Celox lacks for the case, so the change that
//! adds it removes the exclusion. Supported cases run on every backend,
//! and natively also from a serialized native image.

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
            fn native_image() {
                crate::suite::run_case($case, "native-image");
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

// Program blocks (IEEE 1800-2023 Clause 24) and the reactive region set.
timed_cases! {
    fn program_reads_design_after_nonblocking_updates_settle = "program::program_reads_design_after_nonblocking_updates_settle";
    fn program_nonblocking_updates_follow_design_updates = "program::program_nonblocking_updates_follow_design_updates";
    fn program_zero_delay_resumes_in_the_re_inactive_region = "program::program_zero_delay_resumes_in_the_re_inactive_region";
    fn design_activity_of_a_program_waits_for_the_reactive_region_set = "program::design_activity_of_a_program_waits_for_the_reactive_region_set";
    fn program_exit_ends_its_initial_procedures = "program::program_exit_ends_its_initial_procedures";
    fn simulation_ends_when_program_procedures_end = "program::simulation_ends_when_program_procedures_end";
    fn program_ports_connect_to_design_nets = "program::program_ports_connect_to_design_nets";
    fn programs_reject_always_procedures = "program::programs_reject_always_procedures";
    fn programs_reject_instances = "program::programs_reject_instances";
    fn program_waits_see_host_driven_edges_after_design_updates = "program::program_waits_see_host_driven_edges_after_design_updates";
}

timed_cases! {
    fn timed_task_automatic_scalar_calls_keep_separate_storage = "timing::timed_task_automatic_scalar_calls_keep_separate_storage";
    fn timed_task_four_state_storage_survives_suspension = "timing::timed_task_four_state_storage_survives_suspension";
    fn timed_task_automatic_calls_keep_separate_storage = "timing::timed_task_automatic_calls_keep_separate_storage";
    fn timed_task_nested_copyback_wait_and_return = "timing::timed_task_nested_copyback_wait_and_return";
    fn timed_task_static_formals_remain_shared = "timing::timed_task_static_formals_remain_shared";
    fn timed_task_in_edge_always_rearms_after_return = "timing::timed_task_in_edge_always_rearms_after_return";
    fn always_star_timed_task_calls_run_as_processes = "timing::always_star_timed_task_calls_run_as_processes";
    fn always_star_timed_task_calls_assign_nonblocking = "timing::always_star_timed_task_calls_assign_nonblocking";
    fn always_star_timed_task_calls_wait_on_nested_call_defaults = "timing::always_star_timed_task_calls_wait_on_nested_call_defaults";
    fn always_star_timed_task_calls_wait_on_folded_branch_reads = "timing::always_star_timed_task_calls_wait_on_folded_branch_reads";
    fn package_scoped_delay_values_wait_their_value = "timing::package_scoped_delay_values_wait_their_value";
    fn time_calls_in_child_port_subroutines = "timing::time_calls_in_child_port_subroutines";
    fn time_calls_in_child_port_actuals = "timing::time_calls_in_child_port_actuals";
    fn time_literal_delays_accept_all_units = "timing::time_literal_delays_accept_all_units";
    fn delays_resume_initial_blocks_at_later_times = "timing::delays_resume_initial_blocks_at_later_times";
    fn zero_delay_process_ends_a_pulse_after_its_edge_triggers = "timing::zero_delay_process_ends_a_pulse_after_its_edge_triggers";
    fn edge_waits_count_generated_clock_edges_in_any_declaration_order = "timing::edge_waits_count_generated_clock_edges_in_any_declaration_order";
    fn delay_process_nonblocking_updates_reach_later_edges = "timing::delay_process_nonblocking_updates_reach_later_edges";
    fn variable_delay_clock_changes_its_period = "timing::variable_delay_clock_changes_its_period";
    fn asynchronous_reset_driven_by_a_delay_process = "timing::asynchronous_reset_driven_by_a_delay_process";
    fn generated_clocks_with_a_common_edge_sample_each_other = "timing::generated_clocks_with_a_common_edge_sample_each_other";
    fn script_writes_between_runs_reach_generated_clock_domains = "timing::script_writes_between_runs_reach_generated_clock_domains";
    fn edge_waits_on_script_inputs_next_to_a_generated_clock = "timing::edge_waits_on_script_inputs_next_to_a_generated_clock";
    fn finish_from_a_delay_process_between_clock_edges = "timing::finish_from_a_delay_process_between_clock_edges";
    fn always_with_a_delay_generates_a_clock = "timing::always_with_a_delay_generates_a_clock";
    fn edge_waits_resume_before_the_registers_of_that_edge_update = "timing::edge_waits_resume_before_the_registers_of_that_edge_update";
    fn any_change_waits_wake_on_every_value_change = "timing::any_change_waits_wake_on_every_value_change";
    fn wait_statements_block_until_their_condition_holds = "timing::wait_statements_block_until_their_condition_holds";
    fn level_sensitive_always_runs_as_a_process = "timing::level_sensitive_always_runs_as_a_process";
    fn explicit_list_of_every_read_waits_for_a_change = "timing::explicit_list_of_every_read_waits_for_a_change";
    fn list_missing_a_read_does_not_wake_on_it = "timing::list_missing_a_read_does_not_wake_on_it";
    fn list_with_an_extra_signal_wakes_on_it = "timing::list_with_an_extra_signal_wakes_on_it";
    fn list_of_every_read_with_a_nonblocking_body_runs = "timing::list_of_every_read_with_a_nonblocking_body_runs";
    fn processes_wake_each_other_at_the_same_time = "timing::processes_wake_each_other_at_the_same_time";
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
    fn staged_nonblocking_updates_keep_their_order = "timing::staged_nonblocking_updates_keep_their_order";
    fn process_nonblocking_assignments_to_array_elements = "timing::process_nonblocking_assignments_to_array_elements";
    fn process_nonblocking_assignments_at_run_time_positions = "timing::process_nonblocking_assignments_at_run_time_positions";
    fn process_nonblocking_assignments_outside_their_target = "timing::process_nonblocking_assignments_outside_their_target";
    fn process_nonblocking_assignments_at_unknown_positions = "timing::process_nonblocking_assignments_at_unknown_positions";
    fn narrow_nonblocking_updates_keep_the_other_bits = "timing::narrow_nonblocking_updates_keep_the_other_bits";
    fn four_state_nonblocking_updates_keep_unknown_bits = "timing::four_state_nonblocking_updates_keep_unknown_bits";
    fn nonblocking_ff_stores_remain_pending_during_inactive_work = "timing::nonblocking_ff_stores_remain_pending_during_inactive_work";
    fn mixed_ff_stores_keep_their_active_and_nba_values = "timing::mixed_ff_stores_keep_their_active_and_nba_values";
    fn printed_finish_notice_is_design_output = "timing::printed_finish_notice_is_design_output";
    fn combinational_finish_ends_timed_execution = "timing::combinational_finish_ends_timed_execution";
    fn inactive_time_directives_do_not_change_scheduler_units = "timing::inactive_time_directives_do_not_change_scheduler_units";
    fn blocking_concatenation_publishes_one_event_update = "timing::blocking_concatenation_publishes_one_event_update";
    fn finite_active_feedback_preserves_convergence = "timing::finite_active_feedback_preserves_convergence";
    fn list_in_a_loop_through_always_comb_converges = "timing::list_in_a_loop_through_always_comb_converges";
    fn list_outside_a_loop_keeps_its_value = "timing::list_outside_a_loop_keeps_its_value";
    fn pure_function_rhs_preserves_distinct_write_times = "timing::pure_function_rhs_preserves_distinct_write_times";
    fn conditional_writes_preserve_distinct_activation_times = "timing::conditional_writes_preserve_distinct_activation_times";
    fn case_writes_preserve_distinct_activation_times = "timing::case_writes_preserve_distinct_activation_times";
    fn constant_repeat_writes_preserve_distinct_activation_times = "timing::constant_repeat_writes_preserve_distinct_activation_times";
    fn separate_comb_assignments_preserve_selected_dependencies = "timing::separate_comb_assignments_preserve_selected_dependencies";
    fn constant_for_writes_preserve_distinct_activation_times = "timing::constant_for_writes_preserve_distinct_activation_times";
    fn untimed_tasks_preserve_distinct_activation_times = "timing::untimed_tasks_preserve_distinct_activation_times";
    fn untimed_task_copyback_preserves_distinct_activation_times = "timing::untimed_task_copyback_preserves_distinct_activation_times";
    fn statically_satisfied_waits_preserve_distinct_activation_times = "timing::statically_satisfied_waits_preserve_distinct_activation_times";
    fn nba_woken_waiters_sample_before_continuations = "timing::nba_woken_waiters_sample_before_continuations";
    fn negative_repeat_counts_do_not_run_the_body = "timing::negative_repeat_counts_do_not_run_the_body";
    fn blocking_ff_woken_waiters_sample_before_continuations = "timing::blocking_ff_woken_waiters_sample_before_continuations";
    fn direct_blocking_ff_woken_waiters_sample_before_continuations = "timing::direct_blocking_ff_woken_waiters_sample_before_continuations";
    fn constant_variable_initializers_do_not_invent_clock_edges = "timing::constant_variable_initializers_do_not_invent_clock_edges";
    fn repeated_active_edges_share_one_nba_batch = "timing::repeated_active_edges_share_one_nba_batch";
    fn false_wait_preserves_separate_hierarchical_write_times = "timing::false_wait_preserves_separate_hierarchical_write_times";
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
    fn real_delays_round_at_the_scope_precision = "timing::real_delays_round_at_the_scope_precision";
    fn negative_real_delays_wait_their_twos_complement = "timing::negative_real_delays_wait_their_twos_complement";
    fn expression_events_see_pulses_within_a_time_slot = "timing::expression_events_see_pulses_within_a_time_slot";
    fn real_expression_events_see_fractional_changes = "timing::real_expression_events_see_fractional_changes";
    fn bit_select_edges_follow_the_selected_bit = "timing::bit_select_edges_follow_the_selected_bit";
    fn part_select_events_see_pulses_of_the_selected_bits = "timing::part_select_events_see_pulses_of_the_selected_bits";
    fn variable_index_events_follow_the_index = "timing::variable_index_events_follow_the_index";
    fn pure_function_events_change_with_the_result = "timing::pure_function_events_change_with_the_result";
    fn event_expressions_read_automatic_task_locals = "timing::event_expressions_read_automatic_task_locals";
    fn event_controls_sample_the_expression_when_they_begin = "timing::event_controls_sample_the_expression_when_they_begin";
    fn implicit_event_controls_in_processes_wait_on_their_reads = "timing::implicit_event_controls_in_processes_wait_on_their_reads";
    fn intermediate_combinational_writes_wake_waiters = "timing::intermediate_combinational_writes_wake_waiters";
    fn intermediate_edge_process_writes_wake_waiters = "timing::intermediate_edge_process_writes_wake_waiters";
    fn staged_nonblocking_writes_wake_waiters = "timing::staged_nonblocking_writes_wake_waiters";
    fn child_port_pulses_reach_waiters_settled = "timing::child_port_pulses_reach_waiters_settled";
    fn intermediate_combinational_writes_keep_other_drivers = "timing::intermediate_combinational_writes_keep_other_drivers";
    fn parallel_flip_flop_parts_keep_pulses = "timing::parallel_flip_flop_parts_keep_pulses";
    fn staged_nonblocking_writes_follow_later_blocking_writes = "timing::staged_nonblocking_writes_follow_later_blocking_writes";
    fn intermediate_combinational_writes_replace_split_definitions = "timing::intermediate_combinational_writes_replace_split_definitions";
    fn intermediate_combinational_writes_reach_part_select_waiters_settled = "timing::intermediate_combinational_writes_reach_part_select_waiters_settled";
    fn intermediate_combinational_writes_reach_concatenated_ports = "timing::intermediate_combinational_writes_reach_concatenated_ports";
    fn blocking_write_waiters_run_before_nonblocking_updates = "timing::blocking_write_waiters_run_before_nonblocking_updates";
    fn blocking_write_waiters_rearm_before_nonblocking_updates = "timing::blocking_write_waiters_rearm_before_nonblocking_updates";
    fn blocking_write_edges_read_values_before_nonblocking_updates = "timing::blocking_write_edges_read_values_before_nonblocking_updates";
    fn plain_always_expression_edges_run_as_processes = "timing::plain_always_expression_edges_run_as_processes";
    fn plain_always_multi_bit_edges_follow_the_lsb = "timing::plain_always_multi_bit_edges_follow_the_lsb";
    fn plain_always_edges_without_a_reset_run_as_processes = "timing::plain_always_edges_without_a_reset_run_as_processes";
    fn derived_clocks_follow_several_nonblocking_updates = "timing::derived_clocks_follow_several_nonblocking_updates";
    fn derived_clocks_follow_active_region_commits = "timing::derived_clocks_follow_active_region_commits";
    fn derived_clocks_follow_superseded_publications = "timing::derived_clocks_follow_superseded_publications";
}

// A second list: one invocation expands one item per recursion level.
timed_cases! {
    fn iff_events_occur_only_while_the_condition_holds = "timing::iff_events_occur_only_while_the_condition_holds";
    fn iff_conditions_see_writes_before_the_event = "timing::iff_conditions_see_writes_before_the_event";
    fn iff_conditions_apply_to_their_own_items = "timing::iff_conditions_apply_to_their_own_items";
    fn iff_conditions_read_automatic_task_arguments = "timing::iff_conditions_read_automatic_task_arguments";
    fn unknown_iff_conditions_are_false = "timing::unknown_iff_conditions_are_false";
    fn plain_always_iff_events_run_as_processes = "timing::plain_always_iff_events_run_as_processes";
    fn iff_conditions_are_judged_before_edge_processes_run = "timing::iff_conditions_are_judged_before_edge_processes_run";
    fn iff_conditions_are_judged_at_the_write = "timing::iff_conditions_are_judged_at_the_write";
    fn iff_conditions_follow_nonblocking_update_order = "timing::iff_conditions_follow_nonblocking_update_order";
    fn iff_conditions_consume_occurrences_while_false = "timing::iff_conditions_consume_occurrences_while_false";
    fn iff_conditions_preserve_cross_module_combinational_order = "timing::iff_conditions_preserve_cross_module_combinational_order";
    fn iff_shared_automatic_gate_uses_edge_time_value = "timing::iff_shared_automatic_gate_uses_edge_time_value";
    fn iff_forked_automatic_gates_are_separate = "timing::iff_forked_automatic_gates_are_separate";
    fn iff_conditions_follow_combinational_process_order = "timing::iff_conditions_follow_combinational_process_order";
}

#[test]
fn timed_task_activations_survive_each_optimization_level() {
    let source = r#"
        module Top(output int a, b);
            timeunit 1ps / 1ps;
            task automatic pulse(input int d, input int v, output int r);
                int saved[2];
                saved[0] = v;
                saved[1] = v + 1;
                #d;
                r = saved[0] + saved[1];
            endtask
            initial begin a = 0; pulse(5, 10, a); end
            initial begin b = 0; #1; pulse(2, 20, b); end
        endmodule
    "#;
    for level in [
        celox::OptLevel::O0,
        celox::OptLevel::O1,
        celox::OptLevel::O2,
    ] {
        for four_state in [false, true] {
            let mut simulation = celox::Simulation::from_sv_sources(
                vec![(source, std::path::Path::new("optimized_task.sv"))],
                "Top",
            )
            .opt_level(level)
            .four_state(four_state)
            .build()
            .unwrap();
            let a = simulation.signal("a");
            let b = simulation.signal("b");
            simulation.run_until(3).unwrap();
            assert_eq!(
                simulation.get(a),
                0u32.into(),
                "{level:?} four_state={four_state}"
            );
            assert_eq!(
                simulation.get(b),
                41u32.into(),
                "{level:?} four_state={four_state}"
            );
            simulation.run_until(5).unwrap();
            assert_eq!(
                simulation.get(a),
                21u32.into(),
                "{level:?} four_state={four_state}"
            );
        }
    }
}

#[test]
fn time_literal_delays_round_at_scope_precision() {
    let source = r#"
        module Fine(output bit done);
            timeunit 1ns / 1ps;
            initial begin #2.75ns; done = 1; end
        endmodule
        module Coarse(output bit done);
            timeunit 1ns / 100ps;
            initial begin #(2.75ns); done = 1; end
        endmodule
        module Top(output bit fine, coarse);
            timeunit 1ns / 1ps;
            Fine f(fine); Coarse c(coarse);
        endmodule
    "#;
    let mut sim = celox::Simulation::from_sv_sources(
        vec![(source, std::path::Path::new("scope_precision.sv"))],
        "Top",
    )
    .build()
    .unwrap();
    let fine = sim.signal("fine");
    let coarse = sim.signal("coarse");
    sim.run_until(2749).unwrap();
    assert_eq!(sim.get(fine), 0u8.into());
    assert_eq!(sim.get(coarse), 0u8.into());
    sim.run_until(2750).unwrap();
    assert_eq!(sim.get(fine), 1u8.into());
    assert_eq!(sim.get(coarse), 0u8.into());
    sim.run_until(2799).unwrap();
    assert_eq!(sim.get(coarse), 0u8.into());
    sim.run_until(2800).unwrap();
    assert_eq!(sim.get(coarse), 1u8.into());
}

/// A real delay is rounded at its scope's precision with ties away from
/// zero (IEEE 1800-2023 3.14.1, 6.12.1): 2.7505 ns waits 2751 ps at 1 ps,
/// and 2.75 ns waits 2.8 ns at 100 ps.
#[test]
fn real_delays_round_at_scope_precision() {
    let source = r#"
        module Fine(output int stage);
            timeunit 1ns / 1ps;
            real r = 2.7505;
            initial begin #r stage = 1; #1.5 stage = 2; end
        endmodule
        module Coarse(output int stage);
            timeunit 1ns / 100ps;
            real r = 2.75;
            shortreal half = 0.05;
            initial begin #r stage = 1; #1.25 stage = 2; #half stage = 3; end
        endmodule
        module Top(output int fine, coarse);
            timeunit 1ns / 1ps;
            Fine f(fine); Coarse c(coarse);
        endmodule
    "#;
    for level in [celox::OptLevel::O0, celox::OptLevel::O1] {
        let mut sim = celox::Simulation::from_sv_sources(
            vec![(source, std::path::Path::new("real_delays.sv"))],
            "Top",
        )
        .opt_level(level)
        .build()
        .unwrap();
        let fine = sim.signal("fine");
        let coarse = sim.signal("coarse");
        // (time in ps, fine stage, coarse stage)
        for (time, f, c) in [
            (2750u64, 0u32, 0u32),
            (2751, 1, 0),
            (2799, 1, 0),
            (2800, 1, 1),
            (4099, 1, 1),
            (4100, 1, 2),
            (4199, 1, 2),
            (4200, 1, 3),
            (4250, 1, 3),
            (4251, 2, 3),
        ] {
            sim.run_until(time).unwrap();
            assert_eq!(sim.get(fine), f.into(), "{level:?} fine at {time}");
            assert_eq!(sim.get(coarse), c.into(), "{level:?} coarse at {time}");
        }
    }
}

/// The variables of an event expression, `$event[k]` and its copies of
/// process-local operands, are state like the process slots: a state file
/// keeps them, while the signal listing and the waveform leave them out.
#[test]
fn event_expression_variables_are_hidden_state() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.vcd");
    let design = |dump: &str| {
        format!(
            r#"
            module Top(output bit [7:0] count);
                bit a;
                task automatic await(input bit i); @(a ^ i); endtask
                {dump}
                initial forever begin await(1); count = count + 8'd1; end
                initial begin #1; a = 1; end
            endmodule
        "#
        )
    };
    let traced = design(&format!(
        r#"initial begin $dumpfile("{}"); $dumpvars(0, Top); end"#,
        path.display()
    ));
    let mut simulation = celox::Simulation::from_sv_sources(
        vec![(traced.as_str(), std::path::Path::new("events.sv"))],
        "Top",
    )
    .build()
    .unwrap();
    simulation.run_until(2000).unwrap();
    assert_eq!(simulation.get(simulation.signal("count")), 1u8.into());
    simulation.flush_vcd().unwrap();
    let waveform = std::fs::read_to_string(&path).unwrap();
    assert!(waveform.contains(" count $end"), "{waveform}");
    assert!(!waveform.contains("$event"), "{waveform}");

    let plain = design("");
    let mut simulation = celox::Simulation::from_sv_sources(
        vec![(plain.as_str(), std::path::Path::new("events.sv"))],
        "Top",
    )
    .build()
    .unwrap();
    simulation.run_until(2000).unwrap();
    assert!(
        simulation
            .named_signals()
            .iter()
            .all(|signal| !signal.name.starts_with('$')),
        "{:?}",
        simulation
            .named_signals()
            .iter()
            .map(|signal| &signal.name)
            .collect::<Vec<_>>()
    );
    let state = simulation.save_state().unwrap();
    // A store of the process does not make the variable a counted signal.
    assert!(
        !state
            .objects
            .iter()
            .any(|object| object.path.starts_with("$counter[$event")),
        "{:?}",
        state.objects
    );
    for name in ["$event[0]", "$event[0].arg[0]", "$initial[0].status"] {
        assert!(
            state
                .objects
                .iter()
                .any(|object| object.path == name && object.role == celox::StateRole::State),
            "{name}: {:?}",
            state.objects
        );
    }
}

#[test]
fn event_expressions_with_effects_are_rejected() {
    for (event, construct) in [
        (
            "count_up(a)",
            "impure call `count_up` in an event expression",
        ),
        ("$time", "impure call `$time` in an event expression"),
        ("a ^ x", "event expression reading a static task argument"),
    ] {
        let source = format!(
            r#"
            module Top;
                bit a;
                int calls;
                function automatic bit count_up(input bit v); calls++; return v; endfunction
                task t(input bit x); @({event}); endtask
                initial t(0);
            endmodule
        "#
        );
        let error = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), std::path::Path::new("effects.sv"))],
            "Top",
        )
        .build()
        .err()
        .unwrap_or_else(|| panic!("`@({event})` must be rejected"))
        .to_string();
        assert!(error.contains(construct), "{event}: {error}");
    }
}

/// Timing features Celox does not implement yet are rejected with a message
/// naming the construct and its tracking issue, instead of being ignored.
/// The comments name the issue that implements a feature when the message
/// still points to the roadmap.
#[test]
fn unimplemented_timing_features_are_rejected() {
    for (body, construct, issue) in [
        ("event e; initial -> e;", "named event", 88),
        (
            "bit a, b; initial a = #1 b;",
            "intra-assignment timing control",
            88,
        ),
        (
            "bit a, b; initial a <= #1 b;",
            "intra-assignment timing control",
            88,
        ),
        (
            "bit a, b, c; initial a = @(posedge c) b;",
            "intra-assignment timing control",
            88,
        ),
        (
            "bit a; task t(input bit x); wait (a ^ x); endtask initial t(0);",
            "wait condition reading a static task argument",
            88,
        ),
        (
            "bit a; task t(input bit x); @(a ^ x); endtask initial t(0);",
            "event expression reading a static task argument",
            88,
        ),
        (
            "bit a; function automatic bit f(); a = ~a; return a; endfunction initial @(f());",
            "impure call `f` in an event expression",
            88,
        ),
        (
            "bit q; task t(); int s = 1; #1 q = s; endtask initial t();",
            "static local in a timed task",
            88,
        ),
        (
            "bit a, y; assign #1 y = a;",
            "delayed continuous assignment",
            444,
        ),
        // A plain `always` of these forms runs as a process.
        (
            "bit [3:0] v; int n; always_ff @(posedge v[0]) n <= n + 1;",
            "always_ff event expression",
            464,
        ),
        (
            "bit [3:0] v; int n; always_ff @(posedge v) n <= n + 1;",
            "multi-bit always_ff event signal",
            88,
        ),
        (
            "bit a, b; int n; always_ff @(posedge a or posedge b) n <= n + 1;",
            "always_ff event control",
            88,
        ),
    ] {
        let source = format!("module Top; {body} endmodule");
        let error = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), std::path::Path::new("unimplemented.sv"))],
            "Top",
        )
        .build()
        .err()
        .unwrap_or_else(|| panic!("`{body}` must be rejected"))
        .to_string();
        // The report wraps long lines behind a `│` gutter.
        let error = error
            .split(|c: char| c.is_whitespace() || c == '│')
            .filter(|word| !word.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            error.contains(&format!("[tracking issue #{issue}]"))
                && error.contains(&format!("Unsupported SystemVerilog construct: {construct}")),
            "{body}: {error}"
        );
    }
}

/// A real has no least significant bit, so no edge event control applies to
/// one (IEEE 1800-2023 6.12); a change of its value is an event.
#[test]
fn edge_events_of_reals_are_errors() {
    for (body, edge) in [
        ("real r; initial @(posedge r);", "posedge"),
        ("real r; int n; always @(negedge r) n++;", "negedge"),
        ("real r; initial @(edge (r * 2.0));", "edge"),
        (
            "shortreal r; bit c; int n; always @(posedge c or posedge r) n++;",
            "posedge",
        ),
    ] {
        let source = format!("module Top; {body} endmodule");
        let error = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), std::path::Path::new("real_edge.sv"))],
            "Top",
        )
        .build()
        .err()
        .unwrap_or_else(|| panic!("`{body}` must be rejected"))
        .to_string();
        assert!(
            error.contains(&format!("`{edge}` of a real expression")),
            "{body}: {error}"
        );
    }
    let source = "module Top; real r; initial @(r); endmodule";
    celox::Simulation::from_sv_sources(vec![(source, std::path::Path::new("real.sv"))], "Top")
        .build()
        .expect("a change of a real is an event");
}

/// Only `initial` and `always` processes may suspend (IEEE 1800-2023
/// 9.2.2.2, 9.2.2.4 and 13.4); a timing control elsewhere is an error.
#[test]
fn timing_controls_outside_suspendable_processes_are_rejected() {
    for (body, construct) in [
        (
            "bit a, y; always_comb begin #1 y = a; end",
            "delay control in an always_comb or always_ff body",
        ),
        (
            "bit clk, a, q; always_ff @(posedge clk) begin @(a); q <= a; end",
            "event control in an always_comb or always_ff body",
        ),
        (
            "bit a; function automatic bit f(); #1; return a; endfunction",
            "delay control in a function",
        ),
        (
            "bit q; task t(); #1 q = 1; endtask always_comb t();",
            "timed task call in an always_comb body",
        ),
        (
            "bit q, e; task t(); #1 q = 1; endtask always_latch if (e) t();",
            "timed task call in an always_latch body",
        ),
    ] {
        let source = format!("module Top; {body} endmodule");
        let error = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), std::path::Path::new("outside.sv"))],
            "Top",
        )
        .build()
        .err()
        .unwrap_or_else(|| panic!("`{body}` must be rejected"))
        .to_string();
        assert!(error.contains(construct), "{body}: {error}");
    }
}

/// The default of an omitted task argument is read by the call, so an
/// `always @*` that calls the task waits on what the default reads, as for a
/// combinational `always @*`. Icarus 13.0 agrees; Verilator 5.052 does not
/// wake the process.
#[test]
fn always_star_timed_task_calls_wait_on_omitted_argument_defaults() {
    let source = r#"
        module Top(output logic [7:0] y);
            timeunit 1ns / 1ns;
            logic [7:0] a = 8'd0;
            logic [7:0] y_r = 8'd0;
            task automatic delayed(input logic [7:0] v = a);
                #1 y_r = v;
            endtask
            always @* delayed();
            initial #2 a = 8'd5;
            assign y = y_r;
        endmodule
    "#;
    let mut sim = celox::Simulation::from_sv_sources(
        vec![(source, std::path::Path::new("star_default.sv"))],
        "Top",
    )
    .build()
    .unwrap();
    sim.run_until(2).unwrap();
    assert_eq!(sim.get(sim.signal("y")), 0u8.into());
    sim.run_until(4).unwrap();
    assert_eq!(sim.get(sim.signal("y")), 5u8.into());
}

/// An `always @*` that is still in a timed task when a variable of its event
/// list changes misses the change: its event control waits only for changes
/// after it is reached again (IEEE 1800-2023 9.4.2). Icarus 13.0 agrees;
/// Verilator 5.052 reruns the call for the missed change.
#[test]
fn always_star_timed_task_calls_miss_changes_while_suspended() {
    let source = r#"
        module Top(output logic [7:0] y);
            timeunit 1ns / 1ns;
            logic [7:0] a = 8'd0;
            logic [7:0] y_r = 8'd0;
            task automatic delayed(input logic [7:0] v);
                #2 y_r = v + 8'd1;
            endtask
            always @* delayed(a);
            initial begin
                #1 a = 8'd5;
                #1 a = 8'd6;
                #5 a = 8'd7;
            end
            assign y = y_r;
        endmodule
    "#;
    let mut sim = celox::Simulation::from_sv_sources(
        vec![(source, std::path::Path::new("star_missed.sv"))],
        "Top",
    )
    .build()
    .unwrap();
    sim.run_until(6).unwrap();
    assert_eq!(
        sim.get(sim.signal("y")),
        6u8.into(),
        "a = 6 came while suspended"
    );
    sim.run_until(10).unwrap();
    assert_eq!(sim.get(sim.signal("y")), 8u8.into());
}

/// A nonblocking site stages its value (`NbaSite::pending`) only if its
/// target has no other writer and nothing observes the order of the
/// updates in the NBA region: no wait reads the target, directly or through
/// combinational logic, including the `$event` variable of an expression
/// event. Every other site keeps the ordered queue.
#[test]
fn nonblocking_sites_stage_only_unobserved_targets_with_one_writer() {
    let source = r#"
        module Top(output logic [7:0] q, output bit seen_wait, seen_comb);
            timeunit 1ps / 1ps;
            logic [7:0] stim = 8'd0, blocking = 8'd0, r = 8'd0;
            logic a = 1'b0, b = 1'b0, d = 1'b0, e = 1'b0, s = 1'b0, en = 1'b1, clk = 1'b0;
            logic [7:0] hits = 8'd0;
            wire c = d & ~e;
            always #5 clk = ~clk;
            initial forever @(posedge clk) stim <= stim + 8'd3;
            always_ff @(posedge clk) r <= stim;
            initial begin #2; a <= 1'b1; b <= 1'b1; d <= 1'b1; e <= 1'b1; end
            initial begin #10; b <= 1'b1; e <= 1'b1; end
            initial begin wait (a && !b); seen_wait = 1'b1; end
            initial begin @(posedge c); seen_comb = 1'b1; end
            initial forever @(posedge clk) s <= ~s;
            initial forever @(posedge (s & en)) hits = hits + 8'd1;
            initial begin #1; blocking <= 8'd1; #1 blocking = 8'd2; end
            initial begin #1; q <= stim; end
        endmodule
    "#;
    let simulation = celox::Simulation::from_sv_sources(
        vec![(source, std::path::Path::new("staged_sites.sv"))],
        "Top",
    )
    .build()
    .unwrap();
    let program = simulation.program();
    let mut staged: Vec<_> = ["stim", "blocking", "a", "b", "d", "e", "s", "q"]
        .into_iter()
        .filter(|name| {
            let target = program.get_addr(&[], &[name]).unwrap();
            program
                .runtime_schema
                .nba_sites
                .iter()
                .filter(|site| site.target == target)
                .all(|site| site.pending.is_some())
        })
        .collect();
    staged.sort_unstable();
    assert_eq!(staged, ["stim"]);

    // The observed sites keep the ordered queue, and behave as before any
    // site could stage: Celox evaluates the waits after the region's
    // updates, so neither sees `a && !b` or `c` hold between two of them.
    let mut sim = celox::Simulation::from_sv_sources(
        vec![(source, std::path::Path::new("staged_sites.sv"))],
        "Top",
    )
    .build()
    .unwrap();
    sim.run_until(26).unwrap();
    assert_eq!(sim.get(sim.signal("seen_wait")), 0u8.into());
    assert_eq!(sim.get(sim.signal("seen_comb")), 0u8.into());
    assert_eq!(sim.get(sim.signal("q")), 0u8.into());
    assert_eq!(sim.get(sim.signal("stim")), 9u8.into());
    assert_eq!(sim.get(sim.signal("r")), 6u8.into());
    assert_eq!(sim.get(sim.signal("hits")), 2u8.into());
}

/// The narrow updates case covers staged and queued sites alike.
#[test]
fn narrow_nonblocking_updates_cover_staged_and_queued_sites() {
    let design = celox_test_suite::sv::cases()
        .find(|case| case.name == "timing::narrow_nonblocking_updates_keep_the_other_bits")
        .unwrap()
        .design();
    let sources = design
        .sources
        .iter()
        .map(|source| (source.text.as_str(), source.path.as_path()))
        .collect::<Vec<_>>();
    let simulation = celox::Simulation::from_sv_sources(sources, &design.top)
        .build()
        .unwrap();
    let program = simulation.program();
    let staged = |name: &str| {
        let target = program.get_addr(&[], &[name]).unwrap();
        let sites = program
            .runtime_schema
            .nba_sites
            .iter()
            .filter(|site| site.target == target)
            .collect::<Vec<_>>();
        assert!(!sites.is_empty(), "{name}");
        sites.iter().all(|site| site.pending.is_some())
    };
    for name in ["p", "w", "x"] {
        assert!(staged(name), "{name} stages its update");
    }
    for name in ["q", "v", "y"] {
        assert!(!staged(name), "{name} queues its updates");
    }
}

sv_backends! {
    fn static_function_storage_is_shared_between_processes(sim) {
        @case "timing::static_function_storage_is_shared_between_processes";
    }
}

sv_backends! {
    fn static_function_readonly_initializer_calls_execute_once(sim) {
        @case "timing::static_function_readonly_initializer_calls_execute_once";
    }
}

sv_backends! {
    fn edge_nonblocking_updates_keep_later_writes_of_other_bits(sim) {
        @case "timing::edge_nonblocking_updates_keep_later_writes_of_other_bits";
    }

    fn edge_nonblocking_updates_follow_writes_their_blocking_results_wake(sim) {
        @case "timing::edge_nonblocking_updates_follow_writes_their_blocking_results_wake";
    }

    fn edge_blocking_results_precede_later_process_writes(sim) {
        @case "timing::edge_blocking_results_precede_later_process_writes";
    }
}

/// An edge whose evaluation a `$fatal` ends before its commit leaves no
/// held write of a watched variable behind: when the host continues, the
/// later edges rewrite `y[0]` to its own value and wake no waiter.
#[test]
fn runtime_error_before_a_commit_leaves_no_held_writes() {
    let source = r#"
        module Top(output bit [1:0] y, output int n, r, f);
            timeunit 1ns / 1ns;
            bit clk, bad = 1;
            initial y = 2'b01;
            always_ff @(posedge clk) begin
                if (bad) begin y[0] <= 0; $fatal(1, "bad edge"); end
                else begin y[0] <= 1; y[0] <= 1; end
            end
            initial #2 forever begin @(y); n++; end
            initial #2 forever begin @(posedge y); r++; end
            initial #2 forever begin @(negedge y); f++; end
            initial begin
                #1 clk = 1;
                #1 clk = 0; bad = 0;
                #1 clk = 1;
                #1 clk = 0;
                #1 clk = 1;
            end
        endmodule
    "#;
    let mut backends = vec!["cranelift", "cranelift-parallel"];
    if cfg!(any(target_arch = "x86_64", target_arch = "aarch64")) {
        backends.extend(["native", "native-parallel"]);
    }
    for backend in backends {
        let builder = celox::Simulation::from_sv_sources(
            vec![(source, std::path::Path::new("held_after_error.sv"))],
            "Top",
        );
        let builder = if backend.ends_with("-parallel") {
            builder
                .threads(4)
                .parallel_partition(celox::ParallelPartition::Always)
        } else {
            builder
        };
        // The backends build different simulation types.
        macro_rules! check {
            ($sim:expr) => {{
                let mut sim = $sim.unwrap();
                let [y, n, r, f] = ["y", "n", "r", "f"].map(|name| sim.signal(name));
                assert!(
                    sim.run_until(1).is_err(),
                    "{backend}: the first edge reports $fatal"
                );
                sim.run_until(10).unwrap();
                assert_eq!(sim.get(y), 1u8.into(), "{backend}");
                for (signal, label) in [(n, "n"), (r, "r"), (f, "f")] {
                    assert_eq!(sim.get(signal), 0u32.into(), "{backend}: {label}");
                }
            }};
        }
        if backend.starts_with("native") {
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check!(builder.build_native());
        } else {
            check!(builder.build_cranelift());
        }
    }
}

/// The baselines of waits on a counted variable survive a state file: a
/// simulation loaded between two pulses of `y` wakes the waiters as the
/// uninterrupted one does, and saves the same baselines again.
#[test]
fn counted_wait_baselines_survive_a_state_file() {
    let source = r#"
        module Top(output int n, r);
            timeunit 1ns / 1ns;
            bit a, b, c;
            logic y;
            always_comb begin y = 0; if (a) y = 1; c = b; end
            initial #1 forever begin @(y); n++; end
            initial #1 forever begin @(posedge y); r++; end
            initial begin
                #2 a = 1;
                #1 b = 1;
                #1 b = 0;
                #1 b = 1;
                #1 a = 0;
            end
        endmodule
    "#;
    let build = |four_state: bool| {
        celox::Simulation::from_sv_sources(
            vec![(source, std::path::Path::new("counted_state.sv"))],
            "Top",
        )
        .four_state(four_state)
        .build()
        .unwrap()
    };
    let through_bytes = |file: &celox::StateFile| {
        let mut bytes = Vec::new();
        file.write_to(&mut bytes).unwrap();
        celox::StateFile::read_from(bytes.as_slice()).unwrap()
    };
    for four_state in [false, true] {
        let counts = |sim: &mut celox::Simulation| {
            ["n", "r"].map(|name| {
                let signal = sim.signal(name);
                sim.get(signal)
            })
        };
        let mut whole = build(four_state);
        whole.run_until(10).unwrap();
        let expected = counts(&mut whole);
        assert_eq!(
            expected,
            [5u32.into(), 4u32.into()],
            "four_state={four_state}"
        );

        let mut first = build(four_state);
        first.run_until(3).unwrap();
        let file = through_bytes(&first.save_state().unwrap());
        assert!(
            file.schedule
                .as_ref()
                .unwrap()
                .processes
                .waiters
                .iter()
                .flat_map(|waiter| &waiter.baselines)
                .any(|baseline| matches!(baseline, celox::BaselineRecord::Counter { .. })),
            "four_state={four_state}: `y` is counted"
        );
        let mut loaded = build(four_state);
        loaded.load_state(&file).unwrap();
        assert_eq!(
            through_bytes(&loaded.save_state().unwrap()),
            file,
            "four_state={four_state}"
        );
        loaded.run_until(10).unwrap();
        assert_eq!(counts(&mut loaded), expected, "four_state={four_state}");
        // A baseline taken at a check in this run saves as the loaded one.
        first.run_until(4).unwrap();
        loaded = build(four_state);
        let file = through_bytes(&first.save_state().unwrap());
        loaded.load_state(&file).unwrap();
        loaded.run_until(10).unwrap();
        assert_eq!(counts(&mut loaded), expected, "four_state={four_state}");
    }
}

/// The run-time position cases cover staged and queued sites alike.
#[test]
fn run_time_nonblocking_positions_cover_staged_and_queued_sites() {
    for (case, staged, queued) in [
        (
            "timing::process_nonblocking_assignments_at_run_time_positions",
            &["r", "p"][..],
            &["q"][..],
        ),
        (
            "timing::process_nonblocking_assignments_to_array_elements",
            &[][..],
            &["m", "grid", "mem"][..],
        ),
    ] {
        let design = celox_test_suite::sv::cases()
            .find(|candidate| candidate.name == case)
            .unwrap()
            .design();
        let sources = design
            .sources
            .iter()
            .map(|source| (source.text.as_str(), source.path.as_path()))
            .collect::<Vec<_>>();
        let simulation = celox::Simulation::from_sv_sources(sources, &design.top)
            .build()
            .unwrap();
        let program = simulation.program();
        let sites = |name: &str| {
            let target = program.get_addr(&[], &[name]).unwrap();
            let sites = program
                .runtime_schema
                .nba_sites
                .iter()
                .filter(|site| site.target == target)
                .collect::<Vec<_>>();
            assert!(
                sites.iter().any(|site| site.position.is_some()),
                "{name} has a run-time position"
            );
            sites
        };
        for name in staged {
            assert!(
                sites(name).iter().all(|site| site.pending.is_some()),
                "{name} stages its updates"
            );
        }
        for name in queued {
            assert!(
                sites(name).iter().all(|site| site.pending.is_none()),
                "{name} queues its updates"
            );
        }
    }
}

/// A nonblocking assignment to a slice of an unpacked array whose range
/// runs against the array's takes the elements in reverse order, as a
/// blocking one does (IEEE 1800-2023 7.6). Neither Icarus nor Verilator
/// compiles the slice.
#[test]
fn nonblocking_assignments_to_reversed_array_slices_reverse_the_elements() {
    let source = r#"
        module Top(output logic [31:0] elements);
            timeunit 1ps / 1ps;
            logic [7:0] m [4];
            logic [7:0] w [2];
            assign elements = {m[3], m[2], m[1], m[0]};
            initial begin
                m[0] = 8'd1; m[1] = 8'd2; m[2] = 8'd3; m[3] = 8'd4;
                w[0] = 8'haa; w[1] = 8'hbb;
                #1;
                m[1:0] <= w;
                m[3:2] = w;
            end
        endmodule
    "#;
    let mut sim = celox::Simulation::from_sv_sources(
        vec![(source, std::path::Path::new("reversed_slice.sv"))],
        "Top",
    )
    .build()
    .unwrap();
    sim.run_until(2).unwrap();
    // m[1:0] <= w writes w[0] to m[1], as m[3:2] = w writes w[0] to m[3].
    assert_eq!(sim.get(sim.signal("elements")), 0xaabb_aabbu32.into());
}

/// A program that executes `$stop` leaves its other procedures of the time
/// slot in the reactive region set (IEEE 1800-2023 24.3.1, 20.2). The
/// simulation cannot be saved there; continued, the slot runs its Re-Inactive
/// and Re-NBA regions and then the design activity they create (4.5), which
/// leaves the reactive region set empty, so a state file saved then resumes as
/// the uninterrupted simulation does.
#[test]
fn programs_continue_after_a_stop_and_survive_a_state_file() {
    let source = r#"
        module Top(output bit [7:0] a, b, c);
            timeunit 1ns / 1ns;
            bit go;
            always @(posedge go) c <= c + 8'd1;
            Tb t(a, b, go);
        endmodule
        program Tb(output bit [7:0] a, b, output bit go);
            timeunit 1ns / 1ns;
            initial begin #1 $stop; a = 1; #0 a = 2; b <= 3; go = 1; #2 go = 0; #1 go = 1; end
            initial begin #1 b = 4; #0 b = 5; end
            // Keeps the program running past the stop.
            initial forever #1;
        endprogram
    "#;
    let build = |four_state: bool| {
        celox::Simulation::from_sv_sources(
            vec![(source, std::path::Path::new("reactive_state.sv"))],
            "Top",
        )
        .four_state(four_state)
        .build()
        .unwrap()
    };
    let through_bytes = |file: &celox::StateFile| {
        let mut bytes = Vec::new();
        file.write_to(&mut bytes).unwrap();
        celox::StateFile::read_from(bytes.as_slice()).unwrap()
    };
    let values = |sim: &mut celox::Simulation| {
        ["a", "b", "c"].map(|name| {
            let signal = sim.signal(name);
            sim.get(signal)
        })
    };
    for four_state in [false, true] {
        let mut whole = build(four_state);
        whole.run_until(10).unwrap();
        assert!(whole.is_stopped(), "four_state={four_state}");
        assert!(whole.save_state().is_err(), "four_state={four_state}");
        whole.run_until(1).unwrap();
        // The Re-NBA update `b <= 3` follows `#0 b = 5`, and the flip-flop the
        // program clocks runs after both.
        assert_eq!(
            values(&mut whole),
            [2u8.into(), 3u8.into(), 1u8.into()],
            "four_state={four_state}"
        );
        let file = through_bytes(&whole.save_state().unwrap());
        let mut loaded = build(four_state);
        loaded.load_state(&file).unwrap();
        assert_eq!(
            through_bytes(&loaded.save_state().unwrap()),
            file,
            "four_state={four_state}"
        );
        whole.run_until(10).unwrap();
        loaded.run_until(10).unwrap();
        let expected = values(&mut whole);
        assert_eq!(expected[2], 2u8.into(), "four_state={four_state}");
        assert_eq!(values(&mut loaded), expected, "four_state={four_state}");
    }
}

/// A design process that a program's write wakes runs in the active region
/// set with the flip-flop that write clocks, before the nonblocking update
/// (IEEE 1800-2023 4.5): it reads the flip-flop's old value, as Icarus 13.0,
/// Verilator 5.052 and xezim 0.11.3 do.
#[test]
fn design_processes_a_program_wakes_run_before_the_nonblocking_updates() {
    let source = r#"
        module Top(output logic [7:0] seen, q);
            timeunit 1ns / 1ns;
            wire go;
            initial q = 0;
            always_ff @(posedge go) q <= q + 8'd1;
            always @(posedge go) seen = q;
            P p(go);
        endmodule
        program P(output logic go);
            timeunit 1ns / 1ns;
            initial begin go = 0; #1 go = 1; #10; end
        endprogram
    "#;
    for four_state in [false, true] {
        let mut sim = celox::Simulation::from_sv_sources(
            vec![(source, std::path::Path::new("program_wakes.sv"))],
            "Top",
        )
        .four_state(four_state)
        .build()
        .unwrap();
        sim.run_until(5).unwrap();
        let values = ["seen", "q"].map(|name| {
            let signal = sim.signal(name);
            sim.get(signal)
        });
        assert_eq!(values, [0u8.into(), 1u8.into()], "four_state={four_state}");
    }
}

/// An `iff` wait on a clock only the scheduler drives: the occurrence is
/// judged with the condition after the edge settles, on every backend.
#[test]
fn iff_waits_on_scheduled_clocks_see_their_condition() {
    let source = r#"
        module Top(input bit clk, output int n, m);
            timeunit 1ns / 1ns;
            bit en, q;
            wire gate = en & 1'b1;
            always_ff @(posedge clk) q <= ~q;
            initial forever begin @(posedge clk iff gate); n++; end
            initial forever begin @(posedge clk); m++; end
            initial begin
                #25 en = 1;
                #40 en = 0;
            end
        endmodule
    "#;
    let mut backends = vec!["cranelift", "cranelift-parallel"];
    if cfg!(any(target_arch = "x86_64", target_arch = "aarch64")) {
        backends.extend(["native", "native-parallel"]);
    }
    for backend in backends {
        let builder = celox::Simulation::from_sv_sources(
            vec![(source, std::path::Path::new("iff_clock.sv"))],
            "Top",
        );
        let builder = if backend.ends_with("-parallel") {
            builder
                .threads(4)
                .parallel_partition(celox::ParallelPartition::Always)
        } else {
            builder
        };
        // The backends build different simulation types.
        macro_rules! check {
            ($sim:expr) => {{
                let mut sim = $sim.unwrap();
                // Rising edges at 0, 10, ..., 90; the waits begin after the
                // first. `en` is 1 from 25 to 65.
                sim.add_clock("clk", 10, 0);
                sim.run_until(95).unwrap();
                let [n, m] = ["n", "m"].map(|name| sim.signal(name));
                assert_eq!(sim.get(m), 9u32.into(), "{backend}");
                assert_eq!(sim.get(n), 4u32.into(), "{backend}");
            }};
        }
        if backend.starts_with("native") {
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check!(builder.build_native());
        } else {
            check!(builder.build_cranelift());
        }
    }
}

/// An `iff` wait on a clock only the scheduler drives, with a gate the host
/// writes: each edge is judged with the gate the host left, on every
/// backend.
#[test]
fn iff_waits_on_scheduled_clocks_judge_host_written_gates() {
    let source = r#"
        module Top(input bit clk, input bit en, output int n);
            timeunit 1ns / 1ns;
            bit q;
            always_ff @(posedge clk) q <= ~q;
            initial forever begin @(posedge clk iff en); n++; end
        endmodule
    "#;
    let mut backends = vec!["cranelift", "cranelift-parallel"];
    if cfg!(any(target_arch = "x86_64", target_arch = "aarch64")) {
        backends.extend(["native", "native-parallel"]);
    }
    for backend in backends {
        let builder = celox::Simulation::from_sv_sources(
            vec![(source, std::path::Path::new("iff_host_gate.sv"))],
            "Top",
        );
        let builder = if backend.ends_with("-parallel") {
            builder
                .threads(4)
                .parallel_partition(celox::ParallelPartition::Always)
        } else {
            builder
        };
        // The backends build different simulation types.
        macro_rules! check {
            ($sim:expr) => {{
                let mut sim = $sim.unwrap();
                let [en, n] = ["en", "n"].map(|name| sim.signal(name));
                // Rising edges at 0, 10, ..., 90; `en` is 1 from 25 to 65.
                sim.add_clock("clk", 10, 0);
                sim.run_until(25).unwrap();
                sim.modify(|io| io.set(en, 1u8)).unwrap();
                sim.run_until(65).unwrap();
                sim.modify(|io| io.set(en, 0u8)).unwrap();
                sim.run_until(95).unwrap();
                assert_eq!(sim.get(n), 4u32.into(), "{backend}");
            }};
        }
        if backend.starts_with("native") {
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check!(builder.build_native());
        } else {
            check!(builder.build_cranelift());
        }
    }
}

/// The baselines of `iff` waits whose gate is judged at the write survive a
/// state file: a simulation loaded between edges wakes as the uninterrupted
/// one does, and saves the same baselines again.
#[test]
fn gated_wait_baselines_survive_a_state_file() {
    let source = r#"
        module Top(output int n);
            timeunit 1ns / 1ns;
            bit clk, en;
            initial forever begin @(posedge clk iff en); n++; end
            initial begin
                #1 en = 1;
                #1 clk = 1; en = 0;
                #1 clk = 0;
                #1 en = 1;
                #1 clk = 1;
                #1 clk = 0; en = 0;
                #1 clk = 1;
            end
        endmodule
    "#;
    let build = || {
        celox::Simulation::from_sv_sources(
            vec![(source, std::path::Path::new("gated_state.sv"))],
            "Top",
        )
        .build()
        .unwrap()
    };
    let through_bytes = |file: &celox::StateFile| {
        let mut bytes = Vec::new();
        file.write_to(&mut bytes).unwrap();
        celox::StateFile::read_from(bytes.as_slice()).unwrap()
    };
    let count = |sim: &mut celox::Simulation| {
        let n = sim.signal("n");
        sim.get(n)
    };
    let mut whole = build();
    whole.run_until(10).unwrap();
    assert_eq!(count(&mut whole), 2u32.into());
    for at in [2, 4] {
        let mut first = build();
        first.run_until(at).unwrap();
        let file = through_bytes(&first.save_state().unwrap());
        let mut loaded = build();
        loaded.load_state(&file).unwrap();
        assert_eq!(
            through_bytes(&loaded.save_state().unwrap()),
            file,
            "at {at}"
        );
        loaded.run_until(10).unwrap();
        assert_eq!(count(&mut loaded), 2u32.into(), "at {at}");
    }
}

/// A host pulse `clk = 1; clk = 0;` between two steps, while the `iff`
/// condition holds, is one event of the wait (IEEE 1800-2023 9.4.2.3).
#[test]
fn host_pulses_of_iff_gated_clocks_wake_once() {
    let source = r#"
        module Top(input bit clk, input bit en, output int n);
            timeunit 1ns / 1ns;
            initial forever begin @(posedge clk iff en); n++; end
        endmodule
    "#;
    let mut backends = vec!["cranelift", "cranelift-parallel"];
    if cfg!(any(target_arch = "x86_64", target_arch = "aarch64")) {
        backends.extend(["native", "native-parallel"]);
    }
    for backend in backends {
        let builder = celox::Simulation::from_sv_sources(
            vec![(source, std::path::Path::new("iff_host_pulse.sv"))],
            "Top",
        );
        let builder = if backend.ends_with("-parallel") {
            builder
                .threads(4)
                .parallel_partition(celox::ParallelPartition::Always)
        } else {
            builder
        };
        // The backends build different simulation types.
        macro_rules! check {
            ($sim:expr) => {{
                let mut sim = $sim.unwrap();
                let [clk, en, n] = ["clk", "en", "n"].map(|name| sim.signal(name));
                sim.run_until(1).unwrap();
                sim.modify(|io| io.set(en, 1u8)).unwrap();
                sim.run_until(2).unwrap();
                sim.modify(|io| {
                    io.set(clk, 1u8);
                    io.set(clk, 0u8);
                })
                .unwrap();
                sim.run_until(3).unwrap();
                assert_eq!(sim.get(n), 1u32.into(), "{backend}");
            }};
        }
        if backend.starts_with("native") {
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check!(builder.build_native());
        } else {
            check!(builder.build_cranelift());
        }
    }
}

const IFF_HOST_GATE: &str = r#"
    module Top(input bit clk, input bit en, output int n);
        timeunit 1ns / 1ns;
        initial forever begin @(posedge clk iff en); n++; end
    endmodule
"#;

fn iff_host_gate() -> celox::Simulation {
    let mut sim = celox::Simulation::from_sv_sources(
        vec![(IFF_HOST_GATE, std::path::Path::new("iff_host_gate.sv"))],
        "Top",
    )
    .build()
    .unwrap();
    sim.run_until(1).unwrap();
    sim
}

/// A state file saved between host writes keeps what the writes already
/// counted: `clk = 1` while `en` is 0 is no event, and `en = 1` after it
/// does not turn it into one, in the loaded simulation as in the saved one.
#[test]
fn state_files_keep_occurrences_an_iff_gate_rejected() {
    let mut whole = iff_host_gate();
    let [clk, en, n] = ["clk", "en", "n"].map(|name| whole.signal(name));
    whole.modify(|io| io.set(clk, 1u8)).unwrap();
    whole.modify(|io| io.set(en, 1u8)).unwrap();
    let file = whole.save_state().unwrap();
    whole.run_until(2).unwrap();
    assert_eq!(whole.get(n), 0u32.into());

    let mut bytes = Vec::new();
    file.write_to(&mut bytes).unwrap();
    let file = celox::StateFile::read_from(bytes.as_slice()).unwrap();
    let mut loaded = iff_host_gate();
    loaded.load_state(&file).unwrap();
    loaded.run_until(2).unwrap();
    assert_eq!(loaded.get(n), 0u32.into());
}

/// A raw write after tracked host writes is judged from where they left
/// the signal: a rejected pulse `clk = 1; clk = 0;` while `en` is 0, then
/// raw writes of `en = 1` and `clk = 1`, wake the wait once.
#[test]
fn raw_edges_after_rejected_host_pulses_wake_iff_waits() {
    let mut sim = iff_host_gate();
    let [clk, en, n] = ["clk", "en", "n"].map(|name| sim.signal(name));
    sim.modify(|io| {
        io.set(clk, 1u8);
        io.set(clk, 0u8);
    })
    .unwrap();
    let (memory, size) = sim.memory_as_mut_ptr();
    assert!(clk.offset < size && en.offset < size);
    // Safety: clk and en are live one-bit inputs within the allocation.
    unsafe {
        memory.add(en.offset).write(1);
        memory.add(clk.offset).write(1);
    }
    sim.note_raw_writes();
    sim.run_until(2).unwrap();
    assert_eq!(sim.get(n), 1u32.into());
}

/// What the host does between and at the steps of an `iff` wait.
#[derive(Clone, Copy, Debug)]
enum HostOp {
    Run(u64),
    Set(&'static str, u8),
    Raw(&'static str, u8),
    NoteRaw,
}

const IFF_HOST_OPS: &[HostOp] = &[
    HostOp::Run(1),
    // An edge while the gate is 0 is no event; the gate rising after it is
    // none either, nor is an unrelated raw write.
    HostOp::Set("clk", 1),
    HostOp::Set("en", 1),
    HostOp::Raw("u", 1),
    HostOp::NoteRaw,
    HostOp::Run(2),
    // A host pulse while the gate holds is one event.
    HostOp::Set("clk", 0),
    HostOp::Set("clk", 1),
    HostOp::Set("clk", 0),
    HostOp::Run(3),
    // Raw edges: a rejected one, then one while the gate holds.
    HostOp::Set("en", 0),
    HostOp::Raw("clk", 1),
    HostOp::NoteRaw,
    HostOp::Raw("clk", 0),
    HostOp::Set("en", 1),
    HostOp::Raw("clk", 1),
    HostOp::NoteRaw,
    HostOp::Run(4),
];

/// The count after each step of `ops`.
fn apply_host_ops(sim: &mut celox::Simulation, ops: &[HostOp], counts: &mut Vec<u32>) {
    for &op in ops {
        match op {
            HostOp::Run(time) => {
                sim.run_until(time).unwrap();
                let n = sim.signal("n");
                counts.push(u32::try_from(sim.get(n)).unwrap());
            }
            HostOp::Set(name, value) => {
                let signal = sim.signal(name);
                sim.modify(|io| io.set(signal, value)).unwrap();
            }
            HostOp::Raw(name, value) => {
                let signal = sim.signal(name);
                let (memory, size) = sim.memory_as_mut_ptr();
                assert!(signal.offset < size);
                // Safety: a live one-bit input within the allocation.
                unsafe { memory.add(signal.offset).write(value) };
            }
            HostOp::NoteRaw => sim.note_raw_writes(),
        }
    }
}

/// Every piece of the state of an `iff` wait, its gate counters and the
/// host's tracked and raw writes survives a state file and a checkpoint at
/// any point: the rest of the run gives the same counts as without them.
#[test]
fn iff_wait_state_survives_saves_between_host_writes() {
    let source = r#"
        module Top(input bit clk, input bit en, input bit u, output int n);
            timeunit 1ns / 1ns;
            initial forever begin @(posedge clk iff en); n++; end
        endmodule
    "#;
    let build = || {
        celox::Simulation::from_sv_sources(
            vec![(source, std::path::Path::new("iff_host_ops.sv"))],
            "Top",
        )
        .build()
        .unwrap()
    };
    let mut expected = Vec::new();
    apply_host_ops(&mut build(), IFF_HOST_OPS, &mut expected);
    assert_eq!(expected, [0, 0, 1, 2], "the uninterrupted run");
    for at in 1..IFF_HOST_OPS.len() {
        let (before, after) = IFF_HOST_OPS.split_at(at);
        let mut first = build();
        let mut counts = Vec::new();
        apply_host_ops(&mut first, before, &mut counts);
        let mut bytes = Vec::new();
        first.save_state().unwrap().write_to(&mut bytes).unwrap();
        let file = celox::StateFile::read_from(bytes.as_slice()).unwrap();
        let checkpoint = first.checkpoint().unwrap();

        let mut loaded = build();
        loaded.load_state(&file).unwrap();
        let mut loaded_counts = counts.clone();
        apply_host_ops(&mut loaded, after, &mut loaded_counts);
        assert_eq!(loaded_counts, expected, "state file saved after {before:?}");

        let mut continued = counts.clone();
        apply_host_ops(&mut first, after, &mut continued);
        assert_eq!(continued, expected, "the run saved after {before:?}");
        first.restore(&checkpoint).unwrap();
        let mut restored = counts;
        apply_host_ops(&mut first, after, &mut restored);
        assert_eq!(restored, expected, "checkpoint taken after {before:?}");
    }
}

// Forked child activations and join boundaries.
timed_cases! {
    fn fork_join_any_keeps_other_children_running = "fork::join_any_keeps_other_children_running";
    fn fork_join_none_starts_after_parent_blocks = "fork::join_none_starts_after_parent_blocks";
    fn fork_repeated_forks_capture_each_automatic_local = "fork::repeated_forks_capture_each_automatic_local";
    fn fork_nested_forks_wait_for_their_own_children = "fork::nested_forks_wait_for_their_own_children";
    fn fork_automatic_task_inputs_outlive_the_call = "fork::automatic_task_inputs_outlive_the_call";
    fn fork_fork_children_wait_for_signal_changes = "fork::fork_children_wait_for_signal_changes";
    fn fork_repeated_forks_reuse_finished_instances = "fork::repeated_forks_reuse_finished_instances";
    fn fork_static_locals_are_shared_between_overlapping_children = "fork::static_locals_are_shared_between_overlapping_children";
    fn fork_fork_nonblocking_updates_keep_each_child_value = "fork::fork_nonblocking_updates_keep_each_child_value";
    fn fork_children_share_a_live_automatic_local = "fork::children_share_a_live_automatic_local";
    fn fork_wait_on_shared_automatic_local_wakes_after_child_write = "fork::wait_on_shared_automatic_local_wakes_after_child_write";
    fn fork_event_on_shared_automatic_local_wakes_after_child_write = "fork::event_on_shared_automatic_local_wakes_after_child_write";
    fn fork_suspended_child_keeps_automatic_string_alive = "fork::suspended_child_keeps_automatic_string_alive";
    fn fork_child_delay_uses_current_simulation_time = "fork::child_delay_uses_current_simulation_time";
    fn fork_join_any_waits_only_for_immediate_children = "fork::join_any_waits_only_for_immediate_children";
}

timed_cases! { fn fork_four_state_automatic_values_survive_fork_suspension = "fork::four_state_automatic_values_survive_fork_suspension"; }

timed_cases! {
    fn fork_control_no_children_does_not_suspend = "fork_control::no_children_does_not_suspend";
    fn fork_control_wait_in_task_includes_earlier_forks = "fork_control::wait_in_task_includes_earlier_forks";
    fn fork_control_wait_excludes_descendants_of_completed_children = "fork_control::wait_excludes_descendants_of_completed_children";
    fn fork_control_disable_before_children_start = "fork_control::disable_before_children_start";
    fn fork_control_disable_after_join_any_cancels_waiters_and_delays = "fork_control::disable_after_join_any_cancels_waiters_and_delays";
    fn fork_control_disable_reaches_through_completed_parents = "fork_control::disable_reaches_through_completed_parents";
    fn fork_control_disable_does_not_kill_siblings_or_caller = "fork_control::disable_does_not_kill_siblings_or_caller";
    fn fork_control_disable_removes_nested_join_waits = "fork_control::disable_removes_nested_join_waits";
    fn fork_control_cancelled_ids_can_be_reused_without_stale_wakeups = "fork_control::cancelled_ids_can_be_reused_without_stale_wakeups";
    fn fork_control_program_waits_then_cancels_in_reactive_region = "fork_control::program_waits_then_cancels_in_reactive_region";
}

timed_cases! {
    fn fork_control_function_disable_uses_calling_process = "fork_control::function_disable_uses_calling_process";
}

timed_cases! {
    fn fork_control_four_state_values_survive_wait_and_cancellation = "fork_control::four_state_values_survive_wait_and_cancellation";
}

timed_cases! {
    fn fork_control_review_wait_predicate_preserves_cancellation = "fork_control::wait_predicate_preserves_cancellation";
    fn fork_control_review_wait_predicate_repeats_effects_on_each_wakeup = "fork_control::wait_predicate_repeats_effects_on_each_wakeup";
    fn fork_control_review_event_driven_caller_propagates_wait_fork_through_tasks = "fork_control::event_driven_caller_propagates_wait_fork_through_tasks";
    fn fork_control_review_event_driven_caller_spawns_untimed_children = "fork_control::event_driven_caller_spawns_untimed_children";
}

timed_cases! {
    fn fork_control_function_join_none_automatic_values_outlive_returns = "fork_control::function_join_none_automatic_values_outlive_returns";
    fn fork_control_function_join_none_children_can_call_timed_tasks = "fork_control::function_join_none_children_can_call_timed_tasks";
    fn fork_control_function_join_none_preserves_expression_temporaries = "fork_control::function_join_none_preserves_expression_temporaries";
    fn fork_control_function_join_none_retains_child_only_class_handle = "fork_control::function_join_none_retains_child_only_class_handle";
    fn fork_control_function_join_none_event_driven_caller = "fork_control::function_join_none_event_driven_caller";
    fn fork_control_function_join_none_calls_in_defaults_need_process_frames = "fork_control::function_join_none_calls_in_defaults_need_process_frames";
}

#[test]
fn function_join_none_interpreter() {
    for name in [
        "automatic_values_outlive_returns",
        "children_can_call_timed_tasks",
        "calls_in_defaults_need_process_frames",
        "event_driven_caller",
        "preserves_expression_temporaries",
        "retains_child_only_class_handle",
    ] {
        crate::suite::run_case(
            &format!("fork_control::function_join_none_{name}"),
            "interpreter",
        );
    }
}

// Activation-local compound event expressions (IEEE 1800-2023 9.4.2).
timed_cases! {
    fn fork_compound_event_tracks_result_not_operands = "fork::compound_event_tracks_result_not_operands";
    fn fork_compound_event_activations_outlive_parent = "fork::compound_event_activations_outlive_parent";
    fn fork_compound_event_function_scratch_isolated = "fork::compound_event_function_scratch_isolated";
    fn fork_compound_event_four_state_lsb_edges = "fork::compound_event_four_state_lsb_edges";
}

#[test]
fn fork_compound_event_rejects_nonautomatic_operands() {
    let source = r#"
        module Top;
            int operand;
            function automatic int pulse(); operand=1; operand=0; return 0; endfunction
            initial begin
                automatic int local_value=1;
                int unused_result;
                fork
                    begin @(operand && local_value); end
                    begin #1; unused_result=pulse(); end
                join
            end
        endmodule
    "#;
    let error = celox::Simulation::from_sv_sources(
        vec![(source, std::path::Path::new("function_operand_write.sv"))],
        "Top",
    )
    .build()
    .err()
    .unwrap_or_else(|| panic!("mixed global operands need additional observation boundaries"))
    .to_string();
    assert!(
        error.contains("activation-local event expression reading a non-automatic variable"),
        "{error}"
    );
}

#[test]
fn fork_compound_event_interpreter() {
    for name in [
        "compound_event_tracks_result_not_operands",
        "compound_event_activations_outlive_parent",
        "compound_event_function_scratch_isolated",
        "compound_event_four_state_lsb_edges",
    ] {
        crate::suite::run_case(&format!("fork::{name}"), "interpreter");
    }
}

#[test]
fn fork_compound_event_rejects_transitive_global_and_default_reads() {
    for expression in ["outer(local_value)", "with_default(local_value)"] {
        let source = format!(
            r#"
            module Top;
                int global_value=1;
                function automatic int inner(); return global_value; endfunction
                function automatic int outer(input int x); return x+inner(); endfunction
                function automatic int with_default(input int x, input int y=inner()); return x+y; endfunction
                initial begin
                    automatic int local_value=0;
                    fork begin @({expression}); end begin #1; local_value=1; end join
                end
            endmodule
        "#
        );
        let error = celox::Simulation::from_sv_sources(
            vec![(
                source.as_str(),
                std::path::Path::new("global_probe_read.sv"),
            )],
            "Top",
        )
        .build()
        .err()
        .unwrap_or_else(|| panic!("global reads require additional event observation"))
        .to_string();
        assert!(
            error.contains("activation-local event expression reading a non-automatic variable"),
            "{expression}: {error}"
        );
    }
}

#[test]
fn fork_compound_event_rejects_implicit_expression_writes() {
    for expression in [
        "put(value,1)+put(value,0)",
        "$random(value)",
        "$dist_uniform(value,0,1)",
        "$value$plusargs(\"v=%d\",value)",
    ] {
        let source = format!(
            r#"
            module Top;
                function automatic int put(output int dst,input int x); dst=x; return 0; endfunction
                initial begin
                    automatic int value=0;
                    int discard;
                    fork
                        begin @(value==1); end
                        begin #1; discard={expression}; end
                    join
                end
            endmodule
        "#
        );
        let error = celox::Simulation::from_sv_sources(
            vec![(
                source.as_str(),
                std::path::Path::new("implicit_probe_write.sv"),
            )],
            "Top",
        )
        .build()
        .err()
        .unwrap_or_else(|| panic!("implicit writes need safe observation boundaries"))
        .to_string();
        assert!(
            error.contains("implicit write to an activation-local event operand"),
            "{expression}: {error}"
        );
    }
}

timed_cases! {
    fn fork_deferred_four_state_arguments_are_captured_before_child_exit = "fork_deferred::four_state_arguments_are_captured_before_child_exit";
    fn fork_deferred_reactive_wakeup_preserves_mature_observed_only = "fork_deferred::reactive_wakeup_preserves_mature_observed_only";
    fn fork_deferred_sibling_wait_queues_are_independent = "fork_deferred::sibling_wait_queues_are_independent";
    fn fork_deferred_sibling_event_queues_are_independent = "fork_deferred::sibling_event_queues_are_independent";
    fn fork_deferred_child_flush_does_not_inherit_parent_reports = "fork_deferred::child_flush_does_not_inherit_parent_reports";
    fn fork_deferred_disable_fork_flushes_live_children_only = "fork_deferred::disable_fork_flushes_live_children_only";
    fn fork_deferred_reused_slot_cannot_flush_completed_reports = "fork_deferred::reused_slot_cannot_flush_completed_reports";
    fn fork_deferred_zero_delay_is_not_a_flush_point = "fork_deferred::zero_delay_is_not_a_flush_point";
    fn fork_deferred_repeated_static_site_keeps_completed_reports = "fork_deferred::repeated_static_site_keeps_completed_reports";
}

#[test]
fn fork_deferred_interpreter() {
    for name in [
        "reactive_wakeup_preserves_mature_observed_only",
        "four_state_arguments_are_captured_before_child_exit",
        "sibling_wait_queues_are_independent",
        "sibling_event_queues_are_independent",
        "child_flush_does_not_inherit_parent_reports",
        "disable_fork_flushes_live_children_only",
        "reused_slot_cannot_flush_completed_reports",
        "zero_delay_is_not_a_flush_point",
        "repeated_static_site_keeps_completed_reports",
    ] {
        crate::suite::run_case(&format!("fork_deferred::{name}"), "interpreter");
    }
}

#[test]
fn iff_fork_waits_keep_gates_across_state_files() {
    let source = r#"
        module Top(output int accepted, rejected, global_seen);
            timeunit 1ns / 1ns;
            bit first, second, clk, en;
            initial begin
                automatic bit enable = 1;
                fork
                    begin @(posedge first iff enable); accepted++; end
                    begin @(posedge second iff enable); rejected++; end
                    begin @(posedge clk iff en); global_seen++; end
                    begin @(posedge clk iff en); global_seen++; end
                    begin #2 first=1; enable=0; second=1; en=1; clk=1; en=0; end
                join_none
                #4;
            end
        endmodule
    "#;
    let builder = || {
        celox::Simulation::from_sv_sources(
            vec![(source, std::path::Path::new("iff_fork_state.sv"))],
            "Top",
        )
    };
    let mut original = builder().build_interpreter().unwrap();
    original.run_until(1).unwrap();
    let mut bytes = Vec::new();
    original.save_state().unwrap().write_to(&mut bytes).unwrap();
    let file = celox::StateFile::read_from(bytes.as_slice()).unwrap();
    let mut loaded = builder().build_native().unwrap();
    loaded.load_state(&file).unwrap();
    original.run_until(4).unwrap();
    loaded.run_until(4).unwrap();
    for (name, expected) in [("accepted", 1u32), ("rejected", 0), ("global_seen", 2)] {
        assert_eq!(
            original.get(original.signal(name)),
            expected.into(),
            "{name}"
        );
        assert_eq!(loaded.get(loaded.signal(name)), expected.into(), "{name}");
    }
}

timed_cases! {
    fn iff_automatic_clock_preserves_enabled_pulses = "timing::iff_automatic_clock_preserves_enabled_pulses";
}
