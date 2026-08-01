#!/usr/bin/env node
/** Convert a Heliodor results.tsv file to github-action-benchmark data. */

import { readFileSync, writeFileSync } from "node:fs";

const [inputPath, outputPath, mode] = process.argv.slice(2);
if (!inputPath || !outputPath) {
  console.error(
    "Usage: node convert-heliodor-bench.mjs <results.tsv> <output.json> [--jit-only]",
  );
  process.exit(1);
}
if (mode && mode !== "--jit-only") {
  throw new Error(`unknown conversion mode: ${mode}`);
}

const lines = readFileSync(inputPath, "utf8").trim().split("\n");
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
const header = lines.shift()?.split("\t") ?? [];
if (header.join("\t") !== expectedHeader.join("\t")) {
  throw new Error(`unsupported Heliodor result schema: ${header.join("\t")}`);
}

const rows = lines.filter(Boolean).map((line) => {
  const fields = line.split("\t");
  if (fields.length !== expectedHeader.length) {
    throw new Error(`expected 12 fields, found ${fields.length}: ${line}`);
  }
  return Object.fromEntries(expectedHeader.map((name, index) => [name, fields[index]]));
});

function requirePassedRunner(name) {
  const matches = rows.filter((row) => row.runner === name);
  if (matches.length !== 1) {
    throw new Error(`expected exactly one ${name} row, found ${matches.length}`);
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

const celox = requirePassedRunner("celox");
const veryl = requirePassedRunner("veryl-cc-sync");
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

const selectedResults = mode === "--jit-only" ? results.slice(0, 1) : results;

writeFileSync(outputPath, JSON.stringify(selectedResults, null, 2));
console.log(`Converted ${selectedResults.length} Heliodor metrics → ${outputPath}`);
