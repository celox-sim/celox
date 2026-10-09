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
  assert.match(
    gate,
    /needs: \[changes, rust-tests, sv-tests, napi-linux, napi-wasm, external-suites\]/,
  );
  assert.match(
    gate,
    /if: .*outputs\.rust == 'true' \|\| needs\.changes\.outputs\.napi == 'true'/,
  );
  const script = gate.match(/        run: \|\n([\s\S]*)/)[1];
  const success = {
    CHANGES_RESULT: "success",
    RUST_REQUIRED: "true",
    RUST_RESULT: "success",
    SV_TESTS_RESULT: "success",
    NAPI_REQUIRED: "true",
    NATIVE_RESULT: "success",
    WASM_RESULT: "success",
    RELEASE_REQUIRED: "false",
    EXTERNAL_RESULT: "skipped",
  };
  const cases = [
    {
      env: { ...success, RUST_REQUIRED: "", RUST_RESULT: "skipped" },
      passes: false,
    },
    { env: success, passes: true },
    {
      env: {
        ...success,
        RUST_REQUIRED: "false",
        RUST_RESULT: "skipped",
        SV_TESTS_RESULT: "skipped",
      },
      passes: true,
    },
  ];
  for (const producer of [
    "CHANGES_RESULT",
    "RUST_RESULT",
    "SV_TESTS_RESULT",
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
  // A merge group that cuts a release must pass every external comparison.
  const release = {
    ...success,
    RELEASE_REQUIRED: "true",
    EXTERNAL_RESULT: "success",
  };
  cases.push({ env: release, passes: true });
  for (const status of ["failure", "cancelled", "skipped", ""]) {
    cases.push({ env: { ...release, EXTERNAL_RESULT: status }, passes: false });
  }
  // Full runs report comparison failures through an issue, not this gate.
  for (const status of ["success", "failure", "cancelled", ""]) {
    cases.push({ env: { ...success, EXTERNAL_RESULT: status }, passes: true });
  }
  cases.push({ env: { ...success, RELEASE_REQUIRED: "" }, passes: false });
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
  assert.match(external, /needs: changes\n/);
  assert.match(
    external,
    /if: always\(\) && \(github\.event_name == 'schedule' \|\| github\.event_name == 'workflow_dispatch' \|\| needs\.changes\.outputs\.release == 'true'\)/,
  );
  assert.match(
    job("changes"),
    /release: \$\{\{ steps\.classify\.outputs\.release \}\}/,
  );
  assert.match(
    job("changes"),
    /MERGE_GROUP_BASE_REF: \$\{\{ github\.event\.merge_group\.base_ref \}\}/,
  );
  // Only the repository's own release branch may skip PR product checks.
  assert.match(
    job("changes"),
    /RELEASE_PLEASE_PR: \$\{\{ github\.event_name == 'pull_request' && github\.head_ref == 'release-please--branches--master--components--celox' && github\.event\.pull_request\.head\.repo\.full_name == github\.repository \}\}/,
  );
  assert.match(external, /fail-fast: false/);
  assert.match(external, /suite: \[veryl, sv\]/);
  assert.match(external, /tool: \[verilator, icarus\]/);
  assert.match(external, /--test oracles -- --ignored/);
  // The check reads the complete results, with run counts and reuse flags;
  // a retained --report copy omits them.
  assert.match(external, /--output "\$RUNNER_TEMP\/external-suite\/logs"/);
  assert.match(
    external,
    /"\$RUNNER_TEMP\/external-suite\/logs\/results\.json" "\$VERIFY_BASELINE"/,
  );
  assert.match(external, /if: always\(\)/);
  assert.match(
    external,
    /--bin "\$VERIFY_BIN" -- --list > "\$RUNNER_TEMP\/external-suite\/catalogue\.json"/,
  );
  assert.match(
    external,
    /if nix develop[\s\S]*task_verify_status=0[\s\S]*task_verify_status=\$\?/,
  );
  assert.match(
    external,
    /VERIFY_EXIT_STATUS: \$\{\{ steps\.verify\.outputs\.exit_status \}\}/,
  );
  assert.match(external, /node scripts\/check-external-suite-report\.mjs/);
  assert.match(
    external,
    /"\$RUNNER_TEMP\/external-suite\/catalogue\.json" "\$VERIFY_EXIT_STATUS" "\$VERIFY_SUITE"/,
  );
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

test("landed commits are not re-validated by a push run", () => {
  const triggers = workflow.match(/^on:\n([\s\S]*?)\n\S/m)[1];
  assert.doesNotMatch(triggers, /^\s+push:/m);
  assert.match(triggers, /^\s+merge_group:/m);
  assert.match(triggers, /^\s+schedule:/m);
});

test("platform jobs run in merge groups and full runs, not on pull requests", () => {
  const platform = [
    "arm64-backend",
    "napi-windows",
    "napi-linux-arm64",
    "js-windows",
  ];
  for (const name of platform) {
    assert.match(
      job(name),
      /\n    if: always\(\) && github\.event_name != 'pull_request' && \(/,
      name,
    );
  }
  // Linux jobs still give pull requests their feedback.
  for (const name of ["lint", "rust-tests", "napi-linux", "js-ubuntu", "rust"]) {
    assert.doesNotMatch(job(name), /github\.event_name != 'pull_request'/, name);
  }
});

test("full runs on long-lived branches report every job's result", () => {
  const report = job("report-full-validation");
  const jobs = [...workflow.split(/^jobs:\n/m)[1].matchAll(/^  ([\w-]+):\n/gm)]
    .map((match) => match[1])
    .filter((name) => name !== "report-full-validation");
  const needs = report
    .match(/needs:\s*\[([^\]]*)\]/)[1]
    .split(",")
    .map((name) => name.trim())
    .filter(Boolean);
  assert.deepEqual([...needs].sort(), [...jobs].sort());
  assert.match(
    report,
    /if: always\(\) && \(github\.event_name == 'schedule' \|\| github\.event_name == 'workflow_dispatch'\) && \(github\.ref_name == 'master' \|\| github\.ref_name == 'develop'\)/,
  );
  assert.match(report, /issues: write/);
  assert.match(report, /NEEDS: \$\{\{ toJSON\(needs\) \}\}/);
  assert.match(report, /run: node scripts\/report-full-ci\.mjs/);
});
