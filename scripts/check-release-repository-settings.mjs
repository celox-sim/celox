export function releaseRepositorySettingErrors(repository) {
  const errors = [];

  if (repository.allow_merge_commit !== true) {
    errors.push("merge commits must be enabled");
  }

  if (repository.allow_squash_merge !== false) {
    errors.push("squash merges must be disabled");
  }

  if (repository.allow_rebase_merge !== false) {
    errors.push("rebase merges must be disabled");
  }

  if (repository.merge_commit_title !== "PR_TITLE") {
    errors.push(
      `merge_commit_title must be PR_TITLE, got ${JSON.stringify(repository.merge_commit_title)}`,
    );
  }

  if (repository.merge_commit_message !== "BLANK") {
    errors.push(
      `merge_commit_message must be BLANK, got ${JSON.stringify(repository.merge_commit_message)}`,
    );
  }

  return errors;
}

async function githubJson(path) {
  const apiUrl = process.env.GITHUB_API_URL ?? "https://api.github.com";
  const response = await fetch(`${apiUrl}${path}`, {
    headers: {
      Accept: "application/vnd.github+json",
      Authorization: `Bearer ${process.env.GH_TOKEN}`,
      "X-GitHub-Api-Version": "2022-11-28",
    },
  });

  if (!response.ok) {
    throw new Error(`GitHub API ${path} returned ${response.status}`);
  }

  return response.json();
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const repositoryName = process.env.GITHUB_REPOSITORY;
  const token = process.env.GH_TOKEN;

  if (!repositoryName || !token) {
    console.error("GITHUB_REPOSITORY and GH_TOKEN are required");
    process.exit(1);
  }

  const repository = await githubJson(`/repos/${repositoryName}`);
  const errors = releaseRepositorySettingErrors(repository);
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
