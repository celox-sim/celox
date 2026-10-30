import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import test from "node:test";
import { fileURLToPath } from "node:url";

const build = fileURLToPath(new URL("../.github/actions/setup-verilator/build.sh", import.meta.url));
const revision = "ea338be98e1e838d3518809ce8899f85a009963c";

test("Verilator setup rejects floating versions and source refs before building", () => {
  for (const args of [["latest", revision, "/unused"], ["5.052", "master", "/unused"],
    ["5.052", revision, "/"], ["5.052", revision, "/unused", "0"]]) {
    const result = spawnSync("bash", [build, ...args], { encoding: "utf8" });
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /Invalid Verilator/);
  }
});

test("Verilator setup reuses verified installations and rejects stale or incomplete caches", () => {
  const root = mkdtempSync(join(tmpdir(), "verilator-install-"));
  try {
    mkdirSync(join(root, "bin"));
    mkdirSync(join(root, "share/verilator/include"), { recursive: true });
    const binary = join(root, "bin/verilator");
    const stamp = join(root, ".celox-verilator-source");
    const header = join(root, "share/verilator/include/verilated_vpi.h");
    writeFileSync(binary, '#!/bin/sh\ntest "$VERILATOR_ROOT" = "$EXPECTED_ROOT" || exit 9\necho "Verilator 5.052 fixture"\n', { mode: 0o755 });
    writeFileSync(stamp, revision + "\n");
    writeFileSync(header, "");
    const run = () => spawnSync("bash", [build, "5.052", revision, root], {
      encoding: "utf8", env: { ...process.env, EXPECTED_ROOT: join(root, "share/verilator") },
    });
    const valid = run();
    assert.equal(valid.status, 0, valid.stderr);
    assert.match(valid.stdout, /Verilator 5\.052/);
    writeFileSync(stamp, "0".repeat(40));
    const wrongSource = run();
    assert.notEqual(wrongSource.status, 0);
    assert.match(wrongSource.stderr, /source identity/);
    writeFileSync(stamp, revision);
    writeFileSync(binary, '#!/bin/sh\necho "Verilator 5.050 fixture"\n', { mode: 0o755 });
    assert.match(run().stderr, /Unexpected Verilator version/);
    writeFileSync(binary, '#!/bin/sh\necho "Verilator 5.052 fixture"\n', { mode: 0o755 });
    rmSync(header);
    assert.notEqual(run().status, 0);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
