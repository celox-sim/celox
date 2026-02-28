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

for (const file of raw.testResults ?? []) {
  for (const suite of file.children ?? []) {
    for (const task of suite.children ?? []) {
      const r = task.result?.benchmark;
      if (!r) continue;

      // r.mean is in seconds — convert to milliseconds
      const meanMs = r.mean * 1000;
      const rme = r.rme ?? 0;

      results.push({
        name: `ts/${task.name}`,
        unit: "ms",
        value: meanMs,
        range: `± ${rme.toFixed(1)}%`,
        extra: `${r.samples} samples`,
      });
    }
  }
}

writeFileSync(outputPath, JSON.stringify(results, null, 2));
console.log(`Converted ${results.length} benchmark(s) → ${outputPath}`);
