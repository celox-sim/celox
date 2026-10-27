import { execFileSync } from "node:child_process";
import { pathToFileURL } from "node:url";

export async function promoteCandidate({ github, sha, tag }) {
  if (!/^[0-9a-f]{40}$/.test(sha) || !/^v[0-9]+\.[0-9]+\.[0-9]+$/.test(tag)) throw new Error("Invalid candidate identity");
  if (await github.branchSha() !== sha) throw new Error("Candidate was superseded on release; do not publish it");
  const pull = await github.releasePull(sha);
  if (!pull) throw new Error("Candidate is not a merged master release pull request");
  const tagged = await github.tagSha(tag);
  if (tagged && tagged !== sha) throw new Error("Release tag points at a different candidate");
  const release = await github.release(tag);
  if (release) {
    if (release.draft || release.prerelease || tagged !== sha) throw new Error("Existing release is not the qualified stable candidate");
  } else {
    await github.createRelease(tag, sha);
  }
  // Retry publication only for this immutable, qualified tag.
  await github.dispatchPackages(tag);
  await github.markTagged(pull.number);
}

function cli(args, input) {
  return execFileSync("gh", args, { encoding: "utf8", input, stdio: ["pipe", "pipe", "pipe"] }).trim();
}
export function githubClient(repository) {
  if (!/^[\w.-]+\/[\w.-]+$/.test(repository)) throw new Error("Invalid repository");
  const api = (path, optional = false) => {
    try { return JSON.parse(cli(["api", `repos/${repository}/${path}`])); }
    catch (error) {
      if (optional && /\(HTTP 404\)/.test(String(error.stderr))) return null;
      throw error;
    }
  };
  return {
    branchSha: () => api("git/ref/heads/release").object.sha,
    releasePull: sha => api(`commits/${sha}/pulls`).find(pr => pr.merged_at && pr.merge_commit_sha === sha && pr.base.ref === "master" && pr.head.ref === "release-please--branches--master--components--celox" && pr.head.repo?.full_name === repository),
    tagSha: tag => {
      let object = api(`git/ref/tags/${tag}`, true)?.object;
      for (let depth = 0; object?.type === "tag" && depth < 10; depth++) object = api(`git/tags/${object.sha}`).object;
      if (object && object.type !== "commit") throw new Error("Invalid release tag object");
      return object?.sha ?? null;
    },
    release: tag => api(`releases/tags/${tag}`, true),
    createRelease: (tag, sha) => cli(["release", "create", tag, "--repo", repository, "--target", sha, "--title", tag, "--generate-notes"]),
    dispatchPackages: tag => {
      cli(["api", "--method", "POST", `repos/${repository}/dispatches`, "--input", "-"], JSON.stringify({ event_type: "napi-release", client_payload: { tag } }));
      cli(["workflow", "run", "publish-crates.yml", "--repo", repository, "--ref", tag]);
    },
    markTagged: number => cli(["pr", "edit", String(number), "--repo", repository, "--remove-label", "autorelease: pending", "--add-label", "autorelease: tagged"]),
  };
}
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  await promoteCandidate({ github: githubClient(process.env.GITHUB_REPOSITORY), sha: process.env.CANDIDATE_SHA, tag: process.env.RELEASE_TAG });
}
