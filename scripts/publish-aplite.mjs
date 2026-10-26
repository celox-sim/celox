import { execFileSync } from "node:child_process";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { checkApliteRelease } from "./check-aplite-release.mjs";

export function validateRelease(version, env, release) {
  const tag = `aplite-v${version}`;
  if (env.GITHUB_REF_TYPE !== "tag" || env.RELEASE_TAG !== tag) {
    throw new Error(`Run from the matching stable release tag ${tag}`);
  }
  if (release.tagName !== tag || release.isDraft !== false || release.isPrerelease !== false) {
    throw new Error(`Published stable GitHub Release ${tag} was not found`);
  }
}

export async function registryExists(path, request = fetch) {
  const response = await request(`https://crates.io/api/v1/crates/${path}`, {
    headers: { "User-Agent": "aplite-release (https://github.com/celox-sim/celox)" },
    signal: AbortSignal.timeout(30000),
  });
  if (response.status === 200) return true;
  if (response.status === 404) return false;
  throw new Error(`crates.io lookup failed (HTTP ${response.status})`);
}

async function main() {
  const mode = process.argv[2];
  if (!["validate", "publish"].includes(mode)) throw new Error("Usage: publish-aplite.mjs validate|publish");
  const version = checkApliteRelease();
  const env = process.env;
  // Reject branch executions before querying GitHub or requesting publication.
  validateRelease(version, env, { tagName: `aplite-v${version}`, isDraft: false, isPrerelease: false });
  const release = JSON.parse(execFileSync("gh", ["release", "view", env.RELEASE_TAG,
    "--repo", env.GITHUB_REPOSITORY, "--json", "tagName,isDraft,isPrerelease"], { encoding: "utf8" }));
  validateRelease(version, env, release);
  if (!await registryExists("aplite")) throw new Error("Bootstrap aplite on crates.io before Trusted Publishing");
  if (mode === "validate") return;
  if (await registryExists(`aplite/${version}`)) {
    console.log(`aplite@${version} already published; skipping`);
    return;
  }
  if (!env.CARGO_REGISTRY_TOKEN) throw new Error("Missing crates.io OIDC token");
  execFileSync("cargo", ["publish", "--locked", "-p", "aplite"], { stdio: "inherit" });
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  main().catch((error) => { console.error(error.message); process.exitCode = 1; });
}
