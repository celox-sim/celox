import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

export function checkApliteRelease(root = ".") {
  const read = (path) => readFileSync(resolve(root, path), "utf8");
  const manifest = read("crates/aplite/Cargo.toml");
  const version = /^version = "([^"]+)"$/m.exec(manifest)?.[1];
  if (!/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(version ?? "")) {
    throw new Error("Aplite must have an explicit stable SemVer version");
  }
  const releases = JSON.parse(read(".release-please-aplite-manifest.json"));
  if (Object.keys(releases).some((key) => key !== "crates/aplite")) {
    throw new Error("Aplite release manifest contains another component");
  }
  // An empty manifest bootstraps the first Rust release at 0.1.0.
  if ((releases["crates/aplite"] ?? "0.1.0") !== version) {
    throw new Error("Aplite release manifest and package version disagree");
  }
  const entries = read("Cargo.lock").split("[[package]]").slice(1);
  const locked = entries.filter((entry) => /^name = "aplite"$/m.test(entry));
  if (locked.length !== 1 || /^version = "([^"]+)"$/m.exec(locked[0])?.[1] !== version) {
    throw new Error("Cargo.lock must contain exactly the current Aplite version");
  }
  return version;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  console.log(`Validated independent Aplite version ${checkApliteRelease()}`);
}
