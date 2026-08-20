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
const releaseVersionLines = cargoManifest
  .split("\n")
  .filter((line) => line.includes("x-release-please-version"));
if (releaseVersionLines.length === 0) {
  throw new Error("Cargo.toml has no Release Please version markers");
}
for (const line of releaseVersionLines) {
  const match = line.match(/\bversion\s*=\s*"=?([^"]+)"/);
  if (!match || match[1] !== version) {
    throw new Error(`Cargo.toml release version is not ${version}: ${line}`);
  }
}

const cargoLock = fs.readFileSync("Cargo.lock", "utf8");
for (const block of cargoLock.split("[[package]]").slice(1)) {
  const name = block.match(/^\s*name = "([^"]+)"/m)?.[1];
  const candidate = block.match(/^\s*version = "([^"]+)"/m)?.[1];
  if (
    (name === "celox" || name?.startsWith("celox-")) &&
    candidate !== version
  ) {
    throw new Error(`Cargo.lock has ${name} ${candidate}; expected ${version}`);
  }
}

for (const line of cargoManifest.split("\n")) {
  if (/^veryl-[a-z-]+\s*=/.test(line) && /\bgit\s*=/.test(line)) {
    throw new Error(`The stable lane cannot use a Veryl git dependency: ${line}`);
  }
}

console.log(`Validated stable Celox version ${version}`);
