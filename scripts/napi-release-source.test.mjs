import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

const workflow = readFileSync(
  new URL("../.github/workflows/ci-napi.yml", import.meta.url),
  "utf8",
);
const preflight = workflow
  .split("      - name: Validate source before checkout\n")[1]
  .split("      - uses: actions/checkout@")[0];
const script = preflight.split("        run: |\n")[1];

function validate(overrides = {}) {
  const directory = mkdtempSync(join(tmpdir(), "celox-napi-source-"));
  const output = join(directory, "output");
  writeFileSync(output, "");
  writeFileSync(join(directory, "gh"), '#!/bin/bash\nprintf "%s\\n" "$PUBLISHED_TAG"\n', {
    mode: 0o755,
  });
  try {
    const result = spawnSync("bash", ["-e", "-o", "pipefail", "-c", script], {
      env: {
        ...process.env,
        PATH: `${directory}:${process.env.PATH}`,
        GITHUB_OUTPUT: output,
        GITHUB_REF: "refs/heads/master",
        GITHUB_SHA: "1234567890abcdef1234567890abcdef12345678",
        GITHUB_REPOSITORY: "celox-sim/celox",
        NIGHTLY: "false",
        NIGHTLY_CHANNEL: "",
        NPM_TAG: "latest",
        RELEASE_TAG: "v0.8.2",
        PUBLISHED_TAG: "v0.8.2",
        ...overrides,
      },
      encoding: "utf8",
    });
    assert.ifError(result.error);
    return { ...result, output: readFileSync(output, "utf8") };
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

test("a published release selects the tag namespace, never a same-name branch", () => {
  const result = validate();
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.output, "ref=refs/tags/v0.8.2\n");
});

test("arbitrary revisions and non-stable tags cannot reach checkout", () => {
  for (const tag of ["", "master", "refs/pull/1/head", "v0.8.2-rc.1", "v00.8.2", "v0.8.2\nref=master"]) {
    const result = validate({ RELEASE_TAG: tag, PUBLISHED_TAG: tag });
    assert.notEqual(result.status, 0, tag);
    assert.equal(result.output, "", tag);
  }
});

test("a missing or non-public stable release cannot reach checkout", () => {
  const result = validate({ PUBLISHED_TAG: "" });
  assert.notEqual(result.status, 0);
  assert.equal(result.output, "");
});

test("nightlies select the event SHA only from their designated branch", () => {
  for (const [channel, branch] of [["stable", "master"], ["head", "develop"]]) {
    const overrides = { NIGHTLY: "true", NIGHTLY_CHANNEL: channel };
    const accepted = validate({ ...overrides, GITHUB_REF: `refs/heads/${branch}` });
    assert.equal(accepted.status, 0, accepted.stderr);
    assert.equal(accepted.output, "ref=1234567890abcdef1234567890abcdef12345678\n");
    const rejected = validate({ ...overrides, GITHUB_REF: "refs/heads/feature" });
    assert.notEqual(rejected.status, 0);
    assert.equal(rejected.output, "");
  }
});
