#!/usr/bin/env node
/**
 * Convert Verilator benchmark output to github-action-benchmark
 * `customSmallerIsBetter` format with µs units.
 *
 * Input format (one line per benchmark):
 *   BENCH <name> <nanoseconds>
 *
 * Usage: node scripts/convert-verilator-bench.mjs <input.txt> <output.json>
 */

import { readFileSync, writeFileSync } from "node:fs";

const [inputPath, outputPath] = process.argv.slice(2);

if (!inputPath || !outputPath) {
  console.error(
    "Usage: node convert-verilator-bench.mjs <input.txt> <output.json>",
  );
  process.exit(1);
}

const raw = readFileSync(inputPath, "utf8");

const results = [];

const re = /^BENCH\s+(\S+)\s+([\d.]+)/gm;
let match;
while ((match = re.exec(raw)) !== null) {
  const name = match[1];
  const ns = parseFloat(match[2]);
  const us = ns / 1000;

  results.push({
    name: `verilator/${name}`,
    unit: "us",
    value: us,
  });
}

writeFileSync(outputPath, JSON.stringify(results, null, 2));
console.log(`Converted ${results.length} benchmark(s) → ${outputPath}`);
