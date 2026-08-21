import { readFileSync } from "node:fs";

export function releaseRepositorySettingErrors(settings) {
  const errors = [];

  if (settings.allow_merge_commit !== true) {
    errors.push("merge commits must be enabled");
  }

  if (settings.merge_commit_title !== "PR_TITLE") {
    errors.push(
      `merge_commit_title must be PR_TITLE, got ${JSON.stringify(settings.merge_commit_title)}`,
    );
  }

  return errors;
}

if (import.meta.url === `file://${process.argv[1]}`) {
  let settings;

  try {
    settings = JSON.parse(readFileSync(0, "utf8"));
  } catch (error) {
    console.error(
      `Failed to read repository settings from stdin: ${error.message}`,
    );
    process.exit(1);
  }

  const errors = releaseRepositorySettingErrors(settings);
  if (errors.length > 0) {
    console.error("Repository settings cannot preserve release semantics:");
    for (const error of errors) {
      console.error(`- ${error}`);
    }
    console.error(
      "Release Please parses the merge commit subject, so it must be the pull request title.",
    );
    process.exit(1);
  }
}
