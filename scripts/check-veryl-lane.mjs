import fs from "node:fs";

import { verylDependencyNames } from "./use-veryl-head.mjs";

const verylRepository = "https://github.com/veryl-lang/veryl.git";
const exactVersion =
  /^(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)(?:[-+][0-9A-Za-z.-]+)?$/;

function workspaceDependencies(manifest) {
  const header = "[workspace.dependencies]";
  const start = manifest.indexOf(`${header}\n`);
  if (start === -1) {
    throw new Error(`Could not find ${header}`);
  }

  const bodyStart = start + header.length + 1;
  const next = manifest.indexOf("\n[", bodyStart);
  return manifest.slice(bodyStart, next === -1 ? manifest.length : next + 1);
}

function dependencyDeclaration(dependencies, name) {
  const escaped = name.replaceAll("-", "\\-");
  const matches = [
    ...dependencies.matchAll(new RegExp(`^${escaped}\\s*=\\s*(.+)$`, "gm")),
  ];
  if (matches.length !== 1) {
    throw new Error(
      `Expected exactly one workspace dependency ${name}, found ${matches.length}`,
    );
  }
  return matches[0][1].replace(/\s+#.*$/, "").trim();
}

function fieldValues(declaration, field) {
  const occurrences = [
    ...declaration.matchAll(new RegExp(`\\b${field}\\s*=`, "g")),
  ];
  const values = [
    ...declaration.matchAll(new RegExp(`\\b${field}\\s*=\\s*"([^"]*)"`, "g")),
  ];
  if (occurrences.length !== values.length) {
    throw new Error(`Veryl dependency field ${field} must be a string`);
  }
  return values.map((match) => match[1]);
}

function stableVersion(name, declaration) {
  if (/\b(?:git|rev|path|branch|tag|registry|package)\s*=/.test(declaration)) {
    throw new Error(
      `Released Veryl dependency ${name} uses a non-release source`,
    );
  }

  const direct = declaration.match(/^"([^"]+)"$/);
  const versions = fieldValues(declaration, "version");
  const version = direct?.[1] ?? (versions.length === 1 ? versions[0] : undefined);
  if (
    version === undefined ||
    !exactVersion.test(version) ||
    (direct === null && !/^\{.*\}$/.test(declaration))
  ) {
    throw new Error(`Released Veryl dependency ${name} must use one exact version`);
  }
  return version;
}

function headRevision(name, declaration) {
  if (!/^\{.*\}$/.test(declaration)) {
    throw new Error(
      `Veryl HEAD dependency ${name} must use an inline git dependency`,
    );
  }
  if (/\b(?:version|path|branch|tag|registry|package)\s*=/.test(declaration)) {
    throw new Error(
      `Veryl HEAD dependency ${name} mixes incompatible source fields`,
    );
  }

  const repositories = fieldValues(declaration, "git");
  if (repositories.length !== 1 || repositories[0] !== verylRepository) {
    throw new Error(
      `Veryl HEAD dependency ${name} does not use the upstream repository`,
    );
  }

  const revisions = fieldValues(declaration, "rev");
  if (revisions.length !== 1 || !/^[0-9a-f]{40}$/.test(revisions[0])) {
    throw new Error(`Veryl HEAD dependency ${name} is not pinned to one full revision`);
  }
  return revisions[0];
}

export function detectVerylLane(manifest) {
  const dependencies = workspaceDependencies(manifest);
  const lanes = new Set();
  const versions = new Set();
  const revisions = new Set();

  for (const name of verylDependencyNames) {
    const declaration = dependencyDeclaration(dependencies, name);
    if (/\b(?:git|rev|branch|tag)\s*=/.test(declaration)) {
      lanes.add("head");
      revisions.add(headRevision(name, declaration));
    } else {
      lanes.add("stable");
      versions.add(stableVersion(name, declaration));
    }
  }

  if (lanes.size !== 1) {
    throw new Error("Veryl workspace dependencies mix released and HEAD declarations");
  }

  const [lane] = lanes;
  if (lane === "stable") {
    if (versions.size !== 1) {
      throw new Error(
        `Released Veryl dependencies use ${versions.size} different versions`,
      );
    }
    return { lane, version: [...versions][0] };
  }

  if (revisions.size !== 1) {
    throw new Error(`Veryl HEAD dependencies use ${revisions.size} different revisions`);
  }
  return { lane, revision: [...revisions][0] };
}

export function checkVerylLane(manifest, lane) {
  if (lane !== "stable" && lane !== "head") {
    throw new Error(`Expected Veryl lane stable or head, got ${lane}`);
  }

  const detected = detectVerylLane(manifest);
  if (detected.lane !== lane) {
    throw new Error(`Expected Veryl ${lane} lane, detected ${detected.lane}`);
  }
  return detected.lane === "head" ? detected.revision : undefined;
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const lane = process.argv[2] ?? "";
  const manifest = fs.readFileSync("Cargo.toml", "utf8");
  if (lane === "detect") {
    console.log(detectVerylLane(manifest).lane);
  } else {
    const revision = checkVerylLane(manifest, lane);
    console.log(
      lane === "head"
        ? `Validated Veryl HEAD lane at ${revision}`
        : "Validated released Veryl lane",
    );
  }
}
