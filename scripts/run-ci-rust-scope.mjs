import { spawnSync } from "node:child_process";
import { pathToFileURL } from "node:url";

// Keep target flags local to their packages. Cargo applies --test to every -p.
export function cargoCommands(mode, builds, filter) {
  if (!["test", "lint"].includes(mode) || !Array.isArray(builds) || !builds.length)
    throw new Error("Expected a nonempty scoped Cargo build plan");
  return builds.map(({ packages, targets, features }) => {
    for (const names of [packages, targets, features]) {
      if (!Array.isArray(names) || names.some((name) =>
        typeof name !== "string" || !/^[a-zA-Z0-9_][a-zA-Z0-9_./-]*$/.test(name)))
        throw new Error("Invalid Cargo scope argument");
    }
    if (!packages.length || (mode === "test" && !filter))
      throw new Error("Empty package or test selection");
    const args = mode === "test"
      ? ["nextest", "run", "--locked", "--profile", "ci", "--no-tests=pass"]
      : ["clippy", "--locked"];
    for (const name of packages) args.push("-p", name);
    if (features.length) args.push("--features", features.join(","));
    for (const target of targets) args.push("--test", target);
    if (mode === "lint") {
      if (!targets.length) args.push("--all-targets");
      args.push("--no-deps");
    } else args.push("-E", filter);
    return args;
  });
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  for (const args of cargoCommands(
    process.argv[2], JSON.parse(process.env.BUILDS), process.env.FILTER,
  )) {
    console.log("cargo", ...args);
    const result = spawnSync("cargo", args, { stdio: "inherit" });
    if (result.error) throw result.error;
    if (result.status !== 0) process.exit(result.status ?? 1);
  }
}
