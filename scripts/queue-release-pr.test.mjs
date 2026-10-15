import assert from "node:assert/strict";
import test from "node:test";
import {
  queueReleasePullRequest,
  RELEASE_HEAD,
  selectReleasePullRequest,
} from "./queue-release-pr.mjs";

function pr(overrides = {}) {
  return {
    id: "PR_1",
    number: 388,
    state: "OPEN",
    merged: false,
    headRefName: RELEASE_HEAD,
    isCrossRepository: false,
    isInMergeQueue: false,
    autoMergeRequest: null,
    labels: [],
    ...overrides,
  };
}
function fake(
  states = [pr(), pr({ labels: [{ name: "queued" }] })],
  candidates = [pr()],
) {
  const calls = [];
  let last = states.at(-1);
  return {
    calls,
    async listOpenPullRequests() {
      return candidates;
    },
    async getPullRequest() {
      last = states.shift() ?? last;
      return last;
    },
    async addLabel(number, label) {
      calls.push(["add", number, label]);
    },
    async removeLabel(number, label) {
      calls.push(["remove", number, label]);
    },
    async disableAutoMerge(id) {
      calls.push(["disable", id]);
    },
    async dequeuePullRequest(id) {
      calls.push(["dequeue", id]);
    },
  };
}
function run(github) {
  let time = 0;
  return queueReleasePullRequest({
    github,
    now: () => time,
    sleep: async (ms) => {
      time += ms;
    },
    timeoutMs: 30,
    pollIntervalMs: 10,
    log() {},
  });
}
test("selects only the same-repository release branch and rejects ambiguity", () => {
  const expected = pr();
  assert.equal(
    selectReleasePullRequest([
      pr({ isCrossRepository: true }),
      pr({ headRefName: "other" }),
      expected,
    ]),
    expected,
  );
  assert.throws(() => selectReleasePullRequest([pr(), pr()]), /Found 2/);
});
test("missing release does not mutate anything", async () => {
  const github = fake([], []);
  assert.deepEqual(await run(github), { outcome: "missing" });
  assert.deepEqual(github.calls, []);
});
test("requests Mergify and waits for its queue acknowledgment", async () => {
  const github = fake([pr(), pr(), pr({ labels: [{ name: "queued" }] })]);
  assert.deepEqual(await run(github), { outcome: "queued", number: 388 });
  assert.deepEqual(github.calls, [
    ["remove", 388, "dequeued"],
    ["add", 388, "merge-ready"],
  ]);
});
test("clears legacy GitHub auto-merge and queue membership before requesting Mergify", async () => {
  const github = fake([
    pr({ isInMergeQueue: true, autoMergeRequest: {} }),
    pr({ merged: true }),
  ]);
  assert.deepEqual(await run(github), { outcome: "merged", number: 388 });
  assert.deepEqual(github.calls.slice(0, 2), [
    ["dequeue", "PR_1"],
    ["disable", "PR_1"],
  ]);
});
test("a held release is not requested", async () => {
  const github = fake([pr({ labels: [{ name: "release:hold" }] })]);
  assert.deepEqual(await run(github), { outcome: "held", number: 388 });
  assert.deepEqual(github.calls, []);
});
test("a hold added while waiting removes the request before accepting queued status", async () => {
  const github = fake([
    pr(),
    pr({
      labels: ["merge-ready", "release:hold", "queued"].map((name) => ({
        name,
      })),
    }),
  ]);
  assert.deepEqual(await run(github), { outcome: "held", number: 388 });
  assert.deepEqual(github.calls.at(-1), ["remove", 388, "merge-ready"]);
});
test("failure or timeout never reports a successful queue entry", async () => {
  await assert.rejects(
    run(fake([pr(), pr({ labels: [{ name: "dequeued" }] })])),
    /Mergify dequeued/,
  );
  await assert.rejects(run(fake([pr()])), /Timed out queueing/);
  await assert.rejects(
    run(fake([pr(), pr({ state: "CLOSED" })])),
    /became CLOSED/,
  );
});
test("label write failures propagate", async () => {
  const github = fake();
  github.addLabel = async () => {
    throw new Error("forbidden");
  };
  await assert.rejects(run(github), /forbidden/);
});
