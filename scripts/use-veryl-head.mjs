import fs from "node:fs";

const verylRepository = "https://github.com/veryl-lang/veryl.git";
const dependencyNames = [
  "veryl-analyzer",
  "veryl-emitter",
  "veryl-metadata",
  "veryl-parser",
  "veryl-path",
  "veryl-simulator",
  "veryl-std",
];

export function useVerylHead(manifest, revision) {
  if (!/^[0-9a-f]{40}$/.test(revision)) {
    throw new Error(`Expected a full lowercase git revision, got ${revision}`);
  }

  const patchStart = manifest.indexOf("\n[patch.crates-io]");
  const dependencies = patchStart === -1 ? manifest : manifest.slice(0, patchStart);
  const patches = patchStart === -1 ? "" : manifest.slice(patchStart);
  let updated = dependencies;

  for (const name of dependencyNames) {
    const pattern = new RegExp(`^${name}\\s*=.*$`, "m");
    const matches = updated.match(pattern);
    if (matches === null) {
      throw new Error(`Could not find workspace dependency ${name}`);
    }

    const options = [`git = "${verylRepository}"`, `rev = "${revision}"`];
    if (name === "veryl-parser") {
      options.push("default-features = false");
    }
    updated = updated.replace(pattern, `${name} = { ${options.join(", ")} }`);
  }

  return updated + patches;
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const revision = process.argv[2] ?? "";
  const path = "Cargo.toml";
  const manifest = fs.readFileSync(path, "utf8");
  fs.writeFileSync(path, useVerylHead(manifest, revision));
}
