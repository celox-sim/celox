import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { execFileSync, spawnSync } from "node:child_process";
import { matrix, mergeArtifacts, prepareSuiteTestbench, validateResults, runners } from "./heliodor-suite.mjs";

test("suite keeps all four backends in one job for each workload and architecture", () => {
  const jobs = matrix().include;
  assert.equal(jobs.length, 18);
  assert.equal(new Set(jobs.map(j => `${j.arch}/${j.test}`)).size, 18);
  for (const job of jobs) {
    assert.ok(job.timeout_sec + 30 * 60 <= 360 * 60);
    assert.deepEqual(job.runner.split(" "), runners);
    if (job.test.endsWith("4hart") || job.test.endsWith("8hart")) assert.ok(job.timeout_sec > 3600);
  }
  assert.deepEqual(matrix({ test: "test_soc_66_smp_linux_boot_4hart", runner: "celox", arch: "aarch64" }).include,
    jobs.filter(j => j.test === "test_soc_66_smp_linux_boot_4hart" && j.arch === "aarch64").map(j => ({ ...j, runner: "celox" })));
  for (const field of ["test", "runner", "arch"]) assert.throws(() => matrix({ [field]: "invalid" }), /Unknown suite/);
});

test("explicit backends share one job per workload and architecture in the requested order", () => {
  const options = { test: "test_soc_smp_linux_boot_8hart", arch: "x86_64", runner: "celox-tiered veryl-cc-tiered" };
  const jobs = matrix(options).include;
  assert.equal(jobs.length, 1);
  assert.equal(jobs[0].runner, options.runner);
  assert.equal(jobs[0].os, matrix({ ...options, runner: "celox-tiered" }).include[0].os);
  assert.equal(jobs[0].timeout_sec, 19800);
  assert.deepEqual(matrix({ ...options, runner: "  celox-tiered\tveryl-cc-tiered  " }).include, jobs);
  assert.equal(matrix({ runner: options.runner }).include.length, 18);
  assert.equal(matrix({ runner: "celox" }).include.every(job => job.runner === "celox"), true);
  assert.throws(() => matrix({ runner: "celox celox" }), /Duplicate/);
  assert.throws(() => matrix({ runner: "celox invalid" }), /Unknown suite runner/);
});

test("comparison results require every selected backend to complete the same workload", () => {
  const root = mkdtempSync(join(tmpdir(), "heliodor-comparison-"));
  try {
    const path = join(root, "results.tsv");
    const workload = "test_soc_smp_linux_boot_8hart";
    const selected = ["celox-tiered", "veryl-cc-tiered"];
    const header = "runner\ttest\texit_status\tsemantic_status\n";
    const rows = selected.map(runner => `${runner}\t${workload}\t0\tpass\n`);
    writeFileSync(path, header + rows.join(""));
    assert.equal(validateResults(path, workload, selected).length, 3);
    for (const invalid of [
      header + rows[0],
      header + rows[0] + rows[0],
      header + rows.join("").replace("\t0\t", "\t124\t"),
      header + rows.join("").replace("\tpass", "\ttick-limit"),
      header + rows.join("").replace(workload, "test_soc_linux_boot"),
      header + rows.join("").replace("veryl-cc-tiered", "celox"),
      header.replace("semantic_status", "other") + rows.join(""),
    ]) {
      writeFileSync(path, invalid);
      assert.throws(() => validateResults(path, workload, selected));
    }
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("publication requires one successful matching result from every backend", () => {
  const root = mkdtempSync(join(tmpdir(), "heliodor-suite-"));
  try {
    const files = [];
    for (const job of matrix().include) {
      const dir = join(root, `heliodor-suite-${job.arch}-${job.test}`, "target/heliodor/results");
      mkdirSync(dir, { recursive: true });
      const file = join(dir, "results.tsv");
      const content = "runner\ttest\texit_status\tsemantic_status\n" + runners.map(runner => `${runner}\t${job.test}\t0\tpass\n`).join("");
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
    { SUITE_RUNNER: "celox-tiered veryl-cc-tiered" },
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

test("publication rejects the former separate-host backend artifacts", () => {
  const root = mkdtempSync(join(tmpdir(), "heliodor-separate-hosts-"));
  try {
    for (const job of matrix().include) {
      for (const runner of runners) {
        const dir = join(root, `heliodor-suite-${job.arch}-${job.test}-${runner}`, "target/heliodor/results");
        mkdirSync(dir, { recursive: true });
        writeFileSync(join(dir, "results.tsv"), `runner\ttest\texit_status\tsemantic_status\n${runner}\t${job.test}\t0\tpass\n`);
      }
    }
    assert.throws(() => mergeArtifacts(root, join(root, "suite")), /ENOENT/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
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


test("disabling suite mode restores the tracked wrapper and preserves other edits", () => {
  const root = mkdtempSync(join(tmpdir(), "heliodor-suite-restore-"));
  try {
    const source = `module test_soc_smp_linux_boot_8hart {
        // early on success (the N=8 boot reaches shutdown at ~25M). Budget 30M.
        for _i in 0..3000 {
            clk.next(10000);
        }
        $assert(pass, "must reach shutdown");
}`;
    mkdirSync(join(root, "tb"));
    const file = join(root, "tb/test_soc_smp_linux_boot.veryl");
    writeFileSync(file, source);
    const git = (...args) => execFileSync("git", ["-C", root, ...args], { encoding: "utf8", stdio: "pipe" });
    git("init", "--quiet");
    git("add", ".");
    git("-c", "user.name=Suite test", "-c", "user.email=suite@example.invalid", "-c", "commit.gpgsign=false", "commit", "--quiet", "-m", "fixture");
    git("remote", "add", "origin", root);
    const prepareStock = () => spawnSync("bash", ["-c", "source scripts/run-heliodor-bench.sh; prepare"], {
      encoding: "utf8",
      env: { ...process.env, HELIODOR_DIR: root, HELIODOR_REF: "HEAD", HELIODOR_SUITE: "0",
        HELIODOR_RESULTS_DIR: join(root, "results"), HELIODOR_TOOLS_DIR: join(root, "tools") },
    });
    for (let repetition = 0; repetition < 2; repetition++) {
      writeFileSync(file, prepareSuiteTestbench(source));
      const result = prepareStock();
      assert.equal(result.status, 0, result.stderr);
      assert.equal(readFileSync(file, "utf8"), source);
    }
    const editedSuite = prepareSuiteTestbench(source) + "\n// local edit\n";
    writeFileSync(file, editedSuite);
    const rejected = prepareStock();
    assert.notEqual(rejected.status, 0);
    assert.match(rejected.stderr, /additional local edits/);
    assert.equal(readFileSync(file, "utf8"), editedSuite);
    const stockEdit = source + "\n// local edit\n";
    writeFileSync(file, stockEdit);
    assert.equal(prepareStock().status, 0);
    assert.equal(readFileSync(file, "utf8"), stockEdit);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
