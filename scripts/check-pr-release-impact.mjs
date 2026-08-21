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
const PATCH_TYPES = new Set(["fix", "perf", "revert"]);

const impactNames = ["none", "patch", "feature", "breaking", "forced"];

function conventionalLines(message) {
  return message.split(/\r?\n/).filter((line) => /^[a-z][^\s:]*.*:/.test(line));
}

export function titleReleaseImpact(title, { preMajor }) {
  const parsed = parseConventionalPrTitle(title);
  if (!parsed) {
    return null;
  }
  if (parsed.breaking) {
    return 3;
  }
  if (parsed.type === "feat") {
    return preMajor ? 1 : 2;
  }
  if (PATCH_TYPES.has(parsed.type)) {
    return 1;
  }
  if (NO_RELEASE_TYPES.has(parsed.type)) {
    return 0;
  }
  return 0;
}

export function commitReleaseImpact(message, { preMajor }) {
  const [subject = ""] = message.split(/\r?\n/, 1);
  if (/^Merge\b/.test(subject)) {
    return 0;
  }
  if (/^Release-As:\s*\S/im.test(message)) {
    return 4;
  }
  if (
    /^BREAKING(?:[ -])CHANGE:\s*\S/im.test(message) ||
    conventionalLines(message).some((line) =>
      /^[a-z][a-z0-9_-]*(?:\([^\r\n)]*\))?!:/.test(line),
    )
  ) {
    return 3;
  }
  if (
    conventionalLines(message).some((line) =>
      /^(?:feat|feature)(?:\([^\r\n)]*\))?:/.test(line),
    )
  ) {
    return preMajor ? 1 : 2;
  }
  if (
    conventionalLines(message).some((line) =>
      /^(?:fix|perf|revert)(?:\([^\r\n)]*\))?:/.test(line),
    )
  ) {
    return 1;
  }
  return 0;
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

if (import.meta.url === `file://${process.argv[1]}`) {
  const repository = process.env.GITHUB_REPOSITORY;
  const number = process.env.PR_NUMBER;
  const title = process.env.PR_TITLE;
  if (
    !repository ||
    !/^\d+$/.test(number ?? "") ||
    !title ||
    !process.env.GH_TOKEN
  ) {
    console.error(
      "GITHUB_REPOSITORY, PR_NUMBER, PR_TITLE, and GH_TOKEN are required",
    );
    process.exit(1);
  }

  const major = Number.parseInt(readFileSync("VERSION", "utf8"), 10);
  const commits = await loadPullRequestCommits(repository, number);
  const errors = releaseImpactErrors(title, commits, { preMajor: major === 0 });
  if (errors.length > 0) {
    console.error("Commit messages exceed the pull request release impact:");
    for (const error of errors) {
      console.error(`- ${error}`);
    }
    process.exit(1);
  }
}
