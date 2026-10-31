import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { IssueGitHub, selectTracker, TRACKER_MARKER, TRACKER_TITLE, updateReleaseIssue } from "./release-issue.mjs";

const tracker = (extra = {}) => ({ number: 12, title: TRACKER_TITLE, body: `${TRACKER_MARKER}\nOld notes`, labels: [], ...extra });
function fixture(issues = [], candidates = [{ title: "chore(master): release 0.13.0", body: { notes: () => "Release notes" } }]) {
  const calls = [];
  return {
    calls,
    github: {
      listIssues: async () => issues,
      createIssue: async (...args) => calls.push(["create", ...args]),
      updateIssue: async (...args) => calls.push(["update", ...args]),
      closeIssue: async (...args) => calls.push(["close", ...args]),
    },
    manifest: { buildPullRequests: async () => candidates },
  };
}

test("pushes create one release issue with the planner's version and notes", async () => {
  const f = fixture();
  await updateReleaseIssue(f);
  assert.equal(f.calls.length, 1);
  assert.equal(f.calls[0][0], "create");
  assert.equal(f.calls[0][1], TRACKER_TITLE);
  assert.ok(f.calls[0][2].startsWith(TRACKER_MARKER));
  assert.match(f.calls[0][2], /release 0\.13\.0/);
  assert.match(f.calls[0][2], /Release notes/);
});

test("subsequent pushes edit the same held tracker and skip unchanged notes", async () => {
  const f = fixture([tracker({ labels: [{ name: "release:hold" }] })]);
  await updateReleaseIssue(f);
  assert.equal(f.calls[0][0], "update");
  assert.equal(f.calls[0][1], 12);
  const body = f.calls[0][2];
  f.github.listIssues = async () => [tracker({ body })];
  f.calls.length = 0;
  await updateReleaseIssue(f);
  assert.deepEqual(f.calls, []);
});

test("does not create an empty release and closes an obsolete tracker", async () => {
  const f = fixture([], []);
  await updateReleaseIssue(f);
  assert.deepEqual(f.calls, []);
  f.github.listIssues = async () => [tracker()];
  await updateReleaseIssue(f);
  assert.equal(f.calls[0][0], "close");
});

test("weekly PR handoff closes the tracker without invoking the planner", async () => {
  const f = fixture([tracker()]);
  f.manifest.buildPullRequests = () => { throw new Error("Must not plan again"); };
  await updateReleaseIssue({ ...f, pullNumber: "2716" });
  assert.deepEqual(f.calls, [["close", 12, "Release preparation continues in #2716."]]);
});

test("lookup and planner failures do not overwrite or close the tracker", async () => {
  for (const failLookup of [true, false]) {
    const f = fixture([tracker()]);
    const fail = () => { throw new Error("API unavailable"); };
    if (failLookup) f.github.listIssues = fail;
    else f.manifest.buildPullRequests = fail;
    await assert.rejects(updateReleaseIssue(f), /API unavailable/);
    assert.deepEqual(f.calls, []);
  }
  assert.equal(selectTracker([tracker({ body: "Unrelated issue" })]), null);
  assert.throws(() => selectTracker([tracker(), tracker({ number: 13 })]), /Multiple/);
});

test("issue client uses JSON input and PATCH for updates, preserving hold labels", async () => {
  const github = new IssueGitHub("celox-sim/celox");
  const calls = [];
  github.run = (args, input) => { calls.push([args, input]); return "{}"; };
  const body = 'Literal `$(echo secret)`\nnew line';
  await github.updateIssue(12, body);
  assert.deepEqual(calls, [[["api", "repos/celox-sim/celox/issues/12", "--method", "PATCH", "--input", "-"], JSON.stringify({ body })]]);
});

test("push path can only update issues; weekly and manual paths create PRs and honor hold", () => {
  const workflow = readFileSync(new URL("../.github/workflows/release.yml", import.meta.url), "utf8");
  const preview = workflow.split("  release-issue:\n")[1].split("  release-please:\n")[0];
  const prepare = workflow.split("  release-please:\n")[1].split("  queue-release:\n")[0];
  assert.match(preview, /if: github.event_name == 'push'/);
  assert.match(preview, /contents: write/);
  assert.match(preview, /issues: write/);
  assert.match(preview, /pull-requests: read/);
  assert.doesNotMatch(preview, /release-please-action|release-token|pull-requests: write/);
  assert.match(prepare, /if: github.event_name == 'schedule' \|\| github.event_name == 'workflow_dispatch'/);
  assert.match(prepare, /if: steps.release-issue.outputs.held != 'true'/);
  assert.match(prepare, /held="\$\(node scripts\/release-issue.mjs held\)"/);
  assert.match(prepare, /if: steps.release.outputs.prs_created == 'true'/);
  assert.match(prepare, /fromJSON\(steps.release.outputs.pr\).number/);
  assert.match(workflow.split("  queue-release:\n")[1], /needs.release-please.outputs.held != 'true'/);
});
