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
  assert.match(gate, /if: \$\{\{ !cancelled\(\)/);
  assert.match(
    gate,
    /needs: \[changes, rust-tests, sv-tests, arm64-backend, napi-linux, napi-wasm, external-suites\]/,
  );
  assert.match(
    gate,
    /if: .*outputs\.rust == 'true' \|\| needs\.changes\.outputs\.napi == 'true'/,
  );
  assert.match(
    gate,
    /ARM64_REQUIRED: \$\{\{ needs\.changes\.outputs\.queue != 'false' && needs\.changes\.outputs\.rust != 'false' \}\}/,
  );
  assert.match(gate, /ARM64_RESULT: \$\{\{ needs\.arm64-backend\.result \}\}/);
  const script = gate.match(/        run: \|\n([\s\S]*)/)[1];
  const success = {
    CHANGES_RESULT: "success",
    RUST_REQUIRED: "true",
    RUST_RESULT: "success",
    SV_TESTS_REQUIRED: "true",
    SV_TESTS_RESULT: "success",
    ARM64_REQUIRED: "true",
    ARM64_RESULT: "success",
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
        SV_TESTS_REQUIRED: "false",
        SV_TESTS_RESULT: "skipped",
      },
      passes: true,
    },
  ];
  for (const producer of [
    "CHANGES_RESULT",
    "RUST_RESULT",
    "SV_TESTS_RESULT",
    "ARM64_RESULT",
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
  // A scoped Rust PR may skip conformance only when explicitly unneeded.
  for (const status of ["success", "skipped", "failure", "cancelled", ""]) {
    cases.push({
      env: { ...success, SV_TESTS_REQUIRED: "false", SV_TESTS_RESULT: status },
      passes: status === "success" || status === "skipped",
    });
  }
  cases.push({
    env: { ...success, SV_TESTS_REQUIRED: "", SV_TESTS_RESULT: "skipped" },
    passes: false,
  });
  assert.match(gate, /SV_TESTS_REQUIRED: .*outputs\.rust != 'false' && needs\.changes\.outputs\.rust_sv_tests != 'false'/);
  assert.match(job("sv-tests"), /outputs\.rust == 'true' && needs\.changes\.outputs\.rust_sv_tests != 'false'/);
  // Only non-Rust changes may skip ARM64; other results fail closed.
  for (const status of ["success", "skipped", "failure", "cancelled", ""]) {
    cases.push({
      env: { ...success, ARM64_REQUIRED: "false", ARM64_RESULT: status },
      passes: status === "success" || status === "skipped",
    });
  }
  cases.push({
    env: { ...success, ARM64_REQUIRED: "", ARM64_RESULT: "skipped" },
    passes: false,
  });
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
    /if: \$\{\{ !cancelled\(\) && \(github\.event_name == 'schedule' \|\| github\.event_name == 'workflow_dispatch' \|\| needs\.changes\.outputs\.release == 'true'\)/,
  );
  assert.match(
    job("changes"),
    /release: \$\{\{ steps\.classify\.outputs\.release \}\}/,
  );
  assert.match(
    job("changes"),
    /MERGE_GROUP_BASE_REF: .*github\.event\.merge_group\.base_ref.*github\.base_ref/,
  );
  assert.match(job("changes"), /RELEASE_PLEASE_PR: .*github\.event\.pull_request\.head\.repo\.full_name == github\.repository/);
  assert.match(job("changes"), /QUEUE_VALIDATION: .*startsWith\(github\.head_ref, 'mergify\/merge-queue\/'\)/);
  assert.match(external, /fail-fast: false/);
  assert.match(external, /suite: \[veryl, sv\]/);
  assert.match(external, /tool: \[verilator, icarus\]/);
  assert.match(external, /--test oracles -- --ignored/);
  // The check reads the complete results, with run counts and reuse flags;
  // a retained --report copy omits them.
  assert.match(external, /--output "\$RUNNER_TEMP\/external-suite\/logs"/);
  assert.match(external, /--bin "\$VERIFY_BIN" -- --fresh --jobs 2/);
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
    /name: Check CI runtime dependency exclusions and test scopes\n        if: .*outputs\.rust == 'true' && needs\.changes\.outputs\.scripts != 'true'\n        run: node --test scripts\/ci-changes\.test\.mjs scripts\/ci-rust-scope\.test\.mjs scripts\/ci-lydite-changes\.test\.mjs/,
  );
});

