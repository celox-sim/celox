#!/usr/bin/env node
import { execFileSync } from "node:child_process";
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const dataPath = "dev/bench/data.js";
const prefix = "window.BENCHMARK_DATA = ";

function readData(source) {
  if (!source.startsWith(prefix)) throw new Error("Unexpected benchmark data format");
  return JSON.parse(source.slice(prefix.length));
}

// Partial publication means the latest entry may concern another workload.
// Keep PR comparisons pointed at the latest matching successful measurement.
export function comparisonHistory(source, metrics) {
  const data = readData(source);
  data.entries["Heliodor Benchmarks"] = (data.entries["Heliodor Benchmarks"] ?? [])
    .filter((entry) =>
      metrics.every((metric) => entry.benches.some((bench) => bench.name === metric.name)),
    )
    // A retried push can append an older measurement after a newer one.
    // The comparison action searches backward, so keep sample time order.
    .sort((a, b) => a.date - b.date);
  return data;
}

export function appendResult(source, entry) {
  const data = readData(source);
  const history = (data.entries["Heliodor Benchmarks"] ??= []);
  history.push(entry);
  data.lastUpdate = Math.max(data.lastUpdate ?? 0, entry.date);
  return prefix + JSON.stringify(data, null, 2) + "\n";
}

// Multiple matrix jobs publish independently. A rejected fast-forward push is
// retried against the new branch tip, preserving every other job's results.
export function publishResult(remote, entry, { beforePush = () => {} } = {}) {
  if (!entry.benches.length) return;
  const directory = mkdtempSync(join(tmpdir(), "heliodor-publish-"));
  const git = (...args) =>
    execFileSync("git", ["-C", directory, ...args], { encoding: "utf8", stdio: "pipe" });
  try {
    git("init", "--quiet");
    git("config", "user.name", "github-actions[bot]");
    git("config", "user.email", "41898282+github-actions[bot]@users.noreply.github.com");
    // checkout's persisted credentials are scoped to its repository. Pass the
    // HTTP auth headers to this temporary repository without printing them.
    let config = "";
    try {
      config = execFileSync(
        "git",
        ["config", "--local", "--includes", "--get-regexp", "^http\\..*\\.extraheader$"],
        { encoding: "utf8", stdio: "pipe" },
      );
    } catch (error) {
      if (error.status !== 1) throw error;
    }
    for (const line of config.trim().split("\n").filter(Boolean)) {
      const separator = line.indexOf(" ");
      git("config", line.slice(0, separator), line.slice(separator + 1));
    }
    git("remote", "add", "origin", remote);
    for (let attempt = 0; attempt < 20; attempt++) {
      git("fetch", "--quiet", "--depth=1", "origin", "gh-pages");
      git("checkout", "--quiet", "-B", "publish", "FETCH_HEAD");
      const path = join(directory, dataPath);
      mkdirSync(dirname(path), { recursive: true });
      writeFileSync(path, appendResult(readFileSync(path, "utf8"), entry));
      git("add", dataPath);
      git(
        "-c",
        "commit.gpgsign=false",
        "commit",
        "--quiet",
        "-m",
        "chore(bench): publish completed Heliodor result",
      );
      beforePush(attempt);
      try {
        git("push", "--quiet", "origin", "HEAD:gh-pages");
        return;
      } catch (error) {
        if (attempt === 19 || !/rejected|fetch first|non-fast-forward/.test(String(error.stderr)))
          throw error;
      }
    }
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const [results, arch] = process.argv.slice(2);
  if (!results || !arch) throw new Error("Usage: publish-heliodor-bench.mjs <results.tsv> <arch>");
  if (
    process.env.GITHUB_REF !== "refs/heads/master" ||
    !["schedule", "workflow_dispatch"].includes(process.env.GITHUB_EVENT_NAME)
  ) {
    throw new Error("Heliodor publication requires a scheduled or manual master run");
  }
  const directory = mkdtempSync(join(tmpdir(), "heliodor-convert-"));
  try {
    const output = join(directory, "result.json");
    execFileSync(
      process.execPath,
      [
        fileURLToPath(new URL("./convert-heliodor-bench.mjs", import.meta.url)),
        results,
        output,
        "--partial",
        "--latest",
        "--arch",
        arch,
      ],
      { stdio: "inherit" },
    );
    const benches = JSON.parse(readFileSync(output, "utf8"));
    if (benches.length) {
      const git = (...args) => execFileSync("git", args, { encoding: "utf8" }).trim();
      const id = git("rev-parse", "HEAD");
      const remote = git("remote", "get-url", "origin");
      const [authorName, authorEmail, committerName, committerEmail] = git(
        "show",
        "-s",
        "--format=%an%x00%ae%x00%cn%x00%ce",
        "HEAD",
      ).split("\0");
      publishResult(remote, {
        commit: {
          author: { name: authorName, email: authorEmail },
          committer: { name: committerName, email: committerEmail },
          id,
          message: git("show", "-s", "--format=%s", "HEAD"),
          timestamp: git("show", "-s", "--format=%cI", "HEAD"),
          url: `${process.env.GITHUB_SERVER_URL}/${process.env.GITHUB_REPOSITORY}/commit/${id}`,
        },
        date: Date.now(),
        tool: "customSmallerIsBetter",
        benches,
      });
      console.log(`Published ${benches.length} completed Heliodor metrics`);
    }
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}
