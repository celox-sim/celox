const allowedTypes = [
  "build",
  "chore",
  "ci",
  "docs",
  "feat",
  "fix",
  "perf",
  "refactor",
  "revert",
  "test",
];

const typePattern = allowedTypes.join("|");
const titlePattern = new RegExp(
  `^(${typePattern})(\\([a-z0-9][a-z0-9._/-]*\\))?(!)?: \\S(?:.*\\S)?$`,
);

export function isConventionalPrTitle(title) {
  return titlePattern.test(title);
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const title = process.argv[2] ?? "";

  if (!isConventionalPrTitle(title)) {
    console.error(`Invalid pull request title: ${JSON.stringify(title)}`);
    console.error(
      `Expected: <type>(optional-scope)[!]: <description>\nAllowed types: ${allowedTypes.join(", ")}`,
    );
    console.error("Example: fix(parser): preserve enum member widths");
    console.error("Breaking example: feat(api)!: remove legacy simulator options");
    process.exit(1);
  }
}
