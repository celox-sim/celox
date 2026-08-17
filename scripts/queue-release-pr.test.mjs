import assert from "node:assert/strict";
import test from "node:test";

import {
  queueReleasePullRequest,
  RELEASE_HEAD,
  selectReleasePullRequest,
} from "./queue-release-pr.mjs";

function pullRequest(overrides = {}) {
  return {
    id: "PR_1",
    number: 388,
    state: "OPEN",
    merged: false,
    headRefName: RELEASE_HEAD,
    isCrossRepository: false,
    isInMergeQueue: false,
    mergeQueueEntry: null,
    autoMergeRequest: null,
    labels: [],
    ...overrides,
  };
}

function fakeGitHub({ candidates = [pullRequest()], states, enqueue }) {
  const calls = [];
  const queuedStates = [...(states ?? [pullRequest()])];
  let lastState = queuedStates.at(-1);

  return {
    calls,
    async listOpenPullRequests() {
      calls.push("list");
      return candidates;
    },
    async getPullRequest(id) {
      calls.push(["get", id]);
      lastState = queuedStates.shift() ?? lastState;
      return lastState;
    },
    async enableAutoMerge(id) {
      calls.push(["enable", id]);
    },
    async disableAutoMerge(id) {
      calls.push(["disable", id]);
    },
    async enqueuePullRequest(id) {
      calls.push(["enqueue", id]);
      return enqueue ? enqueue() : { id: "MQE_1", position: 1 };
    },
    async dequeuePullRequest(id) {
      calls.push(["dequeue", id]);
    },
  };
}

test("selects only the trusted same-repository release branch", () => {
  const expected = pullRequest();
  assert.equal(
    selectReleasePullRequest([
      pullRequest({ id: "fork", isCrossRepository: true }),
      pullRequest({ id: "other", headRefName: "release-lookalike" }),
      expected,
    ]),
    expected,
  );
});

test("rejects ambiguous release pull requests", () => {
  assert.throws(
    () => selectReleasePullRequest([pullRequest(), pullRequest({ id: "PR_2" })]),
    /Found 2 matching release pull requests/,
  );
});

test("does nothing when no release pull request is open", async () => {
  const github = fakeGitHub({ candidates: [] });
  const result = await queueReleasePullRequest({ github });
  assert.deepEqual(result, { outcome: "missing" });
  assert.deepEqual(github.calls, ["list"]);
});

test("enables auto-merge and requires an explicit queue entry", async () => {
  const github = fakeGitHub({});
  const result = await queueReleasePullRequest({ github });
  assert.deepEqual(result, { outcome: "queued", number: 388, position: 1 });
  assert.deepEqual(github.calls, [
    "list",
    ["get", "PR_1"],
    ["enable", "PR_1"],
    ["get", "PR_1"],
    ["enqueue", "PR_1"],
  ]);
});

test("observes a queue entry created asynchronously", async () => {
  const github = fakeGitHub({
    states: [
      pullRequest({ autoMergeRequest: { enabledAt: "now" } }),
      pullRequest({ autoMergeRequest: { enabledAt: "now" } }),
      pullRequest({
        autoMergeRequest: { enabledAt: "now" },
        isInMergeQueue: true,
      }),
    ],
    enqueue() {
      throw new Error("Required checks have not passed");
    },
  });
  let clock = 0;
  const result = await queueReleasePullRequest({
    github,
    timeoutMs: 100,
    pollIntervalMs: 10,
    now: () => clock,
    sleep: async (duration) => {
      clock += duration;
    },
    log() {},
  });
  assert.deepEqual(result, { outcome: "queued", number: 388 });
});

test("honors a hold added while waiting", async () => {
  const github = fakeGitHub({
    states: [
      pullRequest({ autoMergeRequest: { enabledAt: "now" } }),
      pullRequest({ autoMergeRequest: { enabledAt: "now" } }),
      pullRequest({
        autoMergeRequest: { enabledAt: "now" },
        labels: [{ name: "release:hold" }],
      }),
    ],
    enqueue() {
      throw new Error("Required checks have not passed");
    },
  });
  let clock = 0;
  const result = await queueReleasePullRequest({
    github,
    timeoutMs: 100,
    pollIntervalMs: 10,
    now: () => clock,
    sleep: async (duration) => {
      clock += duration;
    },
    log() {},
  });
  assert.deepEqual(result, { outcome: "held", number: 388 });
  assert.ok(
    github.calls.some(
      (call) => Array.isArray(call) && call[0] === "disable",
    ),
  );
});

test("removes an already queued held release", async () => {
  const held = pullRequest({
    isInMergeQueue: true,
    mergeQueueEntry: { id: "MQE_1", position: 1 },
    autoMergeRequest: { enabledAt: "now" },
    labels: [{ name: "release:hold" }],
  });
  const github = fakeGitHub({ states: [held, held] });
  const result = await queueReleasePullRequest({ github, log() {} });
  assert.deepEqual(result, { outcome: "held", number: 388 });
  assert.ok(
    github.calls.some(
      (call) => call[0] === "dequeue" && call[1] === "MQE_1",
    ),
  );
  assert.ok(github.calls.some((call) => call[0] === "disable"));
});

test("fails instead of reporting success when queueing times out", async () => {
  const github = fakeGitHub({
    enqueue() {
      throw new Error("Required checks have not passed");
    },
  });
  let clock = 0;
  await assert.rejects(
    queueReleasePullRequest({
      github,
      timeoutMs: 20,
      pollIntervalMs: 10,
      now: () => clock,
      sleep: async (duration) => {
        clock += duration;
      },
      log() {},
    }),
    /Timed out queueing release pull request #388.*Required checks have not passed/,
  );
});
