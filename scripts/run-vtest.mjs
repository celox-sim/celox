import { spawnSync } from "node:child_process";
import { fileURLToPath, pathToFileURL } from "node:url";
import { resolve } from "node:path";
import { appendFileSync } from "node:fs";

const root = fileURLToPath(new URL("../", import.meta.url));
export const runtimeCasesFeature = "celox-test-suite/runtime-cases";
const baseFeatures = `celox/systemverilog,${runtimeCasesFeature}`;
// CI's full Rust test build: workspace members and features of the
// "Run workspace tests with CI features" step and its cocotb rerun.
export const workspaceFeatures = "celox/systemverilog,celox/cranelift,celox/wasm-runtime,celox/wasm-component,celox/veryl-reference,celox-vpi/cranelift,celox-vpi/wasm-runtime,celox-bench/cranelift,celox-test-suite/verilator,celox-test-suite/icarus,celox-test-suite/xezim,celox-test-suite/runtime-cases";
// Test binaries build with the executor's profile, so one compilation of the
// workspace crates serves both when the packages and features match too.
export const testProfile = "vtest";
function features(env) {
  return env.CELOX_TEST_FEATURES ? `${baseFeatures},${env.CELOX_TEST_FEATURES}` : baseFeatures;
}

export function executorPath(output) {
  const paths = output.split("\n").filter(Boolean).map((line) => JSON.parse(line))
    .filter((message) => message.reason === "compiler-artifact"
      && message.target.name === "vtest-exec"
      && message.target.kind.includes("example") && message.executable)
    .map((message) => message.executable);
  if (paths.length !== 1) throw new Error("Cargo did not report one vtest executor");
  return paths[0];
}

/// The packages, features and test targets of the Cargo build the executor
/// shares: CI's full workspace build, the scoped build that tests Celox, or
/// the local default. Selecting the same test targets also selects the same
/// dev-dependencies, so the two builds resolve the same features.
export function executorScope(env = process.env) {
  if (env.CELOX_RUST_FULL !== undefined && env.CELOX_RUST_FULL !== "false")
    return ["--workspace", "--exclude", "lydite*", "--features", workspaceFeatures, "--tests"];
  if (env.CELOX_RUST_FULL === "false") {
    const builds = JSON.parse(env.BUILDS ?? "[]");
    const celox = Array.isArray(builds) && builds.find((build) => build?.packages?.includes("celox"));
    if (!celox) throw new Error("The scoped build plan does not test Celox");
    for (const name of [...celox.packages, ...celox.features, ...(celox.targets ?? [])]) {
      if (typeof name !== "string" || !/^[a-zA-Z0-9_][a-zA-Z0-9_./-]*$/.test(name))
        throw new Error("Invalid Cargo scope argument");
    }
    // As run-ci-rust-scope.mjs enables them for a build that tests Celox.
    const enabled = [...new Set([...celox.features, runtimeCasesFeature])];
    const targets = celox.targets?.length ? celox.targets.flatMap((name) => ["--test", name]) : ["--tests"];
    return [...celox.packages.flatMap((name) => ["-p", name]), "--features", enabled.join(","), ...targets];
  }
  return ["-p", "celox", "--features", features(env)];
}

export function buildExecutor(spawn = spawnSync, env = process.env) {
  const build = spawn("cargo", [
    "build", "--locked", ...executorScope(env), "--example", "vtest-exec",
    "--profile", testProfile, "--message-format=json-render-diagnostics",
  ], { cwd: root, env, encoding: "utf8", stdio: ["inherit", "pipe", "inherit"], maxBuffer: 64 * 1024 * 1024 });
  if (build.error) throw build.error;
  if (build.status !== 0) return { status: build.status ?? 1 };
  const executable = executorPath(build.stdout);
  return {
    status: 0,
    environment: { CELOX_VTEST_RUNNER: executable, CELOX_VTEST_ROOT: resolve(root, "crates/celox-test-suite/src") },
  };
}

// Prepare once per CI job, even when scoped tests need several Cargo invocations,
// with the packages and features of the build that tests Celox.
export function prepareCi(spawn = spawnSync, env = process.env, append = appendFileSync) {
  if (!env.GITHUB_ENV) throw new Error("--prepare-ci requires GITHUB_ENV");
  if (env.CELOX_RUST_FULL === undefined) throw new Error("--prepare-ci requires CELOX_RUST_FULL");
  const result = buildExecutor(spawn, env);
  if (result.status !== 0) return result.status;
  const lines = Object.entries(result.environment).map(([name, value]) => {
    if (/[\r\n]/.test(value)) throw new Error("Invalid executor environment path");
    return `${name}=${value}\n`;
  });
  append(env.GITHUB_ENV, lines.join(""));
  return 0;
}

// Keep the registrations in libtest: nextest still owns filters, ignores,
// process isolation, timeouts and parallel scheduling, including Rust API tests.
export function run(args, spawn = spawnSync, env = process.env) {
  const result = buildExecutor(spawn, env);
  if (result.status !== 0) return result.status;
  const tests = spawn("cargo", [
    "nextest", "run", "--locked", "-p", "celox", "--features", features(env), ...args,
  ], {
    cwd: root,
    env: { ...env, ...result.environment },
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
  CELOX_TEST_FEATURES=cranelift,wasm-runtime node scripts/run-vtest.mjs --test systemverilog -E 'test(operators::)'

The runner and registrations both enable systemverilog and runtime-cases.
Set CELOX_TEST_FEATURES=cranelift,wasm-runtime to include optional backends.
Use ordinary cargo nextest run for the existing in-process test path.`);
  } else if (process.argv[2] === "--prepare-ci") {
    if (process.argv.length !== 3) throw new Error("--prepare-ci accepts no test arguments");
    process.exitCode = prepareCi();
  } else {
    process.exitCode = run(process.argv.slice(2));
  }
}
