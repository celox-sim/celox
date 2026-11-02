#![cfg(feature = "external")]
use std::collections::{BTreeMap, BTreeSet};

const STATUSES: [&str; 9] = [
    "passed",
    "rejected",
    "unexpected_accept",
    "mismatch",
    "emission_error",
    "compile_error",
    "runtime_error",
    "unsupported",
    "ignored",
];

/// Metadata that changes on every run stays out of retained reports, so that
/// branches adding cases only conflict where they change the same results.
fn assert_no_run_metadata(report: &serde_json::Value) {
    for key in ["context_fingerprint", "incremental", "run_counts", "counts"] {
        assert!(report.get(key).is_none(), "retained report has {key}");
    }
    for row in report["cases"].as_array().unwrap() {
        for key in ["case_fingerprint", "reused", "verified_at_unix"] {
            assert!(row.get(key).is_none(), "{} has {key}", row["name"]);
        }
    }
}

#[test]
fn retained_reports_are_valid_historical_evidence_for_the_live_catalogue() {
    let catalogue: BTreeSet<_> = celox_test_suite::veryl::cases()
        .map(|case| case.name)
        .collect();
    for file in [
        "verification/verilator.json",
        "verification/icarus.json",
        "verification/xezim.json",
    ] {
        let report = celox_test_suite::verification::read_report(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(file),
        )
        .unwrap();
        assert_eq!(report["schema_version"], 3);
        assert_no_run_metadata(&report);
        assert!(report["include_ignored"].is_boolean());
        let rows = report["cases"].as_array().unwrap();
        let names: BTreeSet<_> = rows
            .iter()
            .map(|row| row["name"].as_str().unwrap())
            .collect();
        assert_eq!(names.len(), rows.len(), "duplicate results");
        // Passing new cases need no historical baseline row. The daily gate
        // separately requires a fresh report of the complete live catalogue
        // and rejects every new or changed failure without matching evidence.
        assert!(
            names.is_subset(&catalogue),
            "retained evidence names an unknown case"
        );
        for name in &catalogue {
            if celox_test_suite::veryl::verification::known_issue(
                report["tool"].as_str().unwrap(),
                name,
            )
            .is_some()
            {
                assert!(
                    names.contains(name),
                    "missing reviewed exclusion for {name}"
                );
            }
        }
        for row in rows {
            let case = celox_test_suite::veryl::case(row["name"].as_str().unwrap()).unwrap();
            assert_eq!(row["expectation"], format!("{:?}", case.expectation));
            if row["status"] == "rejected" || row["status"] == "unexpected_accept" {
                assert_eq!(
                    case.expectation,
                    celox_test_suite::Expectation::CompilationError
                );
            }
            if row["status"] == "passed" {
                assert_eq!(case.expectation, celox_test_suite::Expectation::Simulation);
            }
            let status = row["status"].as_str().unwrap();
            assert!(STATUSES.contains(&status), "unknown result status");
            if status != "passed" {
                assert!(!row["detail"].as_str().unwrap().is_empty());
            }
            if status == "mismatch" {
                assert_eq!(row["phase"], "execute");
            }
            if status == "ignored" {
                assert_eq!(report["include_ignored"], false);
                assert_eq!(row["phase"], "skip");
                let issue = &row["known_issue"];
                assert_eq!(row["detail"], issue["reason"]);
                assert!(!issue["observed_version"].as_str().unwrap().is_empty());
                if issue.get("category").is_some() {
                    assert!(!issue["category"].as_str().unwrap().is_empty());
                    assert!(matches!(
                        issue["phase"].as_str(),
                        Some("emission" | "compile" | "execute")
                    ));
                } else {
                    assert!(
                        issue["standard"]
                            .as_str()
                            .unwrap()
                            .contains("IEEE 1800-2023")
                    );
                }
                let upstream = issue["upstream"].as_array().unwrap();
                let evidence = issue.get("evidence").and_then(|v| v.as_array());
                assert!(!upstream.is_empty() || evidence.is_some_and(|e| !e.is_empty()));
                for path in evidence.into_iter().flatten() {
                    let file = path.as_str().unwrap().split('#').next().unwrap();
                    assert!(
                        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                            .join(file)
                            .is_file()
                    );
                }
            }
        }
    }
}

