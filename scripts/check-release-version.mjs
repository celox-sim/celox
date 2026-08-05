import fs from "node:fs";

const version = fs.readFileSync("VERSION", "utf8").trim();
const stableSemver = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;

if (!stableSemver.test(version)) {
  throw new Error(`VERSION must be a stable SemVer version, got ${version}`);
}

const releaseManifest = JSON.parse(
  fs.readFileSync(".release-please-manifest.json", "utf8"),
);
const packagePaths = [
  "packages/celox/package.json",
  "packages/vite-plugin/package.json",
  "crates/celox-napi/package.json",
];

const versions = new Map([[".release-please-manifest.json", releaseManifest["."]]]);
for (const path of packagePaths) {
  const manifest = JSON.parse(fs.readFileSync(path, "utf8"));
  versions.set(path, manifest.version);
}

for (const [path, candidate] of versions) {
  if (candidate !== version) {
    throw new Error(`${path} has version ${candidate}; expected ${version}`);
  }
}

const cargoManifest = fs.readFileSync("Cargo.toml", "utf8");
for (const line of cargoManifest.split("\n")) {
  if (/^veryl-[a-z-]+\s*=/.test(line) && /\bgit\s*=/.test(line)) {
    throw new Error(`The stable lane cannot use a Veryl git dependency: ${line}`);
  }
}

console.log(`Validated stable Celox version ${version}`);
