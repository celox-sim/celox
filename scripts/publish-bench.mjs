#!/usr/bin/env node
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { publishResult } from "./publish-heliodor-bench.mjs";

if (process.env.GITHUB_REF !== "refs/heads/master" ||
    !["schedule", "workflow_dispatch"].includes(process.env.GITHUB_EVENT_NAME)) {
  throw new Error("Benchmark publication requires a scheduled or manual master run");
}
const files = process.argv.slice(2);
if (files.length !== 3) {
  throw new Error("Usage: publish-bench.mjs <rust.json> <verilator.json> <typescript.json>");
}
const names = ["Rust Benchmarks", "Verilator Benchmarks", "TypeScript Benchmarks"];
// Validate every converted artifact before publishing any of this run.
const samples = files.map(file => {
  const benches = JSON.parse(readFileSync(file, "utf8"));
  if (!Array.isArray(benches) || !benches.length || benches.some(bench =>
    typeof bench.name !== "string" || !bench.name || typeof bench.unit !== "string" ||
    !bench.unit || typeof bench.value !== "number" || !Number.isFinite(bench.value) || bench.value < 0)) {
    throw new Error(`Invalid benchmark metrics: ${file}`);
  }
  return benches;
});
const git = (...args) => execFileSync("git", args, { encoding: "utf8" }).trim();
const id = git("rev-parse", "HEAD");
const remote = git("remote", "get-url", "origin");
const [authorName, authorEmail, committerName, committerEmail] = git(
  "show", "-s", "--format=%an%x00%ae%x00%cn%x00%ce", "HEAD",
).split("\0");
const commit = {
  author: { name: authorName, email: authorEmail },
  committer: { name: committerName, email: committerEmail },
  id,
  message: git("show", "-s", "--format=%s", "HEAD"),
  timestamp: git("show", "-s", "--format=%cI", "HEAD"),
  url: `${process.env.GITHUB_SERVER_URL}/${process.env.GITHUB_REPOSITORY}/commit/${id}`,
};
const date = Date.now();
for (const [index, benches] of samples.entries()) {
  publishResult(remote, { commit, date, tool: "customSmallerIsBetter", benches }, {
    historyName: names[index],
  });
}
console.log("Published Rust, Verilator, and TypeScript benchmark results");
