import fs from "node:fs";

const stableSemver = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
const timestampPattern = /^\d{14}$/;
const revisionPattern = /^[0-9a-f]{40}$/;
const nightlyChannels = new Set(["stable", "head"]);

export function nightlyVersion(stable, channel, timestamp, revision) {
  const match = stable.match(stableSemver);
  if (match === null) {
    throw new Error(`Expected a stable SemVer base, got ${stable}`);
  }
  if (!timestampPattern.test(timestamp)) {
    throw new Error(`Expected a UTC timestamp in YYYYMMDDHHMMSS form, got ${timestamp}`);
  }
  if (!revisionPattern.test(revision)) {
    throw new Error(`Expected a full lowercase git revision, got ${revision}`);
  }
  if (!nightlyChannels.has(channel)) {
    throw new Error(`Expected nightly channel stable or head, got ${channel}`);
  }

  const [, major, minor, patch] = match;
  return `${major}.${minor}.${Number(patch) + 1}-nightly.${channel}.${timestamp}.g${revision.slice(0, 12)}`;
}

function writeJson(path, update) {
  const value = JSON.parse(fs.readFileSync(path, "utf8"));
  update(value);
  fs.writeFileSync(path, `${JSON.stringify(value, null, 2)}\n`);
}

export function setNightlyVersion(root, channel, timestamp, revision) {
  const versionPath = `${root}/VERSION`;
  const stable = fs.readFileSync(versionPath, "utf8").trim();
  const version = nightlyVersion(stable, channel, timestamp, revision);
  fs.writeFileSync(versionPath, `${version}\n`);

  writeJson(`${root}/.release-please-manifest.json`, (manifest) => {
    manifest["."] = version;
  });

  for (const path of [
    "packages/celox/package.json",
    "packages/vite-plugin/package.json",
    "crates/celox-napi/package.json",
  ]) {
    writeJson(`${root}/${path}`, (manifest) => {
      manifest.version = version;
    });
  }

  return version;
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const channel = process.argv[2] ?? "";
  const timestamp = process.argv[3] ?? "";
  const revision = process.argv[4] ?? "";
  console.log(setNightlyVersion(process.cwd(), channel, timestamp, revision));
}
