import { execFileSync } from "node:child_process";
import { appendFileSync } from "node:fs";
import { fileURLToPath, pathToFileURL } from "node:url";

import { readWorkspace, rustTestScope } from "./ci-rust-scope.mjs";

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

// Version-only release PRs defer product validation to the queue draft.
const RELEASE_PLEASE_FILES = new Set([
  ".release-please-manifest.json",
  "CHANGELOG.md",
  "Cargo.lock",
  "Cargo.toml",
  "VERSION",
  "crates/celox-napi/package.json",
  "packages/celox/package.json",
  "packages/vite-plugin/package.json",
]);

// These crates are consumers or test infrastructure, not runtime/build
// dependencies of celox-napi. ci-changes.test.mjs checks the workspace graph
// so adding a dependency cannot silently make this exclusion unsafe.
const RUST_ONLY_CRATES = new Set([
  "celox-test-suite",
  "celox-bench",
  "celox-bench-sv",
  "celox-vpi",
  "celox-wasm",
  "lydite",
  "lydite-celox",
  "lydite-ir",
  "lydite-solver",
  "lydite-syntax",
  "lydite-verify",
]);

function isRustOnlyPath(path) {
  if (/(?:^|\/)Cargo\.(?:toml|lock)$/.test(path)) return false;
  if (startsWithAny(path, ["lydite/", "conformance/"])) return true;
  const crate = /^crates\/([^/]+)\/(.+)$/.exec(path);
  if (!crate) return false;
  return (
    RUST_ONLY_CRATES.has(crate[1]) ||
    /^(?:tests|benches|examples)\//.test(crate[2])
  );
}

function startsWithAny(path, prefixes) {
  return prefixes.some((prefix) => path.startsWith(prefix));
}

export function affectsHeliodorArm64(files) {
  // Keep PR scheduling deliberately narrow. The PR job exercises only the
  // native ARM64 runner; Veryl-CC and Cranelift run in the nightly workflow.
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
          "crates/celox-vpi/src/bin/celox/",
          "scripts/ci-changes.",
        ]) ||
        [
          "crates/celox/src/backend.rs",
          "crates/celox-vpi/src/bin/celox.rs",
          "scripts/run-heliodor-bench.sh",
          "scripts/heliodor-revision",
          "scripts/setup-arm64-backend-dev.sh",
          "scripts/tests/run-heliodor-bench-gate.sh",
        ].includes(path),
    );
}

