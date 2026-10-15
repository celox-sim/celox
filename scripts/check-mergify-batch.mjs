import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { isConventionalPrTitle } from "./check-pr-title.mjs";
import {
  githubJson,
  loadPullRequestCommits,
  loadReleasePolicy,
  releaseImpactErrors,
} from "./check-pr-release-impact.mjs";

export function batchPullRequestNumbers(metadata) {
  const entries = metadata?.pull_requests;
  if (!Array.isArray(entries) || entries.length === 0) {
    throw new Error("Missing merge queue pull requests");
  }
  const numbers = entries.map((entry) => entry.number);
  if (
    numbers.some((number) => !Number.isSafeInteger(number) || number < 1) ||
    new Set(numbers).size !== numbers.length
  ) {
    throw new Error("Invalid merge queue pull request numbers");
  }
  return numbers;
}

export async function validateBatch(
  { repository, number, sha, metadata },
  api = githubJson,
  loadCommits = loadPullRequestCommits,
  loadPolicy = loadReleasePolicy,
) {
  if (!/^[0-9a-f]{40}$/.test(sha ?? ""))
    throw new Error("Exact batch SHA required");
  const batch = await api(`/repos/${repository}/pulls/${number}`);
  if (
    batch.user?.login !== "mergify[bot]" ||
    batch.user?.type !== "Bot" ||
    batch.head?.repo?.full_name !== repository ||
    batch.head?.sha !== sha ||
    !batch.head?.ref?.startsWith("mergify/merge-queue/") ||
    !batch.draft
  ) {
    throw new Error("Not the expected Mergify queue draft");
  }
  const numbers = batchPullRequestNumbers(metadata);
  const repo = await api(`/repos/${repository}`);
  const options =
    batch.base.ref === repo.default_branch
      ? await loadPolicy(repository, sha)
      : null;
  if (options) {
    const basePolicy = await loadPolicy(repository, batch.base.sha);
    if (JSON.stringify(basePolicy) !== JSON.stringify(options)) {
      // A speculative batch can include earlier batches. If policy changes,
      // revalidate their still-open PRs too, under the combined tree's policy.
      const commits = new Set();
      let seen = 0;
      for (let page = 1; ; page++) {
        const comparison = await api(
          `/repos/${repository}/compare/${batch.base.sha}...${sha}?per_page=100&page=${page}`,
        );
        if (
          !Number.isSafeInteger(comparison.total_commits) ||
          comparison.total_commits > 10000 ||
          !Array.isArray(comparison.commits)
        ) {
          throw new Error("Cannot enumerate the speculative batch commits");
        }
        for (const commit of comparison.commits) commits.add(commit.sha);
        seen += comparison.commits.length;
        if (seen >= comparison.total_commits) break;
        if (!comparison.commits.length)
          throw new Error("Incomplete batch commit pagination");
      }
      for (let page = 1; ; page++) {
        const pulls = await api(
          `/repos/${repository}/pulls?state=open&base=${encodeURIComponent(batch.base.ref)}&per_page=100&page=${page}`,
        );
        for (const pull of pulls) {
          if (
            pull.number !== number &&
            !pull.draft &&
            commits.has(pull.head.sha) &&
            !numbers.includes(pull.number)
          )
            numbers.push(pull.number);
        }
        if (pulls.length < 100) break;
      }
    }
  }
  for (const sourceNumber of numbers) {
    if (sourceNumber === number) throw new Error("Batch cannot contain itself");
    const source = await api(`/repos/${repository}/pulls/${sourceNumber}`);
    if (
      source.base?.ref !== batch.base.ref ||
      !isConventionalPrTitle(source.title)
    ) {
      throw new Error(`Invalid title or base for #${sourceNumber}`);
    }
    // Never certify a force-pushed PR that is no longer part of this batch.
    const comparison = await api(
      `/repos/${repository}/compare/${source.head.sha}...${sha}`,
    );
    if (!["ahead", "identical"].includes(comparison.status)) {
      throw new Error(
        `#${sourceNumber} head is not contained in the tested batch`,
      );
    }
    if (options) {
      const commits = await loadCommits(repository, sourceNumber);
      const errors = releaseImpactErrors(source.title, commits, options);
      if (errors.length)
        throw new Error(`#${sourceNumber}: ${errors.join("; ")}`);
    }
  }
  return numbers;
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  await validateBatch({
    repository: process.env.GITHUB_REPOSITORY,
    number: Number(process.env.PR_NUMBER),
    sha: process.env.PR_HEAD_SHA,
    metadata: JSON.parse(
      readFileSync(process.env.MERGIFY_METADATA_FILE, "utf8"),
    ),
  });
}
