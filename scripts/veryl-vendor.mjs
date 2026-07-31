#!/usr/bin/env node

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const VERYL_CRATES = [
  "veryl-analyzer",
  "veryl-emitter",
  "veryl-metadata",
  "veryl-parser",
  "veryl-path",
  "veryl-simulator",
  "veryl-std",
];

function section(manifest, name) {
  const header = `[${name}]`;
  const start = manifest.indexOf(`${header}\n`);
  if (start === -1) {
    throw new Error(`missing ${header} section`);
  }

  const bodyStart = start + header.length + 1;
  const next = manifest.indexOf("\n[", bodyStart);
  return manifest.slice(bodyStart, next === -1 ? manifest.length : next + 1);
}

export function requestedVerylVersion(rootManifest) {
  const dependencies = section(rootManifest, "workspace.dependencies");
  const versions = VERYL_CRATES.map((crate) => {
    const escaped = crate.replaceAll("-", "\\-");
    const match = dependencies.match(
      new RegExp(
        `^${escaped}\\s*=\\s*(?:"([^"]+)"|\\{[^\\n]*\\bversion\\s*=\\s*"([^"]+)")`,
        "m",
      ),
    );
    if (!match) {
      throw new Error(`missing an exact ${crate} version`);
    }
    return match[1] ?? match[2];
  });

  const [version] = versions;
  if (!/^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$/.test(version)) {
    throw new Error(`Veryl version must be exact, got ${version}`);
  }
  if (versions.some((candidate) => candidate !== version)) {
    throw new Error(`Veryl workspace dependencies are not in lockstep: ${versions.join(", ")}`);
  }
  return version;
}

export function vendorPackageVersion(vendorManifest) {
  const packageSection = section(vendorManifest, "package");
  const match = packageSection.match(/^version\s*=\s*"([^"]+)"/m);
  if (!match) {
    throw new Error("missing vendored veryl-metadata package version");
  }
  return match[1];
}

export function defaultFeatures(vendorManifest) {
  const features = section(vendorManifest, "features");
  const match = features.match(/^default\s*=\s*\[([\s\S]*?)\]/m);
  if (!match) {
    throw new Error("missing vendored veryl-metadata default features");
  }
  return [...match[1].matchAll(/"([^"]+)"/g)].map((entry) => entry[1]);
}

export function disableGitoxideDefault(vendorManifest) {
  const features = section(vendorManifest, "features");
  if (!/^default\s*=\s*\[[\s\S]*?\]/m.test(features)) {
    throw new Error("missing vendored veryl-metadata default features");
  }
  const replacement = features.replace(
    /^default\s*=\s*\[[\s\S]*?\]/m,
    'default = [\n    "git-command",\n]',
  );
  return vendorManifest.replace(features, replacement);
}

export function verifyVendor(rootManifest, vendorManifest) {
  const requested = requestedVerylVersion(rootManifest);
  const vendored = vendorPackageVersion(vendorManifest);
  if (vendored !== requested) {
    throw new Error(`vendored veryl-metadata ${vendored} does not match requested Veryl ${requested}`);
  }

  const features = defaultFeatures(vendorManifest);
  if (features.length !== 1 || features[0] !== "git-command") {
    throw new Error(`vendored default features must be only git-command, got ${features.join(", ")}`);
  }
}

export function verifyLockfile(rootManifest, lockfile) {
  const requested = requestedVerylVersion(rootManifest);
  const packages = [...lockfile.matchAll(/\[\[package\]\]\n([\s\S]*?)(?=\n\[\[package\]\]|\s*$)/g)]
    .map((match) => match[1])
    .filter((candidate) => /^name = "veryl-metadata"$/m.test(candidate));
  if (packages.length !== 1) {
    throw new Error(`expected one veryl-metadata lockfile entry, got ${packages.length}`);
  }
  if (!new RegExp(`^version = "${requested.replaceAll(".", "\\.")}"$`, "m").test(packages[0])) {
    throw new Error(`Cargo.lock does not select vendored veryl-metadata ${requested}`);
  }
  if (/^source = /m.test(packages[0])) {
    throw new Error("Cargo.lock selects registry veryl-metadata instead of the vendored patch");
  }
}

function run(argv) {
  const [command = "check", argument] = argv;
  if (command === "version") {
    const manifest = fs.readFileSync(argument ?? "Cargo.toml", "utf8");
    process.stdout.write(`${requestedVerylVersion(manifest)}\n`);
    return;
  }

  if (command === "patch") {
    const manifestPath = argument ?? "vendor/veryl-metadata/Cargo.toml";
    const manifest = fs.readFileSync(manifestPath, "utf8");
    fs.writeFileSync(manifestPath, disableGitoxideDefault(manifest));
    return;
  }

  if (command === "check") {
    const root = argument ?? ".";
    const rootManifest = fs.readFileSync(path.join(root, "Cargo.toml"), "utf8");
    verifyVendor(
      rootManifest,
      fs.readFileSync(path.join(root, "vendor/veryl-metadata/Cargo.toml"), "utf8"),
    );
    verifyLockfile(rootManifest, fs.readFileSync(path.join(root, "Cargo.lock"), "utf8"));
    return;
  }

  throw new Error(`unknown command: ${command}`);
}

if (process.argv[1] && fileURLToPath(import.meta.url) === path.resolve(process.argv[1])) {
  run(process.argv.slice(2));
}
