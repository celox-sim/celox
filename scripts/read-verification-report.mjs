import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { basename, dirname, extname, join } from "node:path";
import { fileURLToPath } from "node:url";

const identifier = /^[A-Za-z0-9_-]+$/;
const read = (path) => JSON.parse(readFileSync(path, "utf8"));

// Schema 4 splits retained evidence by test group. Consumers keep using the
// logical schema-3 report; original historical and per-run reports still load.
export function readVerificationReport(path) {
  if (path instanceof URL) path = fileURLToPath(path);
  const report = read(path);
  if (report.schema_version !== 4) return report;
  assert.ok(!Object.hasOwn(report, "cases"), "split index contains cases");
  const stem = basename(path, extname(path));
  assert.ok(identifier.test(stem), "invalid report filename");
  assert.ok(
    Array.isArray(report.case_files) && report.case_files.length > 0,
    "split report has no case_files",
  );
  const cases = [];
  const names = new Set();
  let previous = "";
  for (const file of report.case_files) {
    assert.ok(typeof file === "string", "case file must be a string");
    assert.ok(file > previous, "case_files must be sorted and unique");
    previous = file;
    assert.ok(
      file.startsWith(`${stem}/`) && file.endsWith(".json"),
      `invalid case file: ${file}`,
    );
    const group = file.slice(stem.length + 1, -5);
    assert.ok(identifier.test(group), `invalid case file: ${file}`);
    const shard = read(join(dirname(path), file));
    assert.ok(
      Array.isArray(shard.cases) && shard.cases.length > 0,
      `case file has no cases: ${file}`,
    );
    for (const row of shard.cases) {
      assert.ok(
        typeof row.name === "string" &&
          row.name.startsWith(`${group}::`),
        `case does not belong in ${file}`,
      );
      assert.ok(!names.has(row.name), `duplicate case: ${row.name}`);
      names.add(row.name);
      cases.push(row);
    }
  }
  cases.sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
  delete report.case_files;
  return { ...report, schema_version: 3, cases };
}
