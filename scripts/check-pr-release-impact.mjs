import { readFileSync } from "node:fs";

import { parseConventionalPrTitle } from "./check-pr-title.mjs";

const NO_RELEASE_TYPES = new Set([
  "build",
  "chore",
  "ci",
  "docs",
  "refactor",
  "test",
]);
const PATCH_TYPES = new Set(["deps", "fix", "perf", "revert"]);

const impactNames = ["none", "patch", "feature", "breaking", "forced"];

function featureImpact({ preMajor, bumpPatchForMinorPreMajor }) {
  return preMajor && bumpPatchForMinorPreMajor ? 1 : 2;
}

export function releasePolicyFromFiles(versionText, configText) {
  const version = versionText.trim().match(/^(\d+)\.\d+\.\d+(?:[-+].*)?$/);
  if (!version) {
    throw new Error("VERSION must contain a semantic version");
  }

  const config = JSON.parse(configText);
  const rootPackage = config.packages?.["."] ?? {};
  const bumpPatchForMinorPreMajor =
    rootPackage["bump-patch-for-minor-pre-major"] ??
    config["bump-patch-for-minor-pre-major"] ??
    false;
  if (typeof bumpPatchForMinorPreMajor !== "boolean") {
    throw new Error("bump-patch-for-minor-pre-major must be a boolean");
  }

  return {
    preMajor: Number.parseInt(version[1], 10) === 0,
    bumpPatchForMinorPreMajor,
  };
}

// Release Please's parser accepts any non-whitespace type casing, optional
// whitespace after the colon, and an empty description. It also parses
// footer-looking lines as additional commits, so this intentionally examines
// every unindented line rather than only the subject.
const conventionalHeaderPattern =
  /^([^\s():!]+)(?:\([^()\r\n]+\))?(!)?:[^\r\n]*$/;

function conventionalHeaders(message) {
  return message
    .split(/\r?\n/)
    .map((line) => line.match(conventionalHeaderPattern))
    .filter((match) => match !== null);
}

// Keep this split behavior aligned with release-please's splitMessages helper.
// A raw git commit can contain several commits, including nested commit blocks.
function splitMessages(message) {
  const parts = message.split("BEGIN_NESTED_COMMIT");
  const messages = [parts.shift()];
  for (const part of parts) {
    const [nestedMessage, ...rest] = part.split("END_NESTED_COMMIT");
    messages.push(nestedMessage);
    messages[0] += rest.join("END_NESTED_COMMIT");
  }

  const conventionalCommits = messages[0]
    .split(
      /\r?\n\r?\n(?=(?:feat|fix|docs|style|refactor|perf|test|build|ci|chore|revert)(?:\(.*?\))?: )/,
    )
    .filter(Boolean);
  return [...conventionalCommits, ...messages.slice(1)];
}

function hasBreakingChange(message) {
  const lines = message.split(/\r?\n/);

  for (let index = 0; index < lines.length; index++) {
    const marker = lines[index].match(/^BREAKING(?: |-)CHANGE:(.*)$/);
    if (!marker) {
      continue;
    }
    if (/\S/.test(marker[1])) {
      return true;
    }

    // The parser accepts a breaking description on a continuation or later
    // body line. A following footer starts a new token instead.
    for (const line of lines.slice(index + 1)) {
      if (!/\S/.test(line)) {
        continue;
      }
      if (
        conventionalHeaderPattern.test(line) ||
        /^[^\s:]+\s+#\d+/.test(line)
      ) {
        break;
      }
      return true;
    }
  }

  return false;
}

function hasReleaseAsFooter(message) {
  const lines = message.split(/\r?\n/);

  // A Release-As subject is an ordinary Conventional Commit type. Only a
  // footer parsed after the subject becomes a forced-version note.
  for (let index = 1; index < lines.length; index++) {
    const marker = lines[index].match(/^Release-As:(.*)$/i);
    if (!marker) {
      continue;
    }
    if (/\S/.test(marker[1])) {
      return true;
    }
    if (/^[ \t]+\S/.test(lines[index + 1] ?? "")) {
      return true;
    }
  }

  return false;
}

export function titleReleaseImpact(title, options) {
  const parsed = parseConventionalPrTitle(title);
  if (!parsed) {
    return null;
  }
  if (parsed.breaking) {
    return 3;
  }
  if (parsed.type === "feat") {
    return featureImpact(options);
  }
  if (PATCH_TYPES.has(parsed.type)) {
    return 1;
  }
  if (NO_RELEASE_TYPES.has(parsed.type)) {
    return 0;
  }
  return 0;
}

