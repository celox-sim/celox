import { execFileSync } from "node:child_process";
import { pathToFileURL } from "node:url";

export function qualificationState(runs, sha) {
  const candidates = runs.filter(run => run.head_sha === sha && run.head_branch === "release" && ["push", "workflow_dispatch"].includes(run.event));
  if (candidates.some(run => run.status === "completed" && run.conclusion === "success")) return "success";
  if (candidates.some(run => ["queued", "in_progress", "waiting", "pending", "requested"].includes(run.status))) return "pending";
  return "missing";
}

export async function requireQualification({ sha, runs, now = Date.now, sleep = ms => new Promise(resolve => setTimeout(resolve, ms)), timeout = 600_000 }) {
  if (!/^[0-9a-f]{40}$/.test(sha)) throw new Error("Expected an immutable release SHA");
  const deadline = now() + timeout;
  for (;;) {
    const state = qualificationState(await runs(), sha);
    if (state === "success") return;
    if (state !== "pending") throw new Error("No successful Release Candidate run for this release SHA");
    if (now() >= deadline) throw new Error("Release Candidate workflow did not finish successfully before the deadline");
    await sleep(10_000);
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const repository = process.env.GITHUB_REPOSITORY;
  const sha = process.env.CANDIDATE_SHA;
  if (!/^[\w.-]+\/[\w.-]+$/.test(repository)) throw new Error("Invalid repository");
  await requireQualification({ sha, runs: () => JSON.parse(execFileSync("gh", ["api", `repos/${repository}/actions/workflows/release-candidate.yml/runs?head_sha=${sha}&branch=release&per_page=100`], { encoding: "utf8" })).workflow_runs });
}
