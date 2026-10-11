import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const read = name => readFileSync(new URL(`../.github/workflows/${name}.yml`, import.meta.url), "utf8");
const heliodor = read("heliodor-bench");
const bench = read("bench");
const codspeed = read("codspeed");
function job(workflow, name) {
  const blocks = workflow.split(/^  ([\w-]+):\n/m);
  const index = blocks.indexOf(name);
  assert.ok(index > 0, `Missing job: ${name}`);
  return blocks[index + 1];
}
function expression(block, property) {
  const match = block.match(new RegExp(`^    ${property}: (.+)\\n`, "m"));
  assert.ok(match, `Missing ${property}`);
  if (match[1] !== ">-") return match[1];
  return block.slice(match.index + match[0].length).match(/^(?:      .+\n)+/)[0].trim().replace(/\s*\n\s*/g, " ");
}
// Exercise the actual job conditions, including combinations that must avoid
// starting expensive simulation jobs. These expressions use JS-compatible
// boolean operators and equality. Supply the cancellation status and input
// validation result explicitly, including the skipped schedule prerequisite.
function runs(name, event, ref, inputs = {}, validation = event === "workflow_dispatch" ? "success" : "skipped", cancelled = false) {
  return Boolean(new Function("github", "inputs", "needs", "cancelled", `return (${expression(job(heliodor, name), "if")});`)(
    { event_name: event, ref: `refs/heads/${ref}` }, inputs,
    { "validate-dispatch": { result: validation } }, () => cancelled,
  ));
}

test("benchmarks run daily and manually; PRs only test the Heliodor tooling", () => {
  for (const workflow of [heliodor, bench, codspeed]) {
    const triggers = workflow.match(/^on:\n([\s\S]*?)\n\S/m)[1];
    assert.doesNotMatch(triggers, /^\s+push:|^\s+merge_group:/m);
    assert.match(triggers, /^\s+schedule:/m);
    assert.match(triggers, /^\s+workflow_dispatch:/m);
  }
  assert.doesNotMatch(codspeed, /^  pull_request:/m);
  const paths = heliodor.match(/  pull_request:\n    paths:\n([\s\S]*?)  schedule:/)[1];
  assert.doesNotMatch(paths, /Cargo\.(lock|toml)|crates\/|ci-changes/);
  assert.match(job(heliodor, "converter-test"), /scripts\/benchmark-workflows\.test\.mjs/);
  assert.doesNotMatch(heliodor, /^  arm64-linux-boot:|^  arm64-changes:/m);
  for (const name of ["heliodor", "linux-suite-matrix", "heliodor-head", "arm64-linux-boot-full"]) {
    assert.equal(runs(name, "pull_request", "master"), false, name);
  }
});

test("CodSpeed reports the current upload even after failure or cancellation", () => {
  const benchmark = job(codspeed, "codspeed");
  const report = job(codspeed, "report");
  assert.match(benchmark, /run-id: \$\{\{ steps\.benchmarks\.outputs\.run-id \}\}/);
  assert.match(benchmark, /id: benchmarks\n\s+uses: CodSpeedHQ\/action@v5/);
  assert.match(report, /needs: codspeed/);
  assert.match(report, /checks: read/);
  assert.match(report, /issues: write/);
  assert.match(report, /BENCHMARK_RESULT: \$\{\{ needs\.codspeed\.result \}\}/);
  assert.match(report, /CODSPEED_RUN_ID: \$\{\{ needs\.codspeed\.outputs\.run-id \}\}/);
  assert.match(report, /run: node scripts\/report-codspeed\.mjs/);
  assert.doesNotMatch(codspeed, /continue-on-error:/);
  assert.match(codspeed, /cancel-in-progress: false/);
  const condition = new Function("github", "always", `return (${expression(report, "if")});`);
  for (const branch of ["master", "develop", "feature"]) {
    assert.equal(condition({ ref_name: branch }, () => true), branch !== "feature");
  }
});

