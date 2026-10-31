import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { promoteCandidate } from "./promote-release-candidate.mjs";
const sha = "a".repeat(40), tag = "v0.11.0";
function fixture(overrides = {}) {
  const calls = [];
  return { calls, github: {
    branchSha: async () => sha,
    releasePull: async () => ({ number: 7 }),
    tagSha: async () => null,
    release: async () => null,
    createRelease: async (...args) => calls.push(["create", ...args]),
    dispatchPackages: async (...args) => calls.push(["dispatch", ...args]),
    markTagged: async (...args) => calls.push(["tagged", ...args]),
    ...overrides,
  } };
}
test("promotion creates the tag/release at the qualified SHA before dispatch", async () => {
  const f = fixture(); await promoteCandidate({ ...f, sha, tag });
  assert.deepEqual(f.calls, [["create", tag, sha], ["dispatch", tag], ["tagged", 7]]);
});
test("superseded, unrelated and conflicting candidates cannot publish", async () => {
  for (const overrides of [
    { branchSha: () => "b".repeat(40) }, { releasePull: () => null },
    { tagSha: () => "b".repeat(40) },
    { tagSha: () => sha, release: () => ({ draft: true }) },
    { tagSha: () => sha, release: () => ({ prerelease: true }) },
    { branchSha: () => { throw new Error("API failure"); } },
  ]) {
    const f = fixture(overrides);
    await assert.rejects(promoteCandidate({ ...f, sha, tag })); assert.deepEqual(f.calls, []);
  }
});
test("an exact existing release can resume package dispatch", async () => {
  const f = fixture({ tagSha: () => sha, release: () => ({ draft: false, prerelease: false }) });
  await promoteCandidate({ ...f, sha, tag });
  assert.deepEqual(f.calls, [["dispatch", tag], ["tagged", 7]]);
});
test("release creation failure cannot dispatch packages", async () => {
  const f = fixture({ createRelease: () => { throw new Error("creation failed"); } });
  await assert.rejects(promoteCandidate({ ...f, sha, tag })); assert.deepEqual(f.calls, []);
});
test("only release branch qualification can create the public release", () => {
  const master = readFileSync(new URL("../.github/workflows/release.yml", import.meta.url), "utf8");
  const stage = readFileSync(new URL("../.github/workflows/stage-release-candidate.yml", import.meta.url), "utf8");
  const rc = readFileSync(new URL("../.github/workflows/release-candidate.yml", import.meta.url), "utf8");
  assert.match(master, /skip-github-release: true/);
  assert.doesNotMatch(master, /release_created|release create|napi-release/);
  assert.match(stage, /git push origin "\$CANDIDATE_SHA:refs\/heads\/release"/);
  assert.match(stage, /ref: \$\{\{ github.event.pull_request.merge_commit_sha \}\}/);
  assert.match(rc, /branches: \[release\]/);
  assert.match(rc, /if: github.ref == 'refs\/heads\/release'/);
  assert.match(rc, /validation_ref: \$\{\{ needs.candidate.outputs.sha \}\}/);
  assert.match(rc, /needs: \[candidate, qualify\]/);
  assert.doesNotMatch(rc, /continue-on-error/);
});


test("one-shot candidate staging cannot be coalesced with release management or other closures", () => {
  const management = readFileSync(new URL("../.github/workflows/release.yml", import.meta.url), "utf8");
  const stage = readFileSync(new URL("../.github/workflows/stage-release-candidate.yml", import.meta.url), "utf8");
  assert.doesNotMatch(management, /^  pull_request:|^  stage-candidate:/m);
  assert.match(management, /group: release-management/);
  assert.match(stage, /pull_request:\n    types: \[closed\]\n    branches: \[master\]/);
  // Neither workflow-level nor job-level concurrency may discard a pending RC.
  assert.doesNotMatch(stage, /^\s*concurrency:/m);
  assert.match(stage, /github.event.pull_request.merged == true/);
  assert.match(stage, /github.event.pull_request.head.repo.full_name == github.repository/);
  assert.match(stage, /github.event.pull_request.head.ref == 'release-please--branches--master--components--celox'/);
  assert.doesNotMatch(stage, /git push[^\n]*(?:--force|\+)/);
});
