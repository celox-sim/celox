import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";

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
  const libraryDependents = new Map(packages.map((pkg) => [pkg.name, new Set()]));
  for (const pkg of packages) {
    for (const dep of pkg.dependencies) dependents.get(dep)?.add(pkg.name);
    for (const dep of pkg.libraryDependencies ?? pkg.dependencies) {
      libraryDependents.get(dep)?.add(pkg.name);
    }
  }

  const sources = new Set();
  const wholeTests = new Set();
  const binaries = new Map();
  const moduleFilters = new Map();
  const addBinary = (name, binary) => {
    if (!binaries.has(name)) binaries.set(name, new Set());
    binaries.get(name).add(binary);
  };

  for (const raw of files) {
    const path = raw.replace(/^\.\//, "");
    if (!classifyFiles([path]).rust) continue;
    if (/(?:^|\/)Cargo\.(?:toml|lock)$/.test(path)) return { full: true };
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
    const sharedCase = /^src\/(veryl|sv)\/cases\/([^/]+)\.vtest$/.exec(inner);
    if (owner.name === "celox-test-suite" && sharedCase && owner.caseTargets) {
      const targets = owner.caseTargets[`${sharedCase[1]}/${sharedCase[2]}`];
      // Unknown/deleted groups retain the dependency-graph fallback.
      if (targets?.length) {
        wholeTests.add(owner.name); // Catalogue/parser and retained-report checks.
        for (const target of targets) {
          addBinary("celox", target);
          if (target === "systemverilog") {
            const modules = owner.caseModules?.[sharedCase[2]];
            if (modules?.length) {
              if (!moduleFilters.has("celox")) moduleFilters.set("celox", new Set());
              for (const module of modules) moduleFilters.get("celox")?.add(module);
            } else moduleFilters.set("celox", null);
          }
        }
        for (const pkg of packages) {
          // Other catalogue consumers still validate their own tests, but a
          // script's contents cannot change a Rust library API or its doctests.
          if (pkg.name !== "celox" && pkg.dependencies.includes(owner.name)) {
            const pending = [pkg.name];
            while (pending.length) {
              const name = pending.pop();
              if (wholeTests.has(name)) continue;
              wholeTests.add(name);
              pending.push(...(dependents.get(name) ?? []));
            }
          }
        }
        continue;
      }
    }
    const test = /^tests\/([^/]+)\.rs$/.exec(inner);
    // Insta prefixes snapshots with their integration-test binary name.
    const snapshot = /^tests\/snapshots\/([^/]+?)__[^/]+\.snap(?:\.new)?$/.exec(inner);
    if (test && owner.tests.includes(test[1])) {
      addBinary(owner.name, test[1]);
    } else if (snapshot && owner.tests.includes(snapshot[1])) {
      addBinary(owner.name, snapshot[1]);
    } else if (
      owner.name === "celox" &&
      inner.startsWith("tests/frontends/systemverilog/")
    ) {
      addBinary(owner.name, "systemverilog");
      const module = /^tests\/frontends\/systemverilog\/(\w+)\.rs$/.exec(inner)?.[1];
      // The adapter and unknown shared helpers affect every SV module.
      if (module && owner.svTestModules?.includes(module)) {
        if (!moduleFilters.has(owner.name)) moduleFilters.set(owner.name, new Set());
        moduleFilters.get(owner.name)?.add(module);
      } else {
        moduleFilters.set(owner.name, null);
      }
    } else if (/^(?:tests|benches|examples)\//.test(inner)) {
      wholeTests.add(owner.name);
    } else {
      sources.add(owner.name);
    }
  }

  // Everything each changed library reaches through its dependents.
  const libraries = new Set();
  for (const seed of sources) {
    const queue = [seed];
    libraries.add(seed);
    while (queue.length > 0) {
      for (const next of libraryDependents.get(queue.pop()) ?? []) {
        if (!libraries.has(next)) {
          libraries.add(next);
          queue.push(next);
        }
      }
    }
  }
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
      const modules = moduleFilters.get(name);
      const selector = `binary_id(${name}::${binary})`;
      // A direct edit to systemverilog.rs already selects the entire binary.
      const wholeBinary = files.some((file) => file.replace(/^\.\//, "") === `${byName.get(name).dir}/tests/${binary}.rs`);
      terms.push(binary === "systemverilog" && modules?.size && !wholeBinary
        ? `(${selector} & test(/^(${[...modules].sort().join("|")})::/))`
        : selector);
    }
  }

  // Cargo accepts `pkg/feature` only when a selected package can see `pkg`.
  const features = (names, external = new Set(names).has("celox-test-suite")) => {
    const visible = new Set(names);
    for (const name of names) {
      for (const dep of byName.get(name).dependencies) visible.add(dep);
    }
    return CI_FEATURES.filter((feature) =>
      visible.has(feature.split("/")[0]) &&
      (!feature.startsWith("celox-test-suite/") || external));
  };
  const wholePackages = [...selected].filter((name) => affected.has(name) || wholeTests.has(name)).sort();
  const builds = [];
  if (wholePackages.length) builds.push({
    packages: wholePackages, targets: [],
    features: features(wholePackages, selected.has("celox-test-suite")),
  });
  for (const name of [...selected].sort()) {
    if (!affected.has(name) && !wholeTests.has(name)) builds.push({
      packages: [name], targets: [...(binaries.get(name) ?? [])].sort(),
      features: features([name], selected.has("celox-test-suite")),
    });
  }
  return {
    full: false,
    packages: [...selected].sort(),
    features: features(selected),
    filter: terms.join(" | "),
    // A nextest filter controls execution, not Cargo compilation. When only
    // specific binaries of one package changed, avoid building all its tests.
    targets: affected.size === 0 && wholeTests.size === 0 && selected.size === 1
      ? [...binaries.get([...selected][0])].sort()
      : [],
    // Cargo target flags apply to every -p in an invocation. Split packages
    // with specific test binaries from whole-package consumers; otherwise one
    // additional package silently makes Cargo compile all integration tests.
    builds,
    libraries: [...libraries].sort(),
    libraryFeatures: features(libraries),
  };
}

