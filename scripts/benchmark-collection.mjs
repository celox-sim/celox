#!/usr/bin/env node
// Circuit identity, stimulus/TB identity and simulator frontend are separate.
// Qualification records are never dashboard performance samples.
import { createHash } from "node:crypto";
import { execFileSync, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { arch, cpus, hostname, platform } from "node:os";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = fileURLToPath(new URL("../", import.meta.url));
export const collection = JSON.parse(readFileSync(new URL("../benchmarks/collection.json", import.meta.url)));
const hash = value => createHash("sha256").update(value).digest("hex");
const git = (cwd, ...args) => execFileSync("git", ["-C", cwd, ...args], { encoding: "utf8" }).trim();
export const revision = repository => repository.revision ?? readFileSync(join(root, repository.revisionFile), "utf8").trim();

export function select(ids) {
  const names = ids === "all" ? collection.workloads.filter(w => w.integration === "qualification").map(w => w.id) : ids.split(",");
  if (!names.length || new Set(names).size !== names.length) throw new Error("Empty or duplicate workload selection");
  return names.map(id => {
    const workload = collection.workloads.find(w => w.id === id);
    if (!workload || workload.integration !== "qualification") {
      throw new Error(`${id}: no collection runner; ${workload?.command ?? workload?.reason ?? "unknown workload"}`);
    }
    return workload;
  });
}

export function prepare(workloads, directory) {
  for (const name of new Set(workloads.map(w => w.repository))) {
    const repository = collection.repositories[name];
    const source = join(directory, name);
    if (!existsSync(source)) {
      mkdirSync(source, { recursive: true });
      git(source, "init", "--quiet");
      git(source, "fetch", "--quiet", "--depth=1", repository.url, revision(repository));
      git(source, "checkout", "--quiet", "--detach", "FETCH_HEAD");
    }
    verifyCheckout(source, revision(repository));
  }
}

function verifyCheckout(source, expected) {
  if (git(source, "rev-parse", "HEAD") !== expected) throw new Error(`${source}: expected pinned revision ${expected}`);
  if (git(source, "status", "--porcelain", "--untracked-files=all")) throw new Error(`${source}: checkout is dirty`);
}

// Require one complete matching protocol record, not just exit zero or a timing
// line preceding an assertion failure. Failed timings stay only in raw logs.
export function parseResult(log, top, exitCode, signal, error) {
  const timing = [...log.matchAll(/^CELOX_TEST_TIMING test=(\S+) compile_ns=(\d+) execute_ns=(\d+) jit_execute_ns=(?:\d+|NA)(?: execute_cpu_ns=\d+)?\r?$/gm)];
  const result = [...log.matchAll(/^CELOX_TEST_RESULT test=(\S+) status=(\S+) elapsed_ns=(\d+)\r?$/gm)];
  if (error || signal || exitCode !== 0 || timing.length !== 1 || result.length !== 1 ||
      timing[0][1] !== top || result[0][1] !== top || result[0][2] !== "pass") {
    return { status: "failed", exitCode, signal: signal ?? null, error: error?.message ?? null,
      reportedStatus: result.length === 1 ? result[0][2] : "missing-or-ambiguous" };
  }
  const [compileNs, executeNs, totalNs] = [timing[0][2], timing[0][3], result[0][3]].map(Number);
  if (![compileNs, executeNs, totalNs].every(n => Number.isSafeInteger(n) && n > 0) || compileNs + executeNs > totalNs) {
    return { status: "failed", error: "Invalid split timings" };
  }
  return { status: "passed", compileNs, executeNs, totalNs };
}

export function measurementSource(source, workload) {
  const count = workload.measurement?.repetitions;
  if (!Number.isSafeInteger(count) || count < 1) throw new Error("Missing reviewed measurement workload");
  if (source.split("initial {").length !== 2 || source.split("$finish();").length !== 2) {
    throw new Error("Pinned testbench shape changed");
  }
  for (const { find, replace, count } of workload.measurement.transforms ?? []) {
    if (source.split(find).length !== count + 1) throw new Error("Pinned workload transform changed");
    source = source.replaceAll(find, replace);
  }
  return source.replace("initial {", `initial {\n        for _benchmark_round in 0..${count} {`)
    .replace("$finish();", `}\n        $display("COLLECTION_COMPLETE ${workload.measurement.id} ${count}");\n        $finish();`);
}

function materialize(source, workload, destination) {
  mkdirSync(destination, { recursive: true });
  // Copy only pinned tracked bytes, never generated/ignored files or local edits.
  const files = git(source, "ls-tree", "-r", "--name-only", "HEAD", "--", workload.project).split("\n");
  const digests = [];
  for (const file of files) {
    const name = relative(workload.project, file);
    if (!name || name.startsWith("..")) throw new Error("Invalid project file");
    const bytes = execFileSync("git", ["-C", source, "show", `HEAD:${file}`], { maxBuffer: 32 * 1024 * 1024 });
    const output = join(destination, name);
    mkdirSync(dirname(output), { recursive: true });
    writeFileSync(output, bytes);
    digests.push([name, hash(bytes)]);
  }
  return hash(JSON.stringify(digests));
}

export function runCollection({ workloads, sourceDirectory, output, celox, mode = "qualify", samples = 3, timeoutMs = 120_000 }) {
  if (!["qualify", "measure"].includes(mode)) throw new Error("Unknown mode");
  if (!Number.isSafeInteger(samples) || samples < 1 || samples > 20) throw new Error("samples must be 1..20");
  celox = resolve(celox);
  sourceDirectory = resolve(sourceDirectory);
  output = resolve(output);
  // Fresh results prevent a failed rerun from exposing stale accepted metrics.
  if (existsSync(output)) throw new Error(`${output}: choose a fresh output directory`);
  const binarySha256 = hash(readFileSync(celox));
  mkdirSync(output, { recursive: true });
  const report = { schemaVersion: 1, mode, date: new Date().toISOString(),
    celox: { commit: git(root, "rev-parse", "HEAD"), binarySha256,
      dirty: Boolean(git(root, "status", "--porcelain", "--untracked-files=all")) },
    host: { cpu: cpus()[0]?.model, arch: arch(), platform: platform(), hostname: hostname() },
    results: [] };
  const save = () => writeFileSync(join(output, "results.json"), `${JSON.stringify(report, null, 2)}\n`);
  save();
  for (const workload of workloads) {
    const entry = { circuit: workload.id, revision: revision(collection.repositories[workload.repository]),
      workload: workload.workload, testbench: workload.testbenches[0].id, testbenchLanguage: "veryl",
      frontend: "veryl", backend: "native", optimization: "O2", stateModel: "two-state", threads: 1,
      status: "failed", samples: [] };
    report.results.push(entry);
    try {
      const source = join(sourceDirectory, workload.repository);
      verifyCheckout(source, entry.revision);
      const directory = join(output, workload.id);
      const project = join(directory, "project");
      entry.sourceSha256 = materialize(source, workload, project);
      const tb = join(project, relative(workload.project, workload.testbenches[0].path));
      const original = readFileSync(tb, "utf8");
      const execute = name => {
        const args = ["run", "--project", project, "--top", workload.top, "--backend", "native", "--report", "--unlimited", "--two-state", "--threads", "1", "-O", "O2"];
        const start = process.hrtime.bigint();
        const result = spawnSync(celox, args, { cwd: project, encoding: "utf8", timeout: timeoutMs, maxBuffer: 16 * 1024 * 1024 });
        const processNs = Number(process.hrtime.bigint() - start);
        const log = `${result.stdout ?? ""}${result.stderr ?? ""}`;
        writeFileSync(join(directory, `${name}.log`), log);
        return { ...parseResult(log, workload.top, result.status, result.signal, result.error), processNs, log };
      };
      const { log: positiveLog, ...positive } = execute("positive");
      entry.qualification = positive;
      if (positive.status !== "passed") throw new Error("Upstream self-checking testbench failed");
      const { find, replace } = workload.negativeControl;
      if (!original.includes(find)) throw new Error("Negative-control anchor missing");
      writeFileSync(tb, original.replace(find, replace));
      const negative = execute("negative");
      entry.negativeControl = { status: negative.exitCode === 1 && !negative.signal && !negative.error &&
        negative.reportedStatus === "fail" && negative.log.includes("assertion failed") ? "passed" : "failed" };
      writeFileSync(tb, original);
      if (entry.negativeControl.status !== "passed") throw new Error("Broken oracle was not rejected by an assertion");
      if (mode === "qualify") {
        entry.status = "qualified";
      } else {
        const text = measurementSource(original, workload);
        writeFileSync(tb, text);
        entry.workload = workload.measurement.id;
        entry.testbench = `collection-${workload.measurement.id}`;
        entry.testbenchSha256 = hash(text);
        entry.workUnits = workload.measurement.repetitions * workload.measurement.unitsPerRepetition;
        entry.workUnit = workload.measurement.unit;
        for (let sample = 0; sample < samples; sample++) {
          const { log, ...result } = execute(`sample-${sample}`);
          if (result.status !== "passed" || !log.split(/\r?\n/).includes(`COLLECTION_COMPLETE ${workload.measurement.id} ${workload.measurement.repetitions}`)) {
            throw new Error(`Measurement sample ${sample} did not complete`);
          }
          entry.samples.push(result);
        }
        entry.status = "measured";
      }
    } catch (error) {
      entry.error = error.message;
    }
    save();
    console.log(`${entry.circuit}: ${entry.status}${entry.error ? ` (${entry.error})` : ""}`);
  }
  return report;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try {
    const [command, ids = "all", sources = "work/benchmark-collection/sources", output, celox = "target/release/celox"] = process.argv.slice(2);
    if (command === "list") {
      for (const w of collection.workloads) console.log(`${w.id}\t${w.integration}\t${w.role}`);
    } else if (command === "prepare") {
      prepare(select(ids), resolve(sources));
    } else if (["qualify", "measure"].includes(command) && output) {
      const report = runCollection({ workloads: select(ids), sourceDirectory: sources, output, celox, mode: command });
      if (report.results.some(r => r.status === "failed")) process.exitCode = 1;
    } else throw new Error("Usage: benchmark-collection.mjs list | prepare [IDS|all] [SOURCES] | qualify|measure IDS SOURCES OUTPUT [CELOX_BIN]");
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
