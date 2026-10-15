import assert from "node:assert/strict";
import test from "node:test";
import {
  batchPullRequestNumbers,
  validateBatch,
} from "./check-mergify-batch.mjs";

const sha = "a".repeat(40);
const baseSha = "c".repeat(40);
const sourceSha = "b".repeat(40);
const metadata = { pull_requests: [{ number: 12 }, { number: 13 }] };
function fixture() {
  const batch = {
    user: { login: "mergify[bot]", type: "Bot" },
    head: {
      repo: { full_name: "o/r" },
      sha,
      ref: "mergify/merge-queue/master/pr-12",
    },
    base: { ref: "master", sha: baseSha },
    draft: true,
  };
  const source = {
    title: "fix: correct behavior",
    head: { sha: sourceSha },
    base: { ref: "master" },
  };
  const calls = [];
  const api = async (path) => {
    calls.push(path);
    if (path === "/repos/o/r/pulls/99") return batch;
    if (path === "/repos/o/r") return { default_branch: "master" };
    if (path.startsWith("/repos/o/r/pulls/")) return source;
    if (path === `/repos/o/r/compare/${sourceSha}...${sha}`)
      return { status: "ahead" };
    throw new Error(`Unexpected API: ${path}`);
  };
  const input = { repository: "o/r", number: 99, sha, metadata };
  const commits = async () => [
    { sha: sourceSha, message: "fix: correct behavior" },
  ];
  const policy = async (repo, ref) => {
    assert.equal(repo, "o/r");
    assert.ok([sha, baseSha].includes(ref));
    return { preMajor: false, bumpPatchForMinorPreMajor: false };
  };
  return { batch, source, calls, api, input, commits, policy };
}

test("batch metadata must identify nonempty, distinct original PRs", () => {
  assert.deepEqual(batchPullRequestNumbers(metadata), [12, 13]);
  for (const value of [
    null,
    {},
    { pull_requests: [] },
    { pull_requests: [{ number: "12" }] },
    { pull_requests: [{ number: 0 }] },
    { pull_requests: [{ number: 12 }, { number: 12 }] },
  ]) {
    assert.throws(() => batchPullRequestNumbers(value));
  }
});

test("validates every source PR against policy from the exact batch tree", async () => {
  const f = fixture();
  assert.deepEqual(
    await validateBatch(f.input, f.api, f.commits, f.policy),
    [12, 13],
  );
  assert.ok(f.calls.includes("/repos/o/r/pulls/12"));
  assert.ok(f.calls.includes("/repos/o/r/pulls/13"));
});

test("rejects spoofed batch authors, forks, stale heads, and non-drafts", async () => {
  for (const mutate of [
    (f) => (f.batch.user.login = "someone"),
    (f) => (f.batch.user.type = "User"),
    (f) => (f.batch.head.repo.full_name = "fork/r"),
    (f) => (f.batch.head.sha = sourceSha),
    (f) => (f.batch.draft = false),
    (f) => (f.batch.head.ref = "feature/test"),
  ]) {
    const f = fixture();
    mutate(f);
    await assert.rejects(
      validateBatch(f.input, f.api, f.commits, f.policy),
      /expected Mergify/,
    );
  }
});

test("rejects source PRs with invalid titles or another target branch", async () => {
  for (const mutate of [
    (f) => (f.source.title = "Merge changes"),
    (f) => (f.source.base.ref = "develop"),
  ]) {
    const f = fixture();
    mutate(f);
    await assert.rejects(
      validateBatch(f.input, f.api, f.commits, f.policy),
      /Invalid title or base/,
    );
  }
});

test("rejects a force-pushed source head absent from the batch", async () => {
  const f = fixture();
  await assert.rejects(
    validateBatch(
      f.input,
      (path) =>
        path.includes("/compare/") ? { status: "diverged" } : f.api(path),
      f.commits,
      f.policy,
    ),
    /not contained/,
  );
});

test("a batch cannot conceal a source PR's understated release impact", async () => {
  const f = fixture();
  await assert.rejects(
    validateBatch(
      f.input,
      f.api,
      async () => [{ sha: sourceSha, message: "feat!: break the API" }],
      f.policy,
    ),
    /exceeds/,
  );
});

test("develop batches still validate titles without applying master release policy", async () => {
  const f = fixture();
  f.batch.base.ref = "develop";
  f.source.base.ref = "develop";
  assert.deepEqual(
    await validateBatch(
      f.input,
      f.api,
      () => assert.fail(),
      () => assert.fail(),
    ),
    [12, 13],
  );
});

test("policy changes revalidate preceding speculative PRs missing from this batch's metadata", async () => {
  const f = fixture();
  const predecessorSha = "d".repeat(40);
  const api = async (path) => {
    if (path.includes(`/compare/${baseSha}...`))
      return { total_commits: 1, commits: [{ sha: predecessorSha }] };
    if (path.includes("/pulls?"))
      return [{ number: 11, draft: false, head: { sha: predecessorSha } }];
    if (path.endsWith("/pulls/11"))
      return { ...f.source, head: { sha: predecessorSha } };
    if (path.includes(`/compare/${predecessorSha}...`))
      return { status: "ahead" };
    return f.api(path);
  };
  await assert.rejects(
    validateBatch(
      f.input,
      api,
      async (repo, number) =>
        number === 11
          ? [
              {
                sha: predecessorSha,
                message: "feat: originally accepted as a pre-major patch",
              },
            ]
          : f.commits(),
      async (repo, ref) => ({
        preMajor: ref === baseSha,
        bumpPatchForMinorPreMajor: true,
      }),
    ),
    /#11:.*exceeds/,
  );
});
