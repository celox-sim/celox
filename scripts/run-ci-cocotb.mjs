import { spawnSync } from "node:child_process";
import { pathToFileURL } from "node:url";
import { clearCurrentReport, saveCurrentReport } from "./ci-rust-reports.mjs";
import { cargoCommands } from "./run-ci-rust-scope.mjs";
import { testProfile, workspaceFeatures } from "./run-vtest.mjs";

export const cocotbFilter = 'binary_id(celox-vpi::cocotb_e2e) & test(/^cocotb_/)';

// Keep exactly the original packages, features, targets and Cargo profile.
// Narrowing -p to celox-vpi would resolve a different dependency feature graph.
export function cocotbCommands(full, builds) {
  if (full !== "false") return [[
    "nextest", "run", "--locked", "--workspace", "--exclude", "lydite*",
    "--profile", "ci", "--cargo-profile", testProfile,
    "--features", workspaceFeatures,
    "--run-ignored", "only", "-E", cocotbFilter,
  ]];
  // Validate all plans first, including ones we do not need to execute.
  const commands = cargoCommands("test", builds, cocotbFilter);
  const selected = commands.filter((_, i) => builds[i].packages.includes("celox-vpi")
    && (!builds[i].targets.length || builds[i].targets.includes("cocotb_e2e")));
  if (!selected.length) throw new Error("Cocotb CI requires a VPI test build");
  return selected.map(args => [
    ...args.filter(arg => arg !== "--no-tests=pass"), "--run-ignored", "only",
  ]);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  if (!process.env.CELOX_COCOTB_PYTHON) throw new Error("Cocotb Python interpreter is required");
  let reportIndex = 0;
  for (const args of cocotbCommands(process.env.RUST_FULL,
    process.env.RUST_FULL === "false" ? JSON.parse(process.env.BUILDS) : null)) {
    if (process.env.CELOX_CI_REPORT_DIR) clearCurrentReport();
    const result = spawnSync("cargo", args, { stdio: "inherit" });
    if (process.env.CELOX_CI_REPORT_DIR)
      saveCurrentReport(process.env.CELOX_CI_REPORT_DIR, `cocotb-${reportIndex++}`);
    if (result.error) throw result.error;
    if (result.status !== 0) process.exit(result.status ?? 1);
  }
}
