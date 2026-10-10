import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  readWorkspace,
  rustTestScope,
  VARIANT_CRATES,
  VARIANTS,
} from "./ci-rust-scope.mjs";

// A small workspace in the shape of the real one: celox over a backend, a
// frontend and a shared library, with a consumer above it.
const workspace = [
  { name: "lib", dir: "crates/lib", dependencies: [], tests: [] },
  {
    name: "celox-backend-x86",
    dir: "crates/celox-backend-x86",
    dependencies: ["lib"],
    tests: [],
  },
  {
    name: "celox-frontend-sv",
    dir: "crates/celox-frontend-sv",
    dependencies: ["lib"],
    tests: [],
  },
  {
    name: "celox",
    dir: "crates/celox",
    dependencies: ["lib", "celox-backend-x86", "celox-frontend-sv"],
    tests: ["flip_flop", "systemverilog"],
  },
  {
    name: "consumer",
    dir: "crates/consumer",
    dependencies: ["celox"],
    tests: ["e2e"],
  },
  { name: "lydite", dir: "crates/lydite", dependencies: [], tests: [] },
];

const PLAIN = `not test(/::(${VARIANTS.join("|")})$/)`;

test("a library change runs the library and everything that depends on it", () => {
  const scope = rustTestScope(["crates/lib/src/lib.rs"], workspace);
  assert.deepEqual(scope.packages, [
    "celox",
    "celox-backend-x86",
    "celox-frontend-sv",
    "consumer",
    "lib",
  ]);
  assert.equal(
    scope.filter,
    "package(celox) | package(celox-backend-x86) | package(celox-frontend-sv) | package(consumer) | package(lib)",
  );
});

test("a backend change runs only its variant of celox's integration tests", () => {
  const scope = rustTestScope(
    ["crates/celox-backend-x86/src/emit.rs"],
    workspace,
  );
  assert.equal(
    scope.filter,
    `package(celox-backend-x86) | package(consumer) | (package(celox) & (test(/::(native|native_parallel)$/) | ${PLAIN}))`,
  );
});

test("a frontend change also runs its own test binary", () => {
  const scope = rustTestScope(
    ["crates/celox-frontend-sv/src/lowering.rs"],
    workspace,
  );
  assert.equal(
    scope.filter,
    `package(celox-frontend-sv) | package(consumer) | (package(celox) & (test(/::(sv)$/) | ${PLAIN} | binary_id(celox::systemverilog)))`,
  );
});

test("narrowing stops when another seed reaches celox", () => {
  const scope = rustTestScope(
    ["crates/celox-backend-x86/src/emit.rs", "crates/celox/src/lib.rs"],
    workspace,
  );
  assert.ok(scope.filter.split(" | ").includes("package(celox)"), scope.filter);
});

test("a test file change runs only that binary and nothing downstream", () => {
  assert.deepEqual(rustTestScope(["crates/celox/tests/flip_flop.rs"], workspace), {
    full: false,
    packages: ["celox"],
    features: ["celox/systemverilog"],
    filter: "binary_id(celox::flip_flop)",
    libraries: [],
    libraryFeatures: [],
  });
  assert.equal(
    rustTestScope(
      ["crates/celox/tests/frontends/systemverilog/operators.rs"],
      workspace,
    ).filter,
    "binary_id(celox::systemverilog)",
  );
  // Shared test helpers belong to every binary of the package.
  assert.equal(
    rustTestScope(["crates/celox/tests/test_utils/mod.rs"], workspace).filter,
    "package(celox)",
  );
});

test("features are passed only to packages that can see them", () => {
  const scope = rustTestScope(["crates/lydite/src/lib.rs"], workspace);
  assert.deepEqual(scope.features, []);
  assert.equal(scope.filter, "package(lydite)");
});

test("lydite's shared folder runs every lydite package", () => {
  assert.equal(
    rustTestScope(["lydite/examples/sum.lyd"], workspace).filter,
    "package(lydite)",
  );
});

test("changes outside the workspace graph run every test", () => {
  for (const path of [
    "Cargo.lock",
    "Cargo.toml",
    "testdata/verilator/adder.sv",
    "rust-toolchain.toml",
    ".config/nextest.toml",
  ]) {
    assert.deepEqual(rustTestScope([path], workspace), { full: true }, path);
  }
});

test("files Rust tests never read select no tests", () => {
  const scope = rustTestScope(
    ["docs/guide/systemverilog.md", "packages/celox/src/index.ts"],
    workspace,
  );
  assert.deepEqual(scope.packages, []);
  assert.equal(scope.filter, "");
});

const root = fileURLToPath(new URL("../", import.meta.url));

test("the variant suffixes are the tests the backend macros generate", () => {
  const generated = new Set();
  for (const file of [
    "crates/celox/tests/test_utils/mod.rs",
    "crates/celox/tests/systemverilog.rs",
  ]) {
    const text = readFileSync(`${root}${file}`, "utf8");
    // Only the macros' functions sit this deep; plain tests start a line.
    for (const match of text.matchAll(/^ {12,}fn (\w+)\(\) \{$/gm)) {
      generated.add(match[1]);
    }
  }
  assert.deepEqual([...generated].sort(), [...VARIANTS].sort());
});

test("variant crates reach celox only through crates of the same variant", () => {
  const packages = readWorkspace(root);
  const byName = new Map(packages.map((pkg) => [pkg.name, pkg]));
  const celoxDeps = new Set(byName.get("celox").libraryDependencies);
  for (const [name, scope] of VARIANT_CRATES) {
    assert.ok(byName.has(name), name);
    // Every workspace crate that depends on it and that celox also depends
    // on must narrow to the same variants, or celox would miss a variant.
    for (const pkg of packages) {
      if (
        !pkg.libraryDependencies.includes(name) ||
        !celoxDeps.has(pkg.name)
      ) {
        continue;
      }
      assert.deepEqual(VARIANT_CRATES.get(pkg.name)?.variants, scope.variants, `${pkg.name} -> ${name}`);
    }
  }
});
