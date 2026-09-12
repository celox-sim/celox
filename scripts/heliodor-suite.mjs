#!/usr/bin/env node
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

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
  } else if (process.argv[2] === "merge" && process.argv.length === 5) {
    mergeArtifacts(process.argv[3], process.argv[4]);
  } else {
    throw new Error("Usage: heliodor-suite.mjs validate | matrix | merge <artifact-dir> <output-prefix>");
  }
}
