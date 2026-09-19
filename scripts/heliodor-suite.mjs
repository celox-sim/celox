#!/usr/bin/env node
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { execFileSync } from "node:child_process";

// Linux 7.1 SMP is quarantined: both hart counts stall in the pinned RTL;
// the two-hart cache-read deadlock also reproduces on Verilator. See docs.
export const workloads = [
  "test_soc_linux_boot",
  "test_soc_smp_linux_boot_2hart",
  "test_soc_smp_linux_boot_4hart",
  "test_soc_smp_linux_boot_8hart",
  "test_soc_66_linux_boot",
  "test_soc_66_smp_linux_boot_2hart",
  "test_soc_66_smp_linux_boot_4hart",
  "test_soc_71_linux_boot",
  "test_soc_71v_linux_boot",
];
export const runners = ["veryl-cc-sync", "celox", "celox-tiered", "veryl-cc-tiered"];
const hosts = { x86_64: "ubuntu-24.04", aarch64: "ubuntu-24.04-arm" };

export function matrix({ test = "", runner = "", arch = "", profile = false } = {}) {
  if (profile && (test || runner || arch)) {
    throw new Error("arm64_profile cannot be combined with suite_test, suite_runner, or suite_arch");
  }
  for (const [label, value, choices] of [
    ["test", test, workloads], ["arch", arch, Object.keys(hosts)],
  ]) {
    if (value && !choices.includes(value)) throw new Error(`Unknown suite ${label}: ${value}`);
  }
  const selected = runner.trim().split(/\s+/).filter(Boolean);
  for (const value of selected) {
    if (!runners.includes(value)) throw new Error(`Unknown suite runner: ${value}`);
  }
  if (new Set(selected).size !== selected.length) throw new Error("Duplicate suite runner");
  // The scheduling unit is a workload and architecture, never a backend.
  // Every backend being compared runs sequentially on that job's VM.
  const group = selected.length ? selected : runners;
  return { include: Object.entries(hosts).flatMap(([a, os]) =>
    workloads.map(t => ({
      arch: a, os, test: t, runner: group.join(" "),
      // ARM N=8 Celox tiering reached 41M cycles at the former five-hour
      // limit; allow the measured ~44.3M-cycle boot to finish with headroom.
      timeout_sec: t.endsWith("8hart") ? 19800 : t.endsWith("4hart") ? 10800 : 3600,
    })),
  ).filter(job => (!test || job.test === test) && (!arch || job.arch === arch)) };
}

// Accept only complete, successful samples from the selected backends. This
// also rejects a partial comparison when the job's shared time budget expires.
export function validateResults(file, test, expectedRunners) {
  const lines = readFileSync(file, "utf8").trimEnd().split("\n");
  const fields = lines[0].split("\t");
  for (const field of ["runner", "test", "semantic_status", "exit_status"]) {
    if (!fields.includes(field)) throw new Error(`${file}: missing ${field} column`);
  }
  if (lines.length !== expectedRunners.length + 1) throw new Error(`${file}: expected ${expectedRunners.length} results`);
  const remaining = new Set(expectedRunners);
  for (const line of lines.slice(1)) {
    const values = line.split("\t");
    const row = Object.fromEntries(fields.map((field, i) => [field, values[i]]));
    if (values.length !== fields.length || row.test !== test || !remaining.delete(row.runner)
        || row.semantic_status !== "pass" || row.exit_status !== "0") {
      throw new Error(`${file}: missing, mismatched or unsuccessful result`);
    }
  }
  if (remaining.size) throw new Error(`${file}: missing backend results`);
  return lines;
}

export const suiteRevision = "6285682fa0a514077da9d17fee385c7841160025";
export const suiteTestbench = "8hart-100m-v1";

// The pinned Veryl N=8 wrapper still budgets 30M cycles, while its Verilator
// wrapper allows 100M. Secondary-hart startup can already exceed 30M.
const originalBudget = `        // early on success (the N=8 boot reaches shutdown at ~25M). Budget 30M.`;
const suiteBudget = `        // early on success. Budget 100M, matching the Verilator N=8 wrapper.
        // Celox suite testbench: ${suiteTestbench}.`;
