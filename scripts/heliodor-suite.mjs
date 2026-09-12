#!/usr/bin/env node
import { readFileSync, writeFileSync } from "node:fs";
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
    ["test", test, workloads], ["runner", runner, runners], ["arch", arch, Object.keys(hosts)],
  ]) {
    if (value && !choices.includes(value)) throw new Error(`Unknown suite ${label}: ${value}`);
  }
  return { include: Object.entries(hosts).flatMap(([a, os]) =>
    workloads.flatMap(t => runners.map(r => ({
      arch: a, os, test: t, runner: r,
      timeout_sec: t.endsWith("8hart") ? 14400 : t.endsWith("4hart") ? 10800 : 3600,
    }))),
  ).filter(job => (!test || job.test === test) && (!runner || job.runner === runner) && (!arch || job.arch === arch)) };
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

// Require each expected backend artifact, not merely a count of TSV files.
// A partial manual rerun must never be accepted as a full nightly result.
export function mergeArtifacts(root, outputPrefix) {
  const output = new Map();
  let header;
  for (const job of matrix().include) {
    const name = `heliodor-suite-${job.arch}-${job.test}-${job.runner}`;
    const file = join(root, name, "target/heliodor/results/results.tsv");
    const lines = readFileSync(file, "utf8").trimEnd().split("\n");
    if (lines.length !== 2) throw new Error(`${name}: expected exactly one result`);
    header ??= lines[0];
    if (lines[0] !== header) throw new Error(`${name}: inconsistent TSV header`);
    const fields = header.split("\t");
    const row = Object.fromEntries(fields.map((field, i) => [field, lines[1].split("\t")[i]]));
    if (row.test !== job.test || row.runner !== job.runner || row.semantic_status !== "pass" || row.exit_status !== "0") {
      throw new Error(`${name}: missing, mismatched or unsuccessful result`);
    }
    if (!output.has(job.arch)) output.set(job.arch, []);
    output.get(job.arch).push(lines[1]);
  }
  for (const [arch, rows] of output) writeFileSync(`${outputPrefix}-${arch}.tsv`, [header, ...rows, ""].join("\n"));
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  if (process.argv[2] === "matrix" || process.argv[2] === "validate") {
    const result = matrix({ test: process.env.SUITE_TEST, runner: process.env.SUITE_RUNNER, arch: process.env.SUITE_ARCH, profile: process.env.ARM64_PROFILE === "true" });
    if (process.argv[2] === "matrix") console.log(JSON.stringify(result));
  } else if (process.argv[2] === "prepare" && process.argv.length === 4) {
    prepareSuite(process.argv[3]);
  } else if (process.argv[2] === "merge" && process.argv.length === 5) {
    mergeArtifacts(process.argv[3], process.argv[4]);
  } else {
    throw new Error("Usage: heliodor-suite.mjs validate | matrix | prepare <source-dir> | merge <artifact-dir> <output-prefix>");
  }
}
