#!/usr/bin/env node
/** Convert a Heliodor results.tsv file to github-action-benchmark data. */

import { readFileSync, writeFileSync } from "node:fs";

const [inputPath, outputPath, ...options] = process.argv.slice(2);
if (!inputPath || !outputPath) {
  console.error(
    "Usage: node convert-heliodor-bench.mjs <results.tsv> <output.json> [--jit-only | --arm64-results <results.tsv>] [--require-tiered] [--suite] | --partial --arch x86_64|aarch64 [--latest]",
  );
  process.exit(1);
}

let suite = false;
let jitOnly = false;
let requireTiered = false;
let partial = false;
let latest = false;
let arch;
let arm64ResultsPath;
for (let index = 0; index < options.length; index += 1) {
  switch (options[index]) {
    case "--partial":
      partial = true;
      break;
    case "--latest":
      latest = true;
      break;
    case "--arch":
      arch = options[++index];
      break;
    case "--suite":
      suite = true;
      break;
    case "--require-tiered":
      requireTiered = true;
      break;
    case "--jit-only":
      jitOnly = true;
      break;
    case "--arm64-results":
      arm64ResultsPath = options[++index];
      break;
    default:
      throw new Error(`unknown conversion option: ${options[index]}`);
  }
}
if (jitOnly && arm64ResultsPath) {
  throw new Error("--jit-only cannot be combined with platform results");
}
if ((latest || arch) && !partial) throw new Error("--latest and --arch require --partial");

const expectedHeader = [
  "runner",
  "test",
  "status",
  "elapsed_ns",
  "log",
  "semantic_status",
  "exit_status",
  "process_elapsed_ns",
  "reported_elapsed_ns",
  "compile_elapsed_ns",
  "execute_elapsed_ns",
  "jit_execute_elapsed_ns",
];

function readResults(path) {
  const lines = readFileSync(path, "utf8").trim().split("\n");
  const header = lines.shift()?.split("\t") ?? [];
  if (header.join("\t") !== expectedHeader.join("\t")) {
    throw new Error(
      `unsupported Heliodor result schema in ${path}: ${header.join("\t")}`,
    );
  }
  return lines.filter(Boolean).map((line) => {
    const fields = line.split("\t");
    if (fields.length !== expectedHeader.length) {
      throw new Error(
        `expected 12 fields in ${path}, found ${fields.length}: ${line}`,
      );
    }
    return Object.fromEntries(
      expectedHeader.map((name, index) => [name, fields[index]]),
    );
  });
}

const rows = readResults(inputPath);

// Each backend has independently useful measurements. Publication does not
// require another backend, workload, or architecture to finish successfully.
function convertPartial() {
  if (!["x86_64", "aarch64"].includes(arch) || arm64ResultsPath || jitOnly || requireTiered) {
    throw new Error("--partial requires --arch x86_64|aarch64 and no comparison options");
  }
  const seen = new Set();
  return (latest ? rows.slice(-1) : rows).flatMap(row => {
    if (row.semantic_status !== "pass" || row.exit_status !== "0") return [];
    if (!/^test_soc_(?:(?:66|71|71v)_)?(?:smp_)?linux_boot(?:_[248]hart)?$/.test(row.test)) {
      throw new Error(`unsupported suite workload: ${row.test}`);
    }
    const key = `${row.runner}/${row.test}`;
    if (seen.has(key)) throw new Error(`duplicate result: ${key}`);
    seen.add(key);
    const platforms = {
      celox: `native-${arch}`,
      "veryl-cc-sync": `veryl-cc-${arch}`,
      "celox-tiered": arch === "x86_64" ? "celox-tiered" : "celox-tiered-aarch64",
      "veryl-cc-tiered": `veryl-tiered-${arch}`,
    };
    const platform = platforms[row.runner];
    if (!platform) throw new Error(`unsupported runner: ${row.runner}`);
    const base = `heliodor-${platform}/heliodor_suite_${row.test.slice("test_soc_".length)}`;
    const compile = ns(row, "compile_elapsed_ns");
    const execute = ns(row, "execute_elapsed_ns");
    if (row.runner.endsWith("-tiered")) {
      const total = ns(row, "reported_elapsed_ns");
      if (compile + execute > total) throw new Error(`${key}: startup and execution exceed total`);
      return [milliseconds(`${base}_startup`, compile), milliseconds(`${base}_execution`, execute), milliseconds(`${base}_end_to_end`, total)];
    }
    const metrics = [milliseconds(`${base}_compilation`, compile), milliseconds(`${base}_execution`, execute)];
    if (row.runner === "celox" && arch === "x86_64") {
      metrics.push(milliseconds(`heliodor-celox-jit/heliodor_suite_${row.test.slice("test_soc_".length)}_execution`, ns(row, "jit_execute_elapsed_ns")));
    }
    return metrics;
  });
}

function requirePassedRunner(resultRows, name, sourcePath) {
  const matches = resultRows.filter((row) => row.runner === name);
  if (matches.length !== 1) {
    throw new Error(
      `expected exactly one ${name} row in ${sourcePath}, found ${matches.length}`,
    );
  }
  const row = matches[0];
  if (row.semantic_status !== "pass" || row.exit_status !== "0") {
    throw new Error(`${name} did not complete successfully`);
  }
  return row;
}

function optionalPassedRunner(resultRows, name, sourcePath) {
  const matches = resultRows.filter((row) => row.runner === name);
  if (matches.length === 0) {
    return undefined;
  }
  return requirePassedRunner(resultRows, name, sourcePath);
}

function ns(row, field) {
  const value = row[field];
  if (!/^\d+$/.test(value) || value === "0") {
    throw new Error(`${row.runner}.${field} is not a positive integer: ${value}`);
  }
  return Number(value);
}

