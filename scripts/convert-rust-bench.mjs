#!/usr/bin/env node
/**
 * Convert Criterion bencher-format output to github-action-benchmark
 * `customSmallerIsBetter` format with µs units.
 *
 * Input format (one line per benchmark):
 *   test <name> ... bench: <ns> ns/iter (+/- <range>)
 *
 * Usage: node scripts/convert-rust-bench.mjs <input.txt> <output.json>
 */

import { readFileSync, writeFileSync } from "node:fs";

const [inputPath, outputPath] = process.argv.slice(2);

if (!inputPath || !outputPath) {
  console.error(
    "Usage: node convert-rust-bench.mjs <input.txt> <output.json>",
  );
  process.exit(1);
}

const raw = readFileSync(inputPath, "utf8");

const results = [];

// Match lines like: test <name> ... bench:       123 ns/iter (+/- 45)
const re =
  /^test\s+(\S+)\s+\.\.\.\s+bench:\s+([\d,]+)\s+ns\/iter\s+\(\+\/-\s+([\d,]+)\)/gm;

let match;
while ((match = re.exec(raw)) !== null) {
  let name = match[1];
  const ns = Number(match[2].replace(/,/g, ""));
  const range = Number(match[3].replace(/,/g, ""));

  // Convert ns → µs
  const us = ns / 1000;
  const rangeUs = range / 1000;

  // DSE benchmarks get a separate runtime prefix so the dashboard
  // can show them as "Rust(DSE)".  Rename dse_ → simulation_ so they
  // group with the baseline chart cards.
  let prefix = "rust";
  if (name.startsWith("dse_")) {
    prefix = "rust-dse";
    name = "simulation_" + name.slice("dse_".length);
  }

  results.push({
    name: `${prefix}/${name}`,
    unit: "us",
    value: us,
    range: `± ${rangeUs.toFixed(3)} us`,
  });
}

writeFileSync(outputPath, JSON.stringify(results, null, 2));
console.log(`Converted ${results.length} benchmark(s) → ${outputPath}`);
