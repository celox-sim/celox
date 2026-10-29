import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { readFileSync, mkdtempSync, writeFileSync, chmodSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { workspaceFeatures } from "./run-vtest.mjs";

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
    /needs: \[changes, rust-tests, rust-default-tests, sv-tests, sv-tests-shard, arm64-backend, napi-linux, napi-wasm\]/,
  );
  assert.match(
    gate,
    /if: .*outputs\.rust == 'true' \|\| needs\.changes\.outputs\.napi == 'true'/,
  );
  assert.match(
    gate,
    /ARM64_REQUIRED: \$\{\{ needs\.changes\.outputs\.full_validation != 'false' && needs\.changes\.outputs\.rust != 'false' \}\}/,
  );
  assert.match(gate, /ARM64_RESULT: \$\{\{ needs\.arm64-backend\.result \}\}/);
  const script = gate.match(/        run: \|\n([\s\S]*)/)[1];
  const success = {
    CHANGES_RESULT: "success",
    RUST_REQUIRED: "true",
    RUST_RESULT: "success",
    DEFAULT_REQUIRED: "true",
    DEFAULT_RESULT: "success",
    SV_TESTS_REQUIRED: "true",
    SV_TESTS_RESULT: "success",
    SV_TESTS_SHARDS_RESULT: "success",
    ARM64_REQUIRED: "true",
    ARM64_RESULT: "success",
    NAPI_REQUIRED: "true",
    NATIVE_RESULT: "success",
    WASM_REQUIRED: "true",
    WASM_RESULT: "success",
  };
  const cases = [
    { env: { ...success, DEFAULT_REQUIRED: "false", DEFAULT_RESULT: "skipped" }, passes: true },
    { env: { ...success, DEFAULT_REQUIRED: "", DEFAULT_RESULT: "skipped" }, passes: false },
    { env: { ...success, DEFAULT_REQUIRED: "false", DEFAULT_RESULT: "failure" }, passes: false },
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
    "DEFAULT_RESULT",
    "SV_TESTS_RESULT",
    "SV_TESTS_SHARDS_RESULT",
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
  // A shard that fails fails the gate even when the driver job succeeded.
  for (const status of ["failure", "cancelled", "skipped", ""]) {
    cases.push({
      env: { ...success, SV_TESTS_REQUIRED: "false", SV_TESTS_SHARDS_RESULT: status },
      passes: false,
    });
  }
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
    WASM_REQUIRED: "false",
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

test("external comparisons run independently of product validation", () => {
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
  const external = readFileSync(new URL("../.github/workflows/external-comparisons.yml", import.meta.url), "utf8");
  assert.doesNotMatch(workflow, /external-suites|EXTERNAL_RESULT|RELEASE_REQUIRED/);
  assert.match(external, /schedule:/);
  assert.match(external, /workflow_dispatch:/);
  assert.doesNotMatch(external, /workflow_call:|pull_request:|push:/);
  assert.match(external, /REPORT_NAME: External simulator comparisons/);
  assert.match(job("changes"), /RELEASE_PLEASE_PR: .*github\.event\.pull_request\.head\.repo\.full_name == github\.repository/);
  assert.match(job("changes"), /QUEUE_VALIDATION: .*startsWith\(github\.head_ref, 'mergify\/merge-queue\/'\)/);
  assert.match(external, /fail-fast: false/);
  assert.match(external, /suite: \[veryl, sv\]/);
  assert.match(external, /tool: \[verilator, icarus, xezim\]/);
  // Only the xezim jobs build xezim, through its own development shell.
  assert.match(
    external,
    /VERIFY_SHELL: \$\{\{ matrix\.tool == 'xezim' && '\.#xezim' \|\| '\.' \}\}/,
  );
  assert.doesNotMatch(external, /nix develop --no-update-lock-file/);
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

test("full validation retains the whole workspace; the queue uses bounded integration tests", () => {
  const rust = job("rust-tests");
  assert.match(job("changes"), /QUEUE_VALIDATION: .*startsWith\(github\.head_ref, 'mergify\/merge-queue\/'\)/);
  // Anything but an explicit scope, including a failed detection, runs all.
  assert.match(
    rust,
    /name: Run workspace tests with CI features\n        id: workspace-tests\n        if: needs\.changes\.outputs\.fast_queue != 'true' && \(needs\.changes\.outputs\.rust_full != 'false'\)\n/,
  );
  assert.match(
    rust,
    /name: Run the tests this pull request can affect\n        id: scoped-tests\n        if: needs\.changes\.outputs\.fast_queue != 'true' && \(needs\.changes\.outputs\.rust_full == 'false' && /,
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

test("master pushes do not compete with the queue for full-matrix runners", () => {
  const triggers = workflow.match(/^on:\n([\s\S]*?)\n\S/m)[1];
  assert.doesNotMatch(triggers, /^\s+push:/m);
  assert.match(triggers, /^\s+merge_group:/m);
  assert.match(triggers, /^\s+schedule:/m);
});

test("platform jobs are deferred to full validation", () => {
  const platform = [
    "arm64-backend",
    "napi-windows",
    "napi-linux-arm64",
    "js-windows",
  ];
  for (const name of platform) {
    assert.match(job(name), /\n    if: \$\{\{ !cancelled\(\) && needs\.changes\.outputs\.full_validation != 'false' && \(/, name);
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
    /if: \$\{\{ !cancelled\(\) && inputs\.validation_ref == '' && \(github\.event_name == 'push' \|\| github\.event_name == 'schedule' \|\| github\.event_name == 'workflow_dispatch'\) && \(github\.ref_name == 'master' \|\| github\.ref_name == 'develop'\)/,
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
  needs.changes = { result: "success", outputs: { queue: "true", full_validation: "true", fast_queue: "false", rust: "true", napi: "true", napi_arm64: "true", javascript: "true", scripts: "true", docs: "true" } };
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
  assert.equal(cacheSteps.length, 10);
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
    /pull_request:|cargo nextest run/,
  );
  assert.match(warm, /steps.cache.outputs.cache-hit != 'true'/);
  for (const text of [warm, job("rust-tests")]) {
    assert.match(text, /key: vtest-\$\{\{ hashFiles\('scripts\/run-vtest\.mjs'\) \}\}/);
    assert.match(text, /run: node scripts\/run-vtest.mjs --prepare-ci/);
  }
  assert.match(warm, /name: Build the optimized vtest executor to populate dependencies\n        if: steps.cache.outputs.cache-hit != 'true'/);
  assert.match(warm, /"scripts\/run-vtest.mjs"/);
  assert.match(
    warm,
    /cargo nextest list --locked --workspace --exclude 'lydite\*' --cargo-profile vtest --message-format json/,
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
  const directory = mkdtempSync(join(tmpdir(), "celox-scoped-cargo-"));
  try {
    const cargo = join(directory, "cargo");
    writeFileSync(cargo, "#!/bin/sh\nprintf '\\036%s' \"$@\"\n");
    chmodSync(cargo, 0o755);
    const builds = env.BUILDS ?? JSON.stringify([{
      packages: (env.PACKAGES ?? "").split(" ").filter(Boolean),
      targets: (env.TARGETS ?? "").split(" ").filter(Boolean),
      features: (env.FEATURES ?? "").split(",").filter(Boolean),
    }]);
    const result = spawnSync("bash", ["-e", "-c", script], {
      env: { ...process.env, CELOX_VTEST_RUNNER: "/ci/vtest-exec", ...env, BUILDS: builds, PATH: `${directory}:${process.env.PATH}` }, encoding: "utf8",
    });
    assert.ifError(result.error);
    assert.equal(result.status, 0, result.stderr);
    return result.stdout.split("\x1e").slice(1);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

test("scoped Cargo commands build only the selected integration targets", () => {
  const env = {
    PACKAGES: "celox", FEATURES: "celox/systemverilog",
    TARGETS: "flip_flop systemverilog", FILTER: "binary_id(celox::flip_flop) | binary_id(celox::systemverilog)",
  };
  assert.deepEqual(cargoArguments("rust-tests", "Run the tests this pull request can affect", env), [
    "nextest", "run", "--locked", "--profile", "ci", "--cargo-profile", "vtest", "--no-tests=pass",
    "-p", "celox", "--features", "celox/systemverilog,celox-test-suite/runtime-cases",
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

test("CI prepares one executor for full runs and scopes containing Celox", () => {
  const rust = job("rust-tests");
  const step = rust.split("      - name: Build the optimized vtest executor\n")[1]
    .split("\n      #")[0];
  assert.match(step, /if: needs.changes.outputs.fast_queue != 'true' && \(needs.changes.outputs.rust_full != 'false' \|\| contains\(format\(' \{0\} ', needs.changes.outputs.rust_packages\), ' celox '\)/);
  assert.match(step, /run: node scripts\/run-vtest.mjs --prepare-ci/);
  assert.ok(rust.indexOf("Build the optimized vtest executor") < rust.indexOf("Run workspace tests"));
  assert.match(rust, /cargo nextest run[^\n]*\n[^\n]*celox-test-suite\/runtime-cases/);
  assert.deepEqual(cargoArguments("rust-tests", "Run the tests this pull request can affect", {
    PACKAGES: "celox-heap", FEATURES: "", TARGETS: "", FILTER: "package(celox-heap)",
    CELOX_VTEST_RUNNER: "",
  }), ["nextest", "run", "--locked", "--profile", "ci", "--cargo-profile", "vtest", "--no-tests=pass",
    "-p", "celox-heap", "-E", "package(celox-heap)"]);
});

test("CI reports each Rust run to Mergify without weakening test failures", () => {
  const rust = job("rust-tests");
  assert.match(rust, /CELOX_CI_REPORT_DIR: work\/ci-reports\/rust/);
  assert.match(rust, /HAS_MERGIFY_TOKEN: \$\{\{ secrets.MERGIFY_TOKEN != '' \}\}/);
  assert.match(rust, /run: node scripts\/ci-rust-reports.mjs reset/);
  assert.match(rust, /name: Collect Rust test reports\n        if: success\(\) \|\| failure\(\)/);
  assert.match(rust, /run: node scripts\/ci-rust-reports.mjs collect/);
  assert.match(rust, /name: Run workspace tests with CI features\n        id: workspace-tests/);
  assert.match(rust, /name: Run the tests this pull request can affect\n        id: scoped-tests/);
  assert.doesNotMatch(rust, /continue-on-error/);
  const upload = rust.split("      - name: Upload Rust test results to Mergify\n")[1]
    .split("\n      #")[0];
  assert.match(upload, /if: \(success\(\) \|\| failure\(\)\) && env.HAS_MERGIFY_TOKEN == 'true' && hashFiles\('work\/ci-reports\/rust\/\*.xml'\) != ''/);
  assert.match(upload, /uses: mergifyio\/gha-mergify-ci@[0-9a-f]{40} # v27/);
  assert.match(upload, /report_path: work\/ci-reports\/rust\/\*.xml/);
  assert.match(upload, /job_name: rust-tests/);
  assert.match(upload, /test_step_outcome: .*steps.workspace-tests.outcome.*steps.scoped-tests.outcome/);
  assert.match(rust, /name: rust-test-results\n          path: work\/ci-reports\/rust\/\*.xml\n          retention-days: 14/);
  const nextest = readFileSync(new URL("../.config/nextest.toml", import.meta.url), "utf8");
  assert.match(nextest, /\[profile.ci.junit\]\npath = "junit.xml"/);
});

test("scoped checks preserve full-run fallbacks and use their own package scopes", () => {
  const lint = job("lint");
  assert.match(lint, /name: cargo clippy\n        if: .*outputs\.rust_full != 'false'/);
  assert.match(lint, /name: Build Rust API documentation\n        if: .*outputs\.rust_full != 'false'/);
  assert.match(lint, /name: Lint the crates this pull request can affect\n        if: .*outputs\.rust_lint_packages != ''/);
  assert.match(lint, /BUILDS: \$\{\{ needs\.changes\.outputs\.rust_lint_builds \}\}/);
  assert.match(lint, /name: Build the API documentation this pull request can affect\n        if: .*outputs\.rust_doc_libraries != ''/);
});

test("cocotb reuses the original scoped Cargo build", () => {
  const builds = [{ packages: ["celox", "celox-vpi", "celox-test-suite"], targets: [],
    features: ["celox/systemverilog", "celox-test-suite/icarus"] }];
  const args = cargoArguments("rust-tests", "Run cocotb VPI end-to-end test", {
    RUST_FULL: "false", BUILDS: JSON.stringify(builds),
  });
  assert.deepEqual(args, ["nextest", "run", "--locked", "--profile", "ci", "--cargo-profile", "vtest",
    "-p", "celox", "-p", "celox-vpi", "-p", "celox-test-suite", "--features",
    "celox/systemverilog,celox-test-suite/icarus,celox-test-suite/runtime-cases",
    "-E", 'binary_id(celox-vpi::cocotb_e2e) & test(/^cocotb_/)', "--run-ignored", "only"]);
});

test("full cocotb runs reuse the workspace build and report failures before upload", () => {
  const rust = job("rust-tests");
  const full = rust.match(/run: >-\n\s+(cargo nextest run[^\n]*)\n\s+(--features [^\n]*)/);
  assert.ok(full);
  const original = `${full[1]} ${full[2]}`.replace(' -E "$CELOX_BACKEND_FILTER"', "").replaceAll("'", "").split(/\s+/).slice(1);
  const args = cargoArguments("rust-tests", "Run cocotb VPI end-to-end test", { RUST_FULL: "true" });
  assert.deepEqual(args.slice(0, -4), original);
  assert.ok(rust.indexOf("Collect Rust test reports") < rust.indexOf("Run cocotb VPI end-to-end test"));
  assert.ok(rust.indexOf("Run cocotb VPI end-to-end test") < rust.indexOf("Retain Rust test reports"));
  assert.match(rust, /test_step_outcome: .*steps.cocotb-tests.outcome == 'failure' && 'failure'/);
});

test("default-feature validation is parallel and mandatory when selected", () => {
  const defaults = job("rust-default-tests");
  assert.match(defaults, /needs: changes/);
  assert.match(defaults, /needs.changes.result != 'success'/);
  assert.match(defaults, /outputs.rust_full != 'false'/);
  assert.match(defaults, /outputs.rust_libraries/);
  assert.match(defaults, /cargo test --locked -p celox --lib/);
  assert.match(defaults, /cargo test --locked -p celox --doc/);
  assert.doesNotMatch(defaults, /--features|--no-default-features/);
  assert.doesNotMatch(job("rust-tests"), /cargo test --locked -p celox --lib/);
  assert.match(job("rust"), /DEFAULT_RESULT: .*needs.rust-default-tests.result/);
  assert.match(job("report-full-validation"), /rust-default-tests,/);
});

test("CI builds tests with the executor's profile and warms each platform's cache", () => {
  const rust = job("rust-tests");
  assert.match(rust, /--profile ci --cargo-profile vtest -E "\$CELOX_BACKEND_FILTER"/);
  // The executor build shares the test build's packages and features.
  assert.match(rust, /CELOX_RUST_FULL: \$\{\{ needs\.changes\.outputs\.rust_full \}\}\n          BUILDS: \$\{\{ needs\.changes\.outputs\.rust_builds \}\}\n        run: node scripts\/run-vtest\.mjs --prepare-ci/);
  const full = rust.match(/--cargo-profile vtest -E "\$CELOX_BACKEND_FILTER"\n          --features (\S+)/);
  assert.equal(full?.[1], workspaceFeatures);
  // Documentation tests reuse the libraries of that build.
  assert.match(rust, /cargo test --locked --workspace --exclude 'lydite\*' --doc --profile vtest\n          --features (\S+)/);
  assert.equal(rust.match(/--doc --profile vtest\n          --features (\S+)/)?.[1], workspaceFeatures);
  assert.match(rust, /cargo test --locked --doc --profile vtest "\$\{args\[@\]\}"/);
  assert.match(rust, /cargo test --locked --workspace/);
  const warm = readFileSync(new URL("../.github/workflows/rust-cache.yml", import.meta.url), "utf8");
  const windows = warm.split("  napi-windows:")[1];
  assert.match(windows, /runs-on: windows-latest/);
  assert.match(windows, /run: pnpm run build:napi/);
  assert.match(windows, /steps.cache.outputs.cache-hit != 'true'/);
  assert.match(windows, /if: github.ref == format/);
  assert.match(warm, /rust-default-tests:[\s\S]*cargo test --locked -p celox --lib --no-run/);
});

test("full Clippy validation excludes the disabled bridge", () => {
  assert.match(job("lint"), /run: cargo clippy --locked --workspace --exclude lydite-celox --all-targets --features/);
});

test("upstream SV TB runs on full CI and feeds failure reporting", () => {
  const upstream = job("common-cells");
  assert.match(upstream, /needs.changes.outputs.full_validation == 'true'/);
  assert.doesNotMatch(upstream, /continue-on-error/);
  assert.match(upstream, /conformance\/common-cells\/revision/);
  assert.match(upstream, /cargo build --locked -p celox-vpi --features systemverilog --bin celox/);
  assert.match(upstream, /node scripts\/run-common-cells.mjs/);
  assert.match(upstream, /if: always\(\)/);
  assert.match(upstream, /retention-days: 14/);
  assert.match(job("report-full-validation"), /common-cells,/);
});


test("sv-tests shards reuse one driver and together cover every case", () => {
  const driver = job("sv-tests");
  assert.match(driver, /cargo build --locked --release -p celox --features systemverilog --example sv_tests/);
  assert.match(driver, /name: sv-tests-driver/);
  const shard = job("sv-tests-shard");
  assert.match(shard, /needs: \[changes, sv-tests\]/);
  assert.match(shard, /if: \$\{\{ !cancelled\(\) && needs\.sv-tests\.result == 'success' \}\}/);
  assert.doesNotMatch(shard, /cargo |rust-cache/);
  const count = shard.match(/shard: \[([\d, ]+)\]/)[1].split(",").map(Number);
  assert.deepEqual(count, count.map((_, index) => index + 1));
  assert.match(shard, new RegExp(`--shard \\$\\{\\{ matrix\\.shard \\}\\}/${count.length}\n`));
  assert.match(shard, /fail-fast: false/);
  assert.match(shard, /name: sv-tests-report-\$\{\{ matrix\.shard \}\}/);
});

test("sv-tests generates the pinned main corpus before running and retains its summary", () => {
  const sv = job("sv-tests-shard");
  assert.match(sv, /third_party\/tests\/easyUVM third_party\/tests\/uvm-examples/);
  assert.ok(sv.indexOf("conformance/sv-tests/prepare.py") < sv.indexOf("conformance/sv-tests/run.py"));
  assert.match(sv, /sv-tests-report\.summary\.json/);
});


test("release builds share download-only caches and cannot retain target matrices", () => {
  const release = readFileSync(new URL("../.github/workflows/ci-napi.yml", import.meta.url), "utf8");
  const caches = [...release.matchAll(/      - uses: Swatinem\/rust-cache@v2\n([\s\S]*?)(?=\n      -|$)/g)];
  assert.equal(caches.length, 1);
  const cache = caches[0][1];
  assert.match(cache, /prefix-key: celox-napi-downloads-v1/);
  assert.match(cache, /shared-key: napi-downloads/);
  assert.match(cache, /cache-targets: false/);
  assert.match(cache, /cache-bin: false/);
  assert.match(cache, /save-if: \$\{\{ github.ref == format\('refs\/heads\/\{0\}', github.event.repository.default_branch\) \}\}/);
  assert.doesNotMatch(cache, /matrix.target|refs\/heads\/develop/);
});

test("evicted default-branch dependency caches recover without dependency changes", () => {
  const warm = readFileSync(new URL("../.github/workflows/rust-cache.yml", import.meta.url), "utf8");
  assert.match(warm, /schedule:\n    - cron: "17 \* \* \* \*"/);
  assert.match(warm, /workflow_dispatch:/);
  for (const id of cacheJobs(workflow)) {
    const block = warm.split(`  ${id}:\n`)[1].split(/\n  [a-z][a-z-]*:/)[0];
    assert.match(block, /if: github.ref == format\('refs\/heads\/\{0\}', github.event.repository.default_branch\)/);
    assert.match(block, /if: steps.cache.outputs.cache-hit != 'true'/);
  }
  assert.doesNotMatch(warm, /pull_request_target:|workflow_run:/);
});


test("PR backend policy reaches scoped, workspace fallback and queue execution", () => {
  assert.match(job("changes"), /rust_backend_filter: \$\{\{ steps\.classify\.outputs\.rust_backend_filter \}\}/);
  const rust = job("rust-tests");
  assert.match(rust, /CELOX_BACKEND_FILTER: \$\{\{ needs\.changes\.outputs\.rust_backend_filter \|\| 'all\(\)' \}\}/);
  assert.equal((rust.match(/-E "\$CELOX_BACKEND_FILTER"/g) ?? []).length, 2);
  const args = cargoArguments("rust-tests", "Run the tests this pull request can affect", {
    BUILDS: JSON.stringify([{ packages: ["celox"], targets: ["counter"], features: [] }]),
    FILTER: "binary_id(celox::counter)", CELOX_BACKEND_FILTER: "not test(parallel)",
    CELOX_VTEST_RUNNER: "/executor",
  });
  assert.equal(args.at(-1), "(binary_id(celox::counter)) & (not test(parallel))");
});


// rust-cache keys hash the job ID (or `shared-key`), the runner, the toolchain
// and every environment variable with these prefixes, so a job-level variable
// that varies between pull requests gives each of them a key no warm run saved.
const CACHE_KEY_ENV = /^(CARGO|CC|CFLAGS|CXX|CMAKE|RUST)/;

function jobBlocks(text) {
  const jobs = text.split(/^jobs:\n/m)[1];
  const parts = jobs.split(/^  ([\w-]+):\n/m);
  const blocks = new Map();
  for (let i = 1; i < parts.length; i += 2) blocks.set(parts[i], parts[i + 1]);
  return blocks;
}

function cacheJobs(text) {
  return [...jobBlocks(text)]
    .filter(([, block]) => block.includes("uses: Swatinem/rust-cache@v2"))
    .map(([id]) => id);
}

function stepWith(block, uses) {
  const step = block.match(new RegExp(`      - uses: ${uses.replace(/[.*+?^${}()|[\]\\/]/g, "\\$&")}\n((?:        .*\n?)*)`));
  if (!step) return null;
  const inputs = step[1].match(/        with:\n((?:          .*\n?)*)/);
  return (inputs ? inputs[1] : "")
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line && !line.startsWith("save-if:"))
    .sort();
}

function keyEnv(block) {
  const env = block.match(/^    env:\n((?:      .*\n)*)/m);
  return (env ? env[1] : "")
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => CACHE_KEY_ENV.test(line));
}

function workflowKeyEnv(text) {
  const env = text.match(/^env:\n((?:  .*\n)*)/m);
  return (env ? env[1] : "").split("\n").map((line) => line.trim()).filter((line) => CACHE_KEY_ENV.test(line));
}

test("every rust-cache job of CI has a warm job that computes the same key", () => {
  const warm = readFileSync(new URL("../.github/workflows/rust-cache.yml", import.meta.url), "utf8");
  const ci = jobBlocks(workflow);
  const warming = jobBlocks(warm);
  assert.deepEqual(workflowKeyEnv(warm), workflowKeyEnv(workflow));
  const ids = cacheJobs(workflow);
  assert.ok(ids.length >= 10, ids.join(","));
  for (const id of ids) {
    const job = ci.get(id);
    const twin = warming.get(id);
    assert.ok(twin, `rust-cache.yml warms no cache for CI job ${id}`);
    assert.equal(twin.match(/^    runs-on: (.*)$/m)?.[1], job.match(/^    runs-on: (.*)$/m)?.[1], id);
    assert.deepEqual(stepWith(twin, "./.github/actions/setup-rust"), stepWith(job, "./.github/actions/setup-rust"), id);
    assert.deepEqual(stepWith(twin, "Swatinem/rust-cache@v2"), stepWith(job, "Swatinem/rust-cache@v2"), id);
    assert.deepEqual(keyEnv(job), [], `${id} sets a job-level variable that enters its cache key`);
    assert.deepEqual(keyEnv(twin), [], id);
  }
});
