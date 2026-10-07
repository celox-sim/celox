import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { dirname, relative } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { affectsHeliodorArm64, classifyFiles } from "./ci-changes.mjs";

const none = {
  docs: false,
  javascript: false,
  napi: false,
  napi_arm64: false,
  rust: false,
  scripts: false,
};

const all = {
  docs: true,
  javascript: true,
  napi: true,
  napi_arm64: true,
  rust: true,
  scripts: true,
};

test("documentation changes only build documentation", () => {
  assert.deepEqual(
    classifyFiles(["docs/index.md", "adr/README.md", "CONTRIBUTING.md"]),
    {
      ...none,
      docs: true,
    },
  );
});

test("Rust test infrastructure skips bindings while retaining Rust checks", () => {
  for (const path of [
    "crates/celox-test-suite/src/veryl/cases/basic.vtest",
    "crates/celox-test-suite/src/verilator.rs",
    "crates/celox/tests/test_utils/mod.rs",
    "crates/celox/tests/fixtures/design.veryl",
    "crates/celox-napi/tests/fixture.json",
    "crates/celox/benches/simulation.rs",
    "crates/celox-bench/src/bin/celox-heliodor.rs",
    "crates/celox-bench-sv/src/main.rs",
    "crates/celox-vpi/src/lib.rs",
    "crates/celox-wasm/src/lib.rs",
    "crates/lydite/src/lib.rs",
    "crates/lydite-ir/src/lib.rs",
    "crates/lydite-solver/src/lib.rs",
    "crates/lydite-syntax/src/lib.rs",
    "crates/lydite-verify/src/lib.rs",
    "crates/lydite-celox/src/lib.rs",
    "lydite/conformance/veryl-proof/test_proof.py",
  ]) {
    assert.deepEqual(classifyFiles([path]), { ...none, rust: true }, path);
  }
});

test("manifests and mixed runtime changes retain binding validation", () => {
  for (const path of [
    "Cargo.toml",
    "Cargo.lock",
    "crates/celox-test-suite/Cargo.toml",
    "crates/lydite-ir/Cargo.toml",
    "crates/celox/tests/fixtures/custom/Cargo.toml",
    "crates/celox/tests/fixtures/custom/Cargo.lock",
    "lydite/conformance/veryl/Cargo.toml",
    "crates/celox/src/tests.rs",
    "crates/new-crate/src/lib.rs",
  ]) {
    assert.deepEqual(
      classifyFiles([path]),
      path.startsWith("lydite/")
        ? all
        : {
            ...all,
            docs: false,
            scripts: false,
          },
      path,
    );
  }
  assert.deepEqual(
    classifyFiles([
      "crates/celox/tests/counter.rs",
      "crates/celox-runtime/src/lib.rs",
    ]),
    { ...all, docs: false, scripts: false },
  );
  assert.deepEqual(
    classifyFiles([
      "crates/celox-test-suite/src/lib.rs",
      "packages/celox/src/index.ts",
    ]),
    { ...all, napi_arm64: false, scripts: false },
  );
});

test("full validation overrides even an empty valid diff", () => {
  const root = fileURLToPath(new URL("../", import.meta.url));
  const head = execFileSync("git", ["rev-parse", "HEAD"], {
    cwd: root,
    encoding: "utf8",
  }).trim();
  const output = execFileSync(
    process.execPath,
    ["scripts/ci-changes.mjs", head, head],
    {
      cwd: root,
      env: { ...process.env, FULL_VALIDATION: "true", GITHUB_OUTPUT: "" },
      encoding: "utf8",
    },
  );
  assert.deepEqual(
    Object.fromEntries(
      output
        .trim()
        .split("\n")
        .map((line) => {
          const [name, value] = line.split("=");
          return [name, value === "true"];
        }),
    ),
    { ...all, heliodor_arm64: true },
  );
});

