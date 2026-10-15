import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { pathToFileURL } from "node:url";

const execFileAsync = promisify(execFile);

export const RELEASE_HEAD =
  "release-please--branches--master--components--celox";
export const RELEASE_HOLD_LABEL = "release:hold";

const PULL_REQUEST_STATE_QUERY = `
query($prId: ID!) {
  node(id: $prId) {
    ... on PullRequest {
      id
      number
      state
      merged
      isInMergeQueue
      mergeQueueEntry {
        id
        position
      }
      autoMergeRequest {
        enabledAt
      }
      labels(first: 100) {
        nodes {
          name
        }
      }
    }
  }
}`;

const DISABLE_AUTO_MERGE_MUTATION = `
mutation($prId: ID!) {
  disablePullRequestAutoMerge(input: {pullRequestId: $prId}) {
    pullRequest {
      id
    }
  }
}`;

const DEQUEUE_PULL_REQUEST_MUTATION = `
mutation($prId: ID!) {
  dequeuePullRequest(input: {id: $prId}) {
    clientMutationId
  }
}`;

function hasLabel(pullRequest, label) {
  return pullRequest.labels.some((item) => item.name === label);
}

export function selectReleasePullRequest(pullRequests) {
  const matches = pullRequests.filter(
    (pullRequest) =>
      pullRequest.headRefName === RELEASE_HEAD &&
      pullRequest.isCrossRepository === false,
  );

  if (matches.length > 1) {
    throw new Error(`Found ${matches.length} matching release pull requests`);
  }

  return matches[0] ?? null;
}

async function stopHeldRelease(github, pullRequest, log) {
  if (hasLabel(pullRequest, "merge-ready")) {
    await github.removeLabel(pullRequest.number, "merge-ready");
  }
  if (pullRequest.isInMergeQueue) {
    await github.dequeuePullRequest(pullRequest.id);
    log(
      `Removed release pull request #${pullRequest.number} from the merge queue.`,
    );
    pullRequest = await github.getPullRequest(pullRequest.id);
  }

  if (pullRequest.autoMergeRequest !== null) {
    await github.disableAutoMerge(pullRequest.id);
    log(
      `Disabled auto-merge for held release pull request #${pullRequest.number}.`,
    );
  }

  log(
    `Release pull request #${pullRequest.number} is held by ${RELEASE_HOLD_LABEL}.`,
  );
  return { outcome: "held", number: pullRequest.number };
}

export async function queueReleasePullRequest({
  github,
  timeoutMs = 60 * 60 * 1000,
  pollIntervalMs = 15 * 1000,
  now = Date.now,
  sleep = (duration) => new Promise((resolve) => setTimeout(resolve, duration)),
  log = console.log,
}) {
  if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) {
    throw new Error("timeoutMs must be a positive finite number");
  }
  if (!Number.isFinite(pollIntervalMs) || pollIntervalMs <= 0) {
    throw new Error("pollIntervalMs must be a positive finite number");
  }

  const candidate = selectReleasePullRequest(
    await github.listOpenPullRequests(),
  );
  if (candidate === null) {
    log("No release pull request is open.");
    return { outcome: "missing" };
  }

  let pullRequest = await github.getPullRequest(candidate.id);
  if (hasLabel(pullRequest, RELEASE_HOLD_LABEL)) {
    return stopHeldRelease(github, pullRequest, log);
  }
  // Clear legacy GitHub auto-merge before requesting the Mergify queue.
  if (pullRequest.isInMergeQueue)
    await github.dequeuePullRequest(pullRequest.id);
  if (pullRequest.autoMergeRequest !== null)
    await github.disableAutoMerge(pullRequest.id);
  await github.removeLabel(pullRequest.number, "dequeued");
  await github.addLabel(pullRequest.number, "merge-ready");

  const deadline = now() + timeoutMs;
  let attempt = 0;

  while (true) {
    attempt += 1;
    pullRequest = await github.getPullRequest(candidate.id);

    if (pullRequest.merged || pullRequest.state === "MERGED") {
      log(`Release pull request #${pullRequest.number} merged while waiting.`);
      return { outcome: "merged", number: pullRequest.number };
    }
    if (pullRequest.state !== "OPEN") {
      throw new Error(
        `Release pull request #${pullRequest.number} became ${pullRequest.state}`,
      );
    }
    if (hasLabel(pullRequest, RELEASE_HOLD_LABEL)) {
      return stopHeldRelease(github, pullRequest, log);
    }
    if (hasLabel(pullRequest, "queued")) {
      log(
        `Release pull request #${pullRequest.number} entered the merge queue.`,
      );
      return { outcome: "queued", number: pullRequest.number };
    }

    if (hasLabel(pullRequest, "dequeued")) {
      throw new Error(
        `Mergify dequeued release pull request #${pullRequest.number}; inspect its queue check before retrying`,
      );
    }

    if (now() >= deadline) {
      throw new Error(
        `Timed out queueing release pull request #${pullRequest.number} after ${attempt} attempts: Mergify has not reported a queue entry`,
      );
    }

    log(
      `Release pull request #${pullRequest.number} is not queueable yet; retrying (Mergify has not reported a queue entry).`,
    );
    await sleep(pollIntervalMs);
  }
}

