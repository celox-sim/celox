import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { isConventionalPrTitle } from "./check-pr-title.mjs";

test("accepts Conventional Commit pull request titles", () => {
  for (const title of [
    "fix: handle empty designs",
    "feat(parser): support a new construct",
    "perf(native/codegen): reduce register pressure",
    "feat(api)!: remove legacy simulator options",
    "chore(deps): update Rust dependencies",
  ]) {
    assert.equal(isConventionalPrTitle(title), true, title);
  }
});

test("rejects titles that cannot drive release automation", () => {
  for (const title of [
    "Fix empty designs",
    "feature: add a new construct",
    "feat(parser):",
    "feat(Parser): use lowercase scopes",
    "feat:  leading whitespace",
    "Merge pull request #123",
  ]) {
    assert.equal(isConventionalPrTitle(title), false, title);
  }
});

test("guards the repository setting that exposes the title to release automation", () => {
  const workflow = readFileSync(
    new URL("../.github/workflows/pr-title.yml", import.meta.url),
    "utf8",
  );

  assert.match(
    workflow,
    /run: node scripts\/check-release-repository-settings\.mjs/,
  );
  assert.match(workflow, /run: node scripts\/check-pr-release-impact\.mjs/);
  assert.match(workflow, /permissions:\n  contents: read\n  pull-requests: read/);
  assert.match(
    workflow,
    /ref: \$\{\{ github\.event\.repository\.default_branch \}\}/,
  );
  assert.match(
    workflow,
    /Locate trusted release validators[\s\S]*id: release-validators[\s\S]*check-release-repository-settings\.mjs[\s\S]*check-pr-release-impact\.mjs/,
  );
  assert.match(
    workflow,
    /Authorize the initial validator rollout[\s\S]*id: initial-rollout[\s\S]*MERGE_GROUP_BASE_SHA:[\s\S]*MERGE_GROUP_HEAD_SHA:[\s\S]*compare\/\$MERGE_GROUP_BASE_SHA\.\.\.\$MERGE_GROUP_HEAD_SHA[\s\S]*grep -Fxq "\$rollout_head"/,
  );
  assert.match(
    workflow,
    /Validate release merge settings\n\s+if: steps\.release-validators\.outputs\.available == 'true'/,
  );
  assert.match(
    workflow,
    /Validate pull request commit release impact\n\s+if: github\.event_name == 'pull_request_target' && github\.event\.pull_request\.base\.ref == github\.event\.repository\.default_branch/,
  );
  assert.match(
    workflow,
    /Revalidate merge group release impact\n\s+if: github\.event_name == 'merge_group' && github\.event\.merge_group\.base_ref == format\('refs\/heads\/\{0\}', github\.event\.repository\.default_branch\)[\s\S]*MERGE_GROUP_BASE_REF:[\s\S]*MERGE_GROUP_HEAD_REF:[\s\S]*MERGE_GROUP_SHA:[\s\S]*run: node scripts\/check-pr-release-impact\.mjs/,
  );
  assert.match(
    workflow,
    /Preserve the initial validator rollout\n\s+if: steps\.initial-rollout\.outputs\.authorized == 'true'/,
  );
  assert.match(
    workflow,
    /Reject missing trusted release validators\n\s+if: steps\.release-validators\.outputs\.available != 'true'/,
  );
});
