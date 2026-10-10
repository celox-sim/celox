import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { checkExternalSuiteReport } from "./check-external-suite-report.mjs";

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

test("all four retained baselines accept only their existing failures", () => {
  for (const suite of ["veryl", "sv"]) {
    for (const tool of ["verilator", "icarus"]) {
      const file = `../crates/celox-test-suite/verification/${suite === "sv" ? "sv/" : ""}${tool}.json`;
      const baseline = JSON.parse(
        readFileSync(new URL(file, import.meta.url), "utf8"),
      );
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
      assert.equal(
        result.accepted_failures.length,
        suite === "sv" ? 0 : tool === "icarus" ? 11 : 2,
      );
    }
  }
});
