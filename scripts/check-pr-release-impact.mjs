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

async function githubGraphql(query, variables) {
  const graphqlUrl =
    process.env.GITHUB_GRAPHQL_URL ?? "https://api.github.com/graphql";
  const response = await fetch(graphqlUrl, {
    method: "POST",
    headers: {
      Accept: "application/vnd.github+json",
      Authorization: `Bearer ${process.env.GH_TOKEN}`,
      "Content-Type": "application/json",
      "X-GitHub-Api-Version": "2022-11-28",
    },
    body: JSON.stringify({ query, variables }),
  });
  if (!response.ok) {
    throw new Error(`GitHub GraphQL returned ${response.status}`);
  }
  const payload = await response.json();
  if (payload.errors?.length > 0) {
    throw new Error(
      `GitHub GraphQL returned errors: ${payload.errors.map((error) => error.message).join("; ")}`,
    );
  }
  return payload.data;
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

export function collectMergeQueuePullRequests(entries, targetPosition, baseRef) {
  const baseBranch = baseRef.replace(/^refs\/heads\//, "");
  const pullRequests = new Map();
  for (const entry of entries) {
    const pullRequest = entry.pullRequest;
    if (
      entry.position <= targetPosition &&
      pullRequest?.baseRefName === baseBranch
    ) {
      pullRequests.set(pullRequest.number, {
        number: pullRequest.number,
        title: pullRequest.title,
      });
    }
  }

  return [...pullRequests.values()];
}

export function mergeGroupHeadPullRequestNumber(headRef) {
  const match = headRef.match(
    /^(?:refs\/heads\/)?gh-readonly-queue\/.+\/pr-(\d+)-[0-9a-f]{40}$/,
  );
  return match ? Number.parseInt(match[1], 10) : null;
}

async function loadMergeGroupPullRequests(repository, headRef, baseRef) {
  const [owner, name, ...extra] = repository.split("/");
  const targetNumber = mergeGroupHeadPullRequestNumber(headRef);
  if (!owner || !name || extra.length > 0 || targetNumber === null) {
    throw new Error(`Invalid merge group repository or head ref: ${headRef}`);
  }

  const query = `
    query($owner: String!, $name: String!, $number: Int!, $cursor: String) {
      repository(owner: $owner, name: $name) {
        pullRequest(number: $number) {
          mergeQueueEntry { position }
          mergeQueue {
            entries(first: 100, after: $cursor) {
              nodes {
                position
                pullRequest { number title baseRefName }
              }
              pageInfo { hasNextPage endCursor }
            }
          }
        }
      }
    }
  `;
  const entries = [];
  let cursor = null;
  let targetPosition;
  for (;;) {
    const data = await githubGraphql(query, {
      owner,
      name,
      number: targetNumber,
      cursor,
    });
    const pullRequest = data.repository?.pullRequest;
    targetPosition ??= pullRequest?.mergeQueueEntry?.position;
    const connection = pullRequest?.mergeQueue?.entries;
    if (!Number.isSafeInteger(targetPosition) || !connection) {
      throw new Error(`Pull request #${targetNumber} is not in a merge queue`);
    }
    entries.push(...connection.nodes);
    if (!connection.pageInfo.hasNextPage) {
      break;
    }
    cursor = connection.pageInfo.endCursor;
    if (!cursor) {
      throw new Error("Merge queue pagination did not return a cursor");
    }
  }

  const pullRequests = collectMergeQueuePullRequests(
    entries,
    targetPosition,
    baseRef,
  );

  if (!pullRequests.some((pullRequest) => pullRequest.number === targetNumber)) {
    throw new Error(
      `Merge group ${headRef} does not contain its queued pull request #${targetNumber}`,
    );
  }
  return pullRequests;
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const repository = process.env.GITHUB_REPOSITORY;
  const number = process.env.PR_NUMBER;
  const title = process.env.PR_TITLE;
  const mergeGroupSha = process.env.MERGE_GROUP_SHA;
  const mergeGroupHeadRef = process.env.MERGE_GROUP_HEAD_REF;
  const mergeGroupBaseRef = process.env.MERGE_GROUP_BASE_REF;
  const pullRequestMode = /^\d+$/.test(number ?? "") && Boolean(title);
  const mergeGroupMode =
    /^[0-9a-f]{40}$/.test(mergeGroupSha ?? "") &&
    mergeGroupHeadPullRequestNumber(mergeGroupHeadRef ?? "") !== null &&
    /^refs\/heads\/.+/.test(mergeGroupBaseRef ?? "");
  if (
    !repository ||
    !process.env.GH_TOKEN ||
    pullRequestMode === mergeGroupMode
  ) {
    console.error(
      "GITHUB_REPOSITORY and GH_TOKEN plus exactly one of PR_NUMBER/PR_TITLE or MERGE_GROUP_HEAD_REF/MERGE_GROUP_SHA/MERGE_GROUP_BASE_REF are required",
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
        mergeGroupHeadRef,
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
