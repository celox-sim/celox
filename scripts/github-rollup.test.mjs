import assert from "node:assert/strict";
import test from "node:test";
import { execFileSync } from "node:child_process";
import {
  mkdtempSync,
  mkdirSync,
  readFileSync,
  writeFileSync,
  rmSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  collectSources,
  manageRollup,
  eventTargets,
  gitRunner,
} from "./github-rollup.mjs";
import { readPlan, validateRollupSources } from "./rollup-plan.mjs";

function pull(number, head, base = "master", body = "") {
  return {
    number,
    node_id: `PR_${number}`,
    state: "open",
    merged: false,
    draft: false,
    title: "fix: source",
    labels: [],
    body,
    head: {
      ref: head,
      sha: String(number).repeat(40),
      repo: { full_name: "o/r" },
    },
    base: { ref: base, repo: { full_name: "o/r" } },
  };
}

test("stack expansion deduplicates shared predecessors and verifies branch ownership", async () => {
  const pulls = [
    pull(1, "stack/a"),
    pull(2, "stack/b", "stack/a", "Depends-On: https://github.com/o/r/pull/1"),
    pull(3, "stack/c", "stack/a", "Depends-On: #1"),
  ];
  const api = async (path) =>
    structuredClone(pulls.find((p) => path.endsWith(`/pulls/${p.number}`)));
  assert.deepEqual(
    (await collectSources("o/r", [2, 3], "master", api)).map((s) => s.number),
    [1, 2, 3],
  );
  for (const modify of [
    (p) => (p[1].body = "Depends-On: https://github.com/other/r/pull/1"),
    (p) => (p[0].head.ref = "different"),
    (p) => (p[0].head.repo.full_name = "fork/r"),
    (p) => (p[0].draft = true),
    (p) => (p[0].state = "closed"),
    (p) => {
      p[0].base.ref = "stack/b";
      p[0].body = "Depends-On: #2";
    },
  ]) {
    const changed = structuredClone(pulls);
    modify(changed);
    await assert.rejects(
      collectSources("o/r", [2], "master", async (path) =>
        changed.find((p) => path.endsWith(`/pulls/${p.number}`)),
      ),
    );
  }
});

