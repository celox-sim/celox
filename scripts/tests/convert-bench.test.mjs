import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

const converter = new URL("../convert-bench.mjs", import.meta.url);
function convert(report, callback) {
  const dir = mkdtempSync(join(tmpdir(), "convert-bench-"));
  try {
    const input = join(dir, "input.json");
    const output = join(dir, "output.json");
    writeFileSync(input, JSON.stringify(report));
    callback(input, output);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}
function report(tasks) {
  return {
    success: true,
    testResults: [{ assertionResults: [{ benchmarks: [{ name: "group", tasks }] }] }],
  };
}
const measurement = {
  name: "simulation_tick",
  latency: { mean: 0.125, rme: 1.25, samplesCount: 42 },
};

test("converts Vitest 5 task latency and preserves benchmark names", () => {
  convert(report([measurement, { ...measurement, name: "second" }]), (input, output) => {
    execFileSync(process.execPath, [converter.pathname, input, output]);
    assert.deepEqual(JSON.parse(readFileSync(output, "utf8")), [
      { name: "ts/simulation_tick", unit: "us", value: 125, range: "± 1.3%", extra: "42 samples" },
      { name: "ts/second", unit: "us", value: 125, range: "± 1.3%", extra: "42 samples" },
    ]);
  });
});

for (const [name, raw] of [
  ["failed", { ...report([measurement]), success: false }],
  ["empty", report([])],
  ["legacy", { files: [] }],
  ["invalid statistics", report([{ ...measurement, latency: { mean: null, rme: 0, samplesCount: 3 } }])],
]) {
  test(`rejects ${name} reports`, () => {
    convert(raw, (input, output) => {
      const result = spawnSync(process.execPath, [converter.pathname, input, output]);
      assert.notEqual(result.status, 0);
    });
  });
}
