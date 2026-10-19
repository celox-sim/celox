import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import test from "node:test";
import { revision } from "./run-common-cells.mjs";

test("upstream gate rejects wrong revisions, broken positives, and false negative controls", () => {
  const root = mkdtempSync(join(tmpdir(), "common-cells-test-"));
  try {
    const source = join(root, "source with spaces");
    const bin = join(root, "bin");
    mkdirSync(join(source, "src"), { recursive: true });
    mkdirSync(join(source, "test"));
    mkdirSync(bin);
    const original = "assign z_o   = a_i ^ a_ext[Width:1];";
    writeFileSync(join(source, "src/cc_binary_to_gray.sv"), original);
    writeFileSync(join(source, "src/cc_gray_to_binary.sv"), "");
    writeFileSync(join(source, "test/cc_graycode_tb.sv"), "");
    writeFileSync(join(bin, "git"), `#!/usr/bin/env node\nconsole.log(process.env.TEST_REVISION || ${JSON.stringify(revision)});\n`);
    const celox = join(bin, "celox");
    writeFileSync(celox, `#!/usr/bin/env node
const negative = process.argv.at(-1).endsWith("negative.toml");
const mode = process.env.TEST_MODE;
if (!negative) process.exit(mode === "positive-failure" ? 1 : 0);
if (mode === "false-pass") process.exit(0);
if (mode === "compile-error") { console.error("compilation failed"); process.exit(1); }
if (mode === "crash") { process.kill(process.pid, "SIGTERM"); }
console.error("assert failed"); process.exit(1);
`);
    chmodSync(join(bin, "git"), 0o755);
    chmodSync(celox, 0o755);
    for (const mode of ["success", "positive-failure", "false-pass", "compile-error", "crash", "wrong-revision"]) {
      const output = join(root, mode);
      const result = spawnSync(process.execPath, [resolve("scripts/run-common-cells.mjs"), source, celox, output], {
        encoding: "utf8",
        env: { ...process.env, PATH: `${bin}:${process.env.PATH}`, TEST_MODE: mode,
          TEST_REVISION: mode === "wrong-revision" ? "bad" : revision },
      });
      assert.equal(result.status, mode === "success" ? 0 : 1, `${mode}: ${result.stderr}`);
      if (mode === "success") {
        assert.equal(JSON.parse(readFileSync(join(output, "result.json"))).positive, "passed");
        assert.match(readFileSync(join(output, "graycode-negative.log"), "utf8"), /assert failed/);
      }
    }
    assert.equal(readFileSync(join(source, "src/cc_binary_to_gray.sv"), "utf8"), original);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
