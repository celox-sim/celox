import { execFileSync, spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, appendFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { stackDependency } from "./check-mergify-batch.mjs";
import { commitReleaseImpact } from "./check-pr-release-impact.mjs";
import {
  pullNumbers,
  readPlan,
  renderBody,
  rollupBranch,
  resolutionBranch,
  sourceLabel,
  validatePlan,
} from "./rollup-plan.mjs";

export async function github(path, method = "GET", body) {
  return JSON.parse(
    execFileSync(
      "gh",
      [
        "api",
        path,
        "--method",
        method,
        ...(body === undefined ? [] : ["--input", "-"]),
      ],
      {
        encoding: "utf8",
        maxBuffer: 16 * 1024 * 1024,
        input: body === undefined ? undefined : JSON.stringify(body),
      },
    ) || "null",
  );
}

export async function collectSources(repository, tips, base, api = github) {
  const sources = new Map();
  async function visit(number, path = new Set()) {
    if (path.has(number) || path.size >= 20)
      throw new Error("Cyclic or overdeep stack");
    if (sources.has(number)) return sources.get(number);
    const pull = await api(`/repos/${repository}/pulls/${number}`);
    if (
      pull.state !== "open" ||
      pull.draft ||
      pull.head?.repo?.full_name !== repository ||
      pull.base?.repo?.full_name !== repository ||
      !/^[0-9a-f]{40}$/.test(pull.head?.sha ?? "") ||
      pull.head.ref.startsWith("rollup/") ||
      pull.head.ref.startsWith("mergify/")
    )
      throw new Error(
        `Source #${number} must be an open, non-draft PR in this repository`,
      );
    if (pull.base.ref !== base) {
      const below = stackDependency(repository, pull.body);
      if (!pull.base.ref.startsWith("stack/") || !below)
        throw new Error(
          `Source #${number} does not target ${base} or a recognized stack`,
        );
      const parent = await visit(below, new Set([...path, number]));
      if (parent.head !== pull.base.ref)
        throw new Error(`Source #${number} has a mismatched dependency branch`);
    }
    const source = {
      number,
      sha: pull.head.sha,
      head: pull.head.ref,
      title: pull.title,
      heldBefore: pull.labels.some((l) => l.name === "release:hold"),
    };
    sources.set(number, source);
    if (sources.size > 100) throw new Error("Rollup exceeds 100 sources");
    return source;
  }
  for (const tip of tips) await visit(tip);
  return [...sources.values()];
}

// Only git is run in this disposable checkout. Source hooks, build scripts and
// workflow files are never executed with the app's write token.
export function gitRunner(cwd) {
  const git = (...args) =>
    execFileSync("git", ["-c", "core.hooksPath=/dev/null", ...args], {
      cwd,
      encoding: "utf8",
      maxBuffer: 64 * 1024 * 1024,
      stdio: ["pipe", "pipe", "pipe"],
      env: {
        ...process.env,
        GIT_TERMINAL_PROMPT: "0",
        GIT_MERGE_AUTOEDIT: "no",
      },
    }).trim();
  git.ancestor = (a, b) => {
    const result = spawnSync("git", ["merge-base", "--is-ancestor", a, b], {
      cwd,
    });
    if (result.status === 0) return true;
    if (result.status === 1) return false;
    throw new Error(`Cannot compare git ancestry: ${result.stderr}`);
  };
  git.merge = (sha, message) => {
    if (git.ancestor(sha, "HEAD")) return true;
    try {
      git("merge", "--no-ff", "--no-edit", "-m", message, sha);
      return true;
    } catch (error) {
      if (!git("diff", "--name-only", "--diff-filter=U")) throw error;
      git("merge", "--abort");
      return false;
    }
  };
  return git;
}

export function assemble(
  plan,
  git,
  { currentHead, baseSha, resolution = () => null },
) {
  git("checkout", "--detach", currentHead ?? plan.sources[0].sha);
  if (!git.merge(baseSha, "chore(rollup): incorporate destination branch"))
    return { head: git("rev-parse", "HEAD"), conflict: "base" };
  for (const source of plan.sources) {
    if (git.ancestor(source.sha, "HEAD")) continue;
    const head = git("rev-parse", "HEAD");
    const resolved = resolution(source, head);
    // A resolution must include both pinned source and this exact accumulator.
    // A stale helper branch cannot undo a later source or base update.
    const candidate =
      resolved &&
      git.ancestor(head, resolved) &&
      git.ancestor(source.sha, resolved)
        ? resolved
        : source.sha;
    if (!git.merge(candidate, `chore(rollup): incorporate #${source.number}`))
      return { head, conflict: source.number };
  }
  return { head: git("rev-parse", "HEAD"), conflict: null };
}

function hasLabel(pull, label) {
  return pull.labels.some((l) => l.name === label);
}

export async function manageRollup(
  input,
  { api = github, cwd, gitFactory = gitRunner, summarize = console.log } = {},
) {
  const { repository } = input;
  if (!/^[\w.-]+\/[\w.-]+$/.test(repository))
    throw new Error("Invalid repository");
  if (!["create", "resume", "refresh", "cancel"].includes(input.operation))
    throw new Error("Invalid rollup operation");
  if (input.operation === "create" && input.number)
    throw new Error("Leave the rollup PR number blank when creating a rollup");
  const prefix = `/repos/${repository}`;
  const pulls = [];
  for (let page = 1; ; page++) {
    const batch = await api(
      `${prefix}/pulls?state=open&per_page=100&page=${page}`,
    );
    pulls.push(...batch);
    if (batch.length < 100) break;
  }
  const rerun =
    input.operation === "create" &&
    pulls.find(
      (p) =>
        p.head.ref === `rollup/${input.id}` &&
        p.head.repo.full_name === repository,
    );
  let pull = input.number
    ? await api(`${prefix}/pulls/${input.number}`)
    : (rerun ?? null);
  if (rerun) input = { ...input, operation: "resume" };
  let plan = pull ? readPlan(pull.body, repository) : null;
  if (
    pull &&
    (!plan ||
      pull.head.repo.full_name !== repository ||
      pull.head.ref !== rollupBranch(plan) ||
      pull.base.ref !== plan.base)
  )
    throw new Error("Not a managed rollup PR");

  const label = async (number, name, add) => {
    if (add)
      await api(`${prefix}/issues/${number}/labels`, "POST", {
        labels: [name],
      });
    else {
      const current = await api(`${prefix}/pulls/${number}`);
      if (hasLabel(current, name))
        await api(
          `${prefix}/issues/${number}/labels/${encodeURIComponent(name)}`,
          "DELETE",
        );
    }
  };
  const draft = async (value) => {
    if (pull.draft === value) return;
    const mutation = value
      ? "convertPullRequestToDraft"
      : "markPullRequestReadyForReview";
    const response = await api("graphql", "POST", {
      query: `mutation { ${mutation}(input:{pullRequestId:${JSON.stringify(pull.node_id)}}) { pullRequest { id } } }`,
    });
    if (response.errors?.length)
      throw new Error("Cannot change rollup draft state");
    pull.draft = value;
  };
  const cleanup = async () => {
    // Only holds this workflow added are released. Pre-existing holds survive.
    for (const source of plan.sources) {
      await label(source.number, sourceLabel, false);
      if (!source.heldBefore) await label(source.number, "release:hold", false);
    }
    for (const helper of pulls.filter(
      (p) =>
        p.base.ref === rollupBranch(plan) &&
        p.head.ref.startsWith(`rollup/resolve-${plan.id}/`),
    ))
      await api(`${prefix}/pulls/${helper.number}`, "PATCH", {
        state: "closed",
      });
  };
  if (input.operation === "cancel" || pull?.state === "closed") {
    if (!plan) throw new Error("A rollup PR number is required");
    await label(pull.number, "merge-ready", false);
    if (pull.state === "open")
      await api(`${prefix}/pulls/${pull.number}`, "PATCH", { state: "closed" });
    await cleanup();
    summarize(`Rollup #${pull.number} finished; source holds restored.`);
    return;
  }
  if (!plan && input.operation !== "create")
    throw new Error("A rollup PR number is required");
  if (plan && input.operation === "create")
    throw new Error("Use resume or refresh for an existing rollup");

  const oldPlan = plan;
  const queueRequested = plan?.queue || (pull && hasLabel(pull, "merge-ready"));
  if (!plan || input.operation === "refresh") {
    const tips = plan?.tips ?? pullNumbers(input.tips);
    const base = plan?.base ?? input.base;
    if (!["master", "develop"].includes(base))
      throw new Error("Invalid rollup base");
    const sources = await collectSources(repository, tips, base, api);
    for (const source of sources) {
      const original = oldPlan?.sources.find((s) => s.number === source.number);
      if (original) source.heldBefore = original.heldBefore;
      const current = await api(`${prefix}/pulls/${source.number}`);
      if (hasLabel(current, sourceLabel) && !original)
        throw new Error(`Source #${source.number} already belongs to a rollup`);
      for (const other of pulls) {
        if (
          !/^rollup\/[1-9]\d*$/.test(other.head.ref) ||
          other.number === pull?.number ||
          other.head.repo?.full_name !== repository
        )
          continue;
        const otherPlan = readPlan(other.body, repository);
        if (otherPlan?.sources.some((s) => s.number === source.number))
          throw new Error(
            `Source #${source.number} already belongs to rollup #${other.number}`,
          );
      }
    }
    // A refresh cannot silently remove an old dependency and strand its hold.
    if (
      oldPlan?.sources.some((s) => !sources.some((v) => v.number === s.number))
    )
      throw new Error(
        "Stack membership changed; cancel this rollup and create a new one",
      );
    plan = validatePlan(
      {
        repository,
        id: oldPlan?.id ?? input.id,
        base,
        baseSha: (await api(`${prefix}/git/ref/heads/${base}`)).object.sha,
        tips,
        sources,
        queue: oldPlan?.queue ?? input.queue,
      },
      repository,
    );
  }

  const checkSources = async () => {
    for (const source of plan.sources) {
      const current = await api(`${prefix}/pulls/${source.number}`);
      if (
        current.draft ||
        current.head?.sha !== source.sha ||
        current.head.repo.full_name !== repository ||
        (current.state !== "open" && !current.merged)
      )
        throw new Error(`Source #${source.number} changed; run refresh`);
    }
  };
  // Stop an already queued rollup before refreshing or handling stale sources.
  if (pull) {
    await label(pull.number, "merge-ready", false);
    await draft(true);
  }
  await checkSources();
  const directory = cwd ?? mkdtempSync(join(tmpdir(), "celox-rollup-"));
  try {
    const git = gitFactory(directory);
    git("init", "--quiet");
    git("config", "user.name", "celox-rollup[bot]");
    git("config", "user.email", "celox-rollup[bot]@users.noreply.github.com");
    git("config", "commit.gpgsign", "false");
    git("remote", "add", "origin", `https://github.com/${repository}.git`);
    const fetch = (sha) => {
      git("fetch", "--quiet", "--no-tags", "origin", sha);
      return git("rev-parse", "FETCH_HEAD");
    };
    const baseSha = (await api(`${prefix}/git/ref/heads/${plan.base}`)).object
      .sha;
    fetch(baseSha);
    for (const source of plan.sources) fetch(source.sha);
    const currentHead =
      pull && input.operation !== "refresh" ? fetch(pull.head.sha) : null;
    const helpers = pulls.filter(
      (p) =>
        p.base.ref === rollupBranch(plan) &&
        p.head.repo.full_name === repository,
    );
    const result = assemble(plan, git, {
      currentHead,
      baseSha,
      resolution: (source, head) => {
        const branch = `${resolutionBranch(plan, source.number)}-${head.slice(0, 12)}`;
        const helper = helpers.find((p) => p.head.ref === branch);
        return helper ? fetch(helper.head.sha) : null;
      },
    });
    if (git.ancestor(result.head, baseSha))
      throw new Error(
        "All selected changes are already on the destination branch",
      );
    const messages = git(
      "log",
      "--format=%B%x00",
      `${baseSha}..${result.head}`,
    ).split("\0");
    // All source commits, including any not yet integrated, determine the title.
    for (const source of plan.sources)
      messages.push(
        source.title ?? "",
        ...git("log", "--format=%B%x00", `${baseSha}..${source.sha}`).split(
          "\0",
        ),
      );
    const impact = Math.max(
      0,
      ...messages.map((m) =>
        commitReleaseImpact(m.trim(), {
          preMajor: false,
          bumpPatchForMinorPreMajor: false,
        }),
      ),
    );
    if (impact > 3)
      throw new Error("Rollups cannot include forced Release-As commits");
    const type = ["chore", "fix", "feat", "feat"][impact];
    const title = `${type}(rollup)${impact === 3 ? "!" : ""}: roll up stacks ${plan.tips.map((n) => `#${n}`).join(", ")}`;
    await checkSources();
    if (!pull) {
      // Serialization is repository-wide. This label is an ownership marker,
      // separate from release:hold, which a source may already have.
      await api(`${prefix}/labels`, "POST", {
        name: sourceLabel,
        color: "5319E7",
        description: "Owned by a GitHub rollup",
      }).catch(async () => {
        // Existing labels are normal; other API failures must remain failures.
        await api(`${prefix}/labels/${encodeURIComponent(sourceLabel)}`);
      });
    }
    if (pull && input.operation === "refresh")
      await api(`${prefix}/pulls/${pull.number}`, "PATCH", {
        title,
        body: renderBody(
          plan,
          "Refreshing snapshots. Use resume if this operation is interrupted.",
        ),
      });
    const branch = rollupBranch(plan);
    const unpublished = !pull
      ? git("ls-remote", "origin", `refs/heads/${branch}`).split(/\s/)[0]
      : "";
    git(
      "push",
      "--quiet",
      `--force-with-lease=refs/heads/${branch}:${pull?.head.sha ?? unpublished}`,
      "origin",
      `${result.head}:refs/heads/${branch}`,
    );
    if (!pull)
      pull = await api(`${prefix}/pulls`, "POST", {
        head: branch,
        base: plan.base,
        title,
        body: renderBody(
          plan,
          "Preparing rollup. Re-run this workflow if preparation is interrupted.",
        ),
        draft: true,
      });
    // Publish the draft recovery record before claiming any source. Even a
    // runner cancellation between label writes can then be resumed/cancelled
    // from GitHub without leaving unowned holds behind.
    for (const source of plan.sources) {
      await label(source.number, sourceLabel, true);
      await label(source.number, "release:hold", true);
    }
    let status;
    if (result.conflict === "base") {
      status =
        "**Resolve conflicts on this draft rollup PR in GitHub.** The workflow resumes after the resolution is committed.";
    } else if (result.conflict) {
      const helperBranch = `${resolutionBranch(plan, result.conflict)}-${result.head.slice(0, 12)}`;
      let helper = helpers.find((p) => p.head.ref === helperBranch);
      if (!helper) {
        const source = plan.sources.find((s) => s.number === result.conflict);
        const existing = git(
          "ls-remote",
          "origin",
          `refs/heads/${helperBranch}`,
        ).split(/\s/)[0];
        // An orphan branch from an interrupted run is reusable only if it is
        // still the exact source copy; never discard a user's resolution.
        if (existing && existing !== source.sha)
          throw new Error(
            "Unpublished resolution branch changed; inspect it before retrying",
          );
        git(
          "push",
          "--quiet",
          `--force-with-lease=refs/heads/${helperBranch}:${existing}`,
          "origin",
          `${source.sha}:refs/heads/${helperBranch}`,
        );
        helper = await api(`${prefix}/pulls`, "POST", {
          head: helperBranch,
          base: branch,
          draft: true,
          title: `chore(rollup): resolve #${source.number} into rollup ${plan.id}`,
          body: `Resolve this PR's conflicts using GitHub's **Resolve conflicts** button, then commit the resolution. The rollup workflow resumes automatically. This branch is a copy of #${source.number} at ${source.sha}; the original stack is untouched.`,
        });
      }
      status = `**Waiting for conflict resolution in #${helper.number}.** Use its GitHub **Resolve conflicts** button and commit the resolution; this rollup resumes automatically.`;
    } else
      status =
        "All pinned sources are integrated. Review the rollup and add `merge-ready` to request queue validation.";
    const body = renderBody(plan, status);
    await api(`${prefix}/pulls/${pull.number}`, "PATCH", { title, body });
    await checkSources();
    if (!result.conflict) {
      await draft(false);
      if (plan.queue || queueRequested)
        await label(pull.number, "merge-ready", true);
    }
    summarize(`${pull.html_url}\n\n${status}`);
    return { pull, plan, conflict: result.conflict };
  } finally {
    if (!cwd) rmSync(directory, { recursive: true, force: true });
  }
}

export async function eventTargets(repository, event, api = github) {
  const number = event.pull_request?.number;
  const ref = event.pull_request?.head?.ref ?? "";
  if (!number || event.pull_request.head.repo?.full_name !== repository)
    return [];
  if (/^rollup\/[1-9]\d*$/.test(ref))
    return event.action === "converted_to_draft" ? [] : [number];
  const helper = /^rollup\/resolve-([1-9]\d*)\//.exec(ref);
  if (!helper && !hasLabel(event.pull_request, sourceLabel)) return [];
  const result = [];
  for (let page = 1; ; page++) {
    const pulls = await api(
      `/repos/${repository}/pulls?state=open&per_page=100&page=${page}`,
    );
    for (const pull of pulls) {
      if (
        !/^rollup\/[1-9]\d*$/.test(pull.head.ref) ||
        pull.head.repo.full_name !== repository
      )
        continue;
      const plan = readPlan(pull.body, repository);
      if (
        plan &&
        (helper
          ? plan.id === helper[1]
          : plan.sources.some((s) => s.number === number))
      )
        result.push(pull.number);
    }
    if (pulls.length < 100) return result;
  }
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(resolve(process.argv[1])).href
) {
  const repository = process.env.GITHUB_REPOSITORY;
  const summarize = (text) => {
    console.log(text);
    if (process.env.GITHUB_STEP_SUMMARY)
      appendFileSync(process.env.GITHUB_STEP_SUMMARY, `${text}\n`);
  };
  try {
    if (process.env.GITHUB_EVENT_NAME === "workflow_dispatch") {
      const numbers = process.env.ROLLUP_NUMBER
        ? pullNumbers(process.env.ROLLUP_NUMBER)
        : [];
      if (numbers.length > 1)
        throw new Error("Specify exactly one rollup PR number");
      await manageRollup(
        {
          repository,
          operation: process.env.ROLLUP_OPERATION,
          tips: process.env.ROLLUP_TIPS ?? "",
          base: process.env.ROLLUP_BASE,
          number: numbers[0] ?? null,
          id: process.env.GITHUB_RUN_ID,
          queue: process.env.ROLLUP_QUEUE === "true",
        },
        { summarize },
      );
    } else {
      const event = JSON.parse(
        readFileSync(process.env.GITHUB_EVENT_PATH, "utf8"),
      );
      for (const number of await eventTargets(repository, event))
        await manageRollup(
          { repository, number, operation: "resume" },
          { summarize },
        );
    }
  } catch (error) {
    summarize(`Rollup stopped: ${error.message}`);
    process.exitCode = 1;
  }
}