export class GitHubCli {
  constructor(repository) {
    if (!/^[^/]+\/[^/]+$/.test(repository)) {
      throw new Error(`Invalid GITHUB_REPOSITORY: ${repository}`);
    }
    this.repository = repository;
  }

  async run(args) {
    try {
      const { stdout } = await execFileAsync("gh", args, {
        encoding: "utf8",
        maxBuffer: 10 * 1024 * 1024,
      });
      return stdout;
    } catch (error) {
      const detail =
        error.stderr?.trim() || error.stdout?.trim() || error.message;
      throw new Error(detail);
    }
  }

  async graphql(query, pullRequestId) {
    const output = await this.run([
      "api",
      "graphql",
      "-f",
      `query=${query}`,
      "-f",
      `prId=${pullRequestId}`,
    ]);
    return JSON.parse(output).data;
  }

  async listOpenPullRequests() {
    const output = await this.run([
      "pr",
      "list",
      "--repo",
      this.repository,
      "--state",
      "open",
      "--base",
      "master",
      "--limit",
      "1000",
      "--json",
      "id,number,headRefName,isCrossRepository",
    ]);
    return JSON.parse(output);
  }

  async getPullRequest(pullRequestId) {
    const data = await this.graphql(PULL_REQUEST_STATE_QUERY, pullRequestId);
    const pullRequest = data.node;
    if (pullRequest === null) {
      throw new Error(`Pull request ${pullRequestId} was not found`);
    }
    return {
      ...pullRequest,
      labels: pullRequest.labels.nodes,
    };
  }

  async addLabel(number, label) {
    await this.run([
      "pr",
      "edit",
      String(number),
      "--repo",
      this.repository,
      "--add-label",
      label,
    ]);
  }

  async removeLabel(number, label) {
    await this.run([
      "pr",
      "edit",
      String(number),
      "--repo",
      this.repository,
      "--remove-label",
      label,
    ]);
  }

  async disableAutoMerge(pullRequestId) {
    await this.graphql(DISABLE_AUTO_MERGE_MUTATION, pullRequestId);
  }

  async dequeuePullRequest(pullRequestId) {
    await this.graphql(DEQUEUE_PULL_REQUEST_MUTATION, pullRequestId);
  }
}

function positiveSeconds(name, fallback) {
  const raw = process.env[name];
  if (raw === undefined) {
    return fallback;
  }
  const value = Number(raw);
  if (!Number.isFinite(value) || value <= 0) {
    throw new Error(`${name} must be a positive number`);
  }
  return value * 1000;
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  try {
    const repository = process.env.GITHUB_REPOSITORY;
    const github = new GitHubCli(repository ?? "");
    await queueReleasePullRequest({
      github,
      timeoutMs: positiveSeconds("RELEASE_QUEUE_TIMEOUT_SECONDS", 60 * 60),
      pollIntervalMs: positiveSeconds("RELEASE_QUEUE_POLL_SECONDS", 15),
    });
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