function parsedMessageReleaseImpact(message, options) {
  const [subject = ""] = message.split(/\r?\n/, 1);
  if (!conventionalHeaderPattern.test(subject)) {
    return 0;
  }
  if (hasReleaseAsFooter(message)) {
    return 4;
  }

  const headers = conventionalHeaders(message);
  if (
    hasBreakingChange(message) ||
    headers.some((header) => header[2] === "!")
  ) {
    return 3;
  }
  if (
    headers.some((header) => header[1] === "feat" || header[1] === "feature")
  ) {
    return featureImpact(options);
  }
  if (
    headers.some((header) => PATCH_TYPES.has(header[1]))
  ) {
    return 1;
  }
  return 0;
}

export function commitReleaseImpact(message, options) {
  return Math.max(
    0,
    ...splitMessages(message).map((part) =>
      parsedMessageReleaseImpact(part.trim(), options),
    ),
  );
}

export function releaseImpactErrors(title, commits, options) {
  const titleImpact = titleReleaseImpact(title, options);
  if (titleImpact === null) {
    return ["the pull request title is not a Conventional Commit"];
  }

  return commits.flatMap((commit) => {
    const impact = commitReleaseImpact(commit.message, options);
    if (impact <= titleImpact) {
      return [];
    }

    const subject = commit.message.split(/\r?\n/, 1)[0];
    return [
      `${commit.sha.slice(0, 12)} ${JSON.stringify(subject)} has ${impactNames[impact]} impact, which exceeds the ${impactNames[titleImpact]} pull request title`,
    ];
  });
}

async function githubJson(path) {
  const apiUrl = process.env.GITHUB_API_URL ?? "https://api.github.com";
  const response = await fetch(`${apiUrl}${path}`, {
    headers: {
      Accept: "application/vnd.github+json",
      Authorization: `Bearer ${process.env.GH_TOKEN}`,
      "X-GitHub-Api-Version": "2022-11-28",
    },
  });
  if (!response.ok) {
    throw new Error(`GitHub API ${path} returned ${response.status}`);
  }
  return response.json();
}

async function loadPullRequestCommits(repository, number) {
  const pullRequest = await githubJson(`/repos/${repository}/pulls/${number}`);
  // BEGIN_COMMIT_OVERRIDE does not apply to plain merges. The companion
  // repository-settings check enforces that this repository stays merge-only.
  if (pullRequest.commits > 250) {
    throw new Error(
      `Pull request has ${pullRequest.commits} commits; GitHub exposes at most 250 for validation`,
    );
  }

  const commits = [];
  for (let page = 1; ; page++) {
    const batch = await githubJson(
      `/repos/${repository}/pulls/${number}/commits?per_page=100&page=${page}`,
    );
    commits.push(
      ...batch.map((commit) => ({
        sha: commit.sha,
        message: commit.commit.message,
      })),
    );
    if (batch.length < 100) {
      return commits;
    }
  }
}

async function loadRepositoryFile(repository, path, ref) {
  const file = await githubJson(
    `/repos/${repository}/contents/${path}?ref=${encodeURIComponent(ref)}`,
  );
  if (file.type !== "file" || file.encoding !== "base64") {
    throw new Error(`${path} at ${ref} is not a base64-encoded file`);
  }
  return Buffer.from(file.content.replace(/\s/g, ""), "base64").toString(
    "utf8",
  );
}

async function loadCompareCommits(repository, baseSha, headSha) {
  const commits = new Map();
  let totalCommits;

  for (let page = 1; ; page++) {
    const comparison = await githubJson(
      `/repos/${repository}/compare/${baseSha}...${headSha}?per_page=100&page=${page}`,
    );
    if (totalCommits === undefined) {
      if (!Number.isSafeInteger(comparison.total_commits)) {
        throw new Error("GitHub comparison did not return a commit count");
      }
      totalCommits = comparison.total_commits;
    }
    for (const commit of comparison.commits) {
      commits.set(commit.sha, commit.sha);
    }
    if (commits.size >= totalCommits) {
      break;
    }
    if (comparison.commits.length === 0) {
      throw new Error(
        `GitHub returned ${commits.size} of ${totalCommits} merge group commits`,
      );
    }
  }

  if (commits.size === 0) {
    throw new Error(`Merge group ${baseSha}...${headSha} has no commits`);
  }
  return [...commits.keys()];
}

