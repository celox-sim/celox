import { execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import { join } from "node:path";
import type { GenTsJsonOutput } from "./types.js";

/**
 * Discover the `celox-gen-ts` binary.
 *
 * Search order:
 * 1. Explicit path from plugin options
 * 2. `celox-gen-ts` on PATH
 * 3. `./target/release/celox-gen-ts` relative to project root
 * 4. `./target/debug/celox-gen-ts` relative to project root
 */
export function findGenTsBinary(
  explicitPath: string | undefined,
  projectRoot: string,
): string {
  if (explicitPath) {
    return explicitPath;
  }

  // Try PATH
  try {
    execFileSync("celox-gen-ts", ["--help"], { stdio: "ignore" });
    return "celox-gen-ts";
  } catch {
    // not on PATH
  }

  // Try target/release then target/debug
  for (const profile of ["release", "debug"]) {
    const candidate = join(projectRoot, "target", profile, "celox-gen-ts");
    if (existsSync(candidate)) {
      return candidate;
    }
  }

  throw new Error(
    "Could not find celox-gen-ts binary. Install it, add it to PATH, " +
      "or set the genTsBinary plugin option.",
  );
}

/**
 * Run `celox-gen-ts --json` and parse the output.
 */
export function runGenTs(
  binary: string,
  projectRoot: string,
): GenTsJsonOutput {
  const stdout = execFileSync(binary, ["--json"], {
    cwd: projectRoot,
    encoding: "utf-8",
    stdio: ["ignore", "pipe", "pipe"],
    maxBuffer: 50 * 1024 * 1024, // 50 MB
  });

  return JSON.parse(stdout) as GenTsJsonOutput;
}
