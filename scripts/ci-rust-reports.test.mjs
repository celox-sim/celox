import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import test from "node:test";
import { clearCurrentReport, resetReports, saveCurrentReport } from "./ci-rust-reports.mjs";

function fixture(t) {
  const root = mkdtempSync(join(tmpdir(), "celox-ci-reports-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  return { source: join(root, "junit.xml"), directory: join(root, "reports") };
}

test("each scoped run keeps its own report, including failures", (t) => {
  const { source, directory } = fixture(t);
  const passed = '<testsuites><testsuite tests="1" failures="0"/></testsuites>';
  const failed = '<testsuites><testsuite tests="1" failures="1"/></testsuites>';
  for (const [name, text] of [["0", passed], ["1", failed]]) {
    writeFileSync(source, text);
    const saved = saveCurrentReport(directory, name, source);
    assert.equal(readFileSync(saved, "utf8"), text);
    assert.ok(!existsSync(source));
  }
  assert.deepEqual(readdirSync(directory).sort(), ["nextest-0.xml", "nextest-1.xml"]);
  assert.equal(readFileSync(join(directory, "nextest-0.xml"), "utf8"), passed);
  // A subsequent compilation failure must not relabel an earlier report.
  clearCurrentReport(source);
  assert.equal(saveCurrentReport(directory, "2", source), undefined);
  assert.equal(readdirSync(directory).length, 2);
});

test("initialization removes only this job's stale reports and creates no target directory", (t) => {
  const { source, directory } = fixture(t);
  resetReports(directory, source);
  assert.ok(!existsSync(directory));
  mkdirSync(directory);
  for (const name of ["nextest-full.xml", "nextest-0.xml", "unrelated.xml"])
    writeFileSync(join(directory, name), "old");
  writeFileSync(source, "old");
  resetReports(directory, source);
  assert.deepEqual(readdirSync(directory), ["unrelated.xml"]);
  assert.ok(!existsSync(source));
});

test("full runs use a stable report name and reject invalid paths", (t) => {
  const { source, directory } = fixture(t);
  writeFileSync(source, "<testsuites/>");
  assert.throws(() => saveCurrentReport(directory, "../escape", source), /Invalid Rust report name/);
  assert.ok(existsSync(source));
  assert.equal(saveCurrentReport(directory, "full", source), join(directory, "nextest-full.xml"));
});