async function loadAssociatedPullRequests(repository, sha) {
  const pullRequests = [];
  for (let page = 1; ; page++) {
    const batch = await githubJson(
      `/repos/${repository}/commits/${sha}/pulls?per_page=100&page=${page}`,
    );
    pullRequests.push(...batch);
    if (batch.length < 100) {
      return pullRequests;
    }
  }
}

export function collectMergeGroupPullRequests(associatedPullRequests, baseRef) {
  const baseBranch = baseRef.replace(/^refs\/heads\//, "");
  const pullRequests = new Map();
  for (const pullRequest of associatedPullRequests) {
    if (pullRequest.state === "open" && pullRequest.base.ref === baseBranch) {
      pullRequests.set(pullRequest.number, {
        number: pullRequest.number,
        title: pullRequest.title,
      });
    }
  }

  return [...pullRequests.values()];
}

async function loadMergeGroupPullRequests(
  repository,
  baseSha,
  headSha,
  baseRef,
) {
  const commits = await loadCompareCommits(repository, baseSha, headSha);
  const associatedPullRequests = [];
  for (let index = 0; index < commits.length; index += 10) {
    const batches = await Promise.all(
      commits
        .slice(index, index + 10)
        .map((sha) => loadAssociatedPullRequests(repository, sha)),
    );
    associatedPullRequests.push(...batches.flat());
  }

  const pullRequests = collectMergeGroupPullRequests(
    associatedPullRequests,
    baseRef,
  );

  if (pullRequests.length === 0) {
    throw new Error(
      `Merge group ${baseSha}...${headSha} has no open pull requests targeting ${baseRef}`,
    );
  }
  return pullRequests;
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const repository = process.env.GITHUB_REPOSITORY;
  const number = process.env.PR_NUMBER;
  const title = process.env.PR_TITLE;
  const mergeGroupSha = process.env.MERGE_GROUP_SHA;
  const mergeGroupBaseSha = process.env.MERGE_GROUP_BASE_SHA;
  const mergeGroupBaseRef = process.env.MERGE_GROUP_BASE_REF;
  const pullRequestMode = /^\d+$/.test(number ?? "") && Boolean(title);
  const mergeGroupMode =
    /^[0-9a-f]{40}$/.test(mergeGroupSha ?? "") &&
    /^[0-9a-f]{40}$/.test(mergeGroupBaseSha ?? "") &&
    /^refs\/heads\/.+/.test(mergeGroupBaseRef ?? "");
  if (
    !repository ||
    !process.env.GH_TOKEN ||
    pullRequestMode === mergeGroupMode
  ) {
    console.error(
      "GITHUB_REPOSITORY and GH_TOKEN plus exactly one of PR_NUMBER/PR_TITLE or MERGE_GROUP_BASE_SHA/MERGE_GROUP_SHA/MERGE_GROUP_BASE_REF are required",
    );
    process.exit(1);
  }

  const [versionText, configText] = pullRequestMode
    ? [
        readFileSync("VERSION", "utf8"),
        readFileSync("release-please-config.json", "utf8"),
      ]
    : await Promise.all([
        loadRepositoryFile(repository, "VERSION", mergeGroupSha),
        loadRepositoryFile(
          repository,
          "release-please-config.json",
          mergeGroupSha,
        ),
      ]);
  const options = releasePolicyFromFiles(versionText, configText);
  const pullRequests = pullRequestMode
    ? [{ number: Number.parseInt(number, 10), title }]
    : await loadMergeGroupPullRequests(
        repository,
        mergeGroupBaseSha,
        mergeGroupSha,
        mergeGroupBaseRef,
      );
  const errors = [];
  for (const pullRequest of pullRequests) {
    const commits = await loadPullRequestCommits(
      repository,
      pullRequest.number,
    );
    errors.push(
      ...releaseImpactErrors(pullRequest.title, commits, options).map(
        (error) => `#${pullRequest.number}: ${error}`,
      ),
    );
  }
  if (errors.length > 0) {
    console.error("Commit messages exceed the pull request release impact:");
    for (const error of errors) {
      console.error(`- ${error}`);
    }
    process.exit(1);
  }
}
