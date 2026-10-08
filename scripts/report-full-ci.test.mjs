import assert from "node:assert/strict";
import test from "node:test";

import { failedJobs, issueTitle, reportFullCi } from "./report-full-ci.mjs";

const passing = {
  changes: { result: "success" },
  "rust-tests": { result: "success" },
  "external-suites": { result: "success" },
};

function fakeGitHub(openIssues = []) {
  const calls = [];
  return {
    calls,
    async listOpenIssues(title) {
      calls.push(["list", title]);
      return openIssues;
    },
    async comment(number, body) {
      calls.push(["comment", number, body]);
    },
    async close(number) {
      calls.push(["close", number]);
    },
    async create(title, body) {
      calls.push(["create", title, body]);
      return 42;
    },
  };
}

function report(github, needs, branch = "master") {
  return reportFullCi({
    github,
    needs,
    branch,
    sha: "abc123",
    runUrl: "https://example.test/runs/1",
    log: () => {},
  });
}

test("failed and cancelled jobs are failures; skipped jobs are not", () => {
  assert.deepEqual(
    failedJobs({
      ...passing,
      lint: { result: "failure" },
      docs: { result: "skipped" },
      "js-ubuntu": { result: "cancelled" },
    }),
    ["js-ubuntu (cancelled)", "lint (failure)"],
  );
});

test("a first failure opens an issue naming the branch, run, and jobs", async () => {
  const github = fakeGitHub();
  const result = await report(
    github,
    { ...passing, "external-suites": { result: "failure" } },
    "develop",
  );
  assert.deepEqual(result, { outcome: "opened", number: 42 });
  const [, title, body] = github.calls.find((call) => call[0] === "create");
  assert.equal(title, issueTitle("develop"));
  assert.match(body, /`develop` at `abc123`: https:\/\/example\.test\/runs\/1/);
  assert.match(body, /- external-suites \(failure\)/);
});

test("a repeated failure comments on the open issue", async () => {
  const github = fakeGitHub([{ number: 7, title: issueTitle("master") }]);
  const result = await report(github, { ...passing, lint: { result: "failure" } });
  assert.deepEqual(result, { outcome: "commented", number: 7 });
  assert.equal(github.calls.some((call) => call[0] === "create"), false);
  assert.match(github.calls.find((call) => call[0] === "comment")[2], /lint/);
});

test("a passing run closes the open issue", async () => {
  const github = fakeGitHub([{ number: 7, title: issueTitle("master") }]);
  assert.deepEqual(await report(github, passing), {
    outcome: "closed",
    number: 7,
  });
  assert.deepEqual(github.calls.at(-1), ["close", 7]);
});

test("a passing run without an issue does nothing", async () => {
  const github = fakeGitHub();
  assert.deepEqual(await report(github, passing), { outcome: "passed" });
  assert.deepEqual(github.calls.map((call) => call[0]), ["list"]);
});

test("search matches with other titles are ignored", async () => {
  const github = fakeGitHub([
    { number: 3, title: `${issueTitle("master")} (old)` },
    { number: 4, title: issueTitle("develop") },
  ]);
  assert.deepEqual(await report(github, passing), { outcome: "passed" });
});

test("duplicate open issues are an error rather than a guess", async () => {
  const github = fakeGitHub([
    { number: 3, title: issueTitle("master") },
    { number: 4, title: issueTitle("master") },
  ]);
  await assert.rejects(report(github, passing), /Found 2 open issues/);
});
