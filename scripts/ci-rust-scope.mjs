import { execFileSync } from "node:child_process";
import { dirname, relative } from "node:path";

import { classifyFiles } from "./ci-changes.mjs";

// Features the Rust test job enables. Each is passed only when a selected
// package can see the feature's package, as Cargo requires.
export const CI_FEATURES = Object.freeze([
  "celox/systemverilog",
  "celox-test-suite/verilator",
  "celox-test-suite/icarus",
]);

// Suffixes of the tests that `all_backends!` and `sv_backends!` generate for
// each backend or frontend of one celox integration test.
export const VARIANTS = Object.freeze([
  "native",
  "native_parallel",
  "cranelift",
  "cranelift_parallel",
  "wasm",
  "interp",
  "veryl",
  "sv",
]);

const NATIVE = Object.freeze({ variants: ["native", "native_parallel"] });
const SV = Object.freeze({ variants: ["sv"], binaries: ["systemverilog"] });

// A crate that only one backend or frontend of celox uses: a change to it
// needs only that variant of celox's integration tests, plus the tests that
// are not generated per variant. Any other crate celox depends on needs all
// of them.
export const VARIANT_CRATES = new Map([
  ["celox-backend-common", NATIVE],
  ["celox-backend-x86", NATIVE],
  ["celox-backend-arm64", NATIVE],
  [
    "celox-backend-cranelift",
    { variants: ["cranelift", "cranelift_parallel"] },
  ],
  ["celox-backend-wasm", { variants: ["wasm"] }],
  ["celox-frontend-sv", SV],
  ["celox-sv-analyzer", SV],
]);

/**
 * The workspace packages as `rustTestScope` reads them, from
 * `cargo metadata --no-deps`.
 */
export function workspacePackages(metadata, root) {
  const members = new Set(metadata.packages.map((pkg) => pkg.name));
  return metadata.packages.map((pkg) => ({
    name: pkg.name,
    dir: relative(root, dirname(pkg.manifest_path)).replaceAll("\\", "/"),
    // Every kind of edge: a dev-dependency change rebuilds the dependent's
    // tests too.
    dependencies: [
      ...new Set(
        pkg.dependencies
          .map((dep) => dep.name)
          .filter((name) => members.has(name)),
      ),
    ],
    // The edges that link the package's library into its dependents.
    libraryDependencies: [
      ...new Set(
        pkg.dependencies
          .filter((dep) => dep.kind !== "dev" && members.has(dep.name))
          .map((dep) => dep.name),
      ),
    ],
    tests: pkg.targets
      .filter((target) => target.kind.includes("test"))
      .map((target) => target.name),
  }));
}

/**
 * The Rust tests a pull request's changed files can affect.
 *
 * Returns `{ full: true }` when the change reaches outside what the workspace
 * graph describes (manifests, the lockfile, toolchain or shared test data),
 * and otherwise the packages to build, the features to enable, a nextest
 * filterset selecting the tests, and the packages whose library code changed
 * with the features they can take (for documentation tests). `packages` is empty when no Rust test is
 * affected.
 *
 * A change to a package's library code affects the package and everything
 * that depends on it. A change to one of its integration test files affects
 * only that test binary, and any other test, bench or example file only the
 * package's own tests.
 */