#[test]
fn systemverilog_retained_reports_explain_exclusions_and_executed_failures() {
    let catalogue: BTreeSet<_> = celox_test_suite::sv::cases()
        .map(|case| case.name)
        .collect();
    for tool in ["verilator", "icarus", "xezim"] {
        let report = celox_test_suite::verification::read_report(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("verification/sv/{tool}.json")),
        )
        .unwrap();
        assert_eq!(report["schema_version"], 3);
        assert_no_run_metadata(&report);
        let rows = report["cases"].as_array().unwrap();
        let names: BTreeSet<_> = rows
            .iter()
            .map(|row| row["name"].as_str().unwrap())
            .collect();
        assert_eq!(names.len(), rows.len(), "duplicate results");
        // Passing new cases need no historical baseline row. The daily gate
        // separately requires a fresh report of the complete live catalogue
        // and rejects every new or changed failure without matching evidence.
        assert!(
            names.is_subset(&catalogue),
            "retained evidence names an unknown case"
        );
        for name in &catalogue {
            if celox_test_suite::sv::verification::known_issue(tool, name).is_some() {
                assert!(
                    names.contains(name),
                    "missing reviewed exclusion for {name}"
                );
            }
        }
        for row in rows {
            let name = row["name"].as_str().unwrap();
            let case = celox_test_suite::sv::case(name).unwrap();
            let status = row["status"].as_str().unwrap();
            // Reviewed failures stay executable and retain their diagnostics;
            // they are distinct from exclusions and passing cases.
            match status {
                "passed" => {
                    assert_eq!(case.expectation, celox_test_suite::Expectation::Simulation)
                }
                "rejected" => assert_eq!(
                    case.expectation,
                    celox_test_suite::Expectation::CompilationError
                ),
                "unsupported" => assert!(case.script().four_state, "{name}"),
                "ignored" => {
                    let issue = &row["known_issue"];
                    assert_eq!(row["detail"], issue["reason"]);
                    assert_eq!(
                        celox_test_suite::sv::verification::known_issue(tool, name).as_ref(),
                        Some(issue)
                    );
                    for path in issue["evidence"].as_array().unwrap() {
                        let file = path.as_str().unwrap().split('#').next().unwrap();
                        assert!(
                            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                                .join(file)
                                .is_file()
                        );
                    }
                }
                // xezim_retained_failures_match_their_review accounts for
                // each one.
                "mismatch" | "compile_error" | "runtime_error" | "unexpected_accept"
                    if tool == "xezim" =>
                {
                    assert!(!row["detail"].as_str().unwrap().is_empty(), "{name}");
                    assert!(row.get("known_issue").is_none(), "{name}");
                    let (phase, expectation) = match status {
                        "compile_error" => ("compile", celox_test_suite::Expectation::Simulation),
                        "unexpected_accept" => {
                            ("execute", celox_test_suite::Expectation::CompilationError)
                        }
                        _ => ("execute", celox_test_suite::Expectation::Simulation),
                    };
                    assert_eq!(row["phase"], phase, "{name}");
                    assert_eq!(case.expectation, expectation, "{name}");
                }
                "compile_error" | "mismatch" | "runtime_error" => {
                    let (expected_status, phase, diagnostic) = match (tool, name) {
                        (
                            "verilator",
                            "fork_control::process_program_exit_wakes_sibling_observers",
                        ) => (
                            "mismatch",
                            "execute",
                            "the simulation ended before the script did",
                        ),
                        (
                            "verilator",
                            "fork_control::process_ancestor_kill_preserves_caller_rng",
                        ) => (
                            "mismatch",
                            "execute",
                            "assert_eq checks: expected 0x1f, got 0xb",
                        ),
                        (
                            "verilator",
                            "fork_control::process_finished_parent_kills_descendants",
                        ) => (
                            "mismatch",
                            "execute",
                            "assert_eq checks: expected 0xf, got 0x0",
                        ),
                        (
                            "verilator",
                            "fork_control::process_self_kill_finishes_program"
                            | "fork_control::process_external_kill_finishes_program",
                        ) => (
                            "mismatch",
                            "execute",
                            "assert_eq ticks: expected 0x1, got 0x5",
                        ),
                        (
                            "verilator" | "icarus",
                            "timing::final_procedure_output_survives_program_completion",
                        ) => (
                            "mismatch",
                            "execute",
                            "the design did not finish after run_to_finish",
                        ),
                        ("icarus", "procedural::dpi_exports_leave_subroutines_unchanged") => {
                            ("compile_error", "compile", "source_0.sv:3: syntax error")
                        }
                        ("icarus", "fork_control::process_self_kill_finishes_program") => {
                            ("compile_error", "compile", "source_0.sv:10: syntax error")
                        }
                        (
                            "icarus",
                            "fork_control::process_external_kill_finishes_program"
                            | "fork_control::process_final_kill_preserves_cleanup"
                            | "fork_control::process_program_exit_wakes_sibling_observers",
                        ) => ("compile_error", "compile", "source_0.sv:2: syntax error"),
                        (
                            "verilator",
                            "fork_control::process_mutual_status_waits_advance_to_cancellation",
                        ) => (
                            "mismatch",
                            "execute",
                            "assert_eq checks: expected 0x2, got 0x0",
                        ),
                        ("verilator", "fork_control::process_status_wait_and_disable") => (
                            "mismatch",
                            "execute",
                            "assert_eq checks: expected 0x3, got 0x0",
                        ),
                        (
                            "verilator",
                            "fork_control::process_self_suspend_and_await"
                            | "fork_control::process_suspend_and_resume_waits"
                            | "fork_control::process_nested_calls_preserve_level_wait_resume"
                            | "fork_control::process_resume_after_program_exit_is_noop",
                        ) => (
                            "runtime_error",
                            "execute",
                            "native testbench failed: exit status: 1",
                        ),
                        (
                            "verilator",
                            "fork_control::process_resume_resamples_automatic_event_expression",
                        ) => ("compile_error", "compile", "%Error-LIFETIME:"),
                        (
                            "icarus",
                            "fork_control::process_nested_calls_preserve_level_wait_resume",
                        ) => ("compile_error", "compile", "source_0.sv:4: syntax error"),
                        ("icarus", "fork_control::process_handles_index_associative_arrays") => {
                            ("compile_error", "compile", "source_0.sv:5: syntax error")
                        }
                        (
                            "icarus",
                            "fork_control::process_ancestor_kill_preserves_caller_rng"
                            | "fork_control::process_final_procedures_share_identity_and_rng"
                            | "fork_control::process_finished_parent_kills_descendants"
                            | "fork_control::process_identity_and_status"
                            | "fork_control::process_mutual_status_waits_advance_to_cancellation"
                            | "fork_control::process_random_state_and_handles"
                            | "fork_control::process_resume_after_program_exit_is_noop"
                            | "fork_control::process_resume_resamples_automatic_event_expression"
                            | "fork_control::process_self_suspend_and_await"
                            | "fork_control::process_status_wait_and_disable"
                            | "fork_control::process_suspend_and_resume_waits",
                        ) => ("compile_error", "compile", "source_0.sv:3: syntax error"),
                        ("verilator", "fork::compound_event_activations_outlive_parent") => (
                            "runtime_error",
                            "execute",
                            "native testbench failed: exit status: 1",
                        ),
                        (
                            "icarus",
                            "fork::compound_event_activations_outlive_parent"
                            | "fork::compound_event_function_scratch_isolated",
                        ) => (
                            "compile_error",
                            "compile",
                            "complex event expressions are not yet supported in automatic tasks",
                        ),
                        (
                            "icarus",
                            "fork::compound_event_tracks_result_not_operands"
                            | "fork::compound_event_four_state_lsb_edges",
                        ) => (
                            "compile_error",
                            "compile",
                            "Overriding the default variable lifetime is not yet supported",
                        ),
                        ("verilator", "fork_deferred::disable_fork_flushes_live_children_only") => {
                            (
                                "compile_error",
                                "compile",
                                "AstNode is not of expected type",
                            )
                        }
                        (
                            "verilator",
                            "fork_deferred::repeated_static_site_keeps_completed_reports"
                            | "fork_deferred::reused_slot_cannot_flush_completed_reports",
                        ) => ("compile_error", "compile", "Verilator internal fault"),
                        (
                            "verilator",
                            "fork_deferred::reactive_wakeup_preserves_mature_observed_only"
                            | "fork_deferred::sibling_event_queues_are_independent"
                            | "fork_deferred::sibling_wait_queues_are_independent",
                        ) => ("mismatch", "execute", "expect_output: expected"),
                        (
                            "icarus",
                            "fork_deferred::child_flush_does_not_inherit_parent_reports"
                            | "fork_deferred::disable_fork_flushes_live_children_only"
                            | "fork_deferred::four_state_arguments_are_captured_before_child_exit"
                            | "fork_deferred::reactive_wakeup_preserves_mature_observed_only"
                            | "fork_deferred::repeated_static_site_keeps_completed_reports"
                            | "fork_deferred::reused_slot_cannot_flush_completed_reports"
                            | "fork_deferred::sibling_event_queues_are_independent"
                            | "fork_deferred::sibling_wait_queues_are_independent"
                            | "fork_deferred::zero_delay_is_not_a_flush_point",
                        ) => (
                            "compile_error",
                            "compile",
                            "Deferred assertions are not supported",
                        ),
                        (
                            "verilator",
                            "tagged_unions::conditional_struct_payload"
                            | "tagged_unions::constructors_and_checked_members"
                            | "tagged_unions::writable_system_arguments"
                            | "tagged_unions::writable_task_arguments"
                            | "tagged_unions::default_arguments"
                            | "tagged_unions::factory_and_union_arguments"
                            | "tagged_unions::nested_and_void_constructors",
                        ) => ("compile_error", "compile", "Unsupported: tagged union"),
                        (
                            "icarus",
                            "tagged_unions::casts_ports_and_registers"
                            | "tagged_unions::conditional_struct_payload"
                            | "tagged_unions::constructors_and_checked_members"
                            | "tagged_unions::writable_system_arguments"
                            | "tagged_unions::writable_task_arguments"
                            | "tagged_unions::default_arguments"
                            | "tagged_unions::factory_and_union_arguments"
                            | "tagged_unions::nested_and_void_constructors"
                            | "tagged_unions::wide_four_state_payload",
                        ) => ("compile_error", "compile", "syntax error"),
                        (
                            "icarus",
                            "fork_control::function_join_none_automatic_values_outlive_returns"
                            | "fork_control::function_join_none_calls_in_defaults_need_process_frames"
                            | "fork_control::function_join_none_children_can_call_timed_tasks"
                            | "fork_control::function_join_none_event_driven_caller"
                            | "fork_control::function_join_none_preserves_expression_temporaries"
                            | "fork_control::function_join_none_retains_child_only_class_handle",
                        ) => (
                            "compile_error",
                            "compile",
                            "functions cannot have delay statements.",
                        ),
                        (
                            "icarus",
                            "class::imported_member_type_dimensions"
                            | "class::imported_member_type_inheritance",
                        ) => ("compile_error", "compile", "source_0.sv:3: syntax error"),
                        ("icarus", "class::package_constant_class_defaults") => {
                            ("compile_error", "compile", "source_0.sv:4: syntax error")
                        }
                        ("icarus", "class::method_ranges_of_scope_declarations") => {
                            ("compile_error", "compile", "source_0.sv:9: syntax error")
                        }
                        ("icarus", "class::generate_scope_enum_type_arguments") => {
                            ("compile_error", "compile", "source_0.sv:3: syntax error")
                        }
                        ("verilator", "class::generate_scope_enum_type_arguments") => (
                            "mismatch",
                            "execute",
                            "assert_eq sum: expected 0x4bc, got 0x8ae",
                        ),
                        ("icarus", "class::named_aggregate_type_arguments") => {
                            ("compile_error", "compile", "source_0.sv:8: syntax error")
                        }
                        ("icarus", "class::package_constant_type_arguments") => {
                            ("compile_error", "compile", "source_0.sv:5: syntax error")
                        }
                        ("icarus", "class::type_alias_members") => {
                            ("compile_error", "compile", "source_0.sv:3: syntax error")
                        }
                        ("icarus", "class::type_alias_lexical_scopes") => (
                            "compile_error",
                            "compile",
                            "External methods are not yet supported",
                        ),
                        ("icarus", "class::type_parameter_imports") => {
                            ("compile_error", "compile", "source_0.sv:7: syntax error")
                        }
                        ("icarus", "class::type_parameter_shadowing") => {
                            ("compile_error", "compile", "Invalid module item")
                        }
                        ("icarus", "unions::packed_mixed_state") => (
                            "mismatch",
                            "execute",
                            "assert_eq two: expected 0xa0, got 0xaX",
                        ),
                        (
                            "icarus",
                            "unions::soft_overlay"
                            | "unions::soft_signed_ports"
                            | "unions::soft_wide_four_state",
                        ) => ("compile_error", "compile", "syntax error"),
                        (
                            "icarus",
                            "hierarchy::packed_pattern_parameter_arguments_keep_types_and_unknown_bits"
                            | "hierarchy::packed_pattern_parameter_arguments_preserve_wide_values"
                            | "hierarchy::packed_pattern_parameters_forward_whole_values",
                        ) => (
                            "compile_error",
                            "compile",
                            "packed array parameters are not supported yet",
                        ),
                        (
                            "verilator",
                            "operators::two_state_case_equality_compares_literal_structure"
                            | "operators::two_state_case_equality_preserves_conditional_unknowns"
                            | "operators::two_state_case_equality_keeps_unknown_expression_bits",
                        ) => ("compile_error", "compile", "Unsupported tristate construct"),
                        (
                            "verilator",
                            "operators::two_state_case_equality_preserves_unknown_literals",
                        ) => (
                            "compile_error",
                            "compile",
                            "Unsupported: tristate in top-level IO",
                        ),
                        (
                            "icarus",
                            "mailboxes::get_and_bounded_put_block"
                            | "mailboxes::messages_come_out_in_put_order"
                            | "mailboxes::try_methods_do_not_block",
                        ) => ("compile_error", "compile", "Invalid module instantiation"),
                        ("icarus", "mailboxes::strings_and_class_handles") => {
                            ("compile_error", "compile", "Invalid module item")
                        }
                        (
                            "icarus",
                            "semaphores::get_waits_until_enough_keys_are_put"
                            | "semaphores::try_get_takes_only_available_keys",
                        ) => ("compile_error", "compile", "Invalid module instantiation"),
                        ("icarus", "semaphores::one_key_admits_one_process_at_a_time") => {
                            ("compile_error", "compile", "semaphore doesn't name a type")
                        }
                        ("icarus", "events::events_of_class_objects") => {
                            ("compile_error", "compile", "Invalid class item")
                        }
                        ("icarus", "events::triggered_state_lasts_until_time_advances") => (
                            "compile_error",
                            "compile",
                            "Event name blast can't have member names (triggered)",
                        ),
                        ("verilator", "references::ref_actual_of_an_equivalent_type") => (
                            "compile_error",
                            "compile",
                            "Ref argument requires matching types",
                        ),
                        (
                            "verilator",
                            "references::ref_array_element_index_is_fixed_at_the_call"
                            | "references::ref_class_property_names_the_object_of_the_call"
                            | "unpacked_structs::reference_formal_handles_survive_heap_collections"
                            | "unpacked_structs::reference_formals",
                        ) => (
                            "compile_error",
                            "compile",
                            "External variable referenced by non-inlined function/task",
                        ),
                        (
                            "icarus",
                            "references::const_ref_in_a_continuous_assignment"
                            | "references::const_ref_reads_the_current_value"
                            | "references::ref_static_class_properties",
                        ) => ("compile_error", "compile", "syntax error"),
                        (
                            "icarus",
                            "references::ref_actual_of_an_equivalent_type"
                            | "references::ref_array_element_index_is_fixed_at_the_call"
                            | "references::ref_default_and_named_arguments"
                            | "references::ref_element_select_past_the_element"
                            | "references::ref_formals_of_one_actual_alias_each_other"
                            | "references::ref_writes_are_seen_before_the_return",
                        ) => (
                            "compile_error",
                            "compile",
                            "Reference ports not supported yet",
                        ),
                        (
                            "icarus",
                            "references::ref_class_property_names_the_object_of_the_call"
                            | "references::ref_element_after_an_input_in_forked_activations"
                            | "references::ref_elements_of_concurrent_task_activations"
                            | "references::ref_fills_caller_queues"
                            | "references::ref_property_forwarded_to_output_and_inout"
                            | "references::ref_property_of_this"
                            | "references::ref_replaces_a_class_handle"
                            | "references::ref_task_keeps_a_property_while_suspended"
                            | "references::ref_task_keeps_the_reference_while_suspended",
                        ) => (
                            "compile_error",
                            "compile",
                            "Overriding the default variable lifetime is not yet supported",
                        ),
                        ("verilator", "class::foreign_generic_constructor_lexical_scope") => {
                            ("compile_error", "compile", "FuncRef without fvar")
                        }
                        (
                            "verilator",
                            "class::foreign_generic_declaration_scope_and_equivalent_defaults",
                        ) => (
                            "mismatch",
                            "execute",
                            "assert_eq checks: expected 0x7f, got 0x5f",
                        ),
                        ("verilator", "class::foreign_generic_inherited_constants") => (
                            "mismatch",
                            "execute",
                            "assert_eq checks: expected 0x1f, got 0x0",
                        ),
                        ("verilator", "class::foreign_generic_nominal_and_nested_types") => (
                            "compile_error",
                            "compile",
                            "Unsupported: Multiple '::' package/class reference",
                        ),
                        ("icarus", "class::container_type_aliases_of_classes") => (
                            "compile_error",
                            "compile",
                            "Overriding the default variable lifetime is not yet supported",
                        ),
                        ("icarus", "class::members_of_call_results") => {
                            ("compile_error", "compile", "source_0.sv:17: syntax error")
                        }
                        ("icarus", "containers::compound_element_assignments") => (
                            "compile_error",
                            "compile",
                            "source_0.sv:7: error: Type names are not valid expressions here.",
                        ),
                        ("icarus", "containers::string_indices_from_calls") => (
                            "compile_error",
                            "compile",
                            "source_1.sv:6: sorry: Overriding the default variable lifetime is not yet supported.",
                        ),
                        ("icarus", "containers::array_items_in_concatenations") => (
                            "compile_error",
                            "compile",
                            "source_0.sv:26: sorry: Overriding the default variable lifetime is not yet supported.",
                        ),
                        ("verilator", "string_format::formatted_strings_into_integral_outputs") => {
                            (
                                "mismatch",
                                "execute",
                                "assert_eq d: expected 0x5a, got 0x58",
                            )
                        }
                        ("icarus", "string_format::formatted_strings_into_integral_outputs") => (
                            "runtime_error",
                            "execute",
                            "native testbench exited without reaching $finish",
                        ),
                        ("icarus", "class::parameter_literals_hide_module_constants") => (
                            "compile_error",
                            "compile",
                            "source_0.sv:4: error: Invalid module item.",
                        ),
                        ("icarus", "class::parameter_strings_wider_than_a_constant") => {
                            ("compile_error", "compile", "source_0.sv:3: syntax error")
                        }
                        ("icarus", "class::scoped_typedef_handles") => (
                            "compile_error",
                            "compile",
                            "source_0.sv:4: error: Invalid class item.",
                        ),
                        (
                            "icarus",
                            "types::unpacked_array_assignments_through_selects_and_calls",
                        ) => (
                            "compile_error",
                            "compile",
                            "Subroutine ports with unpacked dimensions are not yet supported",
                        ),
                        (
                            "xezim",
                            "types::unpacked_array_assignments_through_selects_and_calls",
                        ) => (
                            "mismatch",
                            "execute",
                            "assert_eq sum: expected 0x1179b59, got 0x117b2e7",
                        ),
                        ("icarus", "containers::wide_associative_keys") => {
                            ("compile_error", "compile", "syntax error")
                        }
                        ("icarus", "containers::wide_associative_keys_with_unknown_bits") => (
                            "compile_error",
                            "compile",
                            "Type names are not valid expressions here",
                        ),
                        ("icarus", "containers::wide_signed_associative_keys") => (
                            "compile_error",
                            "compile",
                            "Type names are not valid expressions here",
                        ),
                        ("icarus", "string_format::pattern_format_structure_containers") => {
                            ("compile_error", "compile", "Unpacked structs not supported")
                        }
                        ("icarus", "string_format::pattern_format_structure_values") => {
                            ("compile_error", "compile", "Unpacked structs not supported")
                        }
                        ("icarus", "containers::package_scoped_container_methods") => {
                            ("compile_error", "compile", "syntax error")
                        }
                        ("verilator", "containers::nested_container_foreach_skipped_index") => {
                            ("compile_error", "compile", "Verilator internal fault")
                        }
                        ("icarus", "containers::nested_container_foreach") => (
                            "compile_error",
                            "compile",
                            "Type names are not valid expressions here",
                        ),
                        ("icarus", "containers::nested_container_foreach_skipped_index") => (
                            "compile_error",
                            "compile",
                            "Type names are not valid expressions here",
                        ),
                        ("icarus", "containers::multidimensional_foreach_break") => {
                            ("compile_error", "compile", "Queue of type")
                        }
                        ("icarus", "class::static_initializers_run_once_before_time_zero") => (
                            "compile_error",
                            "compile",
                            "sorry: Overriding the default variable lifetime is not yet supported.",
                        ),
                        ("icarus", "class::specialized_scope_typedef_calls") => {
                            ("compile_error", "compile", "source_0.sv:3: syntax error")
                        }
                        ("icarus", "class::specialized_scope_typedef_properties") => {
                            ("compile_error", "compile", "source_0.sv:3: syntax error")
                        }
                        ("xezim", "class::specialized_scope_typedef_properties") => (
                            "mismatch",
                            "execute",
                            "reads count with unknown bits in a two-state run",
                        ),
                        ("icarus", "string_format::pattern_format_enums") => {
                            ("compile_error", "compile", "Unpacked structs not supported")
                        }
                        (
                            "icarus",
                            "container_formals::selects_of_four_state_container_elements",
                        ) => (
                            "compile_error",
                            "compile",
                            "failed assertion name_tail.index.size() == 1",
                        ),
                        (
                            "icarus",
                            "container_formals::selects_of_reference_container_elements",
                        ) => (
                            "compile_error",
                            "compile",
                            "Type names are not valid expressions here.",
                        ),
                        ("icarus", "class::casts_to_class_properties") => (
                            "compile_error",
                            "compile",
                            "Type names are not valid expressions here.",
                        ),
                        ("icarus", "class::void_casts_of_class_handles") => (
                            "runtime_error",
                            "execute",
                            "System task/function $cast() is not defined by any module.",
                        ),
                        ("icarus", "class::failing_casts_evaluate_their_sources") => {
                            ("compile_error", "compile", "source_0.sv:23: syntax error")
                        }
                        ("verilator", "class::failing_casts_evaluate_their_sources") => (
                            "mismatch",
                            "execute",
                            "expect_output: expected [49, 10, 50, 10, 51, 10, 49, 32, 49, 10], got [48, 10, 48, 10, 48, 10, 49, 32, 49, 10]",
                        ),
                        ("icarus", "class::members_hide_module_names") => (
                            "compile_error",
                            "compile",
                            "Object f in celox_suite_tb.dut is not a function.",
                        ),
                        ("icarus", "array_locators::array_locator_unique_class_handles") => {
                            ("compile_error", "compile", "source_0.sv:18: syntax error")
                        }
                        ("icarus", "class::methods_called_without_parentheses") => {
                            ("compile_error", "compile", "source_0.sv:27: syntax error")
                        }
                        ("icarus", "types::local_typedefs") => (
                            "compile_error",
                            "compile",
                            "Unable to bind wire/reg/memory `HIGH'",
                        ),
                        ("icarus", "class::prototype_formals_inherit_types") => {
                            ("compile_error", "compile", "source_0.sv:7: syntax error")
                        }
                        ("icarus", "virtual_interfaces::unit_import_package_variables") => {
                            ("compile_error", "compile", "source_0.sv:17: syntax error")
                        }
                        ("icarus", "containers::functions_returning_containers") => (
                            "compile_error",
                            "compile",
                            "Type names are not valid expressions here.",
                        ),
                        ("xezim", "containers::functions_returning_containers") => {
                            ("mismatch", "execute", "got [49, 32, 48, 32, 48, 10")
                        }
                        ("icarus", "class::real_properties") => (
                            "runtime_error",
                            "execute",
                            "recv_real(1.000000) not implemented",
                        ),
                        ("icarus", "operators::one_bit_casts") => (
                            "compile_error",
                            "compile",
                            "Method name nesting is not supported yet",
                        ),
                        ("xezim", "operators::one_bit_casts") => (
                            "mismatch",
                            "execute",
                            "assert_eq was_rand: expected 0x1, got 0x0",
                        ),
                        ("icarus", "timing::static_formals_of_package_tasks") => {
                            ("compile_error", "compile", "syntax error")
                        }
                        ("xezim", "timing::static_formals_of_package_tasks") => (
                            "mismatch",
                            "execute",
                            "assert_eq first: expected 0x3, got 0x2",
                        ),
                        ("icarus", "class::constrained_enumerations_and_randc") => (
                            "compile_error",
                            "compile",
                            "Constraint declarations not supported",
                        ),
                        ("xezim", "class::constrained_enumerations_and_randc") => (
                            "mismatch",
                            "execute",
                            "assert_eq selected: expected 0x1, got 0x0",
                        ),
                        ("verilator", "class::constrained_enumerations_and_randc") => (
                            "mismatch",
                            "execute",
                            "assert_eq independent: expected 0x1, got 0x0",
                        ),
                        ("icarus", "string::foreach_iterates_the_characters") => {
                            ("compile_error", "compile", "syntax error")
                        }
                        ("xezim", "string::foreach_iterates_the_characters") => (
                            "mismatch",
                            "execute",
                            "assert_eq before_b: expected 0x2, got 0x1",
                        ),
                        ("icarus", "array_locators::ordering_methods_with_expressions") => {
                            ("compile_error", "compile", "syntax error")
                        }
                        ("icarus", "containers::foreach_enumeration_index_methods") => (
                            "compile_error",
                            "compile",
                            "Type names are not valid expressions here",
                        ),
                        ("icarus", "containers::four_state_container_equality") => {
                            ("compile_error", "compile", "draw_eval_vec4: Assertion")
                        }
                        ("xezim", "containers::four_state_container_equality") => (
                            "mismatch",
                            "execute",
                            "assert_eq flags: expected 0xf, got 0xd",
                        ),
                        ("icarus", "system_functions::casts_to_enumerated_types") => (
                            "compile_error",
                            "compile",
                            "Method name nesting is not supported yet.",
                        ),
                        ("verilator", "system_functions::casts_to_enumerated_types") => (
                            "compile_error",
                            "compile",
                            "Implicit conversion to enum 'enum{}Top.Colors'",
                        ),
                        ("xezim", "system_functions::casts_to_enumerated_types") => (
                            "mismatch",
                            "execute",
                            "52, 32, 51, 10, 49, 32, 72, 73, 71, 72, 10]",
                        ),
                        (
                            "icarus",
                            "system_functions::casts_to_enumerated_types_check_whole_values",
                        ) => (
                            "compile_error",
                            "compile",
                            "\"inside\" expressions not supported yet.",
                        ),
                        (
                            "icarus",
                            "system_functions::casts_to_enumerated_types_reject_class_handles",
                        ) => ("compile_error", "compile", "source_0.sv:19: syntax error"),
                        (
                            "verilator",
                            "system_functions::casts_to_enumerated_types_reject_class_handles",
                        ) => (
                            "compile_error",
                            "compile",
                            "Unsupported: $cast to 'enum{}Top.Bit' from 'class{}Item'",
                        ),
                        (
                            "xezim",
                            "system_functions::casts_to_enumerated_types_check_whole_values",
                        ) => (
                            "mismatch",
                            "execute",
                            "48, 32, 72, 73, 71, 72, 10, 49, 32, 79, 78, 69, 10, 49, 32, 79, 78, 69, 10, 49, 32, 79, 78, 69, 10]",
                        ),
                        (
                            "xezim",
                            "system_functions::casts_to_enumerated_types_reject_class_handles",
                        ) => (
                            "mismatch",
                            "execute",
                            "49, 32, 90, 69, 82, 79, 10, 49, 32, 90, 69, 82, 79, 10, 49, 32, 90, 69, 82, 79, 10]",
                        ),
                        (
                            "icarus",
                            "system_functions::casts_to_enumerated_types_size_sources_by_themselves",
                        ) => (
                            "runtime_error",
                            "execute",
                            "System task/function $cast() is not defined by any module.",
                        ),
                        (
                            "verilator",
                            "system_functions::casts_to_enumerated_types_size_sources_by_themselves",
                        ) => (
                            "compile_error",
                            "compile",
                            "Implicit conversion to enum 'enum{}Top.Wide'",
                        ),
                        (
                            "icarus",
                            "system_functions::casts_evaluate_destination_operands_once",
                        ) => (
                            "compile_error",
                            "compile",
                            "Type names are not valid expressions here.",
                        ),
                        (
                            "verilator",
                            "system_functions::casts_evaluate_destination_operands_once",
                        ) => (
                            "mismatch",
                            "execute",
                            "got [48, 32, 48, 10, 49, 32, 49, 32, 72",
                        ),
                        ("xezim", "system_functions::casts_evaluate_destination_operands_once") => {
                            (
                                "mismatch",
                                "execute",
                                "got [49, 32, 49, 10, 49, 32, 50, 32, 72",
                            )
                        }
                        (
                            "xezim",
                            "system_functions::casts_to_enumerated_types_size_sources_by_themselves",
                        ) => (
                            "mismatch",
                            "execute",
                            "got [49, 32, 85, 78, 73, 84, 10, 49, 32, 10]",
                        ),
                        ("icarus", "system_functions::casts_of_constants_to_enumerated_types") => {
                            ("compile_error", "compile", "source_0.sv:11: syntax error")
                        }
                        ("xezim", "system_functions::casts_of_constants_to_enumerated_types") => (
                            "compile_error",
                            "compile",
                            "expected RParen, found Identifier 'P'",
                        ),
                        ("icarus", "system_functions::casts_of_comparisons_and_cast_results") => (
                            "runtime_error",
                            "execute",
                            "System task/function $cast() is not defined by any module.",
                        ),
                        (
                            "icarus",
                            "system_functions::casts_through_struct_members_and_string_keys",
                        ) => (
                            "compile_error",
                            "compile",
                            "Unpacked structs not supported.",
                        ),
                        (
                            "xezim",
                            "system_functions::casts_through_struct_members_and_string_keys",
                        ) => (
                            "mismatch",
                            "execute",
                            "49, 32, 50, 10, 51, 32, 49, 32, 78, 73, 78, 69, 10]",
                        ),
                        (
                            "icarus",
                            "system_functions::casts_address_destinations_as_assignments",
                        ) => (
                            "compile_error",
                            "compile",
                            "Streaming concatenation not supported.",
                        ),
                        (
                            "icarus",
                            "system_functions::casts_of_members_of_specialized_structures",
                        ) => (
                            "compile_error",
                            "compile",
                            "Unpacked structs not supported.",
                        ),
                        (
                            "icarus",
                            "system_functions::casts_to_enumerated_types_reject_unknown_bits",
                        ) => ("compile_error", "compile", "draw_eval_vec4: Assertion"),
                        (
                            "xezim",
                            "system_functions::casts_to_enumerated_types_reject_unknown_bits",
                        ) => ("mismatch", "execute", "49, 32, 79, 70, 70, 10]"),
                        ("verilator", "system_functions::wide_enumerated_types") => (
                            "compile_error",
                            "compile",
                            "Unsupported: enum next/prev/name method on enum with > 64 bits",
                        ),
                        ("icarus", "system_functions::wide_enumerated_types") => {
                            ("compile_error", "compile", "source_0.sv:26: syntax error")
                        }
                        ("xezim", "system_functions::wide_enumerated_types") => (
                            "mismatch",
                            "execute",
                            "90, 69, 82, 79, 10, 48, 32, 48, 32, 49, 10]",
                        ),
                        ("icarus", "class::nested_value_selects") => (
                            "compile_error",
                            "compile",
                            "Index expressions don't apply to this type of property",
                        ),
                        ("icarus", "class::property_assignment_sites") => {
                            ("compile_error", "compile", "source_0.sv:19: syntax error")
                        }
                        ("icarus", "class::packed_structure_property_members") => {
                            ("compile_error", "compile", "source_0.sv:27: syntax error")
                        }
                        ("verilator", "class::tagged_members_of_structure_properties") => (
                            "compile_error",
                            "compile",
                            "Unsupported: void (for tagged unions)",
                        ),
                        ("icarus", "class::tagged_members_of_structure_properties") => {
                            ("compile_error", "compile", "source_0.sv:3: syntax error")
                        }
                        ("icarus", "class::property_member_assignment_sites") => {
                            ("compile_error", "compile", "source_0.sv:17: syntax error")
                        }
                        ("icarus", "class::property_place_addresses") => (
                            "compile_error",
                            "compile",
                            "sorry: Streaming concatenation not supported.",
                        ),
                        ("icarus", "class::property_member_place_addresses") => (
                            "compile_error",
                            "compile",
                            "sorry: I do not know how to elaborate assignment patterns using old method.",
                        ),
                        ("icarus", "class::formatting_task_place_outputs") => (
                            "runtime_error",
                            "execute",
                            "native testbench exited without reaching $finish",
                        ),
                        ("icarus", "containers::element_part_selects") => {
                            ("compile_error", "compile", "Unpacked structs not supported")
                        }
                        ("icarus", "scan::sscanf_class_property_outputs") => {
                            ("compile_error", "compile", "is not an input port")
                        }
                        ("icarus", "scan::sscanf_inputs_before_property_outputs") => {
                            ("runtime_error", "execute", "unresolved functor reference")
                        }
                        ("icarus", "class::enumeration_constants_of_classes") => {
                            ("compile_error", "compile", "source_0.sv:12: syntax error")
                        }
                        ("icarus", "class::foreign_generic_constructor_lexical_scope") => {
                            ("compile_error", "compile", "source_0.sv:5: syntax error")
                        }
                        (
                            "icarus",
                            "class::foreign_generic_declaration_scope_and_equivalent_defaults",
                        ) => ("compile_error", "compile", "source_0.sv:5: syntax error"),
                        (
                            "icarus",
                            "class::foreign_generic_in_a_module_with_interface_instances",
                        ) => ("compile_error", "compile", "source_0.sv:3: syntax error"),
                        ("icarus", "class::foreign_generic_inherited_constants") => {
                            ("compile_error", "compile", "source_0.sv:3: syntax error")
                        }
                        ("icarus", "class::foreign_generic_multiple_user_classes") => {
                            ("compile_error", "compile", "source_0.sv:3: syntax error")
                        }
                        ("icarus", "class::foreign_generic_nominal_and_nested_types") => {
                            ("compile_error", "compile", "source_0.sv:3: syntax error")
                        }
                        ("icarus", "class::foreign_generic_package_type_static_storage") => {
                            ("compile_error", "compile", "source_0.sv:10: syntax error")
                        }
                        ("icarus", "class::foreign_generic_template_of_the_named_package") => {
                            ("compile_error", "compile", "source_0.sv:3: syntax error")
                        }
                        ("icarus", "class::foreign_generic_static_handle_identity") => {
                            ("compile_error", "compile", "source_0.sv:11: syntax error")
                        }
                        ("icarus", "class::foreign_generic_static_state_across_modules") => {
                            ("compile_error", "compile", "source_0.sv:3: syntax error")
                        }
                        ("icarus", "class::foreign_generic_unit_type_constructor") => {
                            ("compile_error", "compile", "source_0.sv:3: syntax error")
                        }
                        ("icarus", "types::chandles_hold_null_and_copies") => {
                            ("compile_error", "compile", "Invalid class item")
                        }
                        _ => panic!("unreviewed failure: {tool} {name}: {status}"),
                    };
                    assert_eq!(status, expected_status, "{name}");
                    assert_eq!(row["phase"], phase, "{name}");
                    assert!(
                        row["detail"].as_str().unwrap().contains(diagnostic),
                        "{name}"
                    );
                    assert_eq!(case.expectation, celox_test_suite::Expectation::Simulation);
                    assert!(row.get("known_issue").is_none(), "{name}");
                    assert!(celox_test_suite::sv::verification::known_issue(tool, name).is_none());
                }
                _ => panic!("{tool} {name}: {status}"),
            }
        }
    }
}

