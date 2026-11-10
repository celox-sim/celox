import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const workflow = readFileSync(
  new URL("../.github/workflows/sync-veryl-vendor.yml", import.meta.url),
  "utf8",
);

test("vendor sync does not use the default-policy-blocked PR event", () => {
  assert.doesNotMatch(workflow, /pull_request_target/);
  assert.match(
    workflow,
    /^  pull_request:\n    types: \[opened, synchronize, reopened\]\n    paths:\n      - Cargo\.toml$/m,
  );
  assert.match(workflow, /^  workflow_dispatch:$/m);
});

test("vendor sync stays limited to same-repository Renovate PRs", () => {
  assert.match(
    workflow,
    /github\.event\.pull_request\.head\.repo\.full_name == github\.repository/,
  );
  assert.match(workflow, /startsWith\(github\.event\.pull_request\.head\.ref, 'renovate\/'\)/);
  // Scripts execute only from the trusted base checkout; the PR tree is data.
  assert.match(workflow, /ref: \$\{\{ steps\.pull-request\.outputs\.base_sha \}\}/);
  assert.match(workflow, /path: automation/);
});
