import assert from "node:assert/strict";
import test from "node:test";

import {
  collectMergeQueuePullRequests,
  commitReleaseImpact,
  loadMergeGroupPullRequests,
  mergeGroupHeadPullRequestNumber,
  releasePolicyFromFiles,
  releaseImpactErrors,
  titleReleaseImpact,
} from "./check-pr-release-impact.mjs";

const commit = (message, sha = "0123456789abcdef") => ({ message, sha });

test("maps pull request titles to their pre-major release policy", () => {
  const options = { preMajor: true, bumpPatchForMinorPreMajor: true };
  assert.equal(titleReleaseImpact("chore: update metadata", options), 0);
  assert.equal(titleReleaseImpact("fix: repair output", options), 1);
  assert.equal(titleReleaseImpact("feat: add output", options), 1);
  assert.equal(titleReleaseImpact("feat(api)!: remove output", options), 3);
});

test("derives feature impact from the effective release configuration", () => {
  assert.deepEqual(
    releasePolicyFromFiles(
      "0.3.1\n",
      JSON.stringify({ "bump-patch-for-minor-pre-major": true }),
    ),
    { preMajor: true, bumpPatchForMinorPreMajor: true },
  );
  assert.deepEqual(
    releasePolicyFromFiles(
      "0.3.1\n",
      JSON.stringify({
        "bump-patch-for-minor-pre-major": true,
        packages: { ".": { "bump-patch-for-minor-pre-major": false } },
      }),
    ),
    { preMajor: true, bumpPatchForMinorPreMajor: false },
  );
  assert.equal(
    titleReleaseImpact("feat: add output", {
      preMajor: true,
      bumpPatchForMinorPreMajor: false,
    }),
    2,
  );
});

test("collects only queue entries through the merge group head", () => {
  assert.deepEqual(
    collectMergeQueuePullRequests(
      [
        {
          position: 0,
          pullRequest: {
            number: 1,
            title: "fix: first",
            baseRefName: "master",
          },
        },
        {
          position: 1,
          pullRequest: {
            number: 2,
            title: "feat: group head",
            baseRefName: "feature/first",
          },
        },
        {
          position: 2,
          pullRequest: {
            number: 3,
            title: "feat: stacked but behind",
            baseRefName: "master",
          },
        },
      ],
      1,
    ),
    [
      { number: 1, title: "fix: first" },
      { number: 2, title: "feat: group head" },
    ],
  );
  assert.equal(
    mergeGroupHeadPullRequestNumber(
      "refs/heads/gh-readonly-queue/master/pr-644-0123456789abcdef0123456789abcdef01234567",
    ),
    644,
  );
  assert.equal(
    mergeGroupHeadPullRequestNumber(
      "gh-readonly-queue/master/pr-644-0123456789abcdef0123456789abcdef01234567",
    ),
    644,
  );
  assert.equal(
    mergeGroupHeadPullRequestNumber("refs/heads/feature/pr-644-deadbeef"),
    null,
  );
});

const mergeGroupHeadRef =
  "refs/heads/gh-readonly-queue/master/pr-915-d61c441f12799b8e1056db31ef183e4f5552ba6f";

function queuePage(nodes, endCursor = null) {
  return {
    repository: {
      mergeQueue: {
        entries: {
          nodes,
          pageInfo: { hasNextPage: endCursor !== null, endCursor },
        },
      },
    },
  };
}

test("loads stacked PRs from the event's queue across pages", async () => {
  const entries = [
    {
      position: 0,
      pullRequest: { number: 914, title: "refactor: first", baseRefName: "master" },
    },
    {
      position: 1,
      pullRequest: {
        number: 915,
        title: "refactor: second",
        baseRefName: "refactor/split-large-files",
      },
    },
    {
      position: 2,
      pullRequest: {
        number: 916,
        title: "refactor: third",
        baseRefName: "refactor/split-mir-optimization",
      },
    },
  ];
  const cursors = [];
  const pullRequests = await loadMergeGroupPullRequests(
    "celox-sim/celox",
    mergeGroupHeadRef,
    "refs/heads/master",
    async (query, variables) => {
      assert.match(query, /mergeQueue\(branch: \$branch\)/);
      assert.deepEqual(variables, {
        owner: "celox-sim",
        name: "celox",
        branch: "master",
        cursor: cursors.length === 0 ? null : "next-page",
      });
      cursors.push(variables.cursor);
      return variables.cursor === null
        ? queuePage(entries.slice(0, 1), "next-page")
        : queuePage(entries.slice(1));
    },
  );
  assert.deepEqual(cursors, [null, "next-page"]);
  assert.deepEqual(pullRequests, [
    { number: 914, title: "refactor: first" },
    { number: 915, title: "refactor: second" },
  ]);
  assert.equal(
    releaseImpactErrors(pullRequests[1].title, [commit("fix: hidden patch")], {
      preMajor: false,
    }).length,
    1,
  );
});

