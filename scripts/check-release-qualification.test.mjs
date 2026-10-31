import assert from "node:assert/strict";
import test from "node:test";
import { qualificationState, requireQualification } from "./check-release-qualification.mjs";
const sha = "a".repeat(40);
const good = { head_sha: sha, head_branch: "release", event: "push", status: "completed", conclusion: "success" };
test("only a successful release-branch run at the exact SHA authorizes publication", () => {
  assert.equal(qualificationState([good], sha), "success");
  for (const changes of [{ head_sha: "b".repeat(40) }, { head_branch: "master" }, { event: "pull_request" }, { conclusion: "failure" }, { conclusion: "cancelled" }, { conclusion: "skipped" }]) {
    assert.equal(qualificationState([{ ...good, ...changes }], sha), "missing");
  }
});
test("publication waits for its dispatching candidate workflow to finish", async () => {
  let calls = 0;
  await requireQualification({ sha, runs: () => [++calls === 1 ? { ...good, status: "in_progress", conclusion: null } : good], sleep: async () => {} });
  assert.equal(calls, 2);
});
test("missing, failed and timed-out qualification cannot publish", async () => {
  await assert.rejects(requireQualification({ sha, runs: () => [] }), /No successful/);
  let time = 0;
  await assert.rejects(requireQualification({ sha, runs: () => [{ ...good, status: "in_progress" }], now: () => time++, timeout: 1, sleep: async () => {} }), /deadline/);
});
