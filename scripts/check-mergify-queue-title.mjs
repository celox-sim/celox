import { execFileSync } from "node:child_process";
import { pathToFileURL } from "node:url";
import { validateBatch } from "./check-mergify-batch.mjs";
import { githubJson } from "./check-pr-release-impact.mjs";

export async function checkQueueTitle(
  input,
  {
    api = githubJson,
    readMetadata = () =>
      JSON.parse(
        execFileSync("mergify", ["ci", "queue-info"], {
          cwd: "queue-tree",
          encoding: "utf8",
        }),
      ),
    validate = validateBatch,
  } = {},
) {
  if (!/^[0-9a-f]{40}$/.test(input.sha ?? ""))
    throw new Error("Exact batch SHA required");
  async function obsolete() {
    const batch = await api(`/repos/${input.repository}/pulls/${input.number}`);
    if (batch.state === "closed") return true;
    if (batch.state !== "open" || !/^[0-9a-f]{40}$/.test(batch.head?.sha ?? ""))
      throw new Error("Cannot establish current queue PR state");
    return batch.head.sha !== input.sha;
  }
  if (await obsolete()) return "Skipped obsolete queue PR event";
  try {
    const metadata = await readMetadata();
    await validate({ ...input, metadata });
    return "Queue titles and release impact validated";
  } catch (error) {
    // Mergify can finish the batch and delete its git note while this job runs.
    // Only a fresh API confirmation of closure/replacement permits a skip.
    // Missing notes, invalid titles, and API failures on a live batch still fail.
    if (await obsolete())
      return "Skipped queue PR completed or replaced during validation";
    throw error;
  }
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  console.log(
    await checkQueueTitle({
      repository: process.env.GITHUB_REPOSITORY,
      number: Number(process.env.PR_NUMBER),
      sha: process.env.PR_HEAD_SHA,
    }),
  );
}