test("Rust-only exclusions do not contain NAPI runtime or build dependencies", () => {
  const root = fileURLToPath(new URL("../", import.meta.url));
  const metadata = JSON.parse(
    execFileSync(
      "cargo",
      ["metadata", "--no-deps", "--locked", "--format-version", "1"],
      { cwd: root, encoding: "utf8" },
    ),
  );
  const packages = new Map(metadata.packages.map((pkg) => [pkg.name, pkg]));
  const visited = new Set();
  function visit(name) {
    if (visited.has(name)) return;
    visited.add(name);
    const pkg = packages.get(name);
    const cratePath = relative(root, dirname(pkg.manifest_path)).replaceAll(
      "\\",
      "/",
    );
    assert.equal(classifyFiles([`${cratePath}/src/lib.rs`]).napi, true, name);
    for (const target of pkg.targets) {
      if (
        target.kind.every(
          (kind) => kind === "test" || kind === "bench" || kind === "example",
        )
      )
        continue;
      const targetPath = relative(root, target.src_path).replaceAll("\\", "/");
      assert.equal(classifyFiles([targetPath]).napi, true, targetPath);
    }
    // Include optional and target-specific edges; only dev dependencies are
    // excluded. New runtime edges must invalidate the skip rule.
    for (const dep of pkg.dependencies) {
      if (dep.kind !== "dev" && packages.has(dep.name)) visit(dep.name);
    }
  }
  visit("celox-napi");
});

test("Rust changes exercise native builds and JavaScript bindings", () => {
  assert.deepEqual(classifyFiles(["crates/celox/src/lib.rs"]), {
    ...all,
    docs: false,
    scripts: false,
  });
});

test("test runner configuration changes exercise Rust", () => {
  assert.deepEqual(classifyFiles([".config/nextest.toml"]), {
    ...all,
    docs: false,
    scripts: false,
  });
});

test("JavaScript changes skip Rust tests and the ARM64 native build", () => {
  assert.deepEqual(classifyFiles(["packages/celox/src/index.ts"]), {
    ...none,
    docs: true,
    javascript: true,
    napi: true,
  });
});

test("release and repository metadata do not run product tests", () => {
  assert.deepEqual(
    classifyFiles(["CHANGELOG.md", "VERSION", ".release-please-manifest.json"]),
    none,
  );
});

const releaseFiles = [
  ".release-please-manifest.json",
  "CHANGELOG.md",
  "VERSION",
  "crates/celox-napi/package.json",
  "packages/celox/package.json",
  "packages/vite-plugin/package.json",
];

test("ordinary package version changes exercise JavaScript and NAPI", () => {
  assert.deepEqual(classifyFiles(releaseFiles), {
    ...none,
    docs: true,
    javascript: true,
    napi: true,
  });
});

test("Release Please version updates skip product validation", () => {
  assert.deepEqual(classifyFiles(releaseFiles, { releasePlease: true }), none);
});

test("Release Please source changes still exercise affected products", () => {
  assert.deepEqual(
    classifyFiles([...releaseFiles, "packages/celox/src/index.ts"], {
      releasePlease: true,
    }),
    {
      ...none,
      docs: true,
      javascript: true,
      napi: true,
    },
  );
});

test("CI classifier changes exercise every path", () => {
  assert.deepEqual(classifyFiles(["scripts/ci-changes.mjs"]), all);
});

test("repository script changes only run script tests", () => {
  assert.deepEqual(classifyFiles(["scripts/check-pr-title.mjs"]), {
    ...none,
    scripts: true,
  });
});

test("unknown paths fail open", () => {
  assert.deepEqual(classifyFiles(["new-source-area/input.xyz"]), all);
});

test("ARM64 Heliodor changes include backend and harness integration", () => {
  for (const path of [
    "crates/celox-backend-arm64/src/lib.rs",
    "crates/celox-backend-common/src/lib.rs",
    "crates/celox/src/backend/native/backend.rs",
    "crates/celox/src/backend.rs",
    "crates/celox-bench/src/bin/celox-heliodor.rs",
    "scripts/run-heliodor-bench.sh",
    "scripts/heliodor-revision",
    ".github/actions/setup-rust/action.yml",
    ".github/workflows/heliodor-bench.yml",
    "scripts/ci-changes.mjs",
  ]) {
    assert.equal(affectsHeliodorArm64([path]), true, path);
  }
});

test("generic Rust changes do not schedule ARM64 Heliodor on pull requests", () => {
  assert.equal(
    affectsHeliodorArm64([
      "Cargo.lock",
      "Cargo.toml",
      "crates/celox/Cargo.toml",
      "crates/celox/src/simulator.rs",
      "crates/celox-backend-x86/src/lib.rs",
      "crates/celox-bench/src/bin/veryl-heliodor.rs",
      ".github/workflows/codspeed.yml",
    ]),
    false,
  );
});
