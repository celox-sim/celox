import { appendFileSync } from "node:fs";
import { fileURLToPath, pathToFileURL } from "node:url";
import { isVersionOnlyRelease } from "./ci-changes.mjs";
import { changedFilesBetween, readWorkspace } from "./ci-rust-scope.mjs";

export const isLydite = (name) =>
  name === "lydite" || name.startsWith("lydite-");
const all = () => ({ core: true, bridge: true });

// Follow actual Cargo edges, including optional, build and dev dependencies.
// Celox consumes none of the proof engine; only lydite-celox consumes Celox.
export function lyditeScope(files, packages) {
  if (files === null) return all();
  const byName = new Map(packages.map((pkg) => [pkg.name, pkg]));
  const closure = (roots) => {
    const seen = new Set(roots);
    const queue = [...roots];
    while (queue.length) {
      for (const dep of byName.get(queue.pop())?.dependencies ?? []) {
        if (!seen.has(dep)) {
          seen.add(dep);
          queue.push(dep);
        }
      }
    }
    return seen;
  };
  const core = closure(
    packages
      .filter((p) => isLydite(p.name) && p.name !== "lydite-celox")
      .map((p) => p.name),
  );
  const bridge = closure(["lydite-celox"]);
  const result = { core: false, bridge: false };
  const owners = [...packages].sort((a, b) => b.dir.length - a.dir.length);
  for (const raw of files) {
    const path = raw.replace(/^\.\//, "");
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
        "flake.nix",
        "flake.lock",
        ".github/workflows/lydite-checks.yml",
        ".github/workflows/ci.yml",
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

export function pullRequestScope(files, packages) {
  // Release PRs and their queue drafts only bump versions.
  if (files !== null && isVersionOnlyRelease(files))
    return { core: false, bridge: false };
  const scope = lyditeScope(files, packages);
  if (files === null) return scope;
  return { ...scope, bridge: scope.bridge && files.some(ownsBridge) };
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
      scope = pullRequestScope(
        changedFilesBetween(process.argv[2] ?? "", process.argv[3] ?? ""),
        readWorkspace(fileURLToPath(new URL("../", import.meta.url))),
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