export function rustTestScope(files, packages) {
  const byName = new Map(packages.map((pkg) => [pkg.name, pkg]));
  const owners = [...packages].sort((a, b) => b.dir.length - a.dir.length);
  const dependents = new Map(packages.map((pkg) => [pkg.name, new Set()]));
  for (const pkg of packages) {
    for (const dep of pkg.dependencies) dependents.get(dep)?.add(pkg.name);
  }

  const sources = new Set();
  const wholeTests = new Set();
  const binaries = new Map();
  const addBinary = (name, binary) => {
    if (!binaries.has(name)) binaries.set(name, new Set());
    binaries.get(name).add(binary);
  };

  for (const raw of files) {
    const path = raw.replace(/^\.\//, "");
    if (!classifyFiles([path]).rust) continue;
    // lydite's tests read its examples and audits from the top-level folder.
    if (path.startsWith("lydite/")) {
      for (const pkg of packages) {
        if (pkg.name.startsWith("lydite")) wholeTests.add(pkg.name);
      }
      continue;
    }
    const owner = owners.find((pkg) => path.startsWith(`${pkg.dir}/`));
    if (!owner) return { full: true };
    const inner = path.slice(owner.dir.length + 1);
    const test = /^tests\/([^/]+)\.rs$/.exec(inner);
    if (test && owner.tests.includes(test[1])) {
      addBinary(owner.name, test[1]);
    } else if (
      owner.name === "celox" &&
      inner.startsWith("tests/frontends/systemverilog/")
    ) {
      addBinary(owner.name, "systemverilog");
    } else if (/^(?:tests|benches|examples)\//.test(inner)) {
      wholeTests.add(owner.name);
    } else {
      sources.add(owner.name);
    }
  }

  // Everything each changed library reaches through its dependents.
  const affected = new Set();
  const celoxSeeds = [];
  for (const seed of sources) {
    const reached = new Set([seed]);
    const queue = [seed];
    while (queue.length > 0) {
      for (const next of dependents.get(queue.pop()) ?? []) {
        if (!reached.has(next)) {
          reached.add(next);
          queue.push(next);
        }
      }
    }
    for (const name of reached) affected.add(name);
    if (reached.has("celox")) celoxSeeds.push(seed);
  }

  const terms = [];
  const selected = new Set();
  const narrowCelox =
    affected.has("celox") &&
    !wholeTests.has("celox") &&
    celoxSeeds.every((seed) => VARIANT_CRATES.has(seed));
  for (const name of [...new Set([...affected, ...wholeTests])].sort()) {
    selected.add(name);
    if (name === "celox" && narrowCelox) continue;
    terms.push(`package(${name})`);
  }
  if (narrowCelox) {
    const variants = new Set();
    const celoxBinaries = new Set(binaries.get("celox") ?? []);
    for (const seed of celoxSeeds) {
      const scope = VARIANT_CRATES.get(seed);
      for (const variant of scope.variants) variants.add(variant);
      for (const binary of scope.binaries ?? []) celoxBinaries.add(binary);
    }
    const parts = [
      `test(/::(${[...variants].sort().join("|")})$/)`,
      `not test(/::(${VARIANTS.join("|")})$/)`,
      ...[...celoxBinaries].sort().map((binary) => `binary_id(celox::${binary})`),
    ];
    terms.push(`(package(celox) & (${parts.join(" | ")}))`);
    binaries.delete("celox");
  }
  for (const [name, names] of [...binaries].sort()) {
    if (selected.has(name)) continue;
    selected.add(name);
    for (const binary of [...names].sort()) {
      terms.push(`binary_id(${name}::${binary})`);
    }
  }

  // Cargo accepts `pkg/feature` only when a selected package can see `pkg`.
  const features = (names) => {
    const visible = new Set(names);
    for (const name of names) {
      for (const dep of byName.get(name).dependencies) visible.add(dep);
    }
    return CI_FEATURES.filter((feature) => visible.has(feature.split("/")[0]));
  };
  return {
    full: false,
    packages: [...selected].sort(),
    features: features(selected),
    filter: terms.join(" | "),
    libraries: [...affected].sort(),
    libraryFeatures: features(affected),
  };
}

/** Changed files between two commits, or null when they cannot be compared. */
export function changedFilesBetween(base, head) {
  const sha = /^[0-9a-f]{40,64}$/i;
  if (!sha.test(base) || !sha.test(head) || /^0+$/.test(base)) return null;
  try {
    return execFileSync("git", ["diff", "--name-only", "-z", base, head], {
      encoding: "utf8",
    })
      .split("\0")
      .filter(Boolean);
  } catch {
    return null;
  }
}

/** The workspace packages of the repository checked out at `root`. */
export function readWorkspace(root) {
  const metadata = JSON.parse(
    execFileSync(
      "cargo",
      ["metadata", "--no-deps", "--locked", "--format-version", "1"],
      { cwd: root, encoding: "utf8" },
    ),
  );
  return workspacePackages(metadata, root);
}
