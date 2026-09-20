#!/usr/bin/env node
/**
 * Convert Vitest bench JSON reporter output to github-action-benchmark
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

if (raw.success !== true || !Array.isArray(raw.testResults)) {
  throw new Error("Expected a successful Vitest JSON report");
}

for (const file of raw.testResults) {
  for (const test of file.assertionResults ?? []) {
    for (const bench of (test.benchmarks ?? []).flatMap((group) => group.tasks)) {
      // Vitest reports latency in milliseconds — convert to µs.
      const { mean, rme, samplesCount } = bench.latency;
      if (
        !Number.isFinite(mean) || mean < 0 ||
        !Number.isFinite(rme) || rme < 0 ||
        !Number.isInteger(samplesCount) || samplesCount <= 0
      ) {
        throw new Error(`Invalid benchmark statistics: ${bench.name}`);
      }
      results.push({
        name: `ts/${bench.name}`,
        unit: "us",
        value: mean * 1000,
        range: `± ${rme.toFixed(1)}%`,
        extra: `${samplesCount} samples`,
      });
    }
  }
}

if (results.length === 0) {
  throw new Error("No benchmark results found");
}

writeFileSync(outputPath, JSON.stringify(results, null, 2));
console.log(`Converted ${results.length} benchmark(s) → ${outputPath}`);