#[test]
fn partial_word_sar_discrepancy_remains_an_executed_failure() {
    let report: serde_json::Value = celox_test_suite::verification::read_report(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("verification/verilator.json"),
    )
    .unwrap();
    let name = "partial_word_shift::two_state_matrix";
    let row = report["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == name)
        .expect("retain the reviewed SAR failure");
    assert_eq!(row["status"], "mismatch");
    assert_eq!(row["phase"], "execute");
    assert!(row["detail"].as_str().unwrap().contains("assert_eq c63_64"));
    assert!(celox_test_suite::veryl::verification::known_issue("verilator", name).is_none());
}

/// xezim retains its failures as executed evidence instead of exclusions.
/// Each one is reviewed against IEEE 1800-2023, and a xezim defect names the
/// reported bug it reproduces.
#[test]
fn xezim_retained_failures_match_their_review() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let review: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("verification/xezim-review.json")).unwrap(),
    )
    .unwrap();
    let bugs = review["bugs"].as_object().unwrap();
    for bug in bugs.values() {
        assert!(!bug["title"].as_str().unwrap().is_empty());
        assert!(
            bug["standard"]
                .as_str()
                .unwrap()
                .starts_with("IEEE 1800-2023 ")
        );
    }
    let mut cited = BTreeSet::new();
    for (suite, file) in [
        ("veryl", "verification/xezim.json"),
        ("sv", "verification/sv/xezim.json"),
    ] {
        let report = celox_test_suite::verification::read_report(&root.join(file)).unwrap();
        let failures: BTreeMap<_, _> = report["cases"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| {
                !matches!(
                    row["status"].as_str(),
                    // The statuses check-external-suite-report.mjs counts as
                    // successes.
                    Some("passed" | "rejected" | "unsupported" | "ignored")
                )
            })
            .map(|row| {
                (
                    row["name"].as_str().unwrap(),
                    row["status"].as_str().unwrap(),
                )
            })
            .collect();
        let mut reviewed = BTreeMap::new();
        for case in review["cases"].as_array().unwrap() {
            if case["suite"] != suite {
                continue;
            }
            let name = case["name"].as_str().unwrap();
            assert!(!case["note"].as_str().unwrap().is_empty(), "{name}");
            let classification = case["classification"].as_str().unwrap();
            assert!(
                matches!(
                    classification,
                    "xezim_bug"
                        | "suite_issue"
                        | "lrm_ambiguous"
                        | "lrm_permitted_race"
                        | "adapter_two_state_init"
                        | "adapter_two_state_semantics"
                ),
                "{name}: {classification}"
            );
            let bug = case.get("bug").and_then(|bug| bug.as_str());
            assert_eq!(classification == "xezim_bug", bug.is_some(), "{name}");
            if let Some(bug) = bug {
                assert!(bugs.contains_key(bug), "{name}: unknown bug {bug}");
                cited.insert(bug);
            }
            let status = case["status"].as_str().unwrap();
            assert!(reviewed.insert(name, status).is_none(), "duplicate {name}");
        }
        assert_eq!(
            failures, reviewed,
            "{suite}: unreviewed or stale xezim results"
        );
    }
    assert_eq!(cited.len(), bugs.len(), "every listed bug has a case");
}
