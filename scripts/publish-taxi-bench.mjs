#!/usr/bin/env node
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { hostCpu, publishResult } from "./publish-heliodor-bench.mjs";

const manifest = JSON.parse(readFileSync(new URL("../benchmarks/taxi/manifest.json", import.meta.url)));
const positive = value => typeof value === "number" && Number.isFinite(value) && value > 0;

// Validate the entire paired case before deriving or publishing any metrics.
export function taxiMetrics(report, commit) {
  const spec = manifest.cases[report.case];
  if (!spec || report.schemaVersion !== 1 || report.contract !== manifest.contract ||
      report.status !== "passed" || report.body_clock !== "cocotb-regression-perf_counter-v1" || report.taxi_revision !== manifest.revision ||
      report.celox_commit !== commit || report.seed !== manifest.seed ||
      report.expected_tests !== spec.tests || report.requested_samples !== manifest.samples ||
      report.pairs?.length !== manifest.samples || report.host?.architecture !== "x86_64" ||
      report.cocotb_version !== "2.0.1" || !/^Verilator 5\.052\b/.test(report.verilator_version) ||
      !/^[a-f0-9]{64}$/.test(report.celox_binary_sha256 ?? "")) {
    throw new Error("Incomplete or incompatible Taxi comparison");
  }
  const requiredSources = [...spec.sources, `${spec.testDirectory}/${spec.module}.py`];
  if (requiredSources.some(path => !/^[a-f0-9]{64}$/.test(report.source_sha256?.[path] ?? "")) ||
      Object.keys(report.parameters ?? {}).length !== Object.keys(spec.parameters).length ||
      Object.entries(spec.parameters).some(([key, value]) => report.parameters?.[key] !== value) ||
      report.configuration?.celox !== "native O1 two-state" ||
      report.configuration?.verilator !== "--vpi --public-flat-rw; C++ default -Os") {
    throw new Error("Missing Taxi source identity or mismatched configuration");
  }
  let first;
  for (const pair of report.pairs) {
    for (const engine of ["celox", "verilator"]) {
      const sample = pair[engine];
      const times = sample?.simulation_ns;
      if (!positive(sample?.body_seconds) || !positive(sample?.wall_seconds) ||
          sample.body_seconds > sample.wall_seconds || !times || Object.keys(times).length !== spec.tests ||
          Object.values(times).some(value => !positive(value)) || !positive(report.compilation_seconds?.[engine])) {
        throw new Error("Invalid Taxi test timings");
      }
      first ??= times;
      if (Object.keys(first).some(key => !Object.hasOwn(times, key) || Math.abs(first[key] - times[key]) > 1e-6)) {
        throw new Error("Taxi test identities or simulated durations differ");
      }
    }
  }
  const median = values => values.toSorted((a, b) => a - b)[Math.floor(values.length / 2)];
  const ratio = median(report.pairs.map(p => p.celox.body_seconds / p.verilator.body_seconds));
  const extra = JSON.stringify({ contract: report.contract, taxi: report.taxi_revision,
    clock: report.body_clock, samples: report.pairs.length, tests: spec.tests, seed: report.seed,
    verilator: report.verilator_version, python: report.python_version, cocotb: report.cocotb_version,
    configuration: report.configuration, celoxBinary: report.celox_binary_sha256,
    medianPairedBodyRatio: ratio,
    run: `${process.env.GITHUB_SERVER_URL}/${process.env.GITHUB_REPOSITORY}/actions/runs/${process.env.GITHUB_RUN_ID}` });
  return ["celox", "verilator"].flatMap(engine => [
    ["body", median(report.pairs.map(p => p[engine].body_seconds))],
    ["process", median(report.pairs.map(p => p[engine].wall_seconds))],
    ["compilation", report.compilation_seconds[engine]],
  ].map(([phase, seconds]) => {
    const value = seconds * 1e6;
    if (!positive(value)) throw new Error("Invalid converted Taxi timing");
    return { name: `taxi-${engine}/taxi_${report.case}_v1_x64_${phase}`, unit: "us", value, extra };
  }));
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  if (process.env.GITHUB_REF !== "refs/heads/master" ||
      !["schedule", "workflow_dispatch"].includes(process.env.GITHUB_EVENT_NAME)) {
    throw new Error("Taxi publication requires a scheduled or manual master run");
  }
  const [result, host, option] = process.argv.slice(2);
  if (!result || !host) throw new Error("Usage: publish-taxi-bench.mjs <results.json> <host.txt>");
  const git = (...args) => execFileSync("git", args, { encoding: "utf8" }).trim();
  const id = git("rev-parse", "HEAD");
  const benches = taxiMetrics(JSON.parse(readFileSync(result)), id);
  const [authorName, authorEmail, committerName, committerEmail] = git(
    "show", "-s", "--format=%an%x00%ae%x00%cn%x00%ce", "HEAD",
  ).split("\0");
  const cpu = hostCpu(host);
  if (option === "--validate-only") {
    console.log(`Validated ${benches.length} Taxi metrics`);
    process.exit(0);
  }
  if (option) throw new Error(`Unknown option: ${option}`);
  publishResult(git("remote", "get-url", "origin"), {
    commit: { id, author: { name: authorName, email: authorEmail },
      committer: { name: committerName, email: committerEmail },
      message: git("show", "-s", "--format=%s", "HEAD"), timestamp: git("show", "-s", "--format=%cI", "HEAD"),
      url: `${process.env.GITHUB_SERVER_URL}/${process.env.GITHUB_REPOSITORY}/commit/${id}` },
    date: Date.now(), cpu, tool: "customSmallerIsBetter", benches,
  }, { historyName: "Taxi Benchmarks" });
  console.log(`Published ${benches.length} validated Taxi metrics`);
}