test("queue CI retains the whole workspace and ordinary PRs use scoped tests", () => {
  const rust = job("rust-tests");
  assert.match(job("changes"), /QUEUE_VALIDATION: .*startsWith\(github\.head_ref, 'mergify\/merge-queue\/'\)/);
  // Anything but an explicit scope, including a failed detection, runs all.
  assert.match(
    rust,
    /name: Run workspace tests with CI features\n        if: needs\.changes\.outputs\.rust_full != 'false'\n/,
  );
  assert.match(
    rust,
    /name: Run the tests this pull request can affect\n        if: needs\.changes\.outputs\.rust_full == 'false' && /,
  );
  assert.match(
    job("changes"),
    /rust_filter: \$\{\{ steps\.classify\.outputs\.rust_filter \}\}/,
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

test("platform jobs are deferred to queue validation", () => {
  const platform = [
    "arm64-backend",
    "napi-windows",
    "napi-linux-arm64",
    "js-windows",
  ];
  for (const name of platform) {
    assert.match(job(name), /\n    if: \$\{\{ !cancelled\(\) && needs\.changes\.outputs\.queue != 'false' && \(/, name);
  }
  // Linux jobs still give pull requests their feedback.
  for (const name of [
    "lint",
    "rust-tests",
    "napi-linux",
    "js-ubuntu",
    "rust",
  ]) {
    assert.doesNotMatch(
      job(name).match(/^    if: (.*)$/m)[1],
      /github\.event_name != 'pull_request'/,
      name,
    );
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
    /if: \$\{\{ !cancelled\(\) && \(github\.event_name == 'schedule' \|\| github\.event_name == 'workflow_dispatch'\) && \(github\.ref_name == 'master' \|\| github\.ref_name == 'develop'\)/,
  );
  assert.match(report, /issues: write/);
  assert.match(report, /NEEDS: \$\{\{ toJSON\(needs\) \}\}/);
  assert.match(report, /run: node scripts\/report-full-ci\.mjs/);
});

test("queue validation rejects missing coverage, producer failures and detection failure", () => {
  const gate = job("queue-validation");
  assert.match(gate, /if: \$\{\{ !cancelled\(\) && needs\.changes\.outputs\.queue != 'false'/);
  const script = gate.match(/        run: \|\n([\s\S]*)/)[1];
  const needs = Object.fromEntries(["lint", "rust", "napi-windows", "napi-linux-arm64", "js-ubuntu", "js-windows", "playground-browser", "docs"].map(name => [name, { result: "success" }]));
  needs.changes = { result: "success", outputs: { queue: "true", rust: "true", napi: "true", napi_arm64: "true", javascript: "true", scripts: "true", docs: "true" } };
  const passes = (value) => spawnSync("bash", ["-e", "-c", script], { env: { ...process.env, NEEDS: JSON.stringify(value) } }).status === 0;
  assert.equal(passes(needs), true);
  for (const name of Object.keys(needs)) {
    for (const result of ["failure", "cancelled", "skipped"]) {
      const failed = structuredClone(needs); failed[name].result = result;
      assert.equal(passes(failed), false, `${name}: ${result}`);
    }
  }
  for (const queue of ["false", ""]) {
    const light = structuredClone(needs); light.changes.outputs.queue = queue;
    assert.equal(passes(light), false);
  }
  const docs = structuredClone(needs);
  for (const name of Object.keys(docs)) if (!["changes", "docs"].includes(name)) docs[name].result = "skipped";
  for (const flag of ["rust", "napi", "napi_arm64", "javascript", "scripts"]) docs.changes.outputs[flag] = "false";
  assert.equal(passes(docs), true);
});

test("PR caches cannot evict the shared default-branch Rust caches", () => {
  const cacheSteps = [
    ...workflow.matchAll(
      /      - uses: Swatinem\/rust-cache@v2\n([\s\S]*?)(?=\n      -|\n  [a-z]|$)/g,
    ),
  ];
  assert.equal(cacheSteps.length, 8);
  for (const [, step] of cacheSteps) {
    assert.match(
      step,
      /save-if: \$\{\{ github.ref == format\('refs\/heads\/\{0\}', github.event.repository.default_branch\) \}\}/,
    );
  }
});

test("cache warming shares the Rust Tests key and builds without running tests", () => {
  const warm = readFileSync(
    new URL("../.github/workflows/rust-cache.yml", import.meta.url),
    "utf8",
  );
  assert.match(warm, /branches: \[master\]/);
  assert.match(warm, /rust-tests:\n    if: github.ref == format/);
  assert.match(warm, /CARGO_INCREMENTAL: "0"/);
  assert.match(warm, /cancel-in-progress: false/);
  assert.doesNotMatch(
    warm,
    /pull_request:|cargo nextest run|cargo test|shared-key:/,
  );
  assert.match(warm, /steps.cache.outputs.cache-hit != 'true'/);
  assert.match(
    warm,
    /cargo nextest list --locked --workspace --message-format json/,
  );
  const features = job("rust-tests").match(/--features (\S+)/)[1];
  assert.ok(warm.includes(`--features ${features}`));
  const nextest = job("rust-tests").match(/tool: (cargo-nextest@\S+)/)[1];
  assert.ok(warm.includes(`tool: ${nextest}`));
});

// Execute the workflow's actual shell with a recording Cargo function, so a
// correct classifier cannot accidentally turn into an unscoped Cargo build.
function cargoArguments(jobName, stepName, env) {
  const step = job(jobName).split(`      - name: ${stepName}\n`)[1];
  assert.ok(step, stepName);
  const script = step.match(/        run: \|\n((?:          [^\n]*\n|\n)*)/)[1];
  const result = spawnSync("bash", ["-e", "-c", `cargo() { printf '\\036%s' "$@"; };\n${script}`], {
    env: { ...process.env, ...env }, encoding: "utf8",
  });
  assert.ifError(result.error);
  assert.equal(result.status, 0, result.stderr);
  return result.stdout.split("\x1e").slice(1);
}

test("scoped Cargo commands build only the selected integration targets", () => {
  const env = {
    PACKAGES: "celox", FEATURES: "celox/systemverilog",
    TARGETS: "flip_flop systemverilog", FILTER: "binary_id(celox::flip_flop) | binary_id(celox::systemverilog)",
  };
  assert.deepEqual(cargoArguments("rust-tests", "Run the tests this pull request can affect", env), [
    "nextest", "run", "--locked", "--profile", "ci", "--no-tests=pass",
    "-p", "celox", "--features", "celox/systemverilog",
    "--test", "flip_flop", "--test", "systemverilog", "-E", env.FILTER,
  ]);
  assert.deepEqual(cargoArguments("lint", "Lint the crates this pull request can affect", env), [
    "clippy", "--locked", "-p", "celox", "--features", "celox/systemverilog",
    "--test", "flip_flop", "--test", "systemverilog", "--no-deps",
  ]);
  assert.deepEqual(cargoArguments("lint", "Lint the crates this pull request can affect", {
    PACKAGES: "celox-runtime celox", FEATURES: "", TARGETS: "",
  }), ["clippy", "--locked", "-p", "celox-runtime", "-p", "celox", "--all-targets", "--no-deps"]);
  assert.deepEqual(cargoArguments("lint", "Build the API documentation this pull request can affect", {
    LIBRARIES: "celox-runtime celox",
  }), ["doc", "--locked", "--no-deps", "-p", "celox-runtime", "-p", "celox"]);
});

test("scoped checks preserve full-run fallbacks and use their own package scopes", () => {
  const lint = job("lint");
  assert.match(lint, /name: cargo clippy\n        if: .*outputs\.rust_full != 'false'/);
  assert.match(lint, /name: Build Rust API documentation\n        if: .*outputs\.rust_full != 'false'/);
  assert.match(lint, /name: Lint the crates this pull request can affect\n        if: .*outputs\.rust_lint_packages != ''/);
  assert.match(lint, /PACKAGES: \$\{\{ needs\.changes\.outputs\.rust_lint_packages \}\}/);
  assert.match(lint, /FEATURES: \$\{\{ needs\.changes\.outputs\.rust_lint_features \}\}/);
  assert.match(lint, /TARGETS: \$\{\{ needs\.changes\.outputs\.rust_lint_targets \}\}/);
  assert.match(lint, /name: Build the API documentation this pull request can affect\n        if: .*outputs\.rust_doc_libraries != ''/);
});

test("full Clippy validation covers every workspace crate", () => {
  assert.match(job("lint"), /run: cargo clippy --locked --workspace --all-targets --features/);
});
