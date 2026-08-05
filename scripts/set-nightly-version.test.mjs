import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import { nightlyVersion, setNightlyVersion } from "./set-nightly-version.mjs";

const revision = "0123456789abcdef0123456789abcdef01234567";

test("builds a deterministic next-patch nightly version", () => {
  assert.equal(
    nightlyVersion("0.1.35", "head", "20260805123456", revision),
    "0.1.36-nightly.head.20260805123456.g0123456789ab",
  );
});

test("keeps stable and HEAD nightlies in distinct SemVer lines", () => {
  assert.equal(
    nightlyVersion("0.1.35", "stable", "20260805123456", revision),
    "0.1.36-nightly.stable.20260805123456.g0123456789ab",
  );
});

test("rejects unstable bases and ambiguous build inputs", () => {
  assert.throws(
    () => nightlyVersion("0.1.36-rc.1", "head", "20260805123456", revision),
    /stable SemVer base/,
  );
  assert.throws(
    () => nightlyVersion("0.1.35", "head", "2026-08-05", revision),
    /UTC timestamp/,
  );
  assert.throws(
    () => nightlyVersion("0.1.35", "head", "20260805123456", "main"),
    /full lowercase git revision/,
  );
  assert.throws(
    () => nightlyVersion("0.1.35", "edge", "20260805123456", revision),
    /nightly channel stable or head/,
  );
});

test("updates every published package version in lockstep", (context) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "celox-nightly-version-"));
  context.after(() => fs.rmSync(root, { recursive: true, force: true }));

  fs.mkdirSync(path.join(root, "packages/celox"), { recursive: true });
  fs.mkdirSync(path.join(root, "packages/vite-plugin"), { recursive: true });
  fs.mkdirSync(path.join(root, "crates/celox-napi"), { recursive: true });
  fs.writeFileSync(path.join(root, "VERSION"), "0.1.35\n");
  fs.writeFileSync(path.join(root, ".release-please-manifest.json"), '{".":"0.1.35"}\n');
  for (const packagePath of [
    "packages/celox/package.json",
    "packages/vite-plugin/package.json",
    "crates/celox-napi/package.json",
  ]) {
    fs.writeFileSync(path.join(root, packagePath), '{"name":"example","version":"0.1.35"}\n');
  }

  const version = setNightlyVersion(root, "head", "20260805123456", revision);
  assert.equal(version, "0.1.36-nightly.head.20260805123456.g0123456789ab");
  assert.equal(fs.readFileSync(path.join(root, "VERSION"), "utf8"), `${version}\n`);
  assert.equal(
    JSON.parse(fs.readFileSync(path.join(root, ".release-please-manifest.json"), "utf8"))["."],
    version,
  );
  for (const packagePath of [
    "packages/celox/package.json",
    "packages/vite-plugin/package.json",
    "crates/celox-napi/package.json",
  ]) {
    assert.equal(
      JSON.parse(fs.readFileSync(path.join(root, packagePath), "utf8")).version,
      version,
    );
  }
});
