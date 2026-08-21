import assert from "node:assert/strict";
import test from "node:test";

import {
  commitReleaseImpact,
  releaseImpactErrors,
  titleReleaseImpact,
} from "./check-pr-release-impact.mjs";

const commit = (message, sha = "0123456789abcdef") => ({ message, sha });

test("maps pull request titles to their pre-major release policy", () => {
  const options = { preMajor: true };
  assert.equal(titleReleaseImpact("chore: update metadata", options), 0);
  assert.equal(titleReleaseImpact("fix: repair output", options), 1);
  assert.equal(titleReleaseImpact("feat: add output", options), 1);
  assert.equal(titleReleaseImpact("feat(api)!: remove output", options), 3);
});

test("detects every commit form consumed by release automation", () => {
  const options = { preMajor: false };
  assert.equal(commitReleaseImpact("chore: tidy", options), 0);
  assert.equal(commitReleaseImpact("fix: repair output", options), 1);
  assert.equal(commitReleaseImpact("feat: add output", options), 2);
  assert.equal(commitReleaseImpact("fix!: remove output", options), 3);
  assert.equal(
    commitReleaseImpact(
      "fix: repair output\n\nBREAKING CHANGE: remove output",
      options,
    ),
    3,
  );
  assert.equal(
    commitReleaseImpact("chore: force\n\nRelease-As: 9.0.0", options),
    4,
  );
  assert.equal(
    commitReleaseImpact("Merge branch master\n\nfeat!: already released", options),
    0,
  );
});

test("rejects commit impact above the pull request title", () => {
  assert.deepEqual(
    releaseImpactErrors(
      "chore: update metadata",
      [commit("feat!: remove output")],
      { preMajor: true },
    ),
    [
      '0123456789ab "feat!: remove output" has breaking impact, which exceeds the none pull request title',
    ],
  );
  assert.deepEqual(
    releaseImpactErrors("fix: repair output", [commit("feat: add output")], {
      preMajor: false,
    }),
    [
      '0123456789ab "feat: add output" has feature impact, which exceeds the patch pull request title',
    ],
  );
});

test("allows the commits from the missed breaking-change pull request", () => {
  assert.deepEqual(
    releaseImpactErrors(
      "feat(backend)!: enable aarch64 natively by default",
      [
        commit("feat(backend): enable aarch64 natively by default"),
        commit("docs(bench): remove cranelift boot series"),
        commit("fix(napi): use native backend on aarch64"),
      ],
      { preMajor: true },
    ),
    [],
  );
});