// Exercise actual git merges, conflicts, resolution commits, refreshes and
// leases against a tiny local bare remote. Only GitHub's HTTP API is replaced.
test("GitHub rollup lifecycle preserves sources, resumes web resolutions, rejects stale heads and restores holds", async () => {
  const root = mkdtempSync(join(tmpdir(), "celox-rollup-test-"));
  const remote = join(root, "remote.git");
  const seed = join(root, "seed");
  mkdirSync(seed);
  const git = gitRunner(seed);
  const bare = (...args) =>
    execFileSync("git", ["--git-dir", remote, ...args], {
      encoding: "utf8",
    }).trim();
  try {
    execFileSync("git", ["init", "--bare", "--quiet", remote]);
    git("init", "--quiet");
    git("config", "user.name", "test");
    git("config", "user.email", "test@example.com");
    git("config", "commit.gpgsign", "false");
    writeFileSync(join(seed, "value"), "base\n");
    git("add", ".");
    git("commit", "-qm", "chore: base");
    const base = git("rev-parse", "HEAD");
    git("remote", "add", "origin", remote);
    git("push", "--quiet", "origin", "HEAD:refs/heads/master");
    const rows = new Map();
    for (const [number, content, message] of [
      [1, "first", "fix: first"],
      [2, "second", "feat!: second"],
    ]) {
      git("checkout", "--detach", base);
      writeFileSync(join(seed, "value"), `${content}\n`);
      git("add", ".");
      git("commit", "-qm", message);
      git("push", "--quiet", "origin", `HEAD:refs/heads/source-${number}`);
      const p = pull(number, `source-${number}`);
      p.head.sha = git("rev-parse", "HEAD");
      rows.set(number, p);
    }
    rows.get(2).labels.push({ name: "release:hold" });
    let next = 50;
    const api = async (path, method = "GET", body) => {
      if (path === "graphql") {
        const number = Number(/PR_(\d+)/.exec(body.query)[1]);
        rows.get(number).draft = body.query.includes(
          "convertPullRequestToDraft",
        );
        return {};
      }
      const route = path.replace("/repos/o/r", "");
      const snapshot = (p) => {
        const result = structuredClone(p);
        result.head.sha = bare("rev-parse", `refs/heads/${p.head.ref}`);
        return result;
      };
      if (route.startsWith("/pulls?"))
        return [...rows.values()]
          .filter((p) => p.state === "open")
          .map(snapshot);
      if (route.startsWith("/git/ref/heads/"))
        return {
          object: { sha: bare("rev-parse", `refs/heads/${route.slice(15)}`) },
        };
      if (route === "/labels") return body;
      if (route.startsWith("/compare/")) {
        const [a, b] = route.slice(9).split("...");
        const valid =
          execFileSync("git", ["--git-dir", remote, "merge-base", a, b], {
            encoding: "utf8",
          }).trim() === a;
        return { status: valid ? "ahead" : "diverged" };
      }
      const issue = /^\/issues\/(\d+)\/labels(?:\/(.+))?$/.exec(route);
      if (issue) {
        const p = rows.get(Number(issue[1]));
        if (method === "POST")
          for (const name of body.labels) {
            if (!p.labels.some((l) => l.name === name)) p.labels.push({ name });
          }
        else if (method === "DELETE")
          p.labels = p.labels.filter(
            (l) => l.name !== decodeURIComponent(issue[2]),
          );
        return null;
      }
      if (route === "/pulls" && method === "POST") {
        const p = pull(next++, body.head, body.base, body.body);
        Object.assign(p, {
          title: body.title,
          draft: body.draft,
          html_url: `https://github.com/o/r/pull/${p.number}`,
        });
        rows.set(p.number, p);
        return snapshot(p);
      }
      const match = /^\/pulls\/(\d+)$/.exec(route);
      if (match) {
        const p = rows.get(Number(match[1]));
        if (method === "PATCH") Object.assign(p, body);
        return snapshot(p);
      }
      throw new Error(`Unexpected ${method} ${path}`);
    };
    let attempts = 0;
    const dependencies = () => {
      const cwd = join(root, `attempt-${attempts++}`);
      mkdirSync(cwd);
      return {
        cwd,
        api,
        summarize: () => {},
        gitFactory: (dir) => {
          const run = gitRunner(dir);
          return Object.assign(
            (...args) =>
              run(
                ...(args[0] === "remote"
                  ? ["remote", "add", "origin", remote]
                  : args),
              ),
            run,
          );
        },
      };
    };
    const originalHeads = [rows.get(1).head.sha, rows.get(2).head.sha];
    // Simulate a runner/API interruption halfway through claiming sources.
    // The draft plan must already exist and prevent a second rollup claiming
    // sources even if the first ownership-label write never completed.
    const interrupted = dependencies();
    interrupted.api = async (path, method, body) => {
      if (
        path === "/repos/o/r/issues/1/labels" &&
        body?.labels?.includes("release:hold")
      )
        throw new Error("Interrupted label write");
      return api(path, method, body);
    };
    await assert.rejects(
      manageRollup(
        {
          repository: "o/r",
          operation: "create",
          tips: "1,2",
          base: "master",
          id: "123",
          queue: true,
        },
        interrupted,
      ),
      /Interrupted/,
    );
    assert.equal(rows.get(50).draft, true);
    assert.equal(
      readPlan(rows.get(50).body, "o/r").sources[0].heldBefore,
      false,
    );
    rows.get(1).labels = [];
    await assert.rejects(
      manageRollup(
        {
          repository: "o/r",
          operation: "create",
          tips: "1",
          base: "master",
          id: "124",
          queue: false,
        },
        dependencies(),
      ),
      /already belongs to rollup #50/,
    );
    const first = await manageRollup(
      {
        repository: "o/r",
        operation: "create",
        tips: "1,2",
        base: "master",
        id: "123",
        queue: true,
      },
      dependencies(),
    );
    assert.equal(first.conflict, 2);
    assert.equal(rows.get(50).draft, true);
    assert.equal(rows.get(50).title, "feat(rollup)!: roll up stacks #1, #2");
    assert.match(rows.get(50).body, /#51/);
    assert.ok(rows.get(1).labels.some((l) => l.name === "release:hold"));
    // Retrying a dispatch recovers the same PR and helper, and another rollup
    // cannot claim already-held sources.
    const retry = await manageRollup(
      {
        repository: "o/r",
        operation: "create",
        tips: "1,2",
        base: "master",
        id: "123",
        queue: true,
      },
      dependencies(),
    );
    assert.equal(retry.pull.number, 50);
    assert.equal(rows.size, 4);
    await assert.rejects(
      manageRollup(
        {
          repository: "o/r",
          operation: "create",
          tips: "1",
          base: "master",
          id: "124",
          queue: false,
        },
        dependencies(),
      ),
      /already belongs/,
    );

    // Model GitHub's Resolve conflicts commit on the copied source branch.
    const helper = rows.get(51);
    git("fetch", "--quiet", "origin", `refs/heads/${helper.head.ref}`);
    git("checkout", "--detach", "FETCH_HEAD");
    git("fetch", "--quiet", "origin", "refs/heads/rollup/123");
    assert.equal(git.merge("FETCH_HEAD", "chore: attempt resolution"), false);
    assert.throws(() => git("merge", "--no-ff", "--no-commit", "FETCH_HEAD"));
    writeFileSync(join(seed, "value"), "first and second\n");
    git("add", ".");
    git("commit", "-qm", "fix: resolve both changes");
    git("push", "--quiet", "origin", `HEAD:refs/heads/${helper.head.ref}`);
    const targets = await eventTargets(
      "o/r",
      { action: "synchronize", pull_request: helper },
      api,
    );
    assert.deepEqual(targets, [50]);
    const resumed = await manageRollup(
      { repository: "o/r", operation: "resume", number: 50 },
      dependencies(),
    );
    assert.equal(resumed.conflict, null);
    assert.equal(rows.get(50).draft, false);
    assert.ok(rows.get(50).labels.some((l) => l.name === "merge-ready"));
    assert.equal(
      bare("show", "refs/heads/rollup/123:value"),
      "first and second",
    );
    for (const [i, n] of [1, 2].entries())
      assert.equal(
        bare("rev-parse", `refs/heads/source-${n}`),
        originalHeads[i],
      );
    await validateRollupSources("o/r", await api("/repos/o/r/pulls/50"), api);

    git("checkout", "--detach", originalHeads[1]);
    writeFileSync(join(seed, "new"), "updated\n");
    git("add", ".");
    git("commit", "-qm", "fix: update source");
    git("push", "--quiet", "origin", "HEAD:refs/heads/source-2");
    await assert.rejects(
      validateRollupSources("o/r", await api("/repos/o/r/pulls/50"), api),
      /changed/,
    );
    await assert.rejects(
      manageRollup(
        { repository: "o/r", operation: "resume", number: 50 },
        dependencies(),
      ),
      /changed/,
    );
    assert.equal(rows.get(50).draft, true);
    assert.ok(!rows.get(50).labels.some((l) => l.name === "merge-ready"));
    const refreshed = await manageRollup(
      { repository: "o/r", operation: "refresh", number: 50 },
      dependencies(),
    );
    assert.equal(refreshed.conflict, 2);
    assert.equal(
      readPlan(rows.get(50).body, "o/r").sources[1].sha,
      git("rev-parse", "HEAD"),
    );
    assert.notEqual(rows.get(52).head.ref, helper.head.ref);

    // A destination update can conflict with the accumulator itself. In that
    // case the main draft PR is the GitHub conflict editor, not a source PR.
    git("checkout", "--detach", base);
    writeFileSync(join(seed, "value"), "destination changed\n");
    git("add", ".");
    git("commit", "-qm", "fix: change destination");
    git("push", "--quiet", "origin", "HEAD:refs/heads/master");
    const baseConflict = await manageRollup(
      { repository: "o/r", operation: "resume", number: 50 },
      dependencies(),
    );
    assert.equal(baseConflict.conflict, "base");
    assert.match(
      rows.get(50).body,
      /Resolve conflicts on this draft rollup PR/,
    );
    assert.equal(rows.get(50).draft, true);
    assert.deepEqual(
      await eventTargets(
        "o/r",
        {
          action: "converted_to_draft",
          pull_request: await api("/repos/o/r/pulls/50"),
        },
        api,
      ),
      [],
    );
    await manageRollup(
      { repository: "o/r", operation: "cancel", number: 50 },
      dependencies(),
    );
    assert.equal(rows.get(50).state, "closed");
    assert.equal(rows.get(51).state, "closed");
    assert.equal(rows.get(52).state, "closed");
    assert.deepEqual(rows.get(1).labels, []);
    assert.deepEqual(rows.get(2).labels, [{ name: "release:hold" }]);
    assert.equal(rows.get(1).state, "open");
    assert.equal(rows.get(2).state, "open");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("workflow uses trusted code and app credentials and does not evaluate dispatch text as shell", () => {
  const workflow = readFileSync(
    new URL("../.github/workflows/rollup.yml", import.meta.url),
    "utf8",
  );
  assert.match(
    workflow,
    /ref: \$\{\{ github.event.repository.default_branch \}\}/,
  );
  assert.match(workflow, /persist-credentials: false/);
  assert.match(workflow, /permission-workflows: write/);
  assert.match(workflow, /cancel-in-progress: false/);
  assert.match(workflow, /ROLLUP_TIPS: \$\{\{ inputs.pull_requests \}\}/);
  assert.doesNotMatch(workflow, /run:.*\$\{\{/);
});
