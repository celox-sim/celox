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
                "class::imported_member_type_dimensions",
                "class::imported_member_type_inheritance",
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
                "tagged_unions::casts_ports_and_registers",
                "tagged_unions::conditional_struct_payload",
                "tagged_unions::constructors_and_checked_members",
                "tagged_unions::default_arguments",
                "tagged_unions::factory_and_union_arguments",
                "tagged_unions::nested_and_void_constructors",
                "tagged_unions::wide_four_state_payload",
                "tagged_unions::writable_system_arguments",
                "tagged_unions::writable_task_arguments",
                "unions::packed_mixed_state",
                "unions::soft_overlay",
                "unions::soft_signed_ports",
                "unions::soft_wide_four_state",
              ]
            : [
                "fork::compound_event_activations_outlive_parent",
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
                "tagged_unions::conditional_struct_payload",
                "tagged_unions::constructors_and_checked_members",
                "tagged_unions::default_arguments",
                "tagged_unions::factory_and_union_arguments",
                "tagged_unions::nested_and_void_constructors",
                "tagged_unions::writable_system_arguments",
                "tagged_unions::writable_task_arguments",
              ],
        );
      } else {
        assert.equal(result.accepted_failures.length, tool === "icarus" ? 11 : 2);
      }
    }
  }
});
