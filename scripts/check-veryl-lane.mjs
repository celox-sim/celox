import fs from "node:fs";

import { verylDependencyNames } from "./use-veryl-head.mjs";

const verylRepository = "https://github.com/veryl-lang/veryl.git";

export function checkVerylLane(manifest, lane) {
  if (lane !== "stable" && lane !== "head") {
    throw new Error(`Expected Veryl lane stable or head, got ${lane}`);
  }

  const revisions = new Set();
  for (const name of verylDependencyNames) {
    const match = manifest.match(new RegExp(`^${name}\\s*=\\s*(.+)$`, "m"));
    if (match === null) {
      throw new Error(`Could not find workspace dependency ${name}`);
    }

    const declaration = match[1];
    if (lane === "stable") {
      if (/\bgit\s*=/.test(declaration)) {
        throw new Error(`Stable Veryl lane cannot use a git dependency: ${name}`);
      }
      continue;
    }

    if (!declaration.includes(`git = "${verylRepository}"`)) {
      throw new Error(`Veryl HEAD dependency ${name} does not use the upstream repository`);
    }
    const revision = declaration.match(/\brev\s*=\s*"([0-9a-f]{40})"/);
    if (revision === null) {
      throw new Error(`Veryl HEAD dependency ${name} is not pinned to a full revision`);
    }
    revisions.add(revision[1]);
  }

  if (lane === "head" && revisions.size !== 1) {
    throw new Error(`Veryl HEAD dependencies use ${revisions.size} different revisions`);
  }

  return lane === "head" ? [...revisions][0] : undefined;
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const lane = process.argv[2] ?? "";
  const revision = checkVerylLane(fs.readFileSync("Cargo.toml", "utf8"), lane);
  console.log(
    lane === "head"
      ? `Validated Veryl HEAD lane at ${revision}`
      : "Validated released Veryl lane",
  );
}