/**
 * Files changed on head since it forked from base, or null when they cannot be
 * compared. A PR's base.sha is the current base tip, so a two-point diff would
 * also report base changes that the PR branch has not merged yet.
 */
export function changedFilesBetween(base, head, cwd = undefined) {
  const sha = /^[0-9a-f]{40,64}$/i;
  if (!sha.test(base) || !sha.test(head) || /^0+$/.test(base)) return null;
  try {
    return execFileSync("git", ["diff", "--name-only", "-z", `${base}...${head}`], {
      cwd,
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
  const packages = workspacePackages(metadata, root);
  const suite = packages.find((pkg) => pkg.name === "celox-test-suite");
  const celox = metadata.packages.find((pkg) => pkg.name === "celox");
  if (suite && celox && !packages.find((pkg) => pkg.name === "celox").libraryDependencies.includes(suite.name)) {
    const caseTargets = {};
    for (const target of celox.targets.filter((t) => t.kind.includes("test"))) {
      const text = rustTestSources(target.src_path).join("\n");
      const language = target.name === "systemverilog" ? "sv" : "veryl";
      for (const [, group] of text.matchAll(/"([\w-]+)::[\w-]+"/g)) {
        (caseTargets[`${language}/${group}`] ??= new Set()).add(target.name);
      }
    }
    suite.caseTargets = Object.fromEntries(Object.entries(caseTargets)
      .map(([key, names]) => [key, [...names].sort()]));
    const pkg = packages.find((pkg) => pkg.name === "celox");
    const svRoot = readFileSync(join(root, pkg.dir, "tests/systemverilog.rs"), "utf8");
    const svSources = rustTestSources(join(root, pkg.dir, "tests/systemverilog.rs"));
    pkg.svTestModules = [...svRoot.matchAll(/#\[path = "frontends\/systemverilog\/(\w+)\.rs"\]\s*mod \1;/g)]
      .map(([, name]) => name)
      .filter((name) => {
        const text = readFileSync(join(root, pkg.dir, `tests/frontends/systemverilog/${name}.rs`), "utf8");
        if (!/sv_backends!|#\[test\]/.test(text)) return false;
        return !svSources
          .some((source) => new RegExp(`(?:crate|super)::${name}::`).test(source));
      });
    suite.caseModules = {};
    for (const key of Object.keys(caseTargets).filter((key) => key.startsWith("sv/"))) {
      const group = key.slice(3);
      const reference = new RegExp(`"${group}::[\\w-]+"`);
      // Only narrow when every reference belongs to a known test module.
      const modules = [...svRoot.matchAll(/#\[path = "frontends\/systemverilog\/(\w+)\.rs"\]\s*mod \1;/g)]
        .map(([, name]) => name)
        .filter((name) => rustTestSources(join(root, pkg.dir, `tests/frontends/systemverilog/${name}.rs`))
          .some((text) => reference.test(text)));
      if (!reference.test(svRoot) && modules.length && modules.every((name) => pkg.svTestModules.includes(name)))
        suite.caseModules[group] = modules;
    }
  }
  return packages;
}

// Follow literal Rust module declarations, including shared macro files. Any
// unreadable source aborts scope analysis rather than dropping a consumer.
function rustTestSources(file, seen = new Set()) {
  file = resolve(file);
  if (seen.has(file)) return [];
  seen.add(file);
  const text = readFileSync(file, "utf8");
  const sources = [text];
  for (const match of text.matchAll(/((?:#\[[^\]]*\]\s*)*)(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;/g)) {
    const [, attributes, name] = match;
    const path = /#\[path\s*=\s*"([^"]+)"\]/.exec(attributes)?.[1];
    const base = dirname(file);
    const moduleBase = /(?:^|\/)(?:mod|lib|main)\.rs$/.test(file)
      ? base : file.replace(/\.rs$/, "");
    const candidates = path ? [join(base, path)] : [
      join(moduleBase, `${name}.rs`), join(moduleBase, name, "mod.rs"),
      // Integration tests are crate roots even though their name isn't lib.rs.
      join(base, `${name}.rs`), join(base, name, "mod.rs"),
    ];
    const child = candidates.find(existsSync);
    if (!child) throw new Error(`Cannot resolve test module ${name} from ${file}`);
    sources.push(...rustTestSources(child, seen));
  }
  return sources;
}
