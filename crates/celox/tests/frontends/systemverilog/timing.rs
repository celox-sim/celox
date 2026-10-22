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
    fn timed_task_automatic_scalar_calls_keep_separate_storage = "timing::timed_task_automatic_scalar_calls_keep_separate_storage";
    fn timed_task_four_state_storage_survives_suspension = "timing::timed_task_four_state_storage_survives_suspension";
    fn timed_task_automatic_calls_keep_separate_storage = "timing::timed_task_automatic_calls_keep_separate_storage";
    fn timed_task_nested_copyback_wait_and_return = "timing::timed_task_nested_copyback_wait_and_return";
    fn timed_task_static_formals_remain_shared = "timing::timed_task_static_formals_remain_shared";
    fn timed_task_in_edge_always_rearms_after_return = "timing::timed_task_in_edge_always_rearms_after_return";
    fn time_calls_in_child_port_subroutines = "timing::time_calls_in_child_port_subroutines";
    fn time_calls_in_child_port_actuals = "timing::time_calls_in_child_port_actuals";
    fn time_literal_delays_accept_all_units = "timing::time_literal_delays_accept_all_units";
    fn delays_resume_initial_blocks_at_later_times = "timing::delays_resume_initial_blocks_at_later_times";
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
        ("bit a; initial @(a iff a);", "iff-qualified event", 1444),
        ("initial fork #1; join", "fork-join block", 88),
        ("initial fork #1; join_none", "fork-join block", 88),
        // #1711 names the construct.
        (
            "event e; initial -> e;",
            "unsupported internal data type",
            88,
        ),
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
        ("initial wait fork;", "wait fork statement", 88),
        // #1711 names the construct.
        ("initial disable fork;", "disable statement", 88),
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
        // #1708: a plain `always` that is not a flip-flop runs as a process.
        (
            "bit [3:0] v; int n; always @(posedge v[0]) n++;",
            "always_ff event expression",
            464,
        ),
        (
            "bit [3:0] v; int n; always @(posedge v) n++;",
            "multi-bit always_ff event signal",
            88,
        ),
        (
            "bit a, b; int n; always @(posedge a or posedge b) n <= n + 1;",
            "always_ff event control",
            88,
        ),
        // #1709
        (
            "bit [7:0] m[4]; initial #1 m[1] <= 5;",
            "nonblocking assignment to an unpacked array in a process",
            88,
        ),
        (
            "bit [3:0] q; int i = 2; initial #1 q[i] <= 1;",
            "nonblocking assignment with a run-time position in a process",
            88,
        ),
        (
            "bit [3:0] q; initial #1 q[5] <= 1;",
            "nonblocking assignment outside its target",
            88,
        ),
        // #1710
        (
            "bit q; task t(); #1 q = 1; endtask always @* t();",
            "timed task call in a combinational process",
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
        // #1711 reports this as illegal rather than unsupported.
        (
            "bit a; function automatic bit f(); #1; return a; endfunction",
            "delay control in a subroutine",
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
    let simulator = celox::Simulator::from_sv_sources(
        vec![(source, std::path::Path::new("staged_sites.sv"))],
        "Top",
    )
    .build()
    .unwrap();
    let program = simulator.program();
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
    let simulator = celox::Simulator::from_sv_sources(sources, &design.top)
        .build()
        .unwrap();
    let program = simulator.program();
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
