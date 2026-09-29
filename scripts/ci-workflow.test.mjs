import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import test from "node:test";

const workflow = readFileSync(
  new URL("../.github/workflows/ci.yml", import.meta.url),
  "utf8",
);

function job(name) {
  const blocks = workflow.split(/^  ([\w-]+):\n/m);
  const index = blocks.indexOf(name);
  assert.ok(index > 0, `Missing job: ${name}`);
  return blocks[index + 1];
}

test("the existing required Rust check rejects failed or cancelled producers", () => {
  const gate = job("rust");
  assert.match(gate, /name: Rust Test & NAPI Build/);
  assert.match(gate, /if: always\(\)/);
  assert.match(gate, /needs: \[changes, rust-tests, napi-linux, napi-wasm\]/);
  const script = gate.match(/        run: \|\n([\s\S]*)/)[1];
  const success = {
    CHANGES_RESULT: "success",
    RUST_REQUIRED: "true",
    RUST_RESULT: "success",
    NATIVE_RESULT: "success",
    WASM_RESULT: "success",
  };
  const cases = [
    {
      env: { ...success, RUST_REQUIRED: "", RUST_RESULT: "skipped" },
      passes: false,
    },
    { env: success, passes: true },
    {
      env: { ...success, RUST_REQUIRED: "false", RUST_RESULT: "skipped" },
      passes: true,
    },
  ];
  for (const producer of [
    "CHANGES_RESULT",
    "RUST_RESULT",
    "NATIVE_RESULT",
    "WASM_RESULT",
  ]) {
    for (const status of ["failure", "cancelled", "skipped", ""]) {
      cases.push({ env: { ...success, [producer]: status }, passes: false });
    }
  }
  for (const status of ["failure", "cancelled", "skipped", ""]) {
    cases.push({
      env: { ...success, RUST_REQUIRED: "false", WASM_RESULT: status },
      passes: false,
    });
  }
  for (const status of ["failure", "cancelled", ""]) {
    cases.push({
      env: { ...success, RUST_REQUIRED: "false", RUST_RESULT: status },
      passes: false,
    });
  }
  for (const { env, passes } of cases) {
    const result = spawnSync("bash", ["-e", "-c", script], {
      env: { ...process.env, ...env },
      encoding: "utf8",
    });
    assert.ifError(result.error);
    assert.equal(result.status === 0, passes, JSON.stringify(env));
  }
});

test("artifact consumers wait for their producer rather than the Rust test gate", () => {
  const native = job("napi-linux");
  assert.match(native, /name: napi-linux-x64-gnu/);
  const js = job("js-ubuntu");
  assert.match(js, /needs: \[changes, napi-linux\]/);
  assert.match(js, /name: napi-linux-x64-gnu/);
  const wasm = job("napi-wasm");
  assert.match(wasm, /needs: changes\n/);
  assert.doesNotMatch(wasm, /actions\/download-artifact/);
  assert.match(wasm, /name: napi-wasm32-wasi/);
  const browser = job("playground-browser");
  assert.match(browser, /needs: \[changes, napi-wasm\]/);
  assert.match(browser, /name: napi-wasm32-wasi/);
});
