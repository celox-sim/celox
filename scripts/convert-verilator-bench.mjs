#!/usr/bin/env node
/**
 * Convert Verilator benchmark output to github-action-benchmark
 * `customSmallerIsBetter` format with µs units.
 *
 * Input format:
 *   BENCH <name> <nanoseconds>          ← build time lines
 *   { ... Google Benchmark JSON ... }   ← runtime benchmark blocks
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

// ── Parse BENCH lines (build times) ──
const benchRe = /^BENCH\s+(\S+)\s+([\d.]+)/gm;
let m;
while ((m = benchRe.exec(raw)) !== null) {
  results.push({
    name: `verilator/${m[1]}`,
    unit: "us",
    value: parseFloat(m[2]) / 1000,
  });
}

// ── Parse Google Benchmark JSON blocks ──
// Each binary emits one JSON object; collect all lines between outermost { }
let depth = 0;
let jsonLines = [];
let inJson = false;

for (const line of raw.split("\n")) {
  if (!inJson && line.trimStart().startsWith("{")) {
    inJson = true;
    depth = 0;
  }
  if (inJson) {
    jsonLines.push(line);
    for (const ch of line) {
      if (ch === "{") depth++;
      else if (ch === "}") depth--;
    }
    if (depth === 0) {
      try {
        const obj = JSON.parse(jsonLines.join("\n"));
        for (const bm of obj.benchmarks ?? []) {
          // Skip aggregate rows (mean/median/stddev) when repetitions are used
          if (bm.run_type === "aggregate") continue;
          const timeNs = bm.real_time; // already in time_unit=ns
          const cleanName = bm.name.split("/")[0]; // strip /iterations:N/manual_time etc.
          results.push({
            name: `verilator/${cleanName}`,
            unit: "us",
            value: timeNs / 1000,
          });
        }
      } catch (_) {
        // ignore malformed JSON
      }
      jsonLines = [];
      inJson = false;
    }
  }
}

writeFileSync(outputPath, JSON.stringify(results, null, 2));
console.log(`Converted ${results.length} benchmark(s) → ${outputPath}`);
