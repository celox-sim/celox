import assert from "node:assert/strict";
import test from "node:test";
import { execFileSync } from "node:child_process";
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { collection, measurementSource, parseResult, runCollection, select } from "./benchmark-collection.mjs";

const valid = "CELOX_TEST_TIMING test=tb compile_ns=10 execute_ns=20 jit_execute_ns=15\nCELOX_TEST_RESULT test=tb status=pass elapsed_ns=35\n";
test("only complete, unambiguous successful runs yield timing samples", () => {
  assert.equal(parseResult(valid, "tb", 0).status, "passed");
  for (const [log, code, signal, error] of [
    [valid, 1], [valid, 0, "SIGTERM"], [valid, 0, null, new Error("timeout")],
    [valid + valid, 0], [valid.replace("status=pass", "status=compile-only"), 0],
    [valid.replace("status=pass", "status=tick-limit"), 0],
    [valid.replace("elapsed_ns=35", "elapsed_ns=2"), 0],
    [valid.replaceAll("test=tb", "test=another"), 0],
    [valid.replace("execute_ns=20", "execute_ns=9007199254740992"), 0],
  ]) assert.equal(parseResult(log, "tb", code, signal, error).status, "failed");
});

test("catalog keeps frontend and TB provenance distinct", () => {
  assert.deepEqual(select("all").map(w => w.id), ["sha256", "fft", "lz77"]);
  assert.throws(() => select("sha256,sha256"));
  assert.throws(() => select("tarunes"), /measurement recipe/);
  for (const w of select("all")) {
    assert.equal(w.testbenches[0].language, "veryl");
    assert.equal(w.testbenches[1].repository, "logikbench-original");
    assert.match(w.testbenches[1].acceptance, /not a paired frontend comparison/);
    assert.equal(w.testbenches[2].purpose, "equivalence");
  }
});

test("measurement preserves assertions and finishes only after the reviewed work count", () => {
  const input = "initial { $assert(ok); $finish(); }";
  const output = measurementSource(input, select("sha256")[0]);
  assert.match(output, /for _benchmark_round in 0\.\.4096/);
  assert.match(output, /\$assert\(ok\)/);
  assert.match(output, /COLLECTION_COMPLETE nist-pairs-v1 4096/);
  assert.throws(() => measurementSource(input, {}), /Missing reviewed/);
  assert.throws(() => measurementSource(input + input, select("sha256")[0]), /shape changed/);
});

test("runner rejects broken-oracle false passes and never reuses stale output", () => {
  const temp = mkdtempSync(join(tmpdir(), "collection-test-"));
  try {
    const source = join(temp, "fixture");
    mkdirSync(join(source, "circuit/src"), { recursive: true });
    writeFileSync(join(source, "circuit/src/tb.veryl"), "initial { $assert(good); $finish(); }");
    writeFileSync(join(source, "circuit/Veryl.toml"), "[project]\nname='fixture'\nversion='0.1.0'\n");
    const git = (...args) => execFileSync("git", ["-C", source, ...args], { encoding: "utf8" }).trim();
    git("init", "--quiet"); git("add", ".");
    git("-c", "user.name=Test", "-c", "user.email=test@example.invalid", "-c", "commit.gpgsign=false", "commit", "--quiet", "-m", "fixture");
    collection.repositories.fixture = { revision: git("rev-parse", "HEAD") };
    const binary = join(temp, "celox");
    const script = `#!/usr/bin/env node
import { readFileSync } from 'node:fs';
const tb = readFileSync(process.argv[process.argv.indexOf('--project') + 1] + '/src/tb.veryl', 'utf8');
const negative = tb.includes('$assert(bad)');
if (negative) { console.log((process.argv.includes('--aot-c-async') ? 'VERYL' : 'CELOX') + '_TEST_RESULT test=tb status=fail elapsed_ns=35'); console.error('assertion failed'); process.exit(1); }
const veryl = process.argv.includes('--aot-c-async');
console.log(veryl ? ${JSON.stringify(valid.replaceAll('CELOX_', 'VERYL_').replace(' jit_execute_ns=15',''))} : ${JSON.stringify(valid)});
console.log(veryl ? 'VERYL_TEST_CONFIG test=tb backend=cc aot_c_async=true compile_only=false\\nVERYL_TIERED_STATS test=tb compiled_dispatches=0 fallback_dispatches=1' : 'CELOX_TIERED_STATS test=tb tier=interpreter promotion=pending interpreted_evaluations=1 compiled_evaluations=0');
if(tb.includes('COLLECTION_COMPLETE')) console.log('COLLECTION_COMPLETE fixture-v1 2');
`;
    writeFileSync(binary, script); chmodSync(binary, 0o755);
    const workload = { id: "fixture", repository: "fixture", project: "circuit", top: "tb", workload: "smoke-v1",
      testbenches: [{ id: "smoke-v1", path: "circuit/src/tb.veryl" }], negativeControl: { find: "good", replace: "bad" },
      measurement: { id: "fixture-v1", repetitions: 2, unitsPerRepetition: 1, unit: "frames" } };
    const options = { workloads: [workload], sourceDirectory: temp, celox: binary, veryl: binary, output: join(temp, "qualified") };
    const qualified = runCollection(options);
    assert.deepEqual(qualified.results.map(r => r.status), ["qualified", "qualified"]);
    assert.deepEqual(qualified.results[0].samples, []);
    assert.throws(() => runCollection(options), /fresh output/);
    const measured = runCollection({ ...options, mode: "measure", output: join(temp, "measured"), samples: 2 });
    assert.deepEqual(measured.results.map(r => r.status), ["measured", "measured"]);
    assert.equal(measured.results[0].samples.length, 2);
    assert.equal(measured.results[0].workUnits, 2);
    assert.match(measured.results[0].testbenchSha256, /^[0-9a-f]{64}$/);
    writeFileSync(binary, script.replace("if (negative)", "if (false)"));
    const failed = runCollection({ ...options, mode: "measure", output: join(temp, "failed") });
    assert.equal(failed.results[0].status, "failed");
    assert.deepEqual(failed.results[0].samples, []);
    assert.equal(JSON.parse(readFileSync(join(temp, "failed/results.json"))).results[0].status, "failed");
  } finally {
    delete collection.repositories.fixture;
    rmSync(temp, { recursive: true, force: true });
  }
});
