import assert from "node:assert/strict";
import test from "node:test";

import { checkExternalSuiteReport } from "./check-external-suite-report.mjs";
import { readVerificationReport } from "./read-verification-report.mjs";

const statuses = [
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

function counts(rows) {
  return Object.fromEntries(
    statuses.map((status) => [
      status,
      rows.filter((row) => row.status === status).length,
    ]),
  );
}

function fixture() {
  const metadata = (name) => ({
    name,
    category: "Operators",
    expectation: "Simulation",
    stronger_than_sv: false,
    tags: [],
    tag_reasons: {},
  });
  const cases = [
    {
      ...metadata("known"),
      status: "compile_error",
      phase: "compile",
      detail: "unsupported array parameter",
      reused: false,
    },
    {
      ...metadata("working"),
      status: "passed",
      phase: "execute",
      detail: "",
      reused: false,
    },
  ];
  const current = {
    schema_version: 3,
    suite: "veryl",
    tool: "icarus",
    version: "pinned",
    incremental: false,
    include_ignored: false,
    exclude_stronger_than_sv: false,
    run_counts: { fresh: 2, reused: 0 },
    cases,
    counts: counts(cases),
  };
  const baseline = structuredClone(current);
  const catalogue = {
    schema_version: 3,
    exclude_stronger_than_sv: false,
    cases: cases.map((row) => metadata(row.name)),
  };
  return { current, baseline, catalogue };
}

function check(
  { current, baseline, catalogue },
  exitStatus = 1,
  suite = "veryl",
) {
  return checkExternalSuiteReport(
    current,
    baseline,
    catalogue,
    exitStatus,
    suite,
  );
}

test("an unchanged retained failure remains visible without failing the daily gate", () => {
  const input = fixture();
  assert.deepEqual(check(input), { cases: 2, accepted_failures: ["known"] });
  assert.equal(input.current.cases[0].status, "compile_error");
  input.current.cases[0] = {
    ...input.current.cases[0],
    status: "passed",
    phase: "execute",
    detail: "",
  };
  input.current.counts = counts(input.current.cases);
  input.current.version = "newer tool that fixed the failure";
  assert.deepEqual(check(input, 0), { cases: 2, accepted_failures: [] });
});

test("a retained baseline without run metadata is accepted", () => {
  const input = fixture();
  for (const key of ["counts", "run_counts", "incremental"])
    delete input.baseline[key];
  input.baseline.cases.forEach((row) => delete row.reused);
  assert.deepEqual(check(input), { cases: 2, accepted_failures: ["known"] });
});

test("new passing cases do not require rewriting the retained baseline", () => {
  for (const status of ["passed", "rejected"]) {
    const input = fixture();
    const expectation = status === "rejected" ? "CompilationError" : "Simulation";
    const row = {
      ...input.current.cases[1],
      name: "new::case",
      expectation,
      status,
    };
    input.current.cases.push(row);
    input.current.counts = counts(input.current.cases);
    input.current.run_counts.fresh++;
    input.catalogue.cases.push({ ...input.catalogue.cases[1], name: row.name, expectation });
    assert.deepEqual(check(input), { cases: 3, accepted_failures: ["known"] });
    row.status = "compile_error";
    row.detail = "new failure";
    input.current.counts = counts(input.current.cases);
    assert.throws(() => check(input), /new or changed external verification failures/);
  }
});

test("new failures, changed diagnostics and changed tools cannot use the baseline", () => {
  for (const mutate of [
    (input) => {
      input.current.cases[1].status = "compile_error";
    },
    (input) => {
      input.current.cases[0].detail = "compiler crashed";
    },
    (input) => {
      input.current.cases[0].status = "runtime_error";
    },
    (input) => {
      input.current.cases[0].phase = "execute";
    },
    (input) => {
      input.current.cases[0].expectation = "CompilationError";
      input.catalogue.cases[0].expectation = "CompilationError";
    },
    (input) => {
      input.current.version = "different tool version";
    },
    (input) => {
      input.baseline.tool = "verilator";
    },
  ]) {
    const input = fixture();
    mutate(input);
    input.current.counts = counts(input.current.cases);
    assert.throws(() => check(input));
  }
});

test("checkout-dependent diagnostic padding does not hide actual changes", () => {
  const input = fixture();
  input.baseline.cases[0].detail =
    "compile error\n                   : continuation\n";
  input.current.cases[0].detail = "compile error\r\n    : continuation\r\n";
  assert.deepEqual(check(input).accepted_failures, ["known"]);
  input.current.cases[0].detail = "compile error\n    : changed continuation\n";
  assert.throws(() => check(input));
});

test("truncated, cached, corrupt and incorrectly classified reports fail closed", () => {
  for (const mutate of [
    (input) => {
      input.current.cases.pop();
      input.current.run_counts.fresh = 1;
    },
    (input) => {
      input.current.cases.push(input.current.cases[0]);
    },
    (input) => {
      input.current.cases[0].status = "unknown";
    },
    (input) => {
      input.current.cases[0].reused = true;
    },
    (input) => {
      input.current.incremental = true;
    },
    (input) => {
      input.current.run_counts.reused = 1;
    },
    (input) => {
      input.current.include_ignored = true;
    },
    (input) => {
      input.current.exclude_stronger_than_sv = true;
    },
    (input) => {
      input.catalogue.cases = [];
    },
    (input) => {
      input.catalogue.exclude_stronger_than_sv = true;
    },
    (input) => {
      input.current.cases[0].category = "Changed";
    },
    (input) => {
      input.current.suite = "sv";
    },
    (input) => {
      input.baseline.counts.compile_error = 999;
    },
    (input) => {
      input.baseline.cases = [];
    },
  ]) {
    const input = fixture();
    mutate(input);
    input.current.counts = counts(input.current.cases);
    assert.throws(() => check(input));
  }
  for (const exitStatus of [0, 2, 124, 137, null, undefined]) {
    const input = fixture();
    assert.throws(() =>
      checkExternalSuiteReport(
        input.current,
        input.baseline,
        input.catalogue,
        exitStatus,
        "veryl",
      ),
    );
  }
});

test("every retained baseline accepts only its existing failures", () => {
  for (const suite of ["veryl", "sv"]) {
    for (const tool of ["verilator", "icarus", "xezim"]) {
      const file = `../crates/celox-test-suite/verification/${suite === "sv" ? "sv/" : ""}${tool}.json`;
      const baseline = readVerificationReport(new URL(file, import.meta.url));
      const current = {
        ...structuredClone(baseline),
        suite,
        incremental: false,
        run_counts: { fresh: baseline.cases.length, reused: 0 },
      };
      current.cases.forEach((row) => {
        row.reused = false;
      });
      const catalogue = {
        schema_version: 3,
        exclude_stronger_than_sv: false,
        cases: current.cases,
      };
      const exitStatus = current.cases.some(
        (row) =>
          !["passed", "rejected", "unsupported", "ignored"].includes(
            row.status,
          ),
      )
        ? 1
        : 0;
      const result = checkExternalSuiteReport(
        current,
        baseline,
        catalogue,
        exitStatus,
        suite,
      );
      if (tool === "xezim") {
        // xezim retains its reviewed failures instead of excluding them.
        const failures = current.cases.filter(
          (row) =>
            !["passed", "rejected", "unsupported", "ignored"].includes(
              row.status,
            ),
        ).length;
        assert.equal(result.accepted_failures.length, failures);
      } else if (suite === "sv") {
        assert.deepEqual(
          result.accepted_failures,
          tool === "icarus"
            ? [
                "class::container_type_aliases_of_classes",
                "class::enumeration_constants_of_classes",
                "class::foreign_generic_constructor_lexical_scope",
                "class::foreign_generic_declaration_scope_and_equivalent_defaults",
                "class::foreign_generic_in_a_module_with_interface_instances",
                "class::foreign_generic_inherited_constants",
                "class::foreign_generic_multiple_user_classes",
                "class::foreign_generic_nominal_and_nested_types",
                "class::foreign_generic_package_type_static_storage",
                "class::foreign_generic_static_handle_identity",
                "class::foreign_generic_static_state_across_modules",
                "class::foreign_generic_template_of_the_named_package",
                "class::foreign_generic_unit_type_constructor",
                "class::generate_scope_enum_type_arguments",
                "class::imported_member_type_dimensions",
                "class::imported_member_type_inheritance",
                "class::method_ranges_of_scope_declarations",
                "class::named_aggregate_type_arguments",
                "class::package_constant_class_defaults",
                "class::package_constant_type_arguments",
                "class::type_alias_lexical_scopes",
                "class::type_alias_members",
                "class::type_parameter_imports",
                "class::type_parameter_shadowing",
                "fork::compound_event_activations_outlive_parent",
                "fork::compound_event_four_state_lsb_edges",
                "fork::compound_event_function_scratch_isolated",
                "fork::compound_event_tracks_result_not_operands",
                "fork_control::function_join_none_automatic_values_outlive_returns",
                "fork_control::function_join_none_calls_in_defaults_need_process_frames",
                "fork_control::function_join_none_children_can_call_timed_tasks",
                "fork_control::function_join_none_event_driven_caller",
                "fork_control::function_join_none_preserves_expression_temporaries",
                "fork_control::function_join_none_retains_child_only_class_handle",
                "fork_control::process_ancestor_kill_preserves_caller_rng",
                "fork_control::process_external_kill_finishes_program",
                "fork_control::process_final_kill_preserves_cleanup",
                "fork_control::process_final_procedures_share_identity_and_rng",
                "fork_control::process_finished_parent_kills_descendants",
                "fork_control::process_identity_and_status",
                "fork_control::process_mutual_status_waits_advance_to_cancellation",
                "fork_control::process_nested_calls_preserve_level_wait_resume",
                "fork_control::process_program_exit_wakes_sibling_observers",
                "fork_control::process_random_state_and_handles",
                "fork_control::process_resume_after_program_exit_is_noop",
                "fork_control::process_resume_resamples_automatic_event_expression",
                "fork_control::process_self_kill_finishes_program",
                "fork_control::process_self_suspend_and_await",
                "fork_control::process_status_wait_and_disable",
                "fork_control::process_suspend_and_resume_waits",
                "fork_deferred::child_flush_does_not_inherit_parent_reports",
                "fork_deferred::disable_fork_flushes_live_children_only",
                "fork_deferred::four_state_arguments_are_captured_before_child_exit",
                "fork_deferred::reactive_wakeup_preserves_mature_observed_only",
                "fork_deferred::repeated_static_site_keeps_completed_reports",
                "fork_deferred::reused_slot_cannot_flush_completed_reports",
                "fork_deferred::sibling_event_queues_are_independent",
                "fork_deferred::sibling_wait_queues_are_independent",
                "fork_deferred::zero_delay_is_not_a_flush_point",
                "hierarchy::packed_pattern_parameter_arguments_keep_types_and_unknown_bits",
                "hierarchy::packed_pattern_parameter_arguments_preserve_wide_values",
                "hierarchy::packed_pattern_parameters_forward_whole_values",
                "references::const_ref_in_a_continuous_assignment",
                "references::const_ref_reads_the_current_value",
                "references::ref_actual_of_an_equivalent_type",
                "references::ref_array_element_index_is_fixed_at_the_call",
                "references::ref_class_property_names_the_object_of_the_call",
                "references::ref_default_and_named_arguments",
                "references::ref_element_after_an_input_in_forked_activations",
                "references::ref_element_select_past_the_element",
                "references::ref_elements_of_concurrent_task_activations",
                "references::ref_fills_caller_queues",
                "references::ref_formals_of_one_actual_alias_each_other",
                "references::ref_property_forwarded_to_output_and_inout",
                "references::ref_property_of_this",
                "references::ref_replaces_a_class_handle",
                "references::ref_static_class_properties",
                "references::ref_task_keeps_a_property_while_suspended",
                "references::ref_task_keeps_the_reference_while_suspended",
                "references::ref_writes_are_seen_before_the_return",
                "tagged_unions::casts_ports_and_registers",
                "tagged_unions::conditional_struct_payload",
                "tagged_unions::constructors_and_checked_members",
                "tagged_unions::default_arguments",
                "tagged_unions::factory_and_union_arguments",
                "tagged_unions::nested_and_void_constructors",
                "tagged_unions::wide_four_state_payload",
                "tagged_unions::writable_system_arguments",
                "tagged_unions::writable_task_arguments",
                "timing::final_procedure_output_survives_program_completion",
                "unions::packed_mixed_state",
                "unions::soft_overlay",
                "unions::soft_signed_ports",
                "unions::soft_wide_four_state",
              ]
            : [
                "class::foreign_generic_constructor_lexical_scope",
                "class::foreign_generic_declaration_scope_and_equivalent_defaults",
                "class::foreign_generic_inherited_constants",
                "class::foreign_generic_nominal_and_nested_types",
                "class::generate_scope_enum_type_arguments",
                "fork::compound_event_activations_outlive_parent",
                "fork_control::process_ancestor_kill_preserves_caller_rng",
                "fork_control::process_external_kill_finishes_program",
                "fork_control::process_finished_parent_kills_descendants",
                "fork_control::process_mutual_status_waits_advance_to_cancellation",
                "fork_control::process_nested_calls_preserve_level_wait_resume",
                "fork_control::process_program_exit_wakes_sibling_observers",
                "fork_control::process_resume_after_program_exit_is_noop",
                "fork_control::process_resume_resamples_automatic_event_expression",
                "fork_control::process_self_kill_finishes_program",
                "fork_control::process_self_suspend_and_await",
                "fork_control::process_status_wait_and_disable",
                "fork_control::process_suspend_and_resume_waits",
                "fork_deferred::disable_fork_flushes_live_children_only",
                "fork_deferred::reactive_wakeup_preserves_mature_observed_only",
                "fork_deferred::repeated_static_site_keeps_completed_reports",
                "fork_deferred::reused_slot_cannot_flush_completed_reports",
                "fork_deferred::sibling_event_queues_are_independent",
                "fork_deferred::sibling_wait_queues_are_independent",
                "operators::two_state_case_equality_compares_literal_structure",
                "operators::two_state_case_equality_keeps_unknown_expression_bits",
                "operators::two_state_case_equality_preserves_conditional_unknowns",
                "operators::two_state_case_equality_preserves_unknown_literals",
                "references::ref_actual_of_an_equivalent_type",
                "references::ref_array_element_index_is_fixed_at_the_call",
                "references::ref_class_property_names_the_object_of_the_call",
                "tagged_unions::conditional_struct_payload",
                "tagged_unions::constructors_and_checked_members",
                "tagged_unions::default_arguments",
                "tagged_unions::factory_and_union_arguments",
                "tagged_unions::nested_and_void_constructors",
                "tagged_unions::writable_system_arguments",
                "tagged_unions::writable_task_arguments",
                "timing::final_procedure_output_survives_program_completion",
                "unpacked_structs::reference_formal_handles_survive_heap_collections",
                "unpacked_structs::reference_formals",
              ],
        );
      } else {
        assert.equal(result.accepted_failures.length, tool === "icarus" ? 11 : 2);
      }
    }
  }
});
