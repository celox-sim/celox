import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { appendResult, comparisonHistory, publishResult } from "./publish-heliodor-bench.mjs";

const header =
  "runner\ttest\tstatus\telapsed_ns\tlog\tsemantic_status\texit_status\tprocess_elapsed_ns\treported_elapsed_ns\tcompile_elapsed_ns\texecute_elapsed_ns\tjit_execute_elapsed_ns\n";
function row(runner, { workload = "test_soc_linux_boot", success = true } = {}) {
  return `${runner}\t${workload}\t${success ? 0 : 124}\t400\tlog\t${success ? "pass" : "timeout"}\t${success ? 0 : 124}\t400\t4000000\t1000000\t2000000\t1500000\n`;
}

test("publishes each successful backend without requiring comparison partners", () => {
  const directory = mkdtempSync(join(tmpdir(), "heliodor-partial-"));
  try {
    const input = join(directory, "results.tsv");
    const output = join(directory, "metrics.json");
    const convert = (content, arch, options = []) => {
      writeFileSync(input, header + content);
      execFileSync(
        process.execPath,
        [
          "scripts/convert-heliodor-bench.mjs",
          input,
          output,
          "--partial",
          "--arch",
          arch,
          ...options,
        ],
        { stdio: "pipe" },
      );
      return JSON.parse(readFileSync(output, "utf8"));
    };
    for (const arch of ["x86_64", "aarch64"]) {
      for (const [runner, platform, count] of [
        ["celox", `native-${arch}`, arch === "x86_64" ? 3 : 2],
        ["veryl-cc-sync", `veryl-cc-${arch}`, 2],
        ["celox-tiered", arch === "x86_64" ? "celox-tiered" : "celox-tiered-aarch64", 3],
        ["veryl-cc-tiered", `veryl-tiered-${arch}`, 3],
      ]) {
        const result = convert(row(runner), arch);
        assert.equal(result.length, count);
        assert.equal(
          result.find((x) => x.name === `heliodor-${platform}/heliodor_suite_linux_boot_execution`)
            .value,
          2,
        );
        assert.deepEqual(convert(row(runner, { success: false }), arch), []);
      }
    }
    const successful = row("veryl-cc-sync");
    const failed = row("celox", { success: false });
    assert.deepEqual(convert(successful + failed, "x86_64"), convert(successful, "x86_64"));
    assert.deepEqual(convert(successful + failed, "x86_64", ["--latest"]), []);
    assert.deepEqual(
      convert(failed + successful, "x86_64", ["--latest"]),
      convert(successful, "x86_64"),
    );
    assert.throws(() => convert(successful + successful, "x86_64"));
    assert.throws(() => convert(successful.replace("1000000", "0"), "x86_64"));
    assert.throws(() => convert(row("celox-tiered").replace("4000000", "1"), "x86_64"));
    assert.throws(() => convert(successful, "unknown"));
    assert.throws(() => convert(row("unknown"), "x86_64"));
    assert.throws(() => convert(row("celox", { workload: "other" }), "x86_64"));
    const multi = convert(
      row("celox", { workload: "test_soc_smp_linux_boot_8hart" }) +
        row("veryl-cc-sync", { workload: "test_soc_66_linux_boot" }),
      "aarch64",
    );
    assert.equal(multi.length, 4);
    assert.equal(new Set(multi.map((x) => x.name)).size, 4);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test("history preserves unrelated results and the global latest update", () => {
  const original = {
    lastUpdate: 200,
    entries: { "Rust Benchmarks": [{ date: 200 }], "Heliodor Benchmarks": [{ date: 100 }] },
  };
  const updated = JSON.parse(
    appendResult("window.BENCHMARK_DATA = " + JSON.stringify(original), { date: 150 }).slice(
      "window.BENCHMARK_DATA = ".length,
    ),
  );
  assert.equal(updated.lastUpdate, 200);
  assert.deepEqual(updated.entries["Rust Benchmarks"], original.entries["Rust Benchmarks"]);
  assert.deepEqual(updated.entries["Heliodor Benchmarks"], [{ date: 100 }, { date: 150 }]);
  assert.throws(() => appendResult("unexpected", { date: 150 }));
});

test("concurrent publishers preserve results and compare by measurement time", () => {
  const directory = mkdtempSync(join(tmpdir(), "heliodor-publish-test-"));
  const remote = join(directory, "remote.git");
  const seed = join(directory, "seed");
  const git = (...args) => execFileSync("git", args, { encoding: "utf8", stdio: "pipe" });
  try {
    git("init", "--bare", "--quiet", remote);
    git("init", "--quiet", "--initial-branch=gh-pages", seed);
    git("-C", seed, "config", "user.name", "Fixture");
    git("-C", seed, "config", "user.email", "fixture@example.invalid");
    mkdirSync(join(seed, "dev/bench"), { recursive: true });
    const original = { lastUpdate: 1, entries: { "Rust Benchmarks": [{ date: 1 }] } };
    writeFileSync(
      join(seed, "dev/bench/data.js"),
      "window.BENCHMARK_DATA = " + JSON.stringify(original),
    );
    writeFileSync(join(seed, "index.html"), "docs stay intact");
    git("-C", seed, "add", ".");
    git("-C", seed, "-c", "commit.gpgsign=false", "commit", "--quiet", "-m", "seed");
    git("-C", seed, "push", "--quiet", remote, "HEAD:gh-pages");
    // The older measurement loses its first push to a newer measurement of
    // the same workload, then appends last when its retry succeeds.
    const first = {
      commit: { id: "newer" },
      date: 20,
      benches: [{ name: "jit-linux-boot", value: 1, unit: "ms" }],
    };
    const second = {
      commit: { id: "older" },
      date: 10,
      benches: [{ name: "jit-linux-boot", value: 2, unit: "ms" }],
    };
    let attempts = 0;
    publishResult(remote, second, {
      beforePush(attempt) {
        attempts++;
        if (attempt === 0) publishResult(remote, first);
      },
    });
    assert.equal(attempts, 2);
    git("-C", seed, "fetch", "--quiet", remote, "gh-pages");
    const data = git("-C", seed, "show", "FETCH_HEAD:dev/bench/data.js");
    const parsed = JSON.parse(data.slice("window.BENCHMARK_DATA = ".length));
    assert.deepEqual(parsed.entries["Heliodor Benchmarks"], [first, second]);
    assert.deepEqual(
      comparisonHistory(data, [{ name: "jit-linux-boot" }]).entries["Heliodor Benchmarks"],
      [second, first],
    );
    assert.deepEqual(parsed.entries["Rust Benchmarks"], original.entries["Rust Benchmarks"]);
    assert.equal(git("-C", seed, "show", "FETCH_HEAD:index.html"), "docs stay intact");
    publishResult("nonexistent-remote", { benches: [] });
    // Exercise the real CI entry point, including conversion and commit data.
    git("-C", seed, "remote", "add", "origin", remote);
    const input = join(directory, "results.tsv");
    writeFileSync(input, header + row("celox", { success: false }) + row("veryl-cc-sync"));
    execFileSync(
      process.execPath,
      [fileURLToPath(new URL("./publish-heliodor-bench.mjs", import.meta.url)), input, "aarch64"],
      {
        cwd: seed,
        stdio: "pipe",
        env: {
          ...process.env,
          GITHUB_REF: "refs/heads/master",
          GITHUB_EVENT_NAME: "workflow_dispatch",
          GITHUB_SERVER_URL: "https://github.com",
          GITHUB_REPOSITORY: "fixture/repo",
        },
      },
    );
    git("-C", seed, "fetch", "--quiet", remote, "gh-pages");
    const published = JSON.parse(
      git("-C", seed, "show", "FETCH_HEAD:dev/bench/data.js").slice(
        "window.BENCHMARK_DATA = ".length,
      ),
    );
    const latest = published.entries["Heliodor Benchmarks"].at(-1);
    assert.equal(latest.benches.length, 2);
    assert.ok(latest.benches.every((bench) => bench.name.startsWith("heliodor-veryl-cc-aarch64/")));
    assert.equal(latest.commit.author.name, "Fixture");
    assert.equal(latest.commit.committer.email, "fixture@example.invalid");
    assert.equal(latest.commit.id, git("-C", seed, "rev-parse", "HEAD").trim());
    assert.equal(latest.commit.url, `https://github.com/fixture/repo/commit/${latest.commit.id}`);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test("publication refuses pull requests and non-master branches", () => {
  for (const [event, ref] of [
    ["pull_request", "refs/pull/1/merge"],
    ["workflow_dispatch", "refs/heads/develop"],
    ["push", "refs/heads/master"],
  ]) {
    const result = spawnSync(
      process.execPath,
      ["scripts/publish-heliodor-bench.mjs", "unused.tsv", "x86_64"],
      {
        encoding: "utf8",
        env: { ...process.env, GITHUB_EVENT_NAME: event, GITHUB_REF: ref },
      },
    );
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /requires a scheduled or manual master run/);
  }
});

test("PR comparison finds the latest matching workload among partial publications", () => {
  const matching = { commit: { id: "match" }, benches: [{ name: "jit-linux-boot", value: 1 }] };
  const unrelated = { commit: { id: "newer" }, benches: [{ name: "arm64-other", value: 2 }] };
  const data = { entries: { "Heliodor Benchmarks": [matching, unrelated] } };
  const source = "window.BENCHMARK_DATA = " + JSON.stringify(data);
  assert.deepEqual(
    comparisonHistory(source, [{ name: "jit-linux-boot" }]).entries["Heliodor Benchmarks"],
    [matching],
  );
  assert.deepEqual(
    comparisonHistory(source, [{ name: "missing" }]).entries["Heliodor Benchmarks"],
    [],
  );
});