function milliseconds(name, nanoseconds) {
  return {
    name,
    unit: "ms",
    value: nanoseconds / 1_000_000,
  };
}

function convertResults(rows, arm64Rows) {
  const celox = requirePassedRunner(rows, "celox", inputPath);
  const readTieredRunner = requireTiered ? requirePassedRunner : optionalPassedRunner;
  const tiered = readTieredRunner(rows, "celox-tiered", inputPath);
  const verylTiered = readTieredRunner(rows, "veryl-cc-tiered", inputPath);
  const veryl = requirePassedRunner(rows, "veryl-cc-sync", inputPath);
  if (celox.test !== veryl.test || (tiered && tiered.test !== celox.test)) {
    throw new Error(
      `runner tests differ: Celox=${celox.test}, tiered=${tiered?.test ?? "absent"}, Veryl=${veryl.test}`,
    );
  }

  const results = [
    milliseconds(
      "heliodor-celox-jit/heliodor_linux_boot_execution",
      ns(celox, "jit_execute_elapsed_ns"),
    ),
    milliseconds(
      "heliodor-celox-total/heliodor_linux_boot_execution",
      ns(celox, "execute_elapsed_ns"),
    ),
    milliseconds(
      "heliodor-veryl/heliodor_linux_boot_execution",
      ns(veryl, "execute_elapsed_ns"),
    ),
    milliseconds(
      "heliodor-celox-compile/heliodor_linux_boot_compilation",
      ns(celox, "compile_elapsed_ns"),
    ),
    milliseconds(
      "heliodor-veryl-compile/heliodor_linux_boot_compilation",
      ns(veryl, "compile_elapsed_ns"),
    ),
  ];

  function addTieredMetrics(platform, row) {
    if (!row) return;
    if (row.test !== celox.test) {
      throw new Error(
        `runner tests differ: Celox=${celox.test}, ${platform}=${row.test}`,
      );
    }
    const startup = ns(row, "compile_elapsed_ns");
    const execution = ns(row, "execute_elapsed_ns");
    const total = ns(row, "reported_elapsed_ns");
    if (startup + execution > total) {
      throw new Error(`${platform} startup and execution exceed end-to-end time`);
    }
    results.push(
      milliseconds(`heliodor-${platform}/heliodor_linux_boot_end_to_end`, total),
      milliseconds(`heliodor-${platform}/heliodor_linux_boot_startup`, startup),
      milliseconds(
        `heliodor-${platform}/heliodor_linux_boot_execution`,
        execution,
      ),
    );
  }

  addTieredMetrics("celox-tiered", tiered);
  addTieredMetrics("veryl-tiered-x86_64", verylTiered);

  if (arm64ResultsPath) {
    addTieredMetrics(
      "celox-tiered-aarch64",
      readTieredRunner(arm64Rows, "celox-tiered", arm64ResultsPath),
    );
    addTieredMetrics(
      "veryl-tiered-aarch64",
      readTieredRunner(arm64Rows, "veryl-cc-tiered", arm64ResultsPath),
    );
    const arm64 = requirePassedRunner(arm64Rows, "celox", arm64ResultsPath);
    const verylArm64 = requirePassedRunner(
      arm64Rows,
      "veryl-cc-sync",
      arm64ResultsPath,
    );
    for (const row of [arm64, verylArm64]) {
      if (row.test !== celox.test) {
        throw new Error(
          `runner tests differ: native-x86_64=${celox.test}, ${row.runner}=${row.test}`,
        );
      }
    }

    for (const [platform, row] of [
      ["native-x86_64", celox],
      ["veryl-cc-x86_64", veryl],
      ["native-aarch64", arm64],
      ["veryl-cc-aarch64", verylArm64],
    ]) {
      results.push(
        milliseconds(
          `heliodor-${platform}/heliodor_linux_boot_compilation`,
          ns(row, "compile_elapsed_ns"),
        ),
        milliseconds(
          `heliodor-${platform}/heliodor_linux_boot_execution`,
          ns(row, "execute_elapsed_ns"),
        ),
      );
    }
  }

  return jitOnly ? results.slice(0, 1) : results;
}

const arm64Rows = arm64ResultsPath ? readResults(arm64ResultsPath) : undefined;
let selectedResults;
if (partial) {
  selectedResults = convertPartial();
} else if (suite) {
  const tests = [...new Set(rows.map((row) => row.test))];
  if (tests.length === 0) throw new Error("empty Heliodor suite");
  if (
    arm64Rows &&
    [...new Set(arm64Rows.map((row) => row.test))].sort().join() !==
      [...tests].sort().join()
  ) {
    throw new Error("architecture workload sets differ");
  }
  selectedResults = tests.flatMap((test) => {
    if (!/^test_soc_(?:(?:66|71|71v)_)?(?:smp_)?linux_boot(?:_[248]hart)?$/.test(test)) {
      throw new Error(`unsupported suite workload: ${test}`);
    }
    return convertResults(
      rows.filter((row) => row.test === test),
      arm64Rows?.filter((row) => row.test === test),
    ).map((metric) => ({
      ...metric,
      name: metric.name.replace(
        "heliodor_linux_boot_",
        `heliodor_suite_${test.slice("test_soc_".length)}_`,
      ),
    }));
  });
} else {
  if (rows.some(row => row.test !== "test_soc_linux_boot")) throw new Error("use --suite for other workloads");
  selectedResults = convertResults(rows, arm64Rows).map(metric => ({
    ...metric,
    name: metric.name.replace("heliodor_linux_boot_", "heliodor_suite_linux_boot_"),
  }));
}

writeFileSync(outputPath, JSON.stringify(selectedResults, null, 2));
console.log(`Converted ${selectedResults.length} Heliodor metrics → ${outputPath}`);
