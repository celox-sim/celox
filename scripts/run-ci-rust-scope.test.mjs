import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmodSync, mkdtempSync, writeFileSync, readFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import test from "node:test";
import { cargoCommands } from "./run-ci-rust-scope.mjs";

const builds = [
  { packages: ["celox-test-suite"], targets: [], features: ["celox-test-suite/icarus"] },
  { packages: ["celox"], targets: ["param_override"], features: ["celox/systemverilog"] },
];
const filter = "package(celox-test-suite) | binary_id(celox::param_override)";

test("a shared case never loses its Cargo --test restriction to another package", () => {
  for (const mode of ["test", "lint"]) {
    const commands = cargoCommands(mode, builds, filter);
    assert.equal(commands.length, 2);
    assert.ok(commands[0].includes("celox-test-suite"));
    assert.ok(!commands[0].includes("celox"));
    assert.ok(commands[1].includes("celox"));
    assert.ok(!commands[1].includes("celox-test-suite"));
    assert.ok(!commands[0].includes("--test"));
    assert.equal(commands[1][commands[1].indexOf("--test") + 1], "param_override");
    if (mode === "test") assert.equal(commands[1].at(-1), filter);
  }
});

test("empty or invalid plans cannot silently pass CI", () => {
  for (const plan of [[], null, [{ packages: [], targets: [], features: [] }],
    [{ packages: ["--workspace"], targets: [], features: [] }]]) {
    assert.throws(() => cargoCommands("test", plan, filter));
  }
  assert.throws(() => cargoCommands("unknown", builds, filter));
  assert.throws(() => cargoCommands("test", builds, ""));
});

test("runtime loading is enabled only for Celox test builds, without changing scope", () => {
  const original = structuredClone(builds);
  const tests = cargoCommands("test", builds, filter);
  const lint = cargoCommands("lint", builds, filter);
  assert.equal(tests[0][tests[0].indexOf("--features") + 1], "celox-test-suite/icarus");
  assert.equal(tests[1][tests[1].indexOf("--features") + 1],
    "celox/systemverilog,celox-test-suite/runtime-cases");
  assert.equal(lint[1][lint[1].indexOf("--features") + 1], "celox/systemverilog");
  assert.deepEqual(builds, original);
});

test("Celox CI tests reject missing executor setup before launching Cargo", () => {
  const result = spawnSync(process.execPath, ["scripts/run-ci-rust-scope.mjs", "test"], {
    env: { ...process.env, BUILDS: JSON.stringify([builds[1]]), FILTER: filter,
      CELOX_VTEST_RUNNER: "" }, encoding: "utf8",
  });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /require the prepared vtest executor/);
});

test("the runner stops and propagates a failed scoped Cargo invocation", () => {
  const directory = mkdtempSync(join(tmpdir(), "celox-cargo-failure-"));
  try {
    const executable = join(directory, "cargo");
    const log = join(directory, "calls");
    writeFileSync(executable, '#!/bin/sh\necho called >> "$SCOPE_CALL_LOG"\nexit 17\n');
    chmodSync(executable, 0o755);
    const result = spawnSync(process.execPath, ["scripts/run-ci-rust-scope.mjs", "test"], {
      env: { ...process.env, BUILDS: JSON.stringify(builds), FILTER: filter,
        PATH: `${directory}:${process.env.PATH}`, SCOPE_CALL_LOG: log }, encoding: "utf8",
    });
    assert.equal(result.status, 17);
    assert.equal(readFileSync(log, "utf8"), "called\n");
  } finally { rmSync(directory, { recursive: true, force: true }); }
});


test("backend exclusion intersects each scoped test invocation but never lint", () => {
  const backend = "not test(parallel)";
  for (const args of cargoCommands("test", builds, filter, backend)) {
    assert.equal(args.at(-1), `(${filter}) & (${backend})`);
  }
  assert.deepEqual(cargoCommands("lint", builds, filter, backend), cargoCommands("lint", builds, filter));
});
