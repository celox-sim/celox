import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { checkSource } from "./check-package-includes.mjs";

function fixture(t) {
  const root = mkdtempSync(join(tmpdir(), "celox-includes-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(join(root, "src"));
  writeFileSync(join(root, "data.txt"), "fixture");
  return root;
}

test("accepts packaged local paths with all macro delimiters and raw strings", (t) => {
  const root = fixture(t);
  for (const source of ['include_str!("../data.txt")', 'include_bytes![r#"../data.txt"#,]', 'include!{"../data.txt"}']) {
    assert.deepEqual(checkSource(source, "src/lib.rs", root, new Set(["data.txt"])), []);
  }
});

test("rejects sibling-crate paths including the release regression", (t) => {
  const root = fixture(t);
  for (const macro of ["include_str", "include_bytes", "include"]) {
    const errors = checkSource(`${macro}!(\n"../../celox-test-suite-veryl/fixtures/testbench/concurrent_initial_shared_clock.veryl"\n)`, "tests/runtime.rs", root, new Set());
    assert.match(errors.join("\n"), /runtime.rs:1:.*path escapes the crate/);
  }
});

test("rejects absolute, computed and escaped paths", (t) => {
  const root = fixture(t);
  for (const argument of ['"/tmp/data"', 'concat!(env!("CARGO_MANIFEST_DIR"), "/data.txt")', 'PATH', '"..\\x2fdata.txt"']) {
    assert.equal(checkSource(`include_str!(${argument})`, "src/lib.rs", root, new Set()).length, 1);
  }
});

test("ignores comments, string literals and character literals", (t) => {
  const root = fixture(t);
  const source = `// include_str!("missing")
/* nested /* comment */ include!("missing") */
const TEXT: &str = r##"include_bytes!("missing")"##;
const OTHER: &str = "include_str!(\\\"missing\\\")";
let quote = '"';
include_str!(/* comment */ "../data.txt");`;
  assert.deepEqual(checkSource(source, "src/lib.rs", root, new Set(["data.txt"])), []);
});

test("rejects symlinks outside the package", (t) => {
  const root = fixture(t);
  symlinkSync(tmpdir(), join(root, "outside"));
  const errors = checkSource('include_str!("../outside")', "src/lib.rs", root, new Set(["outside"]));
  assert.match(errors.join("\n"), /symlink escapes/);
});

test("checks Cargo's actual exclusions without needing a published dependency", (t) => {
  const root = fixture(t);
  writeFileSync(join(root, "Cargo.toml"), `[package]
name = "include-regression"
version = "0.1.0"
edition = "2024"
exclude = ["data.txt"]
`);
  const source = 'pub const DATA: &str = include_str!("../data.txt");';
  writeFileSync(join(root, "src/lib.rs"), source);
  const files = new Set(execFileSync("cargo", ["package", "--list", "--allow-dirty"], { cwd: root, encoding: "utf8" }).trim().split("\n"));
  const errors = checkSource(source, "src/lib.rs", root, files);
  assert.match(errors.join("\n"), /absent from cargo package --list/);
});
