import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import test from "node:test";

const workflow = readFileSync(new URL("../.github/workflows/ci.yml", import.meta.url), "utf8");
function job(text, name) {
  const parts = text.split(/^  ([\w-]+):\n/m);
  const index = parts.indexOf(name);
  assert.ok(index >= 0, name);
  return parts[index + 1];
}
function runGate(name, needs) {
  const script = job(workflow, name).split("        run: |\n")[1];
  return spawnSync("bash", ["-e", "-c", script], { env: { ...process.env, NEEDS: JSON.stringify(needs) } }).status;
}

test("qualification fails on every missing, skipped, failed or cancelled required job", () => {
  const names = job(workflow, "full-validation").match(/needs:\s*\[([^\]]+)\]/)[1]
    .split(",").map(name => name.trim()).filter(Boolean);
  for (const required of ["rust-tests", "rust-default-tests", "sv-tests", "common-cells", "arm64-backend", "js-windows", "napi-wasm", "playground-browser"]) {
    assert.ok(names.includes(required), required);
  }
  const needs = Object.fromEntries(names.map(name => [name, { result: "success" }]));
  assert.equal(runGate("full-validation", needs), 0);
  for (const name of names) {
    for (const result of ["skipped", "failure", "cancelled", ""]) {
      assert.notEqual(runGate("full-validation", { ...needs, [name]: { result } }), 0, `${name}: ${result}`);
    }
  }
});

test("queue accepts deferred coverage but rejects any failed queue producer", () => {
  const needs = Object.fromEntries(["lint", "rust", "js-ubuntu", "docs"].map(name => [name, { result: "success" }]));
  for (const name of ["napi-windows", "napi-linux-arm64", "js-windows", "playground-browser"]) needs[name] = { result: "skipped" };
  needs.changes = { result: "success", outputs: { queue: "true", fast_queue: "true", full_validation: "false", rust: "true", napi: "true", napi_arm64: "true", javascript: "true", docs: "true" } };
  assert.equal(runGate("queue-validation", needs), 0);
  for (const name of ["lint", "rust", "js-ubuntu", "docs", "changes"]) {
    for (const result of ["failure", "cancelled", "skipped"]) {
      assert.notEqual(runGate("queue-validation", { ...needs, [name]: { ...needs[name], result } }), 0);
    }
  }
});

test("every full-validation checkout uses the requested immutable source", () => {
  const checkouts = [...workflow.matchAll(/uses: actions\/checkout@v7\n([\s\S]*?)(?=\n      -|\n  [\w-]+:)/g)];
  assert.ok(checkouts.length > 10);
  for (const [, checkout] of checkouts) assert.match(checkout, /ref: \$\{\{ inputs.validation_ref \|\| github.sha \}\}/);
  assert.match(workflow, /FULL_VALIDATION: .*inputs.validation_ref != ''/);
});

test("both stable publishers verify release-branch qualification of their actual build SHA", () => {
  for (const [file, source] of [["publish-crates.yml", "github.sha"], ["ci-napi.yml", "needs.validate-release.outputs.build_sha"]]) {
    const text = readFileSync(new URL(`../.github/workflows/${file}`, import.meta.url), "utf8");
    const qualification = job(text, "qualify");
    assert.match(qualification, /run: node scripts\/check-release-qualification.mjs/);
    assert.ok(qualification.includes(`CANDIDATE_SHA: \${{ ${source} }}`));
    assert.match(job(text, "publish"), /needs: \[[^\]]*qualify\]/);
    if (file === "ci-napi.yml") {
      assert.match(job(text, "publish"), /needs.qualify.result == 'success'/);
      assert.match(job(text, "publish"), /startsWith\(inputs.channel, 'nightly-'\) && needs.qualify.result == 'skipped'/);
      // Debug artifacts from qualification must never enter npm packages.
      assert.match(job(text, "publish"), /pattern: bindings-\*/);
    }
  }
});

test("queue integration tests run before reports are collected and keep normal failure handling", () => {
  const rust = job(workflow, "rust-tests");
  const smoke = rust.split("      - name: Run merge queue integration tests")[1].split("      - name:")[0];
  assert.match(smoke, /if: needs.changes.outputs.fast_queue == 'true'/);
  assert.match(smoke, /cargo nextest run --locked -p celox --features systemverilog --profile ci/);
  assert.doesNotMatch(smoke, /no-tests=pass|continue-on-error/);
  assert.ok(rust.indexOf("Run merge queue integration tests") < rust.indexOf("Collect Rust test reports"));
  assert.match(rust, /test_step_outcome: .*steps.queue-tests.outcome/);
});

test("classification keeps PRs, fast queue and full qualification distinct", () => {
  const classifier = new URL("./ci-changes.mjs", import.meta.url);
  for (const [queue, full, expectedFast] of [["false", "false", "false"], ["true", "false", "true"], ["true", "true", "false"]]) {
    const result = spawnSync(process.execPath, [classifier.pathname, "", ""], {
      env: { ...process.env, GITHUB_EVENT_NAME: "pull_request", QUEUE_VALIDATION: queue, FULL_VALIDATION: full, GITHUB_OUTPUT: "" }, encoding: "utf8",
    });
    assert.equal(result.status, 0, result.stderr);
    const outputs = Object.fromEntries(result.stdout.trim().split("\n").map(line => line.split("=")));
    assert.equal(outputs.full_validation, full);
    assert.equal(outputs.fast_queue, expectedFast);
    // Unknown diffs still request all product paths; queue mode only narrows execution.
    assert.equal(outputs.rust, "true");
    assert.equal(outputs.napi, "true");
  }
});

test("qualification names every CI producer, including future additions", () => {
  const producers = [...workflow.split(/^jobs:\n/m)[1].matchAll(/^  ([\w-]+):\n/gm)]
    .map(match => match[1]).filter(name => !["report-full-validation", "full-validation"].includes(name));
  const required = job(workflow, "full-validation").match(/needs:\s*\[([^\]]+)\]/)[1]
    .split(",").map(name => name.trim()).filter(Boolean);
  assert.deepEqual(required.sort(), producers.sort());
});
