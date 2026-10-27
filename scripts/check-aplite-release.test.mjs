import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { checkApliteRelease } from "./check-aplite-release.mjs";

function fixture(t, { version = "0.1.0", recorded = {}, locked = version } = {}) {
  const root = mkdtempSync(join(tmpdir(), "aplite-version-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(join(root, "crates/aplite"), { recursive: true });
  writeFileSync(join(root, "crates/aplite/Cargo.toml"), `[package]\nname = "aplite"\nversion = "${version}"\n`);
  writeFileSync(join(root, ".release-please-aplite-manifest.json"), JSON.stringify(recorded));
  writeFileSync(join(root, "Cargo.lock"), `[[package]]\nname = "aplite"\nversion = "${locked}"\n\n[[package]]\nname = "celox"\nversion = "99.8.7"\n`);
  return root;
}

test("Aplite bootstrap is independent of the Celox version", (t) => {
  assert.equal(checkApliteRelease(fixture(t)), "0.1.0");
});
test("Aplite releases follow their own version and manifest", (t) => {
  assert.equal(checkApliteRelease(fixture(t, { version: "0.2.0", recorded: { "crates/aplite": "0.2.0" } })), "0.2.0");
});
test("reject stale lockfile and release manifest versions", (t) => {
  assert.throws(() => checkApliteRelease(fixture(t, { locked: "0.0.9" })), /Cargo.lock/);
  assert.throws(() => checkApliteRelease(fixture(t, { recorded: { "crates/aplite": "0.2.0" } })), /disagree/);
  assert.throws(() => checkApliteRelease(fixture(t, { recorded: { ".": "0.1.0" } })), /another component/);
});
test("release configurations separate tags, paths and lockfile updates", () => {
  const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");
  const aplite = JSON.parse(read("release-please-aplite-config.json"));
  const celox = JSON.parse(read("release-please-config.json"));
  assert.deepEqual(Object.keys(aplite.packages), ["crates/aplite"]);
  const pkg = aplite.packages["crates/aplite"];
  assert.equal(pkg.component, "aplite");
  assert.equal(pkg["release-type"], "rust");
  assert.equal(pkg["include-component-in-tag"], true);
  assert.deepEqual(pkg["extra-files"], [{ type: "toml", path: "Cargo.lock", jsonpath: '$.package[?(@.name.value == "aplite")].version' }]);
  assert.ok(celox.packages["."]["exclude-paths"].includes("crates/aplite"));
  const workflow = read(".github/workflows/release-aplite.yml");
  assert.match(workflow, /manifest-file: \.release-please-aplite-manifest.json/);
  assert.match(workflow, /config-file: release-please-aplite-config.json/);
  assert.match(workflow, /release-please--branches--master--components--aplite/);
  assert.doesNotMatch(workflow, /cargo publish|publish-crates.yml|napi-release/);
});
