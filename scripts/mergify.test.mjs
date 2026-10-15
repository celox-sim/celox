import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const read = (path) =>
  readFileSync(new URL(`../${path}`, import.meta.url), "utf8");

test("queue stays in place so original PR checks validate the merge", () => {
  const config = read(".mergify.yml");
  assert.match(config, /max_parallel_checks: 1/);
  assert.match(config, /batch_size: 1/);
  assert.match(config, /max_checks_retries: 0/);
  assert.doesNotMatch(config, /^\s+merge_conditions:/m);
  assert.match(config, /merge_method: merge/);
  assert.match(config, /update_method: merge/);
  assert.match(config, /title: pr-title\n\s+body: empty\n\s+trailers: \[\]/);
  assert.match(config, /branch_protection_injection_mode: queue/);
  assert.match(config, /- -label = release:hold/);
  assert.match(
    config,
    /head != release-please--branches--master--components--celox\n\s+- label = merge-ready/,
  );
});

test("automated lane updates request Mergify rather than native auto-merge", () => {
  for (const name of ["sync-develop", "veryl-head"]) {
    const workflow = read(`.github/workflows/${name}.yml`);
    assert.match(workflow, /--add-label merge-ready/);
    assert.doesNotMatch(workflow, /gh pr merge|--auto/);
    assert.match(workflow, /permission-issues: write/);
  }
});

test("Renovate preserves the manually reviewed compatibility group", () => {
  const rules = JSON.parse(read("renovate.json")).packageRules;
  assert.ok(rules.every((rule) => rule.automerge !== true));
  const compatibility = rules.find(
    (rule) => rule.groupSlug === "napi-rs-emnapi-compatibility",
  );
  assert.deepEqual(compatibility.labels, ["dependencies"]);
  assert.equal(
    rules.filter((rule) => rule.labels?.includes("merge-ready")).length,
    4,
  );
  assert.ok(
    rules.indexOf(compatibility) >
      rules.findLastIndex((rule) => rule.labels?.includes("merge-ready")),
  );
});
