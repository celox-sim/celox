import assert from "node:assert/strict";
import test from "node:test";
import { backendTestFilter, intersectTestFilters } from "./ci-backend-tests.mjs";

function excludedNames(filter, kind, name) {
  const expression = new RegExp(`${kind}\\(/([^/]+?)/\\)`, "g");
  return [...filter.matchAll(expression)].some(([, pattern]) => new RegExp(pattern).test(name));
}

test("ordinary frontend and shared runtime PRs retain native serial tests", () => {
  for (const path of ["crates/celox-sv-analyzer/src/ast.rs", "crates/celox-runtime/src/lib.rs",
    "crates/celox-test-suite/src/sv/cases/class.vtest", "scripts/ci-rust-scope.mjs"]) {
    const filter = backendTestFilter([path]);
    for (const name of ["class::example::cranelift", "cranelift::example", "example::wasm",
      "example::native_parallel", "example::cranelift_parallel", "trace_rtl_native_threads"]) {
      assert.ok(excludedNames(filter, "test", name), `${path}: ${name}`);
    }
    for (const name of ["class::example::native", "native::example", "example::interp", "plain_test"]) {
      assert.ok(!excludedNames(filter, "test", name), name);
    }
    assert.ok(excludedNames(filter, "binary_id", "celox::wasm_backend"));
    assert.ok(excludedNames(filter, "binary_id", "celox-runtime::parallel_tracing"));
    assert.match(filter, /package\(celox-backend-cranelift\)/);
    assert.match(filter, /package\(celox-backend-wasm\)/);
  }
});

test("direct backend or scheduler changes enable the relevant execution lane", () => {
  for (const [path, allowed, blocked] of [
    ["crates/celox-backend-cranelift/src/lib.rs", "example::cranelift", "example::wasm"],
    ["crates/celox/src/backend/wasm_runtime.rs", "example::wasm", "example::native_parallel"],
    ["crates/celox-runtime/src/parallel.rs", "example::native_parallel", "example::cranelift"],
    ["crates/celox-runtime/src/scheduler.rs", "example::native_parallel", "example::wasm"],
    ["crates/celox/tests/wasm_backend.rs", "example::wasm", "example::cranelift"],
  ]) {
    const filter = backendTestFilter([path]);
    assert.ok(!excludedNames(filter, "test", allowed), path);
    assert.ok(excludedNames(filter, "test", blocked), path);
  }
  assert.equal(backendTestFilter(["crates/celox-backend-cranelift/src/lib.rs",
    "crates/celox-backend-wasm/src/lib.rs", "crates/celox-runtime/src/parallel.rs"]), "all()");
});

test("full validation, unknown diffs and dependency changes retain every lane", () => {
  for (const files of [null, ["Cargo.lock"], ["crates/celox/Cargo.toml"],
    ["rust-toolchain.toml"], [".cargo/config.toml"]]) {
    assert.equal(backendTestFilter(files), "all()");
  }
  assert.equal(backendTestFilter(["crates/celox/src/lib.rs"], { full: true }), "all()");
});

test("backend policy intersects the entire scope union", () => {
  assert.equal(intersectTestFilters("package(a) | package(b)", "not test(wasm)"),
    "(package(a) | package(b)) & (not test(wasm))");
  assert.equal(intersectTestFilters("package(a)"), "package(a)");
});
