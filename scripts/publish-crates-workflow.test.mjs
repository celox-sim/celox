import assert from "node:assert/strict";
import { readFileSync, mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { execFileSync, spawnSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

const workflow = readFileSync(new URL("../.github/workflows/publish-crates.yml", import.meta.url), "utf8");

test("workflow repair publishes only the resolved and qualified source tag", () => {
  assert.match(workflow, /ref: \$\{\{ inputs.source_tag && format\('refs\/tags\/\{0\}', inputs.source_tag\) \|\| github.sha \}\}/);
  assert.match(workflow, /test "\$\(git rev-parse HEAD\)" = "\$\(git rev-parse "refs\/tags\/\$SOURCE_TAG\^\{commit\}"\)"/);
  assert.match(workflow, /source_sha: \$\{\{ steps.source.outputs.sha \}\}/);
  assert.equal((workflow.match(/ref: \$\{\{ needs.validate.outputs.source_sha \}\}/g) || []).length, 3);
  assert.match(workflow, /CANDIDATE_SHA: \$\{\{ needs.validate.outputs.source_sha \}\}/);
  assert.match(workflow, /\$GITHUB_REF_TYPE" != "tag"/);
});

test("the public frontend test enables its target's required features", () => {
  const manifest = readFileSync(new URL("../crates/celox/Cargo.toml", import.meta.url), "utf8");
  const target = manifest.split("[[test]]").find(block => /name = "frontend_sdk"/.test(block));
  const features = [...target.match(/required-features = \[([^\]]*)\]/)[1].matchAll(/"([^"]+)"/g)].map(match => match[1]);
  const command = workflow.split("\n").find(line => /cargo test .*--test frontend_sdk/.test(line));
  const enabled = command.match(/--features ([\w,-]+)/)[1].split(",");
  for (const feature of features) assert.ok(enabled.includes(feature), feature);
});

test("actual tag validation pins source and rejects mismatches during workflow repair", () => {
  const cwd = mkdtempSync(join(tmpdir(), "celox-publish-source-"));
  const git = (...args) => execFileSync("git", args, { cwd, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }).trim();
  try {
    git("init");
    git("config", "user.name", "Test");
    git("config", "user.email", "test@example.invalid");
    writeFileSync(join(cwd, "VERSION"), "0.12.0\n");
    git("add", ".");
    git("commit", "-m", "stable source");
    const stable = git("rev-parse", "HEAD");
    git("tag", "v0.12.0");
    writeFileSync(join(cwd, "repair"), "workflow repair");
    git("add", ".");
    git("commit", "-m", "repair workflow");
    git("tag", "v0.12.0-recovery.1");
    const script = workflow.split("      - name: Check release tag and version")[1]
      .split("        run: |\n")[1].split("\n      - name:")[0];
    const output = join(cwd, "output");
    const base = { ...process.env, GITHUB_OUTPUT: output, GITHUB_REF_TYPE: "tag", RELEASE_TAG: "v0.12.0", SOURCE_TAG: "v0.12.0" };
    const check = overrides => spawnSync("bash", ["-e", "-c", script], { cwd, env: { ...base, ...overrides }, encoding: "utf8" });
    // The repair commit cannot masquerade as the stable tag's source.
    assert.notEqual(check({}).status, 0);
    git("checkout", "--detach", "v0.12.0");
    assert.equal(check({}).status, 0);
    assert.match(readFileSync(output, "utf8"), new RegExp("sha=" + stable));
    for (const overrides of [
      { GITHUB_REF_TYPE: "branch" },
      { RELEASE_TAG: "master" },
      { RELEASE_TAG: "v0.13.0" },
      { SOURCE_TAG: "master" },
      { SOURCE_TAG: "v0.12.0-recovery.0" },
      { SOURCE_TAG: "v0.12.0-recovery.1" },
    ]) assert.notEqual(check(overrides).status, 0, JSON.stringify(overrides));
    // Source recovery still requires a matching descendant tag checkout.
    git("checkout", "--detach", "v0.12.0-recovery.1");
    assert.equal(check({ SOURCE_TAG: "v0.12.0-recovery.1" }).status, 0);
  } finally {
    rmSync(cwd, { recursive: true, force: true });
  }
});
