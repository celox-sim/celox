import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { metrics } from "./publish-collection-bench.mjs";
import { collection } from "./benchmark-collection.mjs";

function report() {
  return { schemaVersion: 1, mode: "measure", host: { cpu: "test CPU", arch: "x64" }, results: [{
    circuit: "sha256", status: "measured", revision: collection.repositories.logikbench.revision,
    qualification: { status: "passed" }, negativeControl: { status: "passed" },
    workload: "nist-pairs-v1", testbench: "collection-nist-pairs-v1", testbenchLanguage: "veryl",
    frontend: "veryl", backend: "native", optimization: "O2", stateModel: "two-state", threads: 1,
    workUnits: 8192, workUnit: "hashes", sourceSha256: "a".repeat(64), testbenchSha256: "b".repeat(64),
    samples: [30, 10, 20].map(n => ({ status: "passed", compileNs: n, executeNs: n * 2, totalNs: n * 3 })),
  }] };
}

test("publish medians only and separate every measurement contract", () => {
  const input = report();
  const benches = metrics(input);
  assert.deepEqual(benches.map(b => b.value), [20, 40, 60]);
  assert.equal(benches[0].unit, "ns");
  assert.equal(JSON.parse(benches[0].extra).frontend, "veryl");
  input.results[0].testbenchSha256 = "c".repeat(64);
  assert.notEqual(metrics(input)[0].name, benches[0].name);
  input.host.arch = "arm64";
  assert.match(metrics(input)[0].name, /_arm64_compilation$/);
});

test("qualification, failures, partial samples and altered runner contracts cannot publish", () => {
  for (const change of [
    r => r.mode = "qualify", r => r.results[0].status = "failed",
    r => r.results[0].negativeControl.status = "failed", r => r.results[0].qualification.status = "failed",
    r => r.results[0].samples.pop(), r => r.results[0].samples[0].status = "failed",
    r => r.results[0].samples[0].executeNs = NaN, r => r.results[0].workUnits = 2,
    r => r.results[0].revision = "0".repeat(40), r => r.results[0].frontend = "sv",
    r => r.results[0].testbenchLanguage = "systemverilog", r => r.results[0].stateModel = "four-state",
    r => r.results[0].optimization = "O0", r => r.results[0].threads = 2,
    r => r.results.push(r.results[0]),
  ]) {
    const input = report(); change(input); assert.throws(() => metrics(input));
  }
});

test("workflow retains failed logs, pairs collection on one host and gates publication", () => {
  const workflow = readFileSync(new URL("../.github/workflows/circuit-bench.yml", import.meta.url), "utf8");
  assert.match(workflow, /cargo build --release --locked -p celox-vpi --bin celox/);
  assert.match(workflow, /measure all work\/circuits\/sources work\/circuits\/results target\/release\/celox/);
  assert.match(workflow, /if: always\(\)/);
  assert.match(workflow, /needs: circuits\n    if: github.ref == 'refs\/heads\/master'/);
  assert.doesNotMatch(workflow, /continue-on-error/);
  assert.doesNotMatch(workflow, /pull_request:|push:/);
});
