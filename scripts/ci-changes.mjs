import { execFileSync } from "node:child_process";
import { appendFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

const ALL_AFFECTED = Object.freeze({
  docs: true,
  javascript: true,
  napi: true,
  napi_arm64: true,
  rust: true,
  scripts: true,
});

const NEUTRAL_FILES = new Set([
  ".gitignore",
  ".release-please-manifest.json",
  "AGENTS.md",
  "CHANGELOG.md",
  "CLAUDE.md",
  "LICENSE-APACHE",
  "LICENSE-MIT",
  "VERSION",
  "release-please-config.json",
  "renovate.json",
]);

const RELEASE_PLEASE_FILES = new Set([
  ".release-please-manifest.json",
  "CHANGELOG.md",
  "VERSION",
  "crates/celox-napi/package.json",
  "packages/celox/package.json",
  "packages/vite-plugin/package.json",
]);

function startsWithAny(path, prefixes) {
  return prefixes.some((prefix) => path.startsWith(prefix));
}

export function affectsHeliodorArm64(files) {
  // Keep PR scheduling deliberately narrow. Generic Rust and manifest changes
  // still exercise x86-64/Cranelift in PRs, while master and manual runs always
  // retain the ARM64 Linux boot coverage.
  return files
    .map((path) => path.replace(/^\.\//, ""))
    .some(
      (path) =>
        path === ".github/workflows/heliodor-bench.yml" ||
        startsWithAny(path, [
          ".github/actions/setup-rust/",
          "crates/celox-backend-arm64/",
          "crates/celox-backend-common/",
          "crates/celox/src/backend/native/",
          "scripts/ci-changes.",
        ]) ||
        [
          "crates/celox/src/backend.rs",
          "crates/celox-bench/src/bin/celox-heliodor.rs",
          "crates/celox-bench/src/bin/veryl-heliodor.rs",
          "scripts/run-heliodor-bench.sh",
          "scripts/setup-arm64-backend-dev.sh",
          "scripts/tests/run-heliodor-bench-gate.sh",
        ].includes(path),
    );
}

export function classifyFiles(files, { releasePlease = false } = {}) {
  const affected = {
    docs: false,
    javascript: false,
    napi: false,
    napi_arm64: false,
    rust: false,
    scripts: false,
  };

  const normalizedFiles = files.map((path) => path.replace(/^\.\//, ""));
  if (
    releasePlease &&
    normalizedFiles.length > 0 &&
    normalizedFiles.every((path) => RELEASE_PLEASE_FILES.has(path))
  ) {
    return affected;
  }

  for (const path of normalizedFiles) {
    if (
      path === ".github/workflows/ci.yml" ||
      startsWithAny(path, [".github/actions/", "scripts/ci-changes."])
    ) {
      return { ...ALL_AFFECTED };
    }

    if (
      startsWithAny(path, ["docs/", "adr/"]) ||
      path === "README.md"
    ) {
      affected.docs = true;
      continue;
    }

    if (
      startsWithAny(path, ["crates/celox-napi/"]) &&
      /\.(?:js|json|ts)$/.test(path)
    ) {
      affected.javascript = true;
      affected.napi = true;
      continue;
    }

    if (
      startsWithAny(path, ["crates/", "vendor/", ".cargo/"]) ||
      ["Cargo.lock", "Cargo.toml", "rust-toolchain.toml", ".gitmodules"].includes(
        path,
      )
    ) {
      affected.rust = true;
      affected.napi = true;
      affected.napi_arm64 = true;
      affected.javascript = true;
      continue;
    }

    if (
      startsWithAny(path, ["packages/", "examples/"]) ||
      [
        "biome.json",
        "package.json",
        "pnpm-lock.yaml",
        "pnpm-workspace.yaml",
        "typedoc.json",
      ].includes(path)
    ) {
      affected.javascript = true;
      affected.napi = true;
      if (startsWithAny(path, ["packages/"]) || path === "typedoc.json") {
        affected.docs = true;
      }
      continue;
    }

    if (startsWithAny(path, ["scripts/"])) {
      affected.scripts = true;
      continue;
    }

    if (
      NEUTRAL_FILES.has(path) ||
      startsWithAny(path, [".github/ISSUE_TEMPLATE/", ".github/workflows/"]) ||
      path === ".github/pull_request_template.md"
    ) {
      continue;
    }

    // Unknown paths fail open so a new source/configuration area cannot
    // accidentally bypass validation.
    return { ...ALL_AFFECTED };
  }

  return affected;
}

function changedFiles(base, head) {
  const sha = /^[0-9a-f]{40,64}$/i;
  if (!sha.test(base) || !sha.test(head) || /^0+$/.test(base)) {
    return null;
  }

  try {
    const output = execFileSync(
      "git",
      ["diff", "--name-only", "-z", base, head],
      { encoding: "utf8" },
    );
    return output.split("\0").filter(Boolean);
  } catch (error) {
    console.warn(`Unable to determine changed files: ${error.message}`);
    return null;
  }
}

function writeOutputs(affected) {
  const lines = Object.entries(affected)
    .map(([name, value]) => `${name}=${value}`)
    .join("\n");
  console.log(lines);

  if (process.env.GITHUB_OUTPUT) {
    appendFileSync(process.env.GITHUB_OUTPUT, `${lines}\n`);
  }
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  const files = changedFiles(process.argv[2] ?? "", process.argv[3] ?? "");
  const affected =
    files === null
      ? { ...ALL_AFFECTED, heliodor_arm64: true }
      : {
          ...classifyFiles(files, {
            releasePlease: process.env.RELEASE_PLEASE_PR === "true",
          }),
          heliodor_arm64: affectsHeliodorArm64(files),
        };
  writeOutputs(affected);
}
