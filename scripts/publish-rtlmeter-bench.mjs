#!/usr/bin/env node
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { hostCpu, publishResult } from "./publish-heliodor-bench.mjs";

const manifest = JSON.parse(readFileSync(new URL("../benchmarks/rtlmeter/manifest.json", import.meta.url)));
const locked = JSON.parse(readFileSync(new URL("../flake.lock", import.meta.url))).nodes.xezim;
const digest = value => /^[a-f0-9]{64}$/.test(value ?? "");
const positive = value => Number.isSafeInteger(value) && value > 0;

export function rtlmeterMetrics(report, commit) {
  assert.ok(manifest.cases[report.case], "Unknown RTLMeter case");
  assert.equal(report.schemaVersion, 1);
  assert.equal(report.contract, manifest.contract);
  assert.equal(report.status, "completed");
  assert.equal(report.rtlmeterRevision, manifest.revision);
  assert.equal(report.celoxCommit, commit);
  assert.equal(report.dirty, false, "Refuse measurements of a dirty compiler checkout");
  assert.equal(report.requestedSamples, manifest.samples);
  assert.equal(report.maxCycles, manifest.cases[report.case].maxCycles);
  assert.equal(report.host?.architecture, "x86_64");
  assert.ok(report.host.cpu && Number.isInteger(report.host.affinityCpu));
  assert.equal(report.xezimRelease, locked.original.ref);
  assert.equal(report.xezimRevision, locked.locked.rev);
  assert.match(report.versions?.verilator ?? "", /^Verilator 5\.052\b/);
  assert.equal(report.versions?.xezim?.split("\n")[0], `xezim version ${locked.original.ref.replace(/^v/, "")}`);
  for (const engine of ["celox", "verilator", "xezim"]) {
    assert.ok(report.versions?.[engine] && digest(report.binarySha256?.[engine]));
  }
  assert.deepEqual(report.configuration, {
    celox: "native O2; threads=1; cold source-to-finish",
    verilator: "--binary --timing --assert; -O3; C++ -O3; -j1; fresh Mdir",
    xezim: "release default VM; persistent caches disabled; full four-state",
  });
  assert.ok(digest(report.sourceSha256));
  assert.ok(Object.keys(report.inputSha256 ?? {}).length > 100);
  assert.ok(Object.values(report.inputSha256).every(digest));
  assert.equal(report.sourceSha256, createHash("sha256").update(JSON.stringify(
    Object.fromEntries(Object.entries(report.inputSha256).sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0)),
  )).digest("hex"));
  const design = manifest.cases[report.case].design;
  for (const path of [`designs/${design}/descriptor.yaml`, `designs/${design}/tests/hello/case.pat`,
    "rtl/__rtlmeter_utils.sv", "rtl/__rtlmeter_top_include.vh"]) {
    assert.ok(digest(report.inputSha256[path]), `Missing input identity: ${path}`);
  }
  assert.equal(report.results?.length, 6);
  const expected = new Set(["two-state", "four-state"].flatMap(state =>
    ["celox", "verilator", "xezim"].map(engine => `${engine}/${state}`)));
  for (const row of report.results) {
    assert.ok(expected.delete(`${row.engine}/${row.stateModel}`), "Duplicate or unexpected configuration");
    assert.equal(row.experimental, row.engine === "verilator" && row.stateModel === "four-state");
    assert.ok(["passed", "unsupported", "failed", "mismatch"].includes(row.status));
    assert.ok(Array.isArray(row.samples));
    if (row.engine === "xezim" && row.stateModel === "two-state") {
      assert.equal(row.status, "unsupported");
      assert.equal(row.samples.length, 0);
    }
    if (row.status !== "passed") {
      assert.ok(typeof row.reason === "string" && row.reason.length > 0);
      continue;
    }
    assert.equal(row.probe, "passed");
    assert.equal(row.negativeControl, "passed");
    assert.equal(row.samples.length, manifest.samples);
    for (const sample of row.samples) {
      assert.equal(sample.status, "passed");
      assert.ok(positive(sample.processNs) && positive(sample.cycles));
      assert.ok(sample.cycles < report.maxCycles);
      assert.equal(sample.cycles, row.samples[0].cycles);
    }
  }
  for (const state of ["two-state", "four-state"]) {
    const rows = report.results.filter(row => row.stateModel === state && row.status === "passed");
    if (rows.length) {
      const counts = rows.map(row => row.samples[0].cycles);
      assert.ok(Math.max(...counts) - Math.min(...counts) <= 1, "Mismatched completion edges");
    }
  }
  const run = `${process.env.GITHUB_SERVER_URL}/${process.env.GITHUB_REPOSITORY}/actions/runs/${process.env.GITHUB_RUN_ID}`;
  return report.results.filter(row => row.status === "passed").map(row => {
    const times = row.samples.map(sample => sample.processNs).sort((a, b) => a - b);
    return { name: `rtlmeter-${row.engine}/rtlmeter_${report.case}_hello_v1_x64_${row.stateModel}_total`,
      unit: "us", value: times[Math.floor(times.length / 2)] / 1000,
      extra: JSON.stringify({ contract: report.contract, rtlmeter: report.rtlmeterRevision,
        state: row.stateModel, cycles: row.samples[0].cycles, maxCycles: report.maxCycles, samples: times,
        configuration: report.configuration, versions: report.versions,
        binarySha256: report.binarySha256, sourceSha256: report.sourceSha256, run }) };
  });
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  if (process.env.GITHUB_REF !== "refs/heads/master" ||
      !["schedule", "workflow_dispatch"].includes(process.env.GITHUB_EVENT_NAME)) {
    throw new Error("RTLMeter publication requires a scheduled or manual master run");
  }
  const [result, host, option] = process.argv.slice(2);
  if (!result || !host) throw new Error("Usage: publish-rtlmeter-bench.mjs <results.json> <host.txt> [--validate-only]");
  const git = (...args) => execFileSync("git", args, { encoding: "utf8" }).trim();
  const report = JSON.parse(readFileSync(result));
  const id = git("rev-parse", "HEAD");
  const benches = rtlmeterMetrics(report, id);
  const cpu = hostCpu(host);
  if (option === "--validate-only") {
    console.log(`Validated ${report.case}: ${benches.length} timings, ${6 - benches.length} unavailable configurations`);
  } else {
    if (option) throw new Error(`Unknown option: ${option}`);
    const [authorName, authorEmail, committerName, committerEmail] = git(
      "show", "-s", "--format=%an%x00%ae%x00%cn%x00%ce", "HEAD").split("\0");
    publishResult(git("remote", "get-url", "origin"), {
      commit: { id, author: { name: authorName, email: authorEmail },
        committer: { name: committerName, email: committerEmail },
        message: git("show", "-s", "--format=%s", "HEAD"), timestamp: git("show", "-s", "--format=%cI", "HEAD"),
        url: `${process.env.GITHUB_SERVER_URL}/${process.env.GITHUB_REPOSITORY}/commit/${id}` },
      date: report.date, cpu, tool: "customSmallerIsBetter", benches,
      rtlmeter: { case: report.case, results: report.results, versions: report.versions,
        xezimRelease: report.xezimRelease, run: `${process.env.GITHUB_SERVER_URL}/${process.env.GITHUB_REPOSITORY}/actions/runs/${process.env.GITHUB_RUN_ID}` },
    }, { historyName: "RTLMeter Benchmarks", allowEmpty: true });
  }
}

