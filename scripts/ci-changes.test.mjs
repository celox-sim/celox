import assert from "node:assert/strict";
import test from "node:test";

import { classifyFiles } from "./ci-changes.mjs";

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
  assert.deepEqual(classifyFiles(["docs/index.md", "adr/README.md"]), {
    ...none,
    docs: true,
  });
});

test("Rust changes exercise native builds and JavaScript bindings", () => {
  assert.deepEqual(
    classifyFiles(["crates/celox/src/lib.rs"]),
    { ...all, docs: false, scripts: false },
  );
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

test("release package versions skip Rust tests and ARM64 builds", () => {
  assert.deepEqual(
    classifyFiles([
      ".release-please-manifest.json",
      "CHANGELOG.md",
      "VERSION",
      "crates/celox-napi/package.json",
      "packages/celox/package.json",
      "packages/vite-plugin/package.json",
    ]),
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