test("rejects a target absent from the event's queue", async () => {
  await assert.rejects(
    loadMergeGroupPullRequests(
      "celox-sim/celox",
      mergeGroupHeadRef,
      "refs/heads/master",
      async () =>
        queuePage([
          { position: 0, pullRequest: { number: 914, title: "refactor: first" } },
        ]),
    ),
    /#915 is not in the refs\/heads\/master merge queue/,
  );
});

test("rejects a missing merge queue", async () => {
  await assert.rejects(
    loadMergeGroupPullRequests(
      "celox-sim/celox",
      mergeGroupHeadRef,
      "refs/heads/master",
      async () => ({ repository: { mergeQueue: null } }),
    ),
    /No merge queue found/,
  );
});

test("rejects merge queue pagination without a cursor", async () => {
  const page = queuePage([]);
  page.repository.mergeQueue.entries.pageInfo.hasNextPage = true;
  await assert.rejects(
    loadMergeGroupPullRequests(
      "celox-sim/celox",
      mergeGroupHeadRef,
      "refs/heads/master",
      async () => page,
    ),
    /pagination did not return a cursor/,
  );
});

test("detects every commit form consumed by release automation", () => {
  const options = { preMajor: false };
  assert.equal(commitReleaseImpact("chore: tidy", options), 0);
  assert.equal(commitReleaseImpact("deps: update dependency", options), 1);
  assert.equal(commitReleaseImpact("fix: repair output", options), 1);
  assert.equal(commitReleaseImpact("feat: add output", options), 2);
  assert.equal(commitReleaseImpact("Fix: repair output", options), 0);
  assert.equal(commitReleaseImpact("fix!: remove output", options), 3);
  assert.equal(commitReleaseImpact("Fix!: remove output", options), 3);
  assert.equal(
    commitReleaseImpact(
      "fix: repair output\n\nBREAKING CHANGE: remove output",
      options,
    ),
    3,
  );
  assert.equal(
    commitReleaseImpact(
      "fix: repair output\n\nBREAKING CHANGE:\n continuation",
      options,
    ),
    3,
  );
  assert.equal(
    commitReleaseImpact("chore: force\n\nRelease-As: 9.0.0", options),
    4,
  );
  assert.equal(commitReleaseImpact("Release-As: 9.0.0", options), 0);
  assert.equal(
    commitReleaseImpact("chore: force\n\nRelease-As:\n 9.0.0", options),
    4,
  );
  assert.equal(
    commitReleaseImpact("chore: force\n\nRelease-As:\n\n 9.0.0", options),
    4,
  );
  assert.equal(
    commitReleaseImpact("chore: force\n\nRelease-As:\n9.0.0", options),
    0,
  );
  assert.equal(
    commitReleaseImpact(
      "Merge branch master\n\nRelease-As: 9.0.0",
      options,
    ),
    0,
  );
  assert.equal(
    commitReleaseImpact("Merge: branch master\n\nRelease-As: 9.0.0", options),
    4,
  );
  assert.equal(
    commitReleaseImpact("Merge branch master\n\nfeat: nested change", options),
    2,
  );
  assert.equal(
    commitReleaseImpact("Merge branch master\n\nfeat!: nested break", options),
    0,
  );
  assert.equal(
    commitReleaseImpact(
      "chore: outer\n\nBEGIN_NESTED_COMMIT\nFix!: nested break\nEND_NESTED_COMMIT",
      options,
    ),
    3,
  );
});

test("matches Release Please's permissive header and footer parsing", () => {
  const options = { preMajor: false };

  // @conventional-commits/parser allows optional whitespace and empty text.
  assert.equal(commitReleaseImpact("feat:no space", options), 2);
  assert.equal(commitReleaseImpact("feat:", options), 2);

  // Empty scopes fail parser 0.4.1, while whitespace is a valid, non-empty
  // scope. Keep the header matcher on the same boundary.
  assert.equal(commitReleaseImpact("feat(): add output", options), 0);
  assert.equal(commitReleaseImpact("fix()!: remove output", options), 0);
  assert.equal(commitReleaseImpact("feat( ): add output", options), 2);

  // Release Please deliberately parses commit-shaped footers as additional
  // commits, even when a human might read the line as a body example.
  assert.equal(
    commitReleaseImpact(
      "docs: explain commit syntax\n\nExample follows.\nfeat: body example",
      options,
    ),
    2,
  );

  assert.equal(
    commitReleaseImpact("chore: tidy\n\nBREAKING CHANGE:\n\nRefs: #1", options),
    0,
  );
  assert.equal(
    commitReleaseImpact(
      "chore: tidy\n\nBREAKING CHANGE:\n\nRefs: #1\nBREAKING CHANGE: remove API",
      options,
    ),
    3,
  );
});

test("rejects commit impact above the pull request title", () => {
  assert.deepEqual(
    releaseImpactErrors(
      "chore: update metadata",
      [commit("feat!: remove output")],
      { preMajor: true, bumpPatchForMinorPreMajor: true },
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
      { preMajor: true, bumpPatchForMinorPreMajor: true },
    ),
    [],
  );
});
