import { spawnSync } from "node:child_process";
import { pathToFileURL } from "node:url";
import { intersectTestFilters } from "./ci-backend-tests.mjs";
import { runtimeCasesFeature } from "./run-vtest.mjs";
import { clearCurrentReport, saveCurrentReport } from "./ci-rust-reports.mjs";

// Keep target flags local to their packages. Cargo applies --test to every -p.
export function cargoCommands(mode, builds, filter, backendFilter = "all()") {
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
    const enabled = mode === "test" && packages.includes("celox")
      ? [...new Set([...features, runtimeCasesFeature])] : features;
    if (enabled.length) args.push("--features", enabled.join(","));
    for (const target of targets) args.push("--test", target);
    if (mode === "lint") {
      if (!targets.length) args.push("--all-targets");
      args.push("--no-deps");
    } else args.push("-E", intersectTestFilters(filter, backendFilter));
    return args;
  });
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const reportDirectory = process.argv[2] === "test" && process.env.CELOX_CI_REPORT_DIR;
  let reportIndex = 0;
  for (const args of cargoCommands(
    process.argv[2], JSON.parse(process.env.BUILDS), process.env.FILTER,
    process.env.RUST_BACKEND_FILTER || "all()",
  )) {
    if (process.argv[2] === "test"
      && args.some((arg, index) => arg === "celox" && args[index - 1] === "-p")
      && !process.env.CELOX_VTEST_RUNNER)
      throw new Error("Celox CI tests require the prepared vtest executor");
    console.log("cargo", ...args);
    if (reportDirectory) clearCurrentReport();
    const result = spawnSync("cargo", args, { stdio: "inherit" });
    if (reportDirectory) saveCurrentReport(reportDirectory, String(reportIndex++));
    if (result.error) throw result.error;
    if (result.status !== 0) process.exit(result.status ?? 1);
  }
}