export function prepareSuiteTestbench(source) {
  const marker = "module test_soc_smp_linux_boot_8hart {";
  const parts = source.split(marker);
  if (parts.length !== 2) throw new Error("Expected exactly one N=8 testbench module");
  const [prefix, module] = parts;
  const originalLoop = "        for _i in 0..3000 {";
  const suiteLoop = "        for _i in 0..10000 {";
  if (module.includes(suiteBudget) && module.split(suiteLoop).length === 2 && !module.includes(originalLoop)) return source;
  if (!module.includes(originalBudget) || module.split(originalLoop).length !== 2 || module.includes(suiteLoop)) {
    throw new Error("Unexpected N=8 testbench budget; refusing to patch changed source");
  }
  return prefix + marker + module.replace(originalBudget, suiteBudget).replace(originalLoop, suiteLoop);
}

function prepareSuite(directory) {
  const head = execFileSync("git", ["-C", directory, "rev-parse", "HEAD"], { encoding: "utf8" }).trim();
  if (head !== suiteRevision) throw new Error(`Suite preparation requires ${suiteRevision}, found ${head}`);
  const path = join(directory, "tb/test_soc_smp_linux_boot.veryl");
  const source = readFileSync(path, "utf8");
  const prepared = prepareSuiteTestbench(source);
  if (prepared !== source) writeFileSync(path, prepared);
  console.log(`Heliodor suite testbench: ${suiteTestbench} (RTL ${head})`);
}

// Restore only our exact patch. Keep unrelated local edits intact, and reject
// an edited suite wrapper rather than silently benchmarking a mixed variant.
function restoreSuiteTestbench(directory) {
  const relative = "tb/test_soc_smp_linux_boot.veryl";
  const path = join(directory, relative);
  if (!existsSync(path)) return;
  const source = readFileSync(path, "utf8");
  if (!source.includes(`// Celox suite testbench: ${suiteTestbench}.`)) return;
  const original = execFileSync("git", ["-C", directory, "show", `HEAD:${relative}`], { encoding: "utf8" });
  if (prepareSuiteTestbench(original) !== source) {
    throw new Error("Suite testbench has additional local edits; refusing to overwrite them");
  }
  writeFileSync(path, original);
}

// Require one complete comparison artifact per workload and architecture.
// Never assemble a comparison from backend artifacts produced on separate VMs.
// A partial manual rerun must never be accepted as a full nightly result.
export function mergeArtifacts(root, outputPrefix) {
  const output = new Map();
  let header;
  for (const job of matrix().include) {
    const name = `heliodor-suite-${job.arch}-${job.test}`;
    const file = join(root, name, "target/heliodor/results/results.tsv");
    const lines = validateResults(file, job.test, runners);
    header ??= lines[0];
    if (lines[0] !== header) throw new Error(`${name}: inconsistent TSV header`);
    if (!output.has(job.arch)) output.set(job.arch, []);
    output.get(job.arch).push(...lines.slice(1));
  }
  for (const [arch, rows] of output) writeFileSync(`${outputPrefix}-${arch}.tsv`, [header, ...rows, ""].join("\n"));
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  if (process.argv[2] === "matrix" || process.argv[2] === "validate") {
    const result = matrix({ test: process.env.SUITE_TEST, runner: process.env.SUITE_RUNNER, arch: process.env.SUITE_ARCH, profile: process.env.ARM64_PROFILE === "true" });
    if (process.argv[2] === "matrix") console.log(JSON.stringify(result));
  } else if (process.argv[2] === "prepare" && process.argv.length === 4) {
    prepareSuite(process.argv[3]);
  } else if (process.argv[2] === "restore" && process.argv.length === 4) {
    restoreSuiteTestbench(process.argv[3]);
  } else if (process.argv[2] === "merge" && process.argv.length === 5) {
    mergeArtifacts(process.argv[3], process.argv[4]);
  } else if (process.argv[2] === "validate-results" && process.argv.length === 6) {
    validateResults(process.argv[3], process.argv[4], process.argv[5].trim().split(/\s+/));
  } else {
    throw new Error("Usage: heliodor-suite.mjs validate | matrix | prepare <source-dir> | restore <source-dir> | merge <artifact-dir> <output-prefix> | validate-results <tsv> <test> <runners>");
  }
}
