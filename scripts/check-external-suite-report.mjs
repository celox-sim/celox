import assert from "node:assert/strict";
import { pathToFileURL } from "node:url";
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
const successes = new Set(["passed", "rejected", "unsupported", "ignored"]);

function rowsByName(rows, label) {
  assert.ok(
    Array.isArray(rows) && rows.length > 0,
    `${label}: missing/empty cases`,
  );
  const result = new Map();
  for (const row of rows) {
    assert.equal(typeof row.name, "string", `${label}: missing case name`);
    assert.ok(!result.has(row.name), `${label}: duplicate case ${row.name}`);
    result.set(row.name, row);
  }
  return result;
}

function validateReport(report, label) {
  assert.equal(report.schema_version, 3, `${label}: unsupported report schema`);
  const rows = rowsByName(report.cases, label);
  const counts = Object.fromEntries(statuses.map((status) => [status, 0]));
  for (const row of rows.values()) {
    assert.ok(
      statuses.includes(row.status),
      `${label}: unknown status for ${row.name}`,
    );
    assert.equal(
      typeof row.phase,
      "string",
      `${label}: missing phase for ${row.name}`,
    );
    assert.equal(
      typeof row.detail,
      "string",
      `${label}: missing diagnostic for ${row.name}`,
    );
    counts[row.status]++;
  }
  // Retained reports omit counts so that added cases do not conflict.
  if (report.counts !== undefined)
    assert.deepEqual(
      report.counts,
      counts,
      `${label}: result counts do not match cases`,
    );
  return rows;
}

function diagnostic(detail) {
  // Verilator aligns continuation lines to the original absolute source path.
  // The runner replaces that path with <case>, but the padding survives.
  // Ignore only outer line whitespace/line endings, never diagnostic wording,
  // source locations, or error codes.
  return detail
    .replaceAll("\r\n", "\n")
    .split("\n")
    .map((line) => line.trim())
    .join("\n")
    .trim();
}

// A retained failure is evidence of a limitation, not a passing assertion.
// Keep it in the fresh report and accept only its exact prior classification
// and diagnostic; new cases/failure modes must still fail the daily gate.
export function checkExternalSuiteReport(
  current,
  baseline,
  catalogue,
  exitStatus,
  suite,
) {
  const rows = validateReport(current, "current report");
  const previous = validateReport(baseline, "baseline report");
  assert.ok(
    ["verilator", "icarus"].includes(current.tool),
    "unknown simulator",
  );
  assert.equal(
    current.tool,
    baseline.tool,
    "baseline is for another simulator",
  );
  assert.ok(["veryl", "sv"].includes(suite), "unknown suite");
  assert.equal(current.suite, suite, "report is for another suite");
  assert.equal(
    current.incremental,
    false,
    "daily comparison requires a fresh run",
  );
  assert.equal(
    current.include_ignored,
    false,
    "daily comparison must preserve reviewed exclusions",
  );
  assert.equal(
    current.exclude_stronger_than_sv,
    false,
    "daily comparison must select every case",
  );
  assert.deepEqual(
    current.run_counts,
    { fresh: rows.size, reused: 0 },
    "daily comparison cannot reuse cached results",
  );
  assert.equal(catalogue.schema_version, 3, "unsupported catalogue schema");
  assert.equal(
    catalogue.exclude_stronger_than_sv,
    false,
    "catalogue excludes cases",
  );
  const selected = rowsByName(catalogue.cases, "catalogue");
  assert.deepEqual(
    [...rows.keys()].sort(),
    [...selected.keys()].sort(),
    "report does not cover the complete catalogue",
  );
  for (const row of rows.values()) {
    assert.equal(row.reused, false, `cached evidence for ${row.name}`);
    const expected = selected.get(row.name);
    for (const field of [
      "category",
      "expectation",
      "stronger_than_sv",
      "tags",
      "tag_reasons",
    ]) {
      assert.deepEqual(
        row[field],
        expected[field],
        `catalogue metadata differs for ${row.name}: ${field}`,
      );
    }
  }
  const failures = [...rows.values()].filter(
    (row) => !successes.has(row.status),
  );
  assert.equal(
    exitStatus,
    failures.length === 0 ? 0 : 1,
    "runner exit status does not match its report",
  );
  const unexpected = [];
  const accepted = [];
  for (const row of failures) {
    const old = previous.get(row.name);
    if (
      current.version === baseline.version &&
      old &&
      !successes.has(old.status) &&
      ["status", "phase", "expectation"].every(
        (field) => row[field] === old[field],
      ) &&
      diagnostic(row.detail) === diagnostic(old.detail)
    ) {
      accepted.push(row.name);
    } else {
      unexpected.push(`${row.name}: ${row.status}/${row.phase}: ${row.detail}`);
    }
  }
  assert.equal(
    unexpected.length,
    0,
    `new or changed external verification failures:\n${unexpected.join("\n")}`,
  );
  return { cases: rows.size, accepted_failures: accepted };
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  const [reportPath, baselinePath, cataloguePath, status, suite] =
    process.argv.slice(2);
  try {
    assert.ok(
      reportPath &&
        baselinePath &&
        cataloguePath &&
        /^[01]$/.test(status) &&
        suite,
      "usage: check-external-suite-report.mjs REPORT BASELINE CATALOGUE EXIT_STATUS SUITE",
    );
    const result = checkExternalSuiteReport(
      readVerificationReport(reportPath),
      readVerificationReport(baselinePath),
      readVerificationReport(cataloguePath),
      Number(status),
      suite,
    );
    console.log(
      `Verified ${result.cases} cases; ${result.accepted_failures.length} unchanged retained failures.`,
    );
    for (const name of result.accepted_failures)
      console.log(`retained failure: ${name}`);
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
