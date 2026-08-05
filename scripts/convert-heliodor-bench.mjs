#!/usr/bin/env node
/** Convert a Heliodor results.tsv file to github-action-benchmark data. */

import { readFileSync, writeFileSync } from "node:fs";

const [inputPath, outputPath, ...options] = process.argv.slice(2);
if (!inputPath || !outputPath) {
  console.error(
    "Usage: node convert-heliodor-bench.mjs <results.tsv> <output.json> [--jit-only | --cranelift-results <results.tsv> --arm64-results <results.tsv>]",
  );
  process.exit(1);
}

let jitOnly = false;
let craneliftResultsPath;
let arm64ResultsPath;
for (let index = 0; index < options.length; index += 1) {
  switch (options[index]) {
    case "--jit-only":
      jitOnly = true;
      break;
    case "--cranelift-results":
      craneliftResultsPath = options[++index];
      break;
    case "--arm64-results":
      arm64ResultsPath = options[++index];
      break;
    default:
      throw new Error(`unknown conversion option: ${options[index]}`);
  }
}
if (jitOnly && (craneliftResultsPath || arm64ResultsPath)) {
  throw new Error("--jit-only cannot be combined with platform results");
}
if (Boolean(craneliftResultsPath) !== Boolean(arm64ResultsPath)) {
  throw new Error("Cranelift and ARM64 results must be provided together");
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

const celox = requirePassedRunner(rows, "celox", inputPath);
const veryl = requirePassedRunner(rows, "veryl-cc-sync", inputPath);
if (celox.test !== veryl.test) {
  throw new Error(`runner tests differ: Celox=${celox.test}, Veryl=${veryl.test}`);
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

if (craneliftResultsPath && arm64ResultsPath) {
  const cranelift = requirePassedRunner(
    readResults(craneliftResultsPath),
    "celox-cranelift",
    craneliftResultsPath,
  );
  const arm64Rows = readResults(arm64ResultsPath);
  const arm64 = requirePassedRunner(arm64Rows, "celox", arm64ResultsPath);
  const craneliftArm64 = requirePassedRunner(
    arm64Rows,
    "celox-cranelift",
    arm64ResultsPath,
  );
  for (const row of [cranelift, arm64, craneliftArm64]) {
    if (row.test !== celox.test) {
      throw new Error(
        `runner tests differ: native-x86_64=${celox.test}, ${row.runner}=${row.test}`,
      );
    }
  }

  for (const [platform, row] of [
    ["native-x86_64", celox],
    ["cranelift-x86_64", cranelift],
    ["native-aarch64", arm64],
    ["cranelift-aarch64", craneliftArm64],
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

const selectedResults = jitOnly ? results.slice(0, 1) : results;

writeFileSync(outputPath, JSON.stringify(selectedResults, null, 2));
console.log(`Converted ${selectedResults.length} Heliodor metrics → ${outputPath}`);
