import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const workflow = readFileSync(new URL("../.github/workflows/publish-cli.yml", import.meta.url), "utf8");
const manifest = readFileSync(new URL("../crates/celox/Cargo.toml", import.meta.url), "utf8");
const promotion = readFileSync(new URL("./promote-release-candidate.mjs", import.meta.url), "utf8");

function job(name) {
  const parts = workflow.split(/^  ([\w-]+):\n/m);
  const index = parts.indexOf(name);
  assert.ok(index >= 0, name);
  return parts[index + 1];
}

test("promotion dispatches the CLI publisher at the stable tag", () => {
  assert.match(promotion, /"workflow", "run", "publish-cli.yml", "--repo", repository, "--ref", tag/);
});

test("only a dispatched release run uploads; pull requests build and smoke-test", () => {
  assert.match(job("validate"), /if: github.event_name == 'workflow_dispatch'/);
  assert.match(job("publish"), /if: github.event_name == 'workflow_dispatch'/);
  assert.match(job("publish"), /needs: \[validate, build, qualify\]/);
  assert.match(job("publish"), /contents: write/);
  const build = job("build");
  assert.doesNotMatch(build, /contents: write|gh release/);
  assert.match(build, /ref: \$\{\{ needs.validate.outputs.source_sha \|\| github.sha \}\}/);
  assert.match(build, /cargo build --locked --release -p celox --bin celox --bin veryl-celox/);
  assert.match(build, /Smoke-test/);
});

test("cargo-binstall metadata names the archives the workflow attaches", () => {
  const build = job("build");
  const targets = [...build.matchAll(/- target: ([\w-]+)/g)].map(([, target]) => target);
  assert.ok(targets.length >= 5);
  // Archive celox-<target>.tar.gz (zip on Windows) with a celox-<target>/ directory.
  assert.match(build, /name="celox-\$TARGET"/);
  assert.match(build, /tar -C dist -czf "dist\/\$name.tar.gz" "\$name"/);
  assert.match(build, /7z a -tzip "\$name.zip" "\$name"/);
  assert.match(manifest, /pkg-url = "\{ repo \}\/releases\/download\/v\{ version \}\/celox-\{ target \}\{ archive-suffix \}"/);
  assert.match(manifest, /bin-dir = "celox-\{ target \}\/\{ bin \}\{ binary-ext \}"/);
  assert.match(manifest, /pkg-fmt = "tgz"/);
  for (const target of targets.filter((target) => target.includes("-windows-")))
    assert.match(manifest, new RegExp(`\\[package\\.metadata\\.binstall\\.overrides\\.${target}\\]\\npkg-fmt = "zip"`));
});
