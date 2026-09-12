import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { spawnSync } from "node:child_process";
import { matrix, mergeArtifacts, prepareSuiteTestbench } from "./heliodor-suite.mjs";

test("suite isolates all 72 backend runs and reserves time for large designs", () => {
  const jobs = matrix().include;
  assert.equal(jobs.length, 72);
  assert.equal(new Set(jobs.map(j => `${j.arch}/${j.test}/${j.runner}`)).size, 72);
  for (const job of jobs) {
    assert.ok(job.timeout_sec < 300 * 60);
    if (job.test.endsWith("4hart") || job.test.endsWith("8hart")) assert.ok(job.timeout_sec > 3600);
  }
  assert.deepEqual(matrix({ test: "test_soc_66_smp_linux_boot_4hart", runner: "celox", arch: "aarch64" }).include,
    jobs.filter(j => j.test === "test_soc_66_smp_linux_boot_4hart" && j.runner === "celox" && j.arch === "aarch64"));
  for (const field of ["test", "runner", "arch"]) assert.throws(() => matrix({ [field]: "invalid" }), /Unknown suite/);
});

test("publication requires one successful matching result from every backend", () => {
  const root = mkdtempSync(join(tmpdir(), "heliodor-suite-"));
  try {
    const files = [];
    for (const job of matrix().include) {
      const dir = join(root, `heliodor-suite-${job.arch}-${job.test}-${job.runner}`, "target/heliodor/results");
      mkdirSync(dir, { recursive: true });
      const file = join(dir, "results.tsv");
      const content = `runner\ttest\texit_status\tsemantic_status\n${job.runner}\t${job.test}\t0\tpass\n`;
      writeFileSync(file, content);
      files.push([file, content]);
    }
    const output = join(root, "suite");
    mergeArtifacts(root, output);
    for (const arch of ["x86_64", "aarch64"]) assert.equal(readFileSync(`${output}-${arch}.tsv`, "utf8").trim().split("\n").length, 37);
    const [file, content] = files[0];
    for (const invalid of [content.replace("\tpass", "\tfail"), content.replace("\t0\t", "\t124\t"), content.replace("veryl-cc-sync\t", "celox\t"), content + content.split("\n")[1] + "\n"]) {
      writeFileSync(file, invalid);
      assert.throws(() => mergeArtifacts(root, output));
    }
    rmSync(file);
    assert.throws(() => mergeArtifacts(root, output), /ENOENT/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("dispatch validation rejects profile/filter conflicts instead of succeeding without work", () => {
  const validate = (inputs) => spawnSync(process.execPath, ["scripts/heliodor-suite.mjs", "validate"], {
    encoding: "utf8",
    env: { ...process.env, ARM64_PROFILE: "", SUITE_TEST: "", SUITE_RUNNER: "", SUITE_ARCH: "", ...inputs },
  });
  for (const filter of [
    { SUITE_TEST: "test_soc_66_smp_linux_boot_4hart" },
    { SUITE_RUNNER: "celox" },
    { SUITE_ARCH: "aarch64" },
  ]) {
    const conflict = validate({ ...filter, ARM64_PROFILE: "true" });
    assert.notEqual(conflict.status, 0);
    assert.match(conflict.stderr, /arm64_profile cannot be combined/);
    assert.equal(validate(filter).status, 0);
    assert.equal(validate({ ...filter, ARM64_PROFILE: "false" }).status, 0);
  }
  assert.equal(validate({}).status, 0);
  assert.equal(validate({ ARM64_PROFILE: "true" }).status, 0);
  assert.notEqual(validate({ SUITE_ARCH: "invalid" }).status, 0);
});


test("N=8 suite budget changes only that wrapper and remains idempotent", () => {
  const prefix = "module another_test { for _i in 0..3000 {} }\n";
  const source = prefix + `module test_soc_smp_linux_boot_8hart {
        // early on success (the N=8 boot reaches shutdown at ~25M). Budget 30M.
        for _i in 0..3000 {
            clk.next(10000);
            if pass { break; }
        }
        $assert(pass, "must reach shutdown");
}`;
  const prepared = prepareSuiteTestbench(source);
  assert.ok(prepared.startsWith(prefix));
  assert.match(prepared, /for _i in 0\.\.10000/);
  assert.ok(prepared.includes('$assert(pass, "must reach shutdown")'));
  assert.equal(prepareSuiteTestbench(prepared), prepared);
  assert.throws(() => prepareSuiteTestbench(source.replace("0..3000 {\n", "0..4000 {\n")), /Unexpected/);
  assert.throws(() => prepareSuiteTestbench(source + source), /exactly one/);
  assert.throws(() => prepareSuiteTestbench(prefix), /exactly one/);
});
