import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { readVerificationReport } from "./read-verification-report.mjs";

test("split evidence preserves metadata and fails on missing or misfiled cases", () => {
  const directory = mkdtempSync(join(tmpdir(), "celox-split-report-"));
  const path = join(directory, "icarus.json");
  const shard = join(directory, "icarus/counter.json");
  const write = (path, value) => writeFileSync(path, JSON.stringify(value));
  const row = { name: "counter::increment", status: "compile_error", detail: "retained diagnostic" };
  const metadata = { schema_version: 3, tool: "icarus", version: "fixture" };
  try {
    const report = { ...metadata, cases: [row] };
    write(path, report);
    assert.deepEqual(readVerificationReport(path), report);
    mkdirSync(join(directory, "icarus"));
    const index = { ...metadata, schema_version: 4, case_files: ["icarus/counter.json"] };
    write(path, index);
    assert.throws(() => readVerificationReport(path), /ENOENT/);
    write(shard, { cases: [row] });
    assert.deepEqual(readVerificationReport(path), report);
    for (const cases of [[], [row, row], [{ name: "operators::negative" }]]) {
      write(shard, { cases });
      assert.throws(() => readVerificationReport(path));
    }
    for (const case_files of [[], ["icarus/../outside.json"], ["icarus/counter.json", "icarus/counter.json"]]) {
      write(path, { ...index, case_files });
      assert.throws(() => readVerificationReport(path));
    }
    write(path, { ...index, cases: [row] });
    assert.throws(() => readVerificationReport(path), /index contains cases/);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test("index filenames are unrestricted and references support hashed directories", () => {
  const directory = mkdtempSync(join(tmpdir(), "celox-report-filenames-"));
  const row = { name: "counter::increment", status: "passed", detail: "" };
  const report = { schema_version: 3, cases: [row] };
  try {
    for (const namespace of ["icarus", `.report-${"a".repeat(64)}`]) {
      mkdirSync(join(directory, namespace));
      writeFileSync(join(directory, namespace, "counter.json"), JSON.stringify({ cases: [row] }));
      const index = { schema_version: 4, case_files: [`${namespace}/counter.json`] };
      for (const filename of ["nightly.icarus.json", "nightly report.json", "検証.json", "extensionless"]) {
        const path = join(directory, filename);
        writeFileSync(path, JSON.stringify(index));
        assert.deepEqual(readVerificationReport(path), report);
      }
    }
    writeFileSync(join(directory, "icarus/operators.json"), JSON.stringify({ cases: [{ name: "operators::negative" }] }));
    const path = join(directory, "icarus.json");
    for (const case_files of [
      [".report-bad/counter.json"],
      [".report-../counter.json"],
      [`${`.report-${"a".repeat(64)}`}/counter.json`, "icarus/operators.json"],
    ]) {
      writeFileSync(path, JSON.stringify({ schema_version: 4, case_files }));
      assert.throws(() => readVerificationReport(path));
    }
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});
