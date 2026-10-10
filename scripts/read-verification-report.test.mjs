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
