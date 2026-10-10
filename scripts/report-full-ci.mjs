import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { pathToFileURL } from "node:url";

const execFileAsync = promisify(execFile);

const FAILED_RESULTS = new Set(["failure", "cancelled"]);

export function issueTitle(branch, name = "Full CI") {
  return `${name} is failing on ${branch}`;
}

// `needs` is the workflow's toJSON(needs). Skipped jobs are not failures: a
// full run enables every path, so a skip only follows an upstream failure.
export function failedJobs(needs) {
  return Object.entries(needs)
    .filter(([, job]) => FAILED_RESULTS.has(job.result))
    .map(([name, job]) => `${name} (${job.result})`)
    .sort();
}

export async function reportFullCi({ github, needs, branch, sha, runUrl, name = "Full CI", details = "", log = console.log }) {
  const title = issueTitle(branch, name);
  const failed = failedJobs(needs);
  const open = (await github.listOpenIssues(title)).filter(
    (issue) => issue.title === title,
  );
  if (open.length > 1) {
    throw new Error(`Found ${open.length} open issues titled "${title}"`);
  }
  const issue = open[0] ?? null;

  if (failed.length === 0) {
    if (issue === null) {
      log(`${name} passed on ${branch}.`);
      return { outcome: "passed" };
    }
    await github.comment(
      issue.number,
      `${name} passed again on \`${sha}\`: ${runUrl}`,
    );
    await github.close(issue.number);
    log(`Closed #${issue.number}: ${name} passed on ${branch}.`);
    return { outcome: "closed", number: issue.number };
  }

  const body = [
    `${name} failed on \`${branch}\` at \`${sha}\`: ${runUrl}`,
    "",
    "Failed jobs:",
    ...failed.map((job) => `- ${job}`),
    ...(details ? ["", details] : []),
  ].join("\n");
  if (issue !== null) {
    await github.comment(issue.number, body);
    log(`Commented on #${issue.number}.`);
    return { outcome: "commented", number: issue.number };
  }
  const number = await github.create(
    title,
    `${body}\n\nThis issue closes automatically when ${name} passes on \`${branch}\` again.`,
  );
  log(`Opened #${number}.`);
  return { outcome: "opened", number };
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
      const detail = error.stderr?.trim() || error.stdout?.trim() || error.message;
      throw new Error(detail);
    }
  }

  async listOpenIssues(title) {
    const output = await this.run([
      "issue",
      "list",
      "--repo",
      this.repository,
      "--state",
      "open",
      "--search",
      `"${title}" in:title`,
      "--limit",
      "100",
      "--json",
      "number,title",
    ]);
    return JSON.parse(output);
  }

  async comment(number, body) {
    await this.run([
      "issue",
      "comment",
      String(number),
      "--repo",
      this.repository,
      "--body",
      body,
    ]);
  }

  async close(number) {
    await this.run(["issue", "close", String(number), "--repo", this.repository]);
  }

  async create(title, body) {
    const output = await this.run([
      "issue",
      "create",
      "--repo",
      this.repository,
      "--title",
      title,
      "--body",
      body,
    ]);
    const match = /\/issues\/(\d+)\s*$/.exec(output.trim());
    if (!match) {
      throw new Error(`Unexpected gh issue create output: ${output}`);
    }
    return Number(match[1]);
  }
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  try {
    await reportFullCi({
      github: new GitHubCli(process.env.GITHUB_REPOSITORY ?? ""),
      needs: JSON.parse(process.env.NEEDS ?? ""),
      branch: process.env.BRANCH,
      sha: process.env.SHA,
      runUrl: process.env.RUN_URL,
    });
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
