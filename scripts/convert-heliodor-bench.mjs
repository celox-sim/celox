#!/usr/bin/env node
/** Convert a Heliodor results.tsv file to github-action-benchmark data. */

import { readFileSync, writeFileSync } from "node:fs";

const [inputPath, outputPath, ...options] = process.argv.slice(2);
if (!inputPath || !outputPath) {
  console.error(
    "Usage: node convert-heliodor-bench.mjs <results.tsv> <output.json> [--jit-only | --arm64-results <results.tsv>] [--require-tiered] [--suite]",
  );
  process.exit(1);
}

let suite = false;
let jitOnly = false;
let requireTiered = false;
let arm64ResultsPath;
for (let index = 0; index < options.length; index += 1) {
  switch (options[index]) {
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
if (suite) {
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
  selectedResults = convertResults(rows, arm64Rows);
}

writeFileSync(outputPath, JSON.stringify(selectedResults, null, 2));
console.log(`Converted ${selectedResults.length} Heliodor metrics → ${outputPath}`);
