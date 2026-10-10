import { setTimeout } from "node:timers/promises";
import { pathToFileURL } from "node:url";
import { GitHubCli, reportFullCi } from "./report-full-ci.mjs";

// A SHA can have several uploads (including reruns). Only use the analysis of
// this upload, never an earlier successful check on the same commit.
export function analysisForUpload(checks, runId) {
  if (!runId) throw new Error("CodSpeed did not return an uploaded run ID.");
  return checks.find((check) => {
    if (check.app?.slug !== "codspeed" || check.name !== "CodSpeed Performance Analysis") return false;
    try {
      const url = new URL(check.details_url);
      if (url.hostname !== "app.codspeed.io" && url.hostname !== "codspeed.io") return false;
      const run = url.pathname.split("/runs/")[1];
      return run === runId || (run?.startsWith("compare/") && run.slice(8).split("..")[1] === runId);
    } catch {
      return false;
    }
  });
}

export async function waitForAnalysis({ github, sha, runId, sleep = setTimeout, attempts = 21, intervalMs = 15_000 }) {
  if (!runId) throw new Error("CodSpeed did not return an uploaded run ID.");
  for (let attempt = 0; attempt < attempts; attempt++) {
    const check = analysisForUpload(await github.listChecks(sha), runId);
    if (check?.status === "completed") return check;
    if (attempt + 1 < attempts) await sleep(intervalMs);
  }
  throw new Error(`Timed out waiting for CodSpeed Performance Analysis for upload ${runId}.`);
}

export async function reportCodSpeed({ github, benchmarkResult, runId, branch, sha, runUrl, log = console.log, ...pollOptions }) {
  const needs = { codspeed: { result: benchmarkResult === "success" ? "success" : benchmarkResult === "cancelled" ? "cancelled" : "failure" } };
  let details = "";
  if (benchmarkResult === "success") {
    try {
      const check = await waitForAnalysis({ github, sha, runId, ...pollOptions });
      needs["performance-analysis"] = { result: check.conclusion === "success" ? "success" : "failure" };
      details = [
        `CodSpeed analysis: ${check.conclusion}. [Performance comparison](${check.details_url})`,
        "", check.output?.title ?? "", check.output?.summary ?? "",
      ].join("\n");
    } catch (error) {
      needs["performance-analysis"] = { result: "failure" };
      details = `Could not verify performance: ${error.message}`;
    }
  }
  const outcome = await reportFullCi({ github, needs, branch, sha, runUrl, name: "CodSpeed", details, log });
  return { ...outcome, failed: Object.values(needs).some((job) => job.result !== "success") };
}

class CodSpeedGitHubCli extends GitHubCli {
  async listChecks(sha) {
    const output = await this.run([
      "api", `repos/${this.repository}/commits/${sha}/check-runs?per_page=100`,
      "--paginate", "--slurp",
    ]);
    return JSON.parse(output).flatMap((page) => page.check_runs);
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    const result = await reportCodSpeed({
      github: new CodSpeedGitHubCli(process.env.GITHUB_REPOSITORY ?? ""),
      benchmarkResult: process.env.BENCHMARK_RESULT,
      runId: process.env.CODSPEED_RUN_ID,
      branch: process.env.BRANCH,
      sha: process.env.SHA,
      runUrl: process.env.RUN_URL,
    });
    if (result.failed) process.exitCode = 1;
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
