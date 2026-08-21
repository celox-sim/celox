import assert from "node:assert/strict";
import test from "node:test";

import { releaseRepositorySettingErrors } from "./check-release-repository-settings.mjs";

const validRepository = {
  allow_merge_commit: true,
  allow_squash_merge: false,
  allow_rebase_merge: false,
  merge_commit_title: "PR_TITLE",
  merge_commit_message: "BLANK",
};

const validRulesets = [
  {
    enforcement: "active",
    target: "branch",
    conditions: { ref_name: { include: ["~DEFAULT_BRANCH"] } },
    rules: [{ type: "merge_queue", parameters: { merge_method: "MERGE" } }],
  },
];

test("accepts pull request titles as merge commit subjects", () => {
  assert.deepEqual(
    releaseRepositorySettingErrors(validRepository, validRulesets),
    [],
  );
});

test("rejects the classic merge subject that hides release semantics", () => {
  assert.deepEqual(
    releaseRepositorySettingErrors(
      { ...validRepository, merge_commit_title: "MERGE_MESSAGE" },
      validRulesets,
    ),
    ["merge_commit_title must be PR_TITLE, got \"MERGE_MESSAGE\""],
  );
});

test("rejects merge paths that bypass pull request titles", () => {
  assert.deepEqual(
    releaseRepositorySettingErrors(
      {
        ...validRepository,
        allow_merge_commit: false,
        allow_squash_merge: true,
        allow_rebase_merge: true,
      },
      validRulesets,
    ),
    [
      "merge commits must be enabled",
      "squash merges must be disabled",
      "rebase merges must be disabled",
    ],
  );
});

test("rejects merge commit bodies as release inputs", () => {
  assert.deepEqual(
    releaseRepositorySettingErrors(
      { ...validRepository, merge_commit_message: "PR_BODY" },
      validRulesets,
    ),
    ["merge_commit_message must be BLANK, got \"PR_BODY\""],
  );
});

test("requires a merge-only queue on the default branch", () => {
  assert.deepEqual(releaseRepositorySettingErrors(validRepository, []), [
    "the default branch must have an active merge queue",
  ]);
  assert.deepEqual(
    releaseRepositorySettingErrors(validRepository, [
      {
        ...validRulesets[0],
        rules: [
          { type: "merge_queue", parameters: { merge_method: "SQUASH" } },
        ],
      },
    ]),
    ["the default branch merge queue must use MERGE, got \"SQUASH\""],
  );
});
