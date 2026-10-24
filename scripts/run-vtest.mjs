import { spawnSync } from "node:child_process";
import { fileURLToPath, pathToFileURL } from "node:url";
import { resolve } from "node:path";

const root = fileURLToPath(new URL("../", import.meta.url));
const features = "systemverilog,celox-test-suite/runtime-cases";

export function executorPath(output) {
  const paths = output.split("\n").filter(Boolean).map((line) => JSON.parse(line))
    .filter((message) => message.reason === "compiler-artifact"
      && message.target.name === "vtest-exec"
      && message.target.kind.includes("example") && message.executable)
    .map((message) => message.executable);
  if (paths.length !== 1) throw new Error("Cargo did not report one vtest executor");
  return paths[0];
}

// Keep the registrations in libtest: nextest still owns filters, ignores,
// process isolation, timeouts and parallel scheduling, including Rust API tests.
export function run(args, spawn = spawnSync, env = process.env) {
  const build = spawn("cargo", [
    "build", "--locked", "-p", "celox", "--example", "vtest-exec",
    "--profile", "vtest", "--features", features, "--message-format=json-render-diagnostics",
  ], { cwd: root, env, encoding: "utf8", stdio: ["inherit", "pipe", "inherit"], maxBuffer: 64 * 1024 * 1024 });
  if (build.error) throw build.error;
  if (build.status !== 0) return build.status ?? 1;
  const executable = executorPath(build.stdout);
  const tests = spawn("cargo", [
    "nextest", "run", "--locked", "-p", "celox", "--features", features, ...args,
  ], {
    cwd: root,
    env: { ...env, CELOX_VTEST_RUNNER: executable, CELOX_VTEST_ROOT: resolve(root, "crates/celox-test-suite/src") },
    stdio: "inherit",
  });
  if (tests.error) throw tests.error;
  return tests.status ?? 1;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  if (process.argv.includes("--help")) {
    console.log(`Usage: node scripts/run-vtest.mjs [cargo nextest run options]

Build the shared vtest executor with opt-level=1, then run Celox tests through
nextest. Case bodies are read from disk; Rust API tests run in their test binary.
Examples:
  node scripts/run-vtest.mjs --test counter
  node scripts/run-vtest.mjs --test operators -E 'test(::native$)'
  node scripts/run-vtest.mjs --test systemverilog -E 'test(operators::)'

The runner and registrations both enable systemverilog and runtime-cases.
Use ordinary cargo nextest run for the existing in-process test path.`);
  } else {
    process.exitCode = run(process.argv.slice(2));
  }
}
