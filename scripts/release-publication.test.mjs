import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import {
  existsSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

const workflow = readFileSync(
  new URL("../.github/workflows/release.yml", import.meta.url),
  "utf8",
);
const publication = workflow
  .split("      - name: Queue package publication for this release\n")[1]
  .split("\n  queue-release:")[0];
const script = publication
  .split("        run: |\n")[1]
  .split("\n")
  .map((line) => line.replace(/^          /, ""))
  .join("\n");

function dispatch({ tag = "v0.8.2", publishedTag = tag } = {}) {
  const directory = mkdtempSync(join(tmpdir(), "celox-release-publication-"));
  const callsPath = join(directory, "calls.jsonl");
  writeFileSync(
    join(directory, "gh"),
    `#!/usr/bin/env node
const fs = require("node:fs");
const args = process.argv.slice(2);
const input = args[0] === "api" ? fs.readFileSync(0, "utf8") : "";
fs.appendFileSync(process.env.CALLS_PATH, JSON.stringify({ args, input }) + "\\n");
if (args[0] === "release") console.log(process.env.PUBLISHED_TAG);
`,
    { mode: 0o755 },
  );
  try {
    const result = spawnSync("bash", ["-e", "-o", "pipefail", "-c", script], {
      cwd: directory,
      env: {
        ...process.env,
        PATH: `${directory}:${process.env.PATH}`,
        RELEASE_TAG: tag,
        PUBLISHED_TAG: publishedTag,
        CALLS_PATH: callsPath,
        GITHUB_REPOSITORY: "celox-sim/celox",
        // A batched merge push ends at a commit after the release tag.
        GITHUB_SHA: "259c946c7465644bb856f286295170453958ce78",
      },
      encoding: "utf8",
    });
    const calls = existsSync(callsPath)
      ? readFileSync(callsPath, "utf8").trim().split("\n").map(JSON.parse)
      : [];
    return { ...result, calls };
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

test("publication runs only for a newly created root release on any event", () => {
  assert.match(
    workflow,
    /id: release\n\s+uses: googleapis\/release-please-action@v5/,
  );
  assert.match(
    publication,
    /if: steps\.release\.outputs\.release_created == 'true'/,
  );
  assert.match(
    publication,
    /RELEASE_TAG: \$\{\{ steps\.release\.outputs\.tag_name \}\}/,
  );
  assert.doesNotMatch(publication, /github\.event_name/);
});

test("dispatches both publishers for the created tag after a batched merge", () => {
  const result = dispatch();
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.calls.length, 3);
  assert.deepEqual(result.calls[0].args.slice(0, 3), [
    "release",
    "view",
    "v0.8.2",
  ]);
  assert.deepEqual(result.calls[1].args, [
    "api",
    "--method",
    "POST",
    "repos/celox-sim/celox/dispatches",
    "--input",
    "-",
  ]);
  assert.deepEqual(JSON.parse(result.calls[1].input), {
    event_type: "napi-release",
    client_payload: { tag: "v0.8.2" },
  });
  assert.deepEqual(result.calls[2].args, [
    "workflow",
    "run",
    "publish-crates.yml",
    "--repo",
    "celox-sim/celox",
    "--ref",
    "v0.8.2",
  ]);
});

test("does not dispatch publishers when the stable GitHub release is missing", () => {
  const result = dispatch({ publishedTag: "" });
  assert.notEqual(result.status, 0);
  assert.equal(result.calls.length, 1);
  assert.match(result.stderr, /Published GitHub Release v0.8.2 was not found/);
});

test("rejects missing and non-stable release tags", () => {
  for (const tag of ["", "v0.8.2-rc.1", "master"]) {
    const result = dispatch({ tag });
    assert.notEqual(result.status, 0);
    assert.deepEqual(result.calls, []);
    assert.match(result.stderr, /Expected a v-prefixed stable release tag/);
  }
});
