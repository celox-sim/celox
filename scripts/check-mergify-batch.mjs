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
