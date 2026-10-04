import { execFileSync } from "node:child_process";
import { readFileSync, realpathSync } from "node:fs";
import { dirname, isAbsolute, relative, resolve } from "node:path";
import { pathToFileURL } from "node:url";

// Tokenize comments and strings so examples in docs/embedded HDL are not
// mistaken for Rust macros. Computed paths are deliberately rejected: public
// packages must make their compile-time file dependencies statically auditable.
function tokens(source) {
  const result = [];
  const pattern = /\s+|\/\/[^\n]*|\/\*|(?:br|r)(#*)"[\s\S]*?"\1|b?"(?:\\[\s\S]|[^"\\])*"|'(?:\\.|[^'\\\n])'|[a-zA-Z_][a-zA-Z_0-9]*|./gy;
  let match;
  let line = 1;
  while ((match = pattern.exec(source))) {
    const value = match[0];
    if (value === "/*") {
      let depth = 1;
      while (depth && pattern.lastIndex < source.length) {
        const pair = source.slice(pattern.lastIndex, pattern.lastIndex + 2);
        if (pair === "/*" || pair === "*/") {
          depth += pair === "/*" ? 1 : -1;
          pattern.lastIndex += 2;
        } else {
          pattern.lastIndex++;
        }
      }
    } else if (!/^\s|^\/\//.test(value)) {
      result.push({ value, line });
    }
    line += source.slice(match.index, pattern.lastIndex).split("\n").length - 1;
  }
  return result;
}

export function checkSource(source, file, root, packagedFiles) {
  const errors = [];
  if (!/\binclude(?:_str|_bytes)?\b/.test(source)) return errors;
  const stream = tokens(source);
  for (let i = 0; i < stream.length; i++) {
    const { value, line } = stream[i];
    if (!/^(include|include_str|include_bytes)$/.test(value) || stream[i + 1]?.value !== "!") continue;
    const label = `${file}:${line}: ${value}!`;
    const opening = stream[i + 2]?.value;
    const closing = { "(": ")", "[": "]", "{": "}" }[opening];
    const argument = stream[i + 3]?.value ?? "";
    const literal = /^(?:"([^"\\]*)"|r(#*)"([\s\S]*)"\2)$/.exec(argument);
    let end = i + 4;
    if (stream[end]?.value === ",") end++;
    if (!closing || !literal || stream[end]?.value !== closing) {
      errors.push(`${label}: use a literal package-local path (no computed or escaped paths)`);
      continue;
    }
    const path = literal[1] ?? literal[3];
    const target = resolve(root, dirname(file), path);
    const local = relative(root, target);
    if (isAbsolute(path) || local === ".." || local.startsWith("../") || isAbsolute(local)) {
      errors.push(`${label}: path escapes the crate: ${path}`);
    } else if (!packagedFiles.has(local)) {
      errors.push(`${label}: file is absent from cargo package --list: ${path}`);
    } else {
      const actual = relative(realpathSync(root), realpathSync(target));
      if (actual === ".." || actual.startsWith("../") || isAbsolute(actual)) {
        errors.push(`${label}: symlink escapes the crate: ${path}`);
      }
    }
  }
  return errors;
}

export function checkPackages() {
  const cargo = (args) => execFileSync("cargo", args, { encoding: "utf8", maxBuffer: 16 * 1024 * 1024 });
  const metadata = JSON.parse(cargo(["metadata", "--no-deps", "--format-version", "1", "--locked"]));
  const errors = [];
  let count = 0;
  for (const pkg of metadata.packages) {
    if (!metadata.workspace_members.includes(pkg.id) || pkg.publish?.length === 0) continue;
    const root = dirname(pkg.manifest_path);
    const files = new Set(cargo(["package", "--locked", "--allow-dirty", "--list", "-p", pkg.name]).trim().split("\n"));
    for (const file of files) {
      if (!file.endsWith(".rs")) continue;
      errors.push(...checkSource(readFileSync(resolve(root, file), "utf8"), file, root, files).map((error) => `${pkg.name}: ${error}`));
    }
    count++;
  }
  if (errors.length) throw new Error(errors.join("\n"));
  console.log(`Checked compile-time includes in ${count} publishable crates.`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  checkPackages();
}
