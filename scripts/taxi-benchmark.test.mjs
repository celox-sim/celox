import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { taxiMetrics } from "./publish-taxi-bench.mjs";

const manifest = JSON.parse(readFileSync(new URL("../benchmarks/taxi/manifest.json", import.meta.url)));
function fixture() {
  return { schemaVersion: 1, case: "lfsr64", contract: manifest.contract, status: "passed", body_clock: "cocotb-regression-perf_counter-v1",
    taxi_revision: manifest.revision, celox_commit: "commit", seed: manifest.seed,
    expected_tests: 1, requested_samples: 3, host: { architecture: "x86_64" },
    cocotb_version: "2.0.1", verilator_version: "Verilator 5.052 2026-04-01", celox_binary_sha256: "a".repeat(64),
    parameters: manifest.cases.lfsr64.parameters,
    source_sha256: Object.fromEntries([...manifest.cases.lfsr64.sources,
      `${manifest.cases.lfsr64.testDirectory}/${manifest.cases.lfsr64.module}.py`].map(p => [p, "b".repeat(64)])),
    configuration: { celox: "native O1 two-state", verilator: "--vpi --public-flat-rw; C++ default -Os" },
    compilation_seconds: { celox: 1, verilator: 2 },
    pairs: [1, 2, 3].map(n => ({
      celox: { body_seconds: n, wall_seconds: n + .5, simulation_ns: { "tb/frame": 100 } },
      verilator: { body_seconds: 2, wall_seconds: 3, simulation_ns: { "tb/frame": 100 } },
    })) };
}

test("Taxi parser rejects missing, skipped, failed, duplicate and mismatched test results", () => {
  const result = spawnSync("python3", ["-m", "unittest", "discover", "-s", "scripts/tests", "-p", "test_taxi_bench.py", "-v"],
    { encoding: "utf8", env: { ...process.env, PYTHONDONTWRITEBYTECODE: "1" } });
  assert.equal(result.status, 0, result.stdout + result.stderr);
});

test("Taxi metrics keep body, process and compilation separate for both engines", () => {
  const metrics = taxiMetrics(fixture(), "commit");
  assert.equal(metrics.length, 6);
  assert.deepEqual(metrics.map(m => m.value), [2e6, 2.5e6, 1e6, 2e6, 3e6, 2e6]);
  assert.ok(metrics.every(m => m.unit === "us"));
  assert.equal(JSON.parse(metrics[0].extra).medianPairedBodyRatio, 1);
  assert.equal(metrics[0].name, "taxi-celox/taxi_lfsr64_v1_x64_body");
});

test("Taxi publication rejects stale, partial or invalid comparisons", () => {
  const mutations = [r => r.status = "failed", r => r.status = "incomplete", r => r.case = "unknown",
    r => r.taxi_revision = "wrong", r => r.celox_commit = "stale", r => r.contract = "old", r => r.body_clock = "time.time",
    r => r.parameters = {}, r => r.source_sha256 = {}, r => r.configuration.celox = "O3",
    r => r.seed++, r => r.expected_tests++, r => r.requested_samples--, r => r.pairs.pop(),
    r => r.pairs[0].celox.body_seconds = NaN, r => r.pairs[0].celox.wall_seconds = 0,
    r => r.pairs[0].celox.body_seconds = 100, r => r.pairs[0].verilator.simulation_ns = {},
    r => r.pairs[0].verilator.simulation_ns = { other: 100 },
    r => r.pairs[0].verilator.simulation_ns["tb/frame"]++,
    r => r.compilation_seconds.celox = -1, r => r.compilation_seconds.celox = 1e308, r => r.host.architecture = "aarch64",
    r => r.celox_binary_sha256 = "", r => r.cocotb_version = "other", r => r.verilator_version = "latest"];
  for (const mutate of mutations) {
    const report = fixture(); mutate(report);
    assert.throws(() => taxiMetrics(report, "commit"));
  }
});

test("Taxi daily compares complete configurations on the same VM and publishes only master successes", () => {
  const workflow = readFileSync(new URL("../.github/workflows/taxi-bench.yml", import.meta.url), "utf8");
  assert.match(workflow, /schedule:[\s\S]+workflow_dispatch:/);
  assert.doesNotMatch(workflow, /^\s+(pull_request|push|merge_group):/m);
  assert.match(workflow, /cancel-in-progress: false/);
  assert.match(workflow, /fail-fast: false/);
  assert.match(workflow, /case: \[lfsr64, phy64, rx16, rx8, rx32\]/);
  assert.match(workflow, /version: "5\.052"/);
  assert.match(workflow, /needs: comparison\n    if: github.ref == 'refs\/heads\/master'/);
  assert.doesNotMatch(workflow, /continue-on-error|--no-verify/);
  assert.match(workflow, /if: always\(\)[\s\S]+retention-days: 30/);
});


test("Taxi publication guard and validate-only path never write external state", () => {
  for (const [ref, event] of [["master", "pull_request"], ["develop", "workflow_dispatch"]]) {
    const result = spawnSync(process.execPath, ["scripts/publish-taxi-bench.mjs"], {
      encoding: "utf8", env: { ...process.env, GITHUB_REF: `refs/heads/${ref}`, GITHUB_EVENT_NAME: event },
    });
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /requires a scheduled or manual master run/);
  }
  const directory = mkdtempSync(join(tmpdir(), "taxi-validate-"));
  try {
    const report = fixture();
    report.celox_commit = execFileSync("git", ["rev-parse", "HEAD"], { encoding: "utf8" }).trim();
    const resultPath = join(directory, "result.json");
    const hostPath = join(directory, "host.txt");
    writeFileSync(resultPath, JSON.stringify(report));
    writeFileSync(hostPath, "Model name: Fixture CPU\n");
    const result = spawnSync(process.execPath, ["scripts/publish-taxi-bench.mjs", resultPath, hostPath, "--validate-only"], {
      encoding: "utf8", env: { ...process.env, GITHUB_REF: "refs/heads/master", GITHUB_EVENT_NAME: "workflow_dispatch" },
    });
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /Validated 6 Taxi metrics/);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});
