import assert from "node:assert/strict";
import test from "node:test";
import { registryExists, validateRelease } from "./publish-aplite.mjs";

const env = { GITHUB_REF_TYPE: "tag", RELEASE_TAG: "aplite-v0.1.0" };
const release = { tagName: "aplite-v0.1.0", isDraft: false, isPrerelease: false };

test("accepts the matching independent stable release", () => {
  assert.doesNotThrow(() => validateRelease("0.1.0", env, release));
});

test("rejects branches, Celox tags, wrong versions and prerelease tags", () => {
  for (const invalid of [
    { ...env, GITHUB_REF_TYPE: "branch" },
    { ...env, RELEASE_TAG: "v0.1.0" },
    { ...env, RELEASE_TAG: "aplite-v0.2.0" },
    { ...env, RELEASE_TAG: "aplite-v0.1.0-rc.1" },
  ]) assert.throws(() => validateRelease("0.1.0", invalid, release));
});

test("rejects missing, mismatched, draft and prerelease GitHub releases", () => {
  for (const invalid of [{}, { ...release, tagName: "v0.1.0" },
    { ...release, isDraft: true }, { ...release, isPrerelease: true }]) {
    assert.throws(() => validateRelease("0.1.0", env, invalid));
  }
});

test("only HTTP 404 means an absent registry version", async () => {
  assert.equal(await registryExists("aplite/0.1.0", async () => ({ status: 200 })), true);
  assert.equal(await registryExists("aplite/0.1.0", async () => ({ status: 404 })), false);
  for (const status of [401, 403, 429, 500, 503]) {
    await assert.rejects(registryExists("aplite/0.1.0", async () => ({ status })), /lookup failed/);
  }
  await assert.rejects(registryExists("aplite", async () => { throw new Error("network"); }), /network/);
});

test("release dispatch uses the Aplite component output and exact tag", async (t) => {
  const { readFileSync, mkdtempSync, writeFileSync, rmSync } = await import("node:fs");
  const { tmpdir } = await import("node:os");
  const { join } = await import("node:path");
  const { spawnSync } = await import("node:child_process");
  const workflow = readFileSync(new URL("../.github/workflows/release-aplite.yml", import.meta.url), "utf8");
  const step = workflow.split("      - name: Queue Aplite publication\n")[1];
  assert.match(step, /if: steps.release.outputs\['crates\/aplite--release_created'\] == 'true'/);
  assert.match(step, /RELEASE_TAG: \$\{\{ steps.release.outputs\['crates\/aplite--tag_name'\] \}\}/);
  const script = step.split("        run: |\n")[1].replace(/^          /gm, "");
  const dir = mkdtempSync(join(tmpdir(), "aplite-dispatch-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  writeFileSync(join(dir, "gh"), '#!/bin/sh\nprintf "%s\\n" "$@"\n', { mode: 0o755 });
  for (const tag of ["aplite-v0.1.0", "v0.1.0", "master", "aplite-v0.1.0-rc.1", ""]) {
    const result = spawnSync("bash", ["-e", "-o", "pipefail", "-c", script], {
      encoding: "utf8", env: { ...process.env, PATH: `${dir}:${process.env.PATH}`,
        RELEASE_TAG: tag, GITHUB_REPOSITORY: "celox-sim/celox" },
    });
    if (tag === "aplite-v0.1.0") {
      assert.equal(result.status, 0, result.stderr);
      assert.equal(result.stdout, "workflow\nrun\npublish-aplite.yml\n--repo\ncelox-sim/celox\n--ref\naplite-v0.1.0\n");
    } else {
      assert.notEqual(result.status, 0);
      assert.equal(result.stdout, "");
    }
  }
});
