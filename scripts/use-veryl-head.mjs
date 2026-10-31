import fs from "node:fs";

const verylRepository = "https://github.com/veryl-lang/veryl.git";
export const verylDependencyNames = [
  "veryl-analyzer",
  "veryl-emitter",
  "veryl-metadata",
  "veryl-parser",
  "veryl-path",
  "veryl-simulator",
  "veryl-std",
];

function checkRevision(revision) {
  if (!/^[0-9a-f]{40}$/.test(revision)) {
    throw new Error(`Expected a full lowercase git revision, got ${revision}`);
  }
}

// Reads the versions Cargo.lock resolved for the Veryl crates at `revision`.
// Returns undefined until every crate is locked at that revision, i.e. before
// the first `cargo update` of a new roll.
export function lockedVerylVersions(lockfile, revision) {
  checkRevision(revision);
  const source = `git+${verylRepository}?rev=${revision}#${revision}`;
  const versions = {};
  for (const entry of lockfile.split("\n[[package]]\n").slice(1)) {
    const name = entry.match(/^name = "([^"]+)"$/m)?.[1];
    const version = entry.match(/^version = "([^"]+)"$/m)?.[1];
    const entrySource = entry.match(/^source = "([^"]+)"$/m)?.[1];
    if (verylDependencyNames.includes(name) && entrySource === source) {
      versions[name] = version;
    }
  }
  return verylDependencyNames.every((name) => name in versions)
    ? versions
    : undefined;
}

export function useVerylHead(manifest, revision, versions) {
  checkRevision(revision);

  const patchStart = manifest.indexOf("\n[patch.crates-io]");
  const dependencies = patchStart === -1 ? manifest : manifest.slice(0, patchStart);
  const patches = patchStart === -1 ? "" : manifest.slice(patchStart);
  let updated = dependencies;

  for (const name of verylDependencyNames) {
    const pattern = new RegExp(`^${name}\\s*=.*$`, "m");
    const matches = updated.match(pattern);
    if (matches === null) {
      throw new Error(`Could not find workspace dependency ${name}`);
    }

    // A git dependency without a version requirement is a wildcard to
    // cargo-deny, so declare the version the revision resolved to.
    const options = [];
    if (versions !== undefined) {
      options.push(`version = "${versions[name]}"`);
    }
    options.push(`git = "${verylRepository}"`, `rev = "${revision}"`);
    if (name === "veryl-parser") {
      options.push("default-features = false");
    }
    updated = updated.replace(pattern, `${name} = { ${options.join(", ")} }`);
  }

  return updated + patches;
}

// Run once to pin the revision, `cargo update` to lock it, then again to
// declare the locked versions.
if (import.meta.url === `file://${process.argv[1]}`) {
  const revision = process.argv[2] ?? "";
  const path = "Cargo.toml";
  const manifest = fs.readFileSync(path, "utf8");
  const lockfile = fs.existsSync("Cargo.lock")
    ? fs.readFileSync("Cargo.lock", "utf8")
    : "";
  const versions = lockedVerylVersions(lockfile, revision);
  fs.writeFileSync(path, useVerylHead(manifest, revision, versions));
}
