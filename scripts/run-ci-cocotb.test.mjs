import assert from "node:assert/strict";
import test from "node:test";
import { spawnSync } from "node:child_process";
import { chmodSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { cocotbCommands, cocotbFilter } from "./run-ci-cocotb.mjs";
import { cargoCommands } from "./run-ci-rust-scope.mjs";

test("cocotb preserves build resolution and selects only its ignored tests", () => {
  const builds = [
    { packages: ["celox-heap"], targets: [], features: [] },
    { packages: ["celox", "celox-vpi"], targets: [], features: ["celox/systemverilog"] },
  ];
  const original = structuredClone(builds);
  const expected = cargoCommands("test", builds, cocotbFilter)[1]
    .filter(arg => arg !== "--no-tests=pass");
  assert.deepEqual(cocotbCommands("false", builds), [[...expected, "--run-ignored", "only"]]);
  assert.deepEqual(builds, original);
  for (const full of ["true", "", undefined]) {
    const [args] = cocotbCommands(full, null);
    assert.ok(args.includes("--workspace"));
    assert.ok(args.includes("vtest"));
    assert.ok(!args.includes("--no-tests=pass"));
    assert.deepEqual(args.slice(-4), ["--run-ignored", "only", "-E", cocotbFilter]);
  }
});

test("missing or wrong VPI targets fail closed", () => {
  for (const builds of [[], [{ packages: ["celox"], targets: [], features: [] }],
    [{ packages: ["celox-vpi"], targets: ["other_test"], features: [] }]])
    assert.throws(() => cocotbCommands("false", builds));
  const [args] = cocotbCommands("false", [{ packages: ["celox-vpi"], targets: ["cocotb_e2e"], features: [] }]);
  assert.equal(args[args.indexOf("--test") + 1], "cocotb_e2e");
});

test("cocotb requires Python and propagates Cargo failures without running later plans", () => {
  const directory = mkdtempSync(join(tmpdir(), "celox-cocotb-command-"));
  try {
    const executable = join(directory, "cargo");
    const log = join(directory, "calls");
    writeFileSync(executable, '#!/bin/sh\necho called >> "$COCOTB_CALL_LOG"\nexit 17\n');
    chmodSync(executable, 0o755);
    const build = { packages: ["celox-vpi"], targets: [], features: [] };
    const env = { ...process.env, RUST_FULL: "false", BUILDS: JSON.stringify([build, build]),
      PATH: `${directory}:${process.env.PATH}`, COCOTB_CALL_LOG: log,
      CELOX_CI_REPORT_DIR: "", CELOX_COCOTB_PYTHON: "" };
    const missing = spawnSync(process.execPath, ["scripts/run-ci-cocotb.mjs"], { env, encoding: "utf8" });
    assert.notEqual(missing.status, 0);
    assert.match(missing.stderr, /Python interpreter is required/);
    const failed = spawnSync(process.execPath, ["scripts/run-ci-cocotb.mjs"], {
      env: { ...env, CELOX_COCOTB_PYTHON: "/test/python" }, encoding: "utf8",
    });
    assert.equal(failed.status, 17);
    assert.equal(readFileSync(log, "utf8"), "called\n");
  } finally { rmSync(directory, { recursive: true, force: true }); }
});
