import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const read = (path) =>
  readFileSync(new URL(`../${path}`, import.meta.url), "utf8");

const queueConfig = read(".mergify.yml");
const queues = queueConfig
  .split("queue_rules:\n")[1]
  .split("\npull_request_rules:")[0]
  .split(/^  - name: /m)
  .slice(1)
  .map((block) => ({ name: block.split("\n")[0], block }));

test("all queue categories preserve combined-tree validation and merge protections", () => {
  assert.match(queueConfig, /mode: parallel/);
  assert.match(queueConfig, /max_parallel_checks: 5/);
  assert.deepEqual(
    queues.map((queue) => queue.name),
    ["integration", "dependencies", "changes"],
  );
  for (const { name, block } of queues) {
    const size = { integration: 1, dependencies: 5, changes: 3 }[name];
    assert.match(block, new RegExp(`batch_size: ${size}\\b`));
    assert.match(
      block,
      new RegExp(
        `batch_max_wait_time: ${name === "integration" ? "0 seconds" : "1 minute"}`,
      ),
    );
    for (const pattern of [
      /max_checks_retries: 0/,
      /checks_timeout: 3 hours/,
      /merge_conditions:\n\s+- check-success = Queue validation\n\s+- check-success = Conventional Commit title/,
      /merge_method: merge/,
      /update_method: merge/,
      /title: pr-title\n\s+body: empty\n\s+trailers: \[\]/,
      /branch_protection_injection_mode: queue/,
      /- base ~= \^\(master\|develop\)\$/,
      /- -draft/,
      /- -label = release:hold/,
      /head != release-please--branches--master--components--celox\n\s+- label = merge-ready/,
    ])
      assert.match(block, pattern, name);
  }
  const rule = queueConfig.split("\npull_request_rules:")[1];
  assert.match(rule, /label = merge-ready/);
  assert.match(rule, /- -label = dequeued/);
  assert.match(rule, /queue: \{\}/);
});

test("real branch conventions route to exactly one category", () => {
  // Evaluate the head predicates from the actual configuration, including
  // exclusions. Common admission guards are checked separately above.
  function categories(head) {
    return queues
      .filter(({ block }) =>
        [...block.matchAll(/^      - (-?)head ~= (.+)$/gm)].every(
          ([, negative, pattern]) =>
            new RegExp(pattern).test(head) !== Boolean(negative),
        ),
      )
      .map(({ name }) => name);
  }
  const examples = {
    integration: [
      "integration/veryl-head",
      "integration/master-to-develop",
      "renovate/veryl-crates",
      "upgrade/veryl-0.22",
      "release-please--branches--master--components--celox",
      "master",
    ],
    dependencies: [
      "renovate/syn-ecosystem",
      "renovate/rust-1.x",
      "renovate/typescript-7.x",
      "renovate/napi-rs-emnapi-compatibility",
      "renovate/veryl-crates-extra",
    ],
    changes: [
      "feature/1123-veryl-kernels",
      "fix/parser",
      "ci/mergify-queue-categories",
      "upgrade/other",
      "feature/master",
      "master-extra",
      "manual-dependency-update",
    ],
  };
  for (const [expected, heads] of Object.entries(examples)) {
    for (const head of heads)
      assert.deepEqual(categories(head), [expected], head);
  }
  assert.ok(
    read(".github/workflows/sync-develop.yml").includes(
      "SYNC_BRANCH: integration/master-to-develop",
    ),
  );
  assert.ok(
    read(".github/workflows/veryl-head.yml").includes(
      "--head integration/veryl-head",
    ),
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

test("PR-policy repairs are independent of Veryl while dependencies and features overlap", () => {
  const scopes = queueConfig.split("scopes:\n")[1].split("queue_rules:\n")[0];
  const core = scopes.split("      core:\n")[1].split("      pr-policy:")[0];
  const policy = scopes
    .split("      pr-policy:\n")[1]
    .split("  capacities:")[0];
  assert.match(core, /include:\n          - "\*\*"/);
  const paths = (block) =>
    [...block.matchAll(/^          - ([^"\n]+)$/gm)].map((m) => m[1]);
  const excluded = paths(core.split("        exclude:")[1]);
  const included = paths(policy);
  assert.deepEqual(excluded, included);
  // These scopes deliberately use exact paths, avoiding an expanding glob that
  // could silently classify new runtime-dependent scripts as independent.
  for (const path of included) assert.doesNotMatch(path, /[?*{}\[\]]/);
  function assigned(files) {
    return [
      files.some((file) => !excluded.includes(file)) && "core",
      files.some((file) => included.includes(file)) && "pr-policy",
    ].filter(Boolean);
  }
  const repair = [
    ".github/workflows/pr-title.yml",
    "scripts/check-mergify-queue-title.mjs",
    "scripts/check-mergify-queue-title.test.mjs",
  ];
  assert.deepEqual(assigned(repair), ["pr-policy"]);
  for (const file of [
    "Cargo.lock",
    "Cargo.toml",
    "rust-toolchain.toml",
    "crates/celox/src/lib.rs",
    "packages/celox/package.json",
    "pnpm-lock.yaml",
    "scripts/ci-changes.mjs",
    ".github/workflows/ci.yml",
    "new-component/unknown.rs",
  ])
    assert.deepEqual(assigned([file]), ["core"], file);
  assert.deepEqual(assigned([...repair, "Cargo.lock"]), ["core", "pr-policy"]);
  assert.match(scopes, /capacities:\n    core: 2\n    pr-policy: 1/);
  for (const file of [".mergify.yml", "release-please-config.json", "VERSION"])
    assert.ok(
      scopes.split("  barrier_files:")[1].includes(`      - ${file}\n`),
    );
});
