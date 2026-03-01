#!/usr/bin/env node
/**
 * Convert Vitest bench `--outputJson` output to github-action-benchmark
 * `customSmallerIsBetter` format.
 *
 * Usage: node scripts/convert-bench.mjs <input.json> <output.json>
 */

import { readFileSync, writeFileSync } from "node:fs";

const [inputPath, outputPath] = process.argv.slice(2);

if (!inputPath || !outputPath) {
  console.error("Usage: node convert-bench.mjs <input.json> <output.json>");
  process.exit(1);
}

const raw = JSON.parse(readFileSync(inputPath, "utf8"));

const results = [];

for (const file of raw.files ?? []) {
  for (const group of file.groups ?? []) {
    for (const bench of group.benchmarks ?? []) {
      // bench.mean is in milliseconds — convert to µs
      const meanUs = bench.mean * 1000;
      const rme = bench.rme ?? 0;

      results.push({
        name: `ts/${bench.name}`,
        unit: "us",
        value: meanUs,
        range: `± ${rme.toFixed(1)}%`,
        extra: `${bench.sampleCount} samples`,
      });
    }
  }
}

writeFileSync(outputPath, JSON.stringify(results, null, 2));
console.log(`Converted ${results.length} benchmark(s) → ${outputPath}`);
