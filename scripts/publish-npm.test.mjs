import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { publishNpm } from "./publish-npm.mjs";

const conflict = "npm error code TLOG_CREATE_ENTRY_ERROR\nnpm error error creating tlog entry - (409) an equivalent entry already exists in the transparency log with UUID abc";
const ok = { status: 0 };
const missing = { status: 1, stderr: "E404" };
const failed = { status: 1, stderr: conflict };

async function publish(results, publisher = "npm") {
  const directory = mkdtempSync(join(tmpdir(), "celox-publish-npm-"));
  writeFileSync(join(directory, "package.json"), JSON.stringify({ name: "@test/addon", version: "1.0.0" }));
  const calls = [];
  const waits = [];
  let error;
  try {
    await publishNpm({
      directory,
      publisher,
      tag: "nightly-head",
      run(command, args, options) {
        calls.push({ command, args, cwd: options.cwd });
        assert.equal(options.cwd, directory);
        assert.ok(results.length, "unexpected command");
        return results.shift();
      },
      wait: async (delay) => { waits.push(delay); },
      log() {},
    });
  } catch (caught) {
    error = caught;
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
  assert.equal(results.length, 0, "unconsumed command results");
  return { calls, waits, error };
}

test("an existing registry version is skipped", async () => {
  const result = await publish([ok]);
  assert.ifError(result.error);
  assert.equal(result.calls.length, 1);
  assert.deepEqual(result.calls[0].args, ["view", "@test/addon@1.0.0", "version"]);
});

test("npm and pnpm preserve provenance, tag, and disabled lifecycle scripts", async () => {
  for (const publisher of ["npm", "pnpm"]) {
    const result = await publish([missing, ok], publisher);
    assert.ifError(result.error);
    assert.equal(result.calls[1].command, publisher);
    assert.deepEqual(result.calls[1].args, [
      "publish", "--ignore-scripts", "--provenance", "--access", "public", "--tag", "nightly-head",
      ...(publisher === "pnpm" ? ["--no-git-checks"] : []),
    ]);
    assert.deepEqual(result.waits, []);
  }
});

test("a provenance conflict retries with a fresh publisher process", async () => {
  const result = await publish([missing, failed, missing, ok]);
  assert.ifError(result.error);
  assert.equal(result.calls.length, 4);
  assert.deepEqual(result.calls[1], result.calls[3]);
  assert.deepEqual(result.waits, [5000]);
});

test("a registry version appearing after a conflict prevents republishing", async () => {
  const result = await publish([missing, failed, ok]);
  assert.ifError(result.error);
  assert.equal(result.calls.filter((call) => call.args[0] === "publish").length, 1);
});

test("repeated conflicts fail after three attempts", async () => {
  const result = await publish([missing, failed, missing, failed, missing, failed]);
  assert.match(result.error.message, /Publishing @test\/addon@1.0.0 failed/);
  assert.equal(result.calls.length, 6);
  assert.deepEqual(result.waits, [5000, 5000]);
});

test("other failures and process errors fail without retrying", async () => {
  for (const failure of [
    { status: 1, stderr: "E403 forbidden" },
    { status: 1, stderr: "TLOG_CREATE_ENTRY_ERROR (502) Bad Gateway" },
    { status: 1, stderr: "(409) an equivalent entry already exists in the transparency log" },
    { status: null, signal: "SIGTERM" },
    { error: new Error("spawn failed") },
  ]) {
    const result = await publish([missing, failure]);
    assert.ok(result.error);
    assert.equal(result.calls.length, 2);
    assert.deepEqual(result.waits, []);
  }
});

test("NAPI preparation disables GitHub releases and all npm publication uses recovery", () => {
  const workflow = readFileSync(new URL("../.github/workflows/ci-napi.yml", import.meta.url), "utf8");
  assert.match(workflow, /napi prepublish -t npm --skip-optional-publish --no-gh-release/);
  assert.match(workflow, /node ..\/..\/scripts\/publish-npm.mjs npm "\$dir" "\$NPM_TAG"/);
  assert.match(workflow, /node ..\/..\/scripts\/publish-npm.mjs npm \. "\$NPM_TAG"/);
  assert.equal(workflow.match(/node ..\/..\/scripts\/publish-npm.mjs pnpm \. "\$NPM_TAG"/g).length, 2);
  assert.doesNotMatch(workflow, /^\s+(?:npm|pnpm) publish /m);
});
