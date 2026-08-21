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
    /Validate release merge settings\n\s+if: github\.event_name == 'pull_request_target'/,
  );
});
