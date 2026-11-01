import { execFileSync } from "node:child_process";
import { createRequire } from "node:module";
import { pathToFileURL } from "node:url";

export const TRACKER_TITLE = "Celox release tracker";
export const TRACKER_MARKER = "<!-- celox-release-tracker -->";

export function selectTracker(issues) {
  const matches = issues.filter(issue => issue.title === TRACKER_TITLE && issue.body?.startsWith(TRACKER_MARKER));
  if (matches.length > 1) throw new Error("Multiple Celox release trackers are open");
  return matches[0] ?? null;
}

export async function updateReleaseIssue({ github, manifest, pullNumber }) {
  const issue = selectTracker(await github.listIssues());
  if (pullNumber) {
    if (!/^\d+$/.test(String(pullNumber))) throw new Error("Invalid release PR number");
    if (issue) await github.closeIssue(issue.number, `Release preparation continues in #${pullNumber}.`);
    return;
  }
  // Building candidates calculates the same version and notes as Release Please,
  // without creating commits, branches, pull requests, tags or releases.
  const candidates = await manifest.buildPullRequests();
  if (candidates.length > 1) throw new Error("Expected a single Celox release candidate");
  if (candidates.length === 0) {
    if (issue) await github.closeIssue(issue.number, "No unreleased changes remain.");
    return;
  }
  const pull = candidates[0];
  const body = `${TRACKER_MARKER}\n\n${pull.title.toString()}\n\nThis issue tracks the next Celox release. The release PR is created on Monday at 12:00 JST or by manually running Release Management. Apply \`release:hold\` here to postpone PR creation. Once a PR exists, apply the hold to that PR.\n\n${pull.body.notes()}`;
  if (!issue) await github.createIssue(TRACKER_TITLE, body);
  else if (issue.body !== body) await github.updateIssue(issue.number, body);
}

export class IssueGitHub {
  constructor(repository) {
    if (!/^[\w.-]+\/[\w.-]+$/.test(repository)) throw new Error("Invalid repository");
    this.repository = repository;
  }
  run(args, input) {
    return execFileSync("gh", args, { encoding: "utf8", input, maxBuffer: 10 * 1024 * 1024 });
  }
  api(path, payload, method = "POST") {
    return JSON.parse(this.run(["api", `repos/${this.repository}/${path}`, ...(payload ? ["--method", method, "--input", "-"] : [])], payload && JSON.stringify(payload)));
  }
  listIssues() {
    return JSON.parse(this.run(["issue", "list", "--repo", this.repository, "--state", "open", "--search", `in:title "${TRACKER_TITLE}"`, "--limit", "1000", "--json", "number,title,body,labels"]));
  }
  createIssue(title, body) { return this.api("issues", { title, body }); }
  updateIssue(number, body) { return this.api(`issues/${number}`, { body }, "PATCH"); }
  async closeIssue(number, comment) {
    await this.api(`issues/${number}/comments`, { body: comment });
    return this.api(`issues/${number}`, { state: "closed" }, "PATCH");
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const github = new IssueGitHub(process.env.GITHUB_REPOSITORY);
  if (process.argv[2] === "held") {
    const issue = selectTracker(await github.listIssues());
    console.log(issue?.labels.some(label => label.name === "release:hold") ? "true" : "false");
  } else if (process.env.RELEASE_PR_NUMBER) {
    await updateReleaseIssue({ github, pullNumber: process.env.RELEASE_PR_NUMBER });
  } else {
    const releasePlease = createRequire(new URL("./release-please/package.json", import.meta.url))("release-please");
    const [owner, repo] = process.env.GITHUB_REPOSITORY.split("/");
    const client = await releasePlease.GitHub.create({ owner, repo, token: process.env.GH_TOKEN });
    const manifest = await releasePlease.Manifest.fromManifest(client, "master");
    await updateReleaseIssue({ github, manifest });
  }
}
