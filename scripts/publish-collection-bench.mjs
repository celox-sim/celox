#!/usr/bin/env node
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { collection, revision } from "./benchmark-collection.mjs";
import { publishResult } from "./publish-heliodor-bench.mjs";

export function metrics(report) {
  if (report.schemaVersion !== 1 || report.mode !== "measure" || !report.results?.length ||
      !report.host?.cpu || !["x64", "arm64"].includes(report.host.arch)) throw new Error("Invalid measurement report");
  const seen = new Set();
  return report.results.flatMap(result => {
    const workload = collection.workloads.find(w => w.id === result.circuit);
    if (!workload?.measurement || seen.has(result.circuit)) throw new Error("Unknown or duplicate workload");
    seen.add(result.circuit);
    if (result.status !== "measured" || result.qualification?.status !== "passed" || result.negativeControl?.status !== "passed" ||
        result.revision !== revision(collection.repositories[workload.repository]) ||
        result.workload !== workload.measurement.id || result.testbench !== `collection-${workload.measurement.id}` ||
        result.workUnits !== workload.measurement.repetitions * workload.measurement.unitsPerRepetition ||
        result.workUnit !== workload.measurement.unit || result.frontend !== "veryl" || result.testbenchLanguage !== "veryl" ||
        result.backend !== "native" || result.optimization !== "O2" || result.stateModel !== "two-state" || result.threads !== 1 ||
        !/^[a-f0-9]{64}$/.test(result.sourceSha256 ?? "") || !/^[a-f0-9]{64}$/.test(result.testbenchSha256 ?? "") ||
        result.samples?.length !== 3) throw new Error(`${result.circuit}: incomplete or mismatched measurement`);
    for (const sample of result.samples) {
      if (sample.status !== "passed" || ![sample.compileNs, sample.executeNs, sample.totalNs].every(n => Number.isSafeInteger(n) && n > 0) ||
          sample.compileNs + sample.executeNs > sample.totalNs) throw new Error("Invalid sample");
    }
    // A recipe, input, state model or source change starts a new chart. Frontend
    // and backend identify the series, not the circuit. Future SV TBs need an
    // explicit common workload contract before joining a Veryl comparison.
    const contract = { revision: result.revision, sourceSha256: result.sourceSha256, workload: result.workload,
      testbench: result.testbench, testbenchSha256: result.testbenchSha256, workUnits: result.workUnits,
      stateModel: result.stateModel, optimization: result.optimization, threads: result.threads };
    const id = createHash("sha256").update(JSON.stringify(contract)).digest("hex").slice(0, 12);
    return [["compilation", "compileNs"], ["execution", "executeNs"], ["total", "totalNs"]].map(([phase, field]) => {
      const values = result.samples.map(s => s[field]).sort((a, b) => a - b);
      return { name: `collection-celox-veryl-native/collection_${result.circuit}_${id}_${report.host.arch}_${phase}`,
        unit: "ns", value: values[1], range: `${values[0]}–${values[2]}`,
        extra: JSON.stringify({ ...contract, frontend: result.frontend, backend: result.backend, phase }) };
    });
  });
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  if (process.env.GITHUB_REF !== "refs/heads/master" || !["schedule", "workflow_dispatch"].includes(process.env.GITHUB_EVENT_NAME)) {
    throw new Error("Publication requires a scheduled or manual master run");
  }
  if (process.argv.length !== 3) throw new Error("Usage: publish-collection-bench.mjs RESULTS_JSON");
  const report = JSON.parse(readFileSync(process.argv[2], "utf8"));
  const benches = metrics(report);
  const git = (...args) => execFileSync("git", args, { encoding: "utf8" }).trim();
  const id = git("rev-parse", "HEAD");
  if (report.celox.commit !== id || report.celox.dirty) throw new Error("Measurement must come from this clean commit");
  const [name, email] = git("show", "-s", "--format=%an%x00%ae", "HEAD").split("\0");
  const date = Date.parse(report.date);
  if (!Number.isFinite(date)) throw new Error("Invalid measurement date");
  publishResult(git("remote", "get-url", "origin"), {
    commit: { id, message: git("show", "-s", "--format=%s", "HEAD"), timestamp: git("show", "-s", "--format=%cI", "HEAD"),
      author: { name, email }, committer: { name, email }, url: `${process.env.GITHUB_SERVER_URL}/${process.env.GITHUB_REPOSITORY}/commit/${id}` },
    date, cpu: report.host.cpu, tool: "customSmallerIsBetter", benches,
  }, { historyName: "Circuit Benchmarks" });
}
