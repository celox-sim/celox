import { execFileSync } from "node:child_process";
import { appendFileSync } from "node:fs";
import { fileURLToPath, pathToFileURL } from "node:url";
import { isVersionOnlyRelease } from "./ci-changes.mjs";
import { changedFilesBetween, readWorkspace } from "./ci-rust-scope.mjs";

export const isLydite = (name) =>
  name === "lydite" || name.startsWith("lydite-");
const all = () => ({ core: true, bridge: true });

// Cargo.lock's `[[package]]` entries, keyed by "name version", with the names
// each one depends on.
export function parseLock(text) {
  const entries = new Map();
  for (const block of text.split(/^\[\[package\]\]$/m).slice(1)) {
    const name = /^name = "([^"]+)"$/m.exec(block)?.[1];
    const version = /^version = "([^"]+)"$/m.exec(block)?.[1];
    if (!name || !version) throw new Error("Unrecognized Cargo.lock entry");
    const list = /^dependencies = \[\n([\s\S]*?)^\]$/m.exec(block)?.[1] ?? "";
    const dependencies = [...list.matchAll(/^ "([^" ]+)/gm)].map((m) => m[1]);
    entries.set(`${name} ${version}`, { name, block: block.trim(), dependencies });
  }
  return entries;
}

// The packages whose lock entries differ, and the dependency graph of both
// lockfiles, so a lockfile edit selects only the lanes that reach it.
export function lockChanges(baseText, headText) {
  const base = parseLock(baseText);
  const head = parseLock(headText);
  const changed = new Set();
  const graph = new Map();
  for (const [key, entry] of [...base, ...head]) {
    if (base.get(key)?.block !== head.get(key)?.block) changed.add(entry.name);
    if (!graph.has(entry.name)) graph.set(entry.name, new Set());
    for (const dep of entry.dependencies) graph.get(entry.name).add(dep);
  }
  return { changed, graph };
}

// Follow actual Cargo edges, including optional, build and dev dependencies.
// Celox consumes none of the proof engine; only lydite-celox consumes Celox.
// `lock` is lockChanges() of the diff; without it a Cargo.lock edit runs both.
export function lyditeScope(files, packages, lock = null) {
  if (files === null) return all();
  const byName = new Map(packages.map((pkg) => [pkg.name, pkg]));
  const closure = (roots, edges = (name) => byName.get(name)?.dependencies) => {
    const seen = new Set(roots);
    const queue = [...roots];
    while (queue.length) {
      for (const dep of edges(queue.pop()) ?? []) {
        if (!seen.has(dep)) {
          seen.add(dep);
          queue.push(dep);
        }
      }
    }
    return seen;
  };
  const coreRoots = packages
    .filter((p) => isLydite(p.name) && p.name !== "lydite-celox")
    .map((p) => p.name);
  const core = closure(coreRoots);
  const bridge = closure(["lydite-celox"]);
  const result = { core: false, bridge: false };
  const owners = [...packages].sort((a, b) => b.dir.length - a.dir.length);
  for (const raw of files) {
    const path = raw.replace(/^\.\//, "");
    // Celox's SystemVerilog corpus is independent of Lydite's gates, whose
    // fixtures live under lydite/conformance/.
    if (path.startsWith("conformance/sv-tests/")) continue;
    if (path === "Cargo.lock" && lock !== null) {
      const reaches = (roots) => {
        const seen = closure(roots, (name) => lock.graph.get(name));
        return [...lock.changed].some((name) => seen.has(name));
      };
      result.core ||= reaches(coreRoots);
      result.bridge ||= reaches(["lydite-celox"]);
      continue;
    }
    if (
      path.startsWith("lydite/") ||
      (path.startsWith("crates/lydite") &&
        !path.startsWith("crates/lydite-celox/"))
    )
      return all();
    if (
      [
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
        ".github/workflows/lydite-checks.yml",
        ".config/nextest.toml",
      ].includes(path) ||
      [
        ".cargo/",
        ".github/actions/setup-rust/",
        "scripts/ci-lydite-changes.",
        "scripts/ci-rust-scope.",
        "scripts/ci-changes.",
      ].some((prefix) => path.startsWith(prefix))
    )
      return all();
    const owner = owners.find((pkg) => path.startsWith(`${pkg.dir}/`));
    if (owner) {
      const inner = path.slice(owner.dir.length + 1);
      // Consumer tests do not depend on an upstream crate's private tests/docs.
      if (
        !isLydite(owner.name) &&
        (/^(tests|benches|examples)\//.test(inner) || inner.endsWith(".md"))
      )
        continue;
      result.core ||= core.has(owner.name);
      result.bridge ||= bridge.has(owner.name);
    } else if (
      ["crates/", "vendor/", "testdata/", "conformance/"].some((prefix) =>
        path.startsWith(prefix),
      ) ||
      /Cargo\.(toml|lock)$/.test(path)
    ) {
      // Unmapped source or dependency changes cannot silently bypass checks.
      return all();
    }
  }
  return result;
}

// Celox dependency changes reach the bridge, but its replay and proof corpus
// are too heavy for every Celox PR. PRs run the bridge only for Lydite-owned
// sources; daily runs cover Celox and CI changes and report failures.
const ownsBridge = (path) =>
  ["lydite/", "crates/lydite"].some((prefix) =>
    path.replace(/^\.\//, "").startsWith(prefix),
  );

export function pullRequestScope(files, packages, lock = null) {
  // Release PRs and their queue drafts only bump versions.
  if (files !== null && isVersionOnlyRelease(files))
    return { core: false, bridge: false };
  const scope = lyditeScope(files, packages, lock);
  if (files === null) return scope;
  return { ...scope, bridge: scope.bridge && files.some(ownsBridge) };
}

// The lockfile at the merge base and at head, as changedFilesBetween compares.
function readLockChanges(base, head) {
  const git = (...args) => execFileSync("git", args, { encoding: "utf8" });
  try {
    const fork = git("merge-base", base, head).trim();
    return lockChanges(git("show", `${fork}:Cargo.lock`), git("show", `${head}:Cargo.lock`));
  } catch (error) {
    console.warn(`Unable to compare Cargo.lock: ${error.message}`);
    return null;
  }
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  let scope = all();
  if (
    !["schedule", "workflow_dispatch"].includes(process.env.GITHUB_EVENT_NAME)
  ) {
    try {
      const [base, head] = [process.argv[2] ?? "", process.argv[3] ?? ""];
      const files = changedFilesBetween(base, head);
      scope = pullRequestScope(
        files,
        readWorkspace(fileURLToPath(new URL("../", import.meta.url))),
        files?.includes("Cargo.lock") ? readLockChanges(base, head) : null,
      );
    } catch (error) {
      console.warn(`Unable to scope Lydite checks: ${error.message}`);
    }
  }
  const output = Object.entries(scope)
    .map(([key, value]) => `${key}=${value}`)
    .join("\n");
  console.log(output);
  if (process.env.GITHUB_OUTPUT)
    appendFileSync(process.env.GITHUB_OUTPUT, `${output}\n`);
}
