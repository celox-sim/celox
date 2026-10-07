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

test("the required gate rejects failures and only accepts explicitly optional skips", () => {
  const gate = job("rust");
  assert.match(gate, /name: Rust Test & NAPI Build/);
  assert.match(gate, /if: always\(\)/);
  assert.match(gate, /needs: \[changes, rust-tests, napi-linux, napi-wasm\]/);
  assert.match(
    gate,
    /if: .*outputs\.rust == 'true' \|\| needs\.changes\.outputs\.napi == 'true'/,
  );
  const script = gate.match(/        run: \|\n([\s\S]*)/)[1];
  const success = {
    CHANGES_RESULT: "success",
    RUST_REQUIRED: "true",
    RUST_RESULT: "success",
    NAPI_REQUIRED: "true",
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
  const rustOnly = {
    ...success,
    NAPI_REQUIRED: "false",
    NATIVE_RESULT: "skipped",
    WASM_RESULT: "skipped",
  };
  cases.push({ env: rustOnly, passes: true });
  for (const status of ["failure", "cancelled", "skipped", ""]) {
    cases.push({ env: { ...rustOnly, RUST_RESULT: status }, passes: false });
  }
  for (const producer of ["NATIVE_RESULT", "WASM_RESULT"]) {
    for (const status of ["success", "failure", "cancelled", ""]) {
      cases.push({
        env: { ...rustOnly, [producer]: status },
        passes: status === "success",
      });
    }
  }
  for (const required of ["true", ""]) {
    cases.push({
      env: { ...rustOnly, NAPI_REQUIRED: required },
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

test("scheduled and manual validation run all paths and external suites", () => {
  assert.match(
    workflow,
    /schedule:\n    - cron: "17 1 \* \* \*"\n  workflow_dispatch:/,
  );
  assert.match(
    job("changes"),
    /FULL_VALIDATION: .*github\.event_name == 'schedule' \|\| github\.event_name == 'workflow_dispatch'/,
  );
  assert.match(job("changes"), /STABLE_LANE: .*github\.ref_name == 'master'/);
  assert.match(job("lint"), /VERYL_LANE: .*github\.ref_name == 'develop'/);
  const external = job("external-suites");
  assert.match(
    external,
    /if: github\.event_name == 'schedule' \|\| github\.event_name == 'workflow_dispatch'/,
  );
  assert.match(external, /fail-fast: false/);
  assert.match(external, /suite: \[veryl, sv\]/);
  assert.match(external, /tool: \[verilator, icarus\]/);
  assert.match(external, /--test oracles -- --ignored/);
  assert.match(
    external,
    /--report "\$RUNNER_TEMP\/external-suite\/report\.json"/,
  );
  assert.match(external, /if: always\(\)/);
  assert.doesNotMatch(
    external,
    /continue-on-error|--filter|--exclude-stronger-than-sv/,
  );
  const nightly = readFileSync(
    new URL("../.github/workflows/nightly.yml", import.meta.url),
    "utf8",
  );
  assert.match(
    nightly,
    /gh workflow run ci\.yml --repo "\$GITHUB_REPOSITORY" --ref develop/,
  );
});

test("Rust changes validate dependency exclusions even when script jobs are omitted", () => {
  const lint = job("lint");
  assert.match(
    lint,
    /name: Check CI runtime dependency exclusions\n        if: .*outputs\.rust == 'true' && needs\.changes\.outputs\.scripts != 'true'\n        run: node --test scripts\/ci-changes\.test\.mjs/,
  );
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