test("master daily runs the full suite; develop defaults to pinned and HEAD boot checks", () => {
  for (const [name, master, develop] of [
    ["linux-suite-matrix", true, false], ["heliodor", false, true],
    ["heliodor-head", true, true], ["arm64-linux-boot-full", false, false],
  ]) {
    assert.equal(runs(name, "schedule", "master"), master, name);
    assert.equal(runs(name, "workflow_dispatch", "develop"), develop, name);
  }
  assert.match(job(heliodor, "linux-suite"), /needs: linux-suite-matrix/);
  assert.match(read("nightly"), /gh workflow run heliodor-bench\.yml --repo "\$GITHUB_REPOSITORY" --ref develop/);
});

test("explicit selections and ARM64 profiles retain their separate execution paths", () => {
  for (const inputs of [{ suite_test: "test_soc_linux_boot" }, { suite_runner: "celox" }, { suite_arch: "aarch64" }]) {
    for (const ref of ["master", "develop", "feature"]) {
      assert.equal(runs("linux-suite-matrix", "workflow_dispatch", ref, inputs), true);
      assert.equal(runs("heliodor", "workflow_dispatch", ref, inputs), false);
      assert.equal(runs("heliodor-head", "workflow_dispatch", ref, inputs), false);
    }
  }
  for (const ref of ["master", "develop", "feature"]) {
    assert.equal(runs("arm64-linux-boot-full", "workflow_dispatch", ref, { arm64_profile: true }), true);
    for (const name of ["heliodor", "heliodor-head", "linux-suite-matrix"]) {
      assert.equal(runs(name, "workflow_dispatch", ref, { arm64_profile: true }), false, name);
    }
  }
});

test("measurement queues are independent and retain only one pending run", () => {
  for (const workflow of [heliodor, bench]) {
    const concurrency = workflow.match(/^concurrency:\n([\s\S]*?)\njobs:/m)[1];
    // GitHub defaults to one pending run when queue is omitted.
    assert.doesNotMatch(concurrency, /queue:/);
    assert.doesNotMatch(concurrency, /queue: max|group: bench\n|\|\| 'bench'/);
  }
  assert.match(bench, /group: benchmark-\$\{\{ github\.ref \}\}/);
  assert.match(heliodor, /format\('heliodor-bench-\{0\}', github\.ref\)/);
  assert.match(bench, /cancel-in-progress: false/);
  assert.match(job(heliodor, "linux-suite"), /max-parallel: 2/);
});

test("publication keeps comparisons and retries without pushing a stale shared history", () => {
  const publish = job(bench, "publish");
  assert.match(publish, /if: github.ref == 'refs\/heads\/master'/);
  assert.match(publish, /node scripts\/publish-bench\.mjs rust-converted\.json verilator-converted\.json ts-converted\.json --host artifacts\/bench-host\/benchmark-host\.txt/);
  assert.doesNotMatch(publish, /auto-push: true/);
  assert.equal([...publish.matchAll(/comment-on-alert: true/g)].length, 3);
  // Each history records the CPU that produced it, so the dashboard can keep
  // one series per CPU model.
  assert.match(job(bench, "bench-comparison"), /name: bench-host/);
  assert.match(job(heliodor, "linux-suite"), /HELIODOR_PUBLISH_HOST: .*heliodor-suite-host\.txt/);
});


test("failed manual validation and cancellation cannot start expensive jobs", () => {
  for (const name of ["heliodor", "heliodor-head", "linux-suite-matrix", "arm64-linux-boot-full"]) {
    assert.match(job(heliodor, name), /needs: validate-dispatch/);
    for (const result of ["failure", "cancelled", "skipped"]) {
      for (const ref of ["master", "develop", "feature"]) {
        for (const inputs of [{}, { suite_test: "test_soc_linux_boot" }, { arm64_profile: true }]) {
          assert.equal(runs(name, "workflow_dispatch", ref, inputs, result), false, name);
        }
      }
    }
    assert.equal(runs(name, "schedule", "master", {}, "skipped", true), false, name);
  }
});
