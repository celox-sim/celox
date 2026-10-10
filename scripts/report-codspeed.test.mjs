import assert from "node:assert/strict";
import test from "node:test";
import { analysisForUpload, reportCodSpeed, waitForAnalysis } from "./report-codspeed.mjs";

const runId = "6ac92adeab7a91d8df41f721";
const comparisonUrl = `https://app.codspeed.io/celox-sim/celox/runs/compare/6ac92715aca70434a4c8fb69..${runId}?utm_source=github`;
const analysis = {
  app: { slug: "codspeed" },
  name: "CodSpeed Performance Analysis",
  status: "completed",
  conclusion: "success",
  details_url: comparisonUrl,
  output: { title: "Performance Gate Passed", summary: "4 untouched benchmarks" },
};

function fakeGitHub({ openIssues = [], checks = [[analysis]] } = {}) {
  const calls = [];
  let poll = 0;
  return {
    calls,
    async listChecks(sha) {
      calls.push(["checks", sha]);
      return checks[Math.min(poll++, checks.length - 1)];
    },
    async listOpenIssues(title) { calls.push(["list", title]); return openIssues; },
    async create(title, body) { calls.push(["create", title, body]); return 42; },
    async comment(number, body) { calls.push(["comment", number, body]); },
    async close(number) { calls.push(["close", number]); },
  };
}

function report(github, overrides = {}) {
  return reportCodSpeed({
    github, benchmarkResult: "success", runId, branch: "master", sha: "abc123",
    runUrl: "https://example.test/runs/1", log: () => {},
    sleep: async () => {}, attempts: 2, ...overrides,
  });
}

test("only this upload's CodSpeed check counts, even with older checks on the SHA", () => {
  const unrelated = [
    { ...analysis, app: { slug: "github-actions" } },
    { ...analysis, name: "other" },
    { ...analysis, details_url: "invalid" },
    { ...analysis, details_url: comparisonUrl.replace("app.codspeed.io", "example.test") },
    { ...analysis, details_url: comparisonUrl.replace(`..${runId}`, "..older") },
    { ...analysis, details_url: `https://app.codspeed.io/celox-sim/celox/runs/compare/${runId}..newer` },
  ];
  assert.equal(analysisForUpload(unrelated, runId), undefined);
  assert.equal(analysisForUpload([...unrelated, analysis], runId), analysis);
  const direct = { ...analysis, details_url: `https://app.codspeed.io/celox-sim/celox/runs/${runId}` };
  assert.equal(analysisForUpload([direct], runId), direct);
  assert.throws(() => analysisForUpload([analysis], ""), /run ID/);
});

test("analysis can appear and complete after upload; polling uses the measured SHA", async () => {
  const github = fakeGitHub({ checks: [[], [{ ...analysis, status: "in_progress" }], [analysis]] });
  const delays = [];
  assert.equal(await waitForAnalysis({
    github, sha: "abc123", runId, attempts: 3, intervalMs: 17,
    sleep: async (ms) => delays.push(ms),
  }), analysis);
  assert.deepEqual(delays, [17, 17]);
  assert.deepEqual(github.calls, Array(3).fill(["checks", "abc123"]));
});

test("regressions open an issue with comparison and benchmark details", async () => {
  const github = fakeGitHub({ checks: [[{
    ...analysis, conclusion: "failure",
    output: { title: "Performance Gate Failed", summary: "counter_n1 regressed by 12%" },
  }]] });
  assert.deepEqual(await report(github), { outcome: "opened", number: 42, failed: true });
  const [, title, body] = github.calls.find(([kind]) => kind === "create");
  assert.equal(title, "CodSpeed is failing on master");
  assert.match(body, /abc123.*https:\/\/example.test\/runs\/1/);
  assert.match(body, /performance-analysis \(failure\)/);
  assert.ok(body.includes(comparisonUrl));
  assert.match(body, /counter_n1 regressed by 12%/);
});

test("execution failure and cancellation report without polling analysis", async () => {
  for (const benchmarkResult of ["failure", "cancelled", "skipped", ""]) {
    const github = fakeGitHub();
    assert.equal((await report(github, { benchmarkResult, runId: "" })).failed, true);
    assert.equal(github.calls.some(([kind]) => kind === "checks"), false);
    assert.match(github.calls.find(([kind]) => kind === "create")[2], /codspeed \((failure|cancelled)\)/);
  }
});

test("missing ID, missing analysis, unknown conclusion, and API errors do not pass", async () => {
  for (const options of [
    { runId: "" }, { checks: [[]] },
    ...[null, "neutral", "skipped", "cancelled", "timed_out", "action_required"].map((conclusion) => ({ checks: [[{ ...analysis, conclusion }]] })),
  ]) {
    const github = fakeGitHub(options);
    assert.equal((await report(github, { runId: options.runId ?? runId })).failed, true);
    assert.ok(github.calls.some(([kind]) => kind === "create"));
  }
  const github = fakeGitHub();
  github.listChecks = async () => { throw new Error("API unavailable"); };
  assert.equal((await report(github)).failed, true);
  assert.match(github.calls.find(([kind]) => kind === "create")[2], /API unavailable/);
});

test("a repeated failure updates the issue; a passing analysis closes it", async () => {
  const openIssues = [{ number: 7, title: "CodSpeed is failing on master" }];
  const failing = fakeGitHub({ openIssues, checks: [[{ ...analysis, conclusion: "failure" }]] });
  assert.deepEqual(await report(failing), { outcome: "commented", number: 7, failed: true });
  const passing = fakeGitHub({ openIssues });
  assert.deepEqual(await report(passing), { outcome: "closed", number: 7, failed: false });
  assert.deepEqual(passing.calls.at(-1), ["close", 7]);
});

test("healthy runs leave no issue; branch issues stay separate from full CI", async () => {
  const github = fakeGitHub({ openIssues: [{ number: 7, title: "Full CI is failing on master" }] });
  assert.deepEqual(await report(github), { outcome: "passed", failed: false });
  assert.deepEqual(github.calls.map(([kind]) => kind), ["checks", "list"]);
  const develop = fakeGitHub();
  await report(develop, { branch: "develop", benchmarkResult: "failure" });
  assert.equal(develop.calls.find(([kind]) => kind === "create")[1], "CodSpeed is failing on develop");
});
