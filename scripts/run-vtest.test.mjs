import assert from "node:assert/strict";
import test from "node:test";
import { executorPath, prepareCi, run } from "./run-vtest.mjs";

const artifact = JSON.stringify({
  reason: "compiler-artifact", target: { name: "vtest-exec", kind: ["example"] },
  executable: "/managed target/vtest/examples/vtest-exec",
});

test("builds the optimized executor and preserves nextest selection and environment", () => {
  const calls = [];
  const status = run(["--test", "counter", "-E", "test(::native$)", "--run-ignored", "all"],
    (...call) => {
      calls.push(call);
      return calls.length === 1 ? { status: 0, stdout: `${artifact}\n` } : { status: 7 };
    }, { PATH: "/tools", CELOX_VTEST_ROOT: "/stale", CELOX_VTEST_RUNNER: "/stale" });
  assert.equal(status, 7);
  assert.equal(calls.length, 2);
  const [[, build, options], [, tests, testOptions]] = calls;
  assert.equal(build[build.indexOf("--profile") + 1], "vtest");
  assert.equal(build[build.indexOf("--features") + 1], tests[tests.indexOf("--features") + 1]);
  assert.deepEqual(tests.slice(-6), ["--test", "counter", "-E", "test(::native$)", "--run-ignored", "all"]);
  assert.equal(testOptions.cwd, options.cwd);
  assert.equal(testOptions.env.PATH, "/tools");
  assert.equal(testOptions.env.CELOX_VTEST_RUNNER, "/managed target/vtest/examples/vtest-exec");
  assert.ok(testOptions.env.CELOX_VTEST_ROOT.endsWith("/crates/celox-test-suite/src"));
});

test("failed or interrupted builds never launch tests", () => {
  for (const status of [1, null]) {
    let calls = 0;
    assert.equal(run([], () => { calls++; return { status }; }), 1);
    assert.equal(calls, 1);
  }
  assert.throws(() => run([], () => ({ error: new Error("no cargo") })), /no cargo/);
});

test("missing or ambiguous executors fail closed", () => {
  assert.throws(() => executorPath('{"reason":"build-finished"}\n'), /one vtest executor/);
  assert.throws(() => executorPath(`${artifact}\n${artifact}`), /one vtest executor/);
  assert.equal(executorPath(`{"reason":"build-finished"}\n${artifact}\n`), "/managed target/vtest/examples/vtest-exec");
});

test("CI preparation only builds and exports the executor for later test steps", () => {
  const calls = [];
  const writes = [];
  assert.equal(prepareCi((...args) => {
    calls.push(args);
    return { status: 0, stdout: artifact };
  }, { GITHUB_ENV: "/job/environment" }, (...args) => writes.push(args)), 0);
  assert.equal(calls.length, 1);
  assert.equal(calls[0][1][0], "build");
  assert.ok(calls[0][1].includes("vtest"));
  assert.equal(writes.length, 1);
  assert.equal(writes[0][0], "/job/environment");
  assert.match(writes[0][1], /^CELOX_VTEST_RUNNER=\/managed target\/vtest\/examples\/vtest-exec\n/);
  assert.match(writes[0][1], /\nCELOX_VTEST_ROOT=.*\/crates\/celox-test-suite\/src\n$/);
});

test("failed CI preparation cannot export a stale executor or silently skip setup", () => {
  const unexpected = () => assert.fail("unexpected build or environment export");
  assert.throws(() => prepareCi(unexpected, {}, unexpected), /requires GITHUB_ENV/);
  assert.equal(prepareCi(() => ({ status: 17 }), { GITHUB_ENV: "/job/env" }, unexpected), 17);
  assert.throws(() => prepareCi(() => ({ status: 0, stdout: "" }),
    { GITHUB_ENV: "/job/env" }, unexpected), /one vtest executor/);
  const message = JSON.parse(artifact);
  message.executable += "\nUNRELATED=value";
  assert.throws(() => prepareCi(() => ({ status: 0, stdout: JSON.stringify(message) }),
    { GITHUB_ENV: "/job/env" }, unexpected), /Invalid executor environment/);
});
