import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { setTimeout } from "node:timers/promises";
import { pathToFileURL } from "node:url";

export async function publishNpm({
  publisher,
  directory = ".",
  tag,
  run = spawnSync,
  wait = setTimeout,
  log = console.log,
}) {
  if (!["npm", "pnpm"].includes(publisher) || !tag) {
    throw new Error("Usage: publish-npm.mjs <npm|pnpm> <directory> <tag>");
  }
  const { name, version } = JSON.parse(readFileSync(resolve(directory, "package.json"), "utf8"));
  const spec = `${name}@${version}`;
  const args = ["publish", "--ignore-scripts", "--provenance", "--access", "public", "--tag", tag];
  if (publisher === "pnpm") args.push("--no-git-checks");
  const options = { cwd: resolve(directory), encoding: "utf8" };
  for (let attempt = 1; attempt <= 3; attempt++) {
    const lookup = run("npm", ["view", spec, "version"], options);
    if (lookup.error) throw lookup.error;
    if (lookup.status === 0) {
      log(`${spec} is already published; skipping.`);
      return;
    }
    const result = run(publisher, args, options);
    if (result.stdout) log(result.stdout);
    if (result.stderr) log(result.stderr);
    if (result.error) throw result.error;
    if (result.status === 0) return;
    // Rekor can commit a request whose response is lost, then reject npm's
    // internal retry. A fresh publish creates a new signature. A log entry
    // alone does not mean that the package made it to the npm registry.
    // https://github.com/npm/cli/issues/10078
    const output = `${result.stdout ?? ""}\n${result.stderr ?? ""}`;
    const conflict = output.includes("TLOG_CREATE_ENTRY_ERROR") &&
      /\(409\).*an equivalent entry already exists in the transparency log/.test(output);
    if (!conflict || attempt === 3) {
      throw new Error(`Publishing ${spec} failed (${result.signal ?? result.status}).`);
    }
    log(`Retrying ${spec} after a provenance log conflict (attempt ${attempt + 1}/3).`);
    await wait(5000);
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const [publisher, directory, tag] = process.argv.slice(2);
  publishNpm({ publisher, directory, tag }).catch((error) => {
    console.error(error.message);
    process.exitCode = 1;
  });
}
