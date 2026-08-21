import assert from "node:assert/strict";
import test from "node:test";

import { releaseRepositorySettingErrors } from "./check-release-repository-settings.mjs";

test("accepts pull request titles as merge commit subjects", () => {
  assert.deepEqual(
    releaseRepositorySettingErrors({
      allow_merge_commit: true,
      merge_commit_title: "PR_TITLE",
    }),
    [],
  );
});

test("rejects the classic merge subject that hides release semantics", () => {
  assert.deepEqual(
    releaseRepositorySettingErrors({
      allow_merge_commit: true,
      merge_commit_title: "MERGE_MESSAGE",
    }),
    ["merge_commit_title must be PR_TITLE, got \"MERGE_MESSAGE\""],
  );
});

test("rejects repositories that disable the documented merge method", () => {
  assert.deepEqual(
    releaseRepositorySettingErrors({
      allow_merge_commit: false,
      merge_commit_title: "PR_TITLE",
    }),
    ["merge commits must be enabled"],
  );
});
