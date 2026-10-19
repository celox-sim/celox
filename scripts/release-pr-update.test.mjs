import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

const workflow = readFileSync(
  new URL("../.github/workflows/release.yml", import.meta.url),
  "utf8",
);
const guard = workflow
  .split("      - name: Check whether the release pull request is frozen\n")[1]
  .split("      - name: Create release or update release pull request\n")[0];
const script = guard
  .split("        run: |\n")[1]
  .split("\n")
  .map((line) => line.replace(/^          /, ""))
  .join("\n");

function check(pullRequests, { fail = false } = {}) {
  const directory = mkdtempSync(join(tmpdir(), "celox-release-pr-update-"));
  const output = join(directory, "output");
  writeFileSync(output, "");
  writeFileSync(
    join(directory, "gh"),
    `#!/usr/bin/env node
const assert = require("node:assert/strict");
assert.deepEqual(process.argv.slice(2), [
  "pr", "list", "--repo", "celox-sim/celox",
  "--state", "open", "--base", "master",
  "--head", "release-please--branches--master--components--celox",
  "--json", "isCrossRepository,labels",
]);
if (process.env.FAIL_QUERY === "true") process.exit(1);
console.log(process.env.PULL_REQUESTS);
`,
    { mode: 0o755 },
  );
  try {
    const result = spawnSync("bash", ["-e", "-o", "pipefail", "-c", script], {
      env: {
        ...process.env,
        PATH: `${directory}:${process.env.PATH}`,
        GITHUB_REPOSITORY: "celox-sim/celox",
        GITHUB_OUTPUT: output,
        PULL_REQUESTS: JSON.stringify(pullRequests),
        FAIL_QUERY: String(fail),
      },
      encoding: "utf8",
    });
    return { ...result, output: readFileSync(output, "utf8") };
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

const pr = (...labels) => ({
  isCrossRepository: false,
  labels: labels.map((name) => ({ name })),
});

test("freezes release updates throughout admission, validation and dequeue", () => {
  for (const labels of [
    ["merge-ready"],
    ["merge-ready", "queued"],
    ["merge-ready", "dequeued"],
    ["queued"],
    ["merge-ready", "release:hold"],
  ]) {
    const result = check([pr(...labels)]);
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.output, "frozen=true\n", labels.join(","));
  }
});

test("allows updates before queueing, after withdrawal, and after merging", () => {
  for (const pullRequests of [
    [],
    [pr()],
    [pr("autorelease: pending")],
    [pr("dequeued")],
    [pr("release:hold")],
    [{ ...pr("merge-ready"), isCrossRepository: true }],
  ]) {
    const result = check(pullRequests);
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.output, "frozen=false\n");
  }
});

test("a failed lookup stops the workflow instead of permitting a rewrite", () => {
  const result = check([], { fail: true });
  assert.notEqual(result.status, 0);
  assert.equal(result.output, "");
});

test("freeze suppresses only PR writes, preserving release creation on every event", () => {
  const action = workflow
    .split("      - name: Create release or update release pull request\n")[1]
    .split("      - name: Queue package publication for this release\n")[0];
  assert.match(guard, /id: release-pr/);
  assert.match(guard, /GH_TOKEN: \$\{\{ steps\.release-token\.outputs\.token \}\}/);
  assert.match(
    action,
    /skip-github-pull-request: \$\{\{ steps\.release-pr\.outputs\.frozen == 'true' \}\}/,
  );
  assert.doesNotMatch(action, /skip-github-release:|\n        if:/);
  assert.doesNotMatch(guard, /github\.event_name|\n        if:/);
});