export function cutsRelease(files) {
  return files.some(
    (path) => path.replace(/^\.\//, "") === ".release-please-manifest.json",
  );
}

// A Release Please diff that only bumps versions and changelogs.
export function isVersionOnlyRelease(files) {
  const normalized = files.map((path) => path.replace(/^\.\//, ""));
  return (
    cutsRelease(normalized) &&
    normalized.every((path) => RELEASE_PLEASE_FILES.has(path))
  );
}

// On master, only the release pull request changes the release manifest, so a
// PR or legacy merge group carrying it gets full validation before merging
// instead of the change-based subset. Syncing master into develop carries the
// same change without releasing anything. An undeterminable diff fails open.
export function isReleaseMergeGroup({ event, baseRef, files }) {
  return (
    ["merge_group", "pull_request"].includes(event) &&
    baseRef === "refs/heads/master" &&
    (files === null || cutsRelease(files))
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
      startsWithAny(path, ["docs/"]) ||
      path === "README.md" ||
      path === "CONTRIBUTING.md"
    ) {
      affected.docs = true;
      continue;
    }

    if (isRustOnlyPath(path)) {
      affected.rust = true;
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
      [
        ".config/nextest.toml",
        ".gitmodules",
        "Cargo.lock",
        "Cargo.toml",
        "rust-toolchain.toml",
      ].includes(path)
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

    // Repository script tests also validate workflow files and the Updatecli
    // policy, so these run in the required lint job instead of path-filtered
    // workflows that cannot gate merges.
    if (
      path === ".mergify.yml" ||
      startsWithAny(path, ["scripts/", ".updatecli/", ".github/workflows/"])
    ) {
      affected.scripts = true;
      continue;
    }

    if (
      NEUTRAL_FILES.has(path) ||
      startsWithAny(path, [".github/ISSUE_TEMPLATE/"]) ||
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
      // Diff from the merge base: a PR's base.sha is the current base tip.
      ["diff", "--name-only", "-z", `${base}...${head}`],
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
  const scheduledFull = process.env.FULL_VALIDATION === "true";
  const files = scheduledFull
    ? null
    : changedFiles(process.argv[2] ?? "", process.argv[3] ?? "");
  const queue = process.env.QUEUE_VALIDATION === "true" ||
    (process.env.QUEUE_VALIDATION !== "false" && process.env.GITHUB_EVENT_NAME !== "pull_request");
  const release = queue && isReleaseMergeGroup({
    event: process.env.GITHUB_EVENT_NAME,
    baseRef: process.env.MERGE_GROUP_BASE_REF,
    files,
  });
  const affected =
    files === null || release
      ? { ...ALL_AFFECTED, heliodor_arm64: true }
      : {
          ...classifyFiles(files, { releasePlease: !queue && process.env.RELEASE_PLEASE_PR === "true" }),
          heliodor_arm64: affectsHeliodorArm64(files),
        };
  writeOutputs({
    queue,
    ...affected,
    release,
    ...rustScopeOutputs(
      process.env.GITHUB_EVENT_NAME === "pull_request" &&
        !queue &&
        !release &&
        affected.rust
        ? files
        : null,
    ),
  });
}

// Lydite core tests run independently; bridge CI is disabled.
export function celoxRustTestScope(files, packages) {
  return rustTestScope(
    files.filter((path) => !/^(?:\.\/)?(?:lydite\/|crates\/lydite(?:-|\/))/.test(path)),
    packages.filter((pkg) => !/^lydite(?:-|$)/.test(pkg.name)),
  );
}

// Queue drafts, merge groups, releases and daily runs keep the Celox
// workspace. Lydite tests run independently in lydite-checks.yml.
export function rustScopeOutputs(files) {
  let scope = { full: true };
  let checks;
  if (files !== null) {
    try {
      const root = fileURLToPath(new URL("../", import.meta.url));
      const packages = readWorkspace(root);
      scope = celoxRustTestScope(files, packages);
      // Lydite owns its tests, but lint and API documentation still live here.
      checks = rustTestScope(
        files.filter((path) => !/^(?:\.\/)?crates\/lydite-celox\//.test(path)),
        packages.filter((pkg) => pkg.name !== "lydite-celox"),
      );
    } catch (error) {
      console.warn(`Unable to scope the Rust tests: ${error.message}`);
    }
  }
  if (scope.full || !checks || checks.full) {
    return {
      rust_full: true,
      rust_packages: "",
      rust_targets: "",
      rust_builds: "",
      rust_lint_builds: "",
      rust_lint_packages: "",
      rust_lint_features: "",
      rust_lint_targets: "",
      rust_doc_libraries: "",
      rust_sv_tests: true,
      rust_vpi_e2e: true,
      rust_features: "",
      rust_filter: "",
      rust_libraries: "",
      rust_library_features: "",
    };
  }
  return {
    rust_full: false,
    rust_vpi_e2e: scope.builds.some((build) => build.packages.includes("celox-vpi") &&
      (build.targets.length === 0 || build.targets.includes("cocotb_e2e"))),
    rust_packages: scope.packages.join(" "),
    rust_targets: scope.targets.join(" "),
    rust_builds: JSON.stringify(scope.builds),
    rust_lint_builds: JSON.stringify(checks.builds),
    rust_lint_packages: checks.packages.join(" "),
    rust_lint_features: checks.features.join(","),
    rust_lint_targets: checks.targets.join(" "),
    rust_doc_libraries: checks.libraries.join(" "),
    // Test-only and consumer changes cannot change sv-tests results.
    rust_sv_tests: scope.libraries.includes("celox") || files.some((path) =>
      /^(?:\.\/)?(?:conformance\/sv-tests\/|crates\/celox\/examples\/sv_tests\.rs$)/.test(path)),
    rust_features: scope.features.join(","),
    rust_filter: scope.filter,
    rust_libraries: scope.libraries.join(" "),
    rust_library_features: scope.libraryFeatures.join(","),
  };
}
