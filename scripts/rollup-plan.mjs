import { createHash } from "node:crypto";

const marker = "<!-- celox-rollup-v1\n";
const shaPattern = /^[0-9a-f]{40}$/;
export const sourceLabel = "rollup:source";

export function pullNumbers(value) {
  const entries = value.trim().split(/[\s,]+/);
  if (!entries.length || entries.some((n) => !/^#?[1-9]\d*$/.test(n)))
    throw new Error("Specify comma-separated stack-tip PR numbers");
  const numbers = entries.map((n) => Number(n.replace(/^#/, "")));
  if (
    numbers.some((n) => !Number.isSafeInteger(n)) ||
    new Set(numbers).size !== numbers.length
  )
    throw new Error("Invalid or duplicate PR numbers");
  return numbers;
}

export function generation(plan) {
  return createHash("sha256")
    .update(
      JSON.stringify(plan.sources.map(({ number, sha }) => [number, sha])),
    )
    .digest("hex")
    .slice(0, 12);
}

export function validatePlan(plan, repository) {
  if (
    plan?.repository !== repository ||
    !/^[1-9]\d*$/.test(plan.id ?? "") ||
    !["master", "develop"].includes(plan.base) ||
    !shaPattern.test(plan.baseSha ?? "") ||
    typeof plan.queue !== "boolean" ||
    !Array.isArray(plan.tips) ||
    !plan.tips.length ||
    !Array.isArray(plan.sources) ||
    !plan.sources.length ||
    plan.sources.length > 100
  )
    throw new Error("Invalid rollup plan");
  const numbers = new Set();
  for (const source of plan.sources) {
    if (
      !Number.isSafeInteger(source.number) ||
      source.number < 1 ||
      numbers.has(source.number) ||
      !shaPattern.test(source.sha ?? "") ||
      typeof source.heldBefore !== "boolean"
    )
      throw new Error("Invalid rollup source");
    numbers.add(source.number);
  }
  if (
    new Set(plan.tips).size !== plan.tips.length ||
    plan.tips.some((n) => !numbers.has(n))
  )
    throw new Error("Invalid rollup tips");
  return plan;
}

export function readPlan(body, repository) {
  const start = body?.indexOf(marker) ?? -1;
  if (start < 0) return null;
  const end = body.indexOf("\n-->", start);
  if (end < 0 || body.indexOf(marker, start + marker.length) >= 0)
    throw new Error("Invalid rollup metadata");
  return validatePlan(
    JSON.parse(
      Buffer.from(body.slice(start + marker.length, end), "base64").toString(
        "utf8",
      ),
    ),
    repository,
  );
}

export function rollupBranch(plan) {
  return `rollup/${plan.id}`;
}
export function resolutionBranch(plan, number) {
  return `rollup/resolve-${plan.id}/${number}-${generation(plan)}`;
}

export function renderBody(plan, status) {
  return (
    `${marker}${Buffer.from(JSON.stringify(plan)).toString("base64")}\n-->\n\n` +
    `Roll up stacks ${plan.tips.map((n) => `#${n}`).join(", ")} into \`${plan.base}\`.\n\n` +
    `${status}\n\n` +
    "Source PRs are held while this rollup is active. Their original commits are preserved. " +
    "Use **Actions → Roll up stacks → Run workflow** to refresh or cancel this rollup. " +
    "Cancellation restores holds added by the workflow.\n\n" +
    "| Source PR | Pinned head |\n| --- | --- |\n" +
    plan.sources.map((s) => `| #${s.number} | \`${s.sha}\` |`).join("\n") +
    "\n\n" +
    plan.sources.map((s) => `Closes #${s.number}`).join("\n")
  );
}

// The queue calls this on trusted code before certifying the combined tree.
// A source update cannot silently make a rollup's pinned snapshot current.
export async function validateRollupSources(repository, pull, api) {
  const plan = readPlan(pull.body, repository);
  const managed = /^rollup\/[1-9]\d*$/.test(pull.head?.ref ?? "");
  if (!plan && !managed) return;
  if (
    !plan ||
    pull.head?.ref !== rollupBranch(plan) ||
    pull.head?.repo?.full_name !== repository ||
    pull.base?.ref !== plan.base
  )
    throw new Error("Rollup branch or base does not match its plan");
  for (const source of plan.sources) {
    if (source.number === pull.number)
      throw new Error("Rollup cannot contain itself");
    const current = await api(`/repos/${repository}/pulls/${source.number}`);
    if (
      current.draft ||
      current.head?.sha !== source.sha ||
      current.head?.repo?.full_name !== repository ||
      (current.state !== "open" && !current.merged)
    )
      throw new Error(
        `Rollup source #${source.number} changed; refresh the rollup`,
      );
    const comparison = await api(
      `/repos/${repository}/compare/${source.sha}...${pull.head.sha}`,
    );
    if (!["ahead", "identical"].includes(comparison.status))
      throw new Error(`Rollup is missing source #${source.number}`);
  }
}
