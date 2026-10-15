import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const workflow = readFileSync(
  new URL("../.github/workflows/codeql.yml", import.meta.url),
  "utf8",
);

test("PR analysis stays light while scheduled and manual scans cover all languages", () => {
  const expression = workflow.match(/language: \$\{\{ (.+) \}\}/)[1];
  const languages = new Function("github", "fromJSON", `return ${expression}`);
  assert.deepEqual(languages({ event_name: "pull_request" }, JSON.parse), [
    "actions",
  ]);
  for (const event_name of ["schedule", "workflow_dispatch"]) {
    assert.deepEqual(languages({ event_name }, JSON.parse), [
      "actions",
      "c-cpp",
      "javascript-typescript",
      "python",
      "rust",
    ]);
  }
  assert.match(workflow, /max-parallel: 2/);
  assert.match(workflow, /build-mode: none/);
});

test("cutover is explicit and queue drafts do not repeat PR analysis", () => {
  const expression = workflow.match(/^    if: (.+)$/m)[1];
  const enabled = new Function(
    "vars",
    "github",
    "startsWith",
    `return ${expression}`,
  );
  const startsWith = (value, prefix) => value.startsWith(prefix);
  for (const value of [undefined, "false", "true"]) {
    for (const head_ref of [
      "",
      "feature/example",
      "mergify/merge-queue/batch",
    ]) {
      assert.equal(
        enabled({ CODEQL_ADVANCED_ENABLED: value }, { head_ref }, startsWith),
        value === "true" && !head_ref.startsWith("mergify/merge-queue/"),
      );
    }
  }
});

test("ordinary source changes and pushes do not start CodeQL", () => {
  const triggers = workflow.split("on:\n")[1].split("\nconcurrency:")[0];
  assert.doesNotMatch(triggers, /^  (push|pull_request_target|merge_group):/m);
  const paths = [...triggers.matchAll(/^      - "(.+)"$/gm)].map(
    (match) => match[1],
  );
  assert.deepEqual(paths, [".github/workflows/**", ".github/actions/**"]);
  assert.match(triggers, /cron: "17 4 \* \* 0"/);
  assert.match(triggers, /^  workflow_dispatch:/m);
  assert.match(workflow, /persist-credentials: false/);
});
