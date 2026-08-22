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

test("accepts pull request titles as merge commit subjects", () => {
  assert.deepEqual(releaseRepositorySettingErrors(validRepository), []);
});

test("rejects the classic merge subject that hides release semantics", () => {
  assert.deepEqual(
    releaseRepositorySettingErrors({
      ...validRepository,
      merge_commit_title: "MERGE_MESSAGE",
    }),
    ["merge_commit_title must be PR_TITLE, got \"MERGE_MESSAGE\""],
  );
});

test("rejects merge paths that bypass pull request titles", () => {
  assert.deepEqual(
    releaseRepositorySettingErrors({
      ...validRepository,
      allow_merge_commit: false,
      allow_squash_merge: true,
      allow_rebase_merge: true,
    }),
    [
      "merge commits must be enabled",
      "squash merges must be disabled",
      "rebase merges must be disabled",
    ],
  );
});

test("rejects merge commit bodies as release inputs", () => {
  assert.deepEqual(
    releaseRepositorySettingErrors({
      ...validRepository,
      merge_commit_message: "PR_BODY",
    }),
    ["merge_commit_message must be BLANK, got \"PR_BODY\""],
  );
});
