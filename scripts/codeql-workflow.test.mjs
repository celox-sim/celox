import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const workflow = readFileSync(
  new URL("../.github/workflows/codeql.yml", import.meta.url),
  "utf8",
);

test("scheduled and manual scans cover all languages", () => {
  assert.match(
    workflow,
    /language: \[actions, c-cpp, javascript-typescript, python, rust\]/,
  );
  assert.match(workflow, /max-parallel: 2/);
  assert.match(workflow, /build-mode: none/);
});

test("cutover is explicit", () => {
  assert.match(workflow, /^    if: vars\.CODEQL_ADVANCED_ENABLED == 'true'$/m);
});

test("CodeQL is not a non-required PR, queue or push check", () => {
  const triggers = workflow.split("on:\n")[1].split("\nconcurrency:")[0];
  assert.doesNotMatch(
    triggers,
    /^  (push|pull_request|pull_request_target|merge_group):/m,
  );
  assert.match(triggers, /cron: "17 4 \* \* 0"/);
  assert.match(triggers, /^  workflow_dispatch:/m);
  assert.match(workflow, /persist-credentials: false/);
});
