import assert from "node:assert/strict";
import test from "node:test";
import {
  pullNumbers,
  validatePlan,
  readPlan,
  renderBody,
  generation,
  validateRollupSources,
} from "./rollup-plan.mjs";

export function fixturePlan() {
  return {
    repository: "o/r",
    id: "123",
    base: "master",
    baseSha: "a".repeat(40),
    tips: [1, 2],
    queue: false,
    sources: [
      { number: 1, sha: "b".repeat(40), heldBefore: false },
      { number: 2, sha: "c".repeat(40), heldBefore: true },
    ],
  };
}

test("rollup input and metadata reject ambiguous sources and invalid identity", () => {
  assert.deepEqual(pullNumbers("#1, 2\n3"), [1, 2, 3]);
  for (const value of [
    "",
    "1,1",
    "0",
    "1,garbage",
    "9007199254740992",
    "1; echo bad",
  ])
    assert.throws(() => pullNumbers(value));
  const plan = fixturePlan();
  const body = renderBody(plan, "Ready");
  assert.deepEqual(readPlan(body, "o/r"), plan);
  assert.equal(readPlan("ordinary PR", "o/r"), null);
  assert.throws(() => readPlan(body, "different/repo"));
  assert.throws(() => readPlan(body + body, "o/r"));
  for (const change of [
    (p) => p.sources.push(p.sources[0]),
    (p) => (p.sources[0].sha = "main"),
    (p) => (p.base = "feature/x"),
    (p) => (p.tips = [9]),
    (p) => (p.id = "../master"),
    (p) => (p.sources[0].heldBefore = "false"),
  ]) {
    const invalid = fixturePlan();
    change(invalid);
    assert.throws(() => validatePlan(invalid, "o/r"));
  }
  const updated = fixturePlan();
  updated.sources[1].sha = "d".repeat(40);
  assert.notEqual(generation(updated), generation(plan));
});

test("queue validation requires current source heads contained in the rollup", async () => {
  const plan = fixturePlan();
  const pull = {
    number: 50,
    head: {
      ref: "rollup/123",
      repo: { full_name: "o/r" },
      sha: "d".repeat(40),
    },
    base: { ref: "master" },
    body: renderBody(plan, "Ready"),
  };
  const api = async (path) => {
    if (path.includes("/compare/")) return { status: "ahead" };
    const source = plan.sources.find((s) =>
      path.endsWith(`/pulls/${s.number}`),
    );
    return {
      state: "open",
      head: { sha: source.sha, repo: { full_name: "o/r" } },
    };
  };
  await validateRollupSources("o/r", pull, api);
  await validateRollupSources("o/r", { head: { ref: "normal" } }, api);
  for (const change of [
    (p, result) => result.head && (result.head.sha = "e".repeat(40)),
    (p, result) => result.status && (result.status = "diverged"),
    (p, result) => result.head && (result.state = "closed"),
    (p, result) => result.head && (result.draft = true),
  ]) {
    await assert.rejects(
      validateRollupSources("o/r", pull, async (path) => {
        const result = await api(path);
        change(path, result);
        return result;
      }),
    );
  }
  for (const altered of [
    { ...pull, body: "metadata removed" },
    { ...pull, base: { ref: "develop" } },
    { ...pull, head: { ...pull.head, repo: { full_name: "fork/r" } } },
    { ...pull, number: 1 },
  ])
    await assert.rejects(validateRollupSources("o/r", altered, api));
});
