import { writeFileSync, mkdirSync, unlinkSync, existsSync } from "node:fs";
import { join, resolve, dirname, relative, isAbsolute } from "node:path";
import type { GenTsJsonOutput } from "./types.js";

/** Default directory (relative to project root) for generated sidecar files. */
export const SIDECAR_DIR = ".celox";

/**
 * Generate `.d.veryl.ts` sidecar files in the sidecar directory,
 * mirroring the source tree structure.
 *
 * TypeScript picks these up via `allowArbitraryExtensions` + `rootDirs`
 * in tsconfig.  For example, `src/Adder.veryl` produces
 * `.celox/src/Adder.d.veryl.ts`.
 */
export function generateSidecars(
  data: GenTsJsonOutput,
  projectRoot: string,
): string[] {
  const written: string[] = [];
  const root = resolve(projectRoot);
  const outDir = join(root, SIDECAR_DIR);

  for (const [sourceFile, moduleNames] of Object.entries(data.fileModules)) {
    const verylPath = resolve(projectRoot, sourceFile);
    // Skip files that don't exist on disk (e.g. standard library paths
    // reported with a stripped leading "/" by celox-gen-ts)
    if (!existsSync(verylPath)) continue;

    const rel = relative(root, verylPath);
    // Skip files outside the project root (e.g. standard library from global
    // cache).  On Windows, relative() across different drives returns an
    // absolute path; on all platforms, paths outside root start with "..".
    if (isAbsolute(rel) || rel.startsWith("..")) continue;
    const sidecarPath = sidecarPathFor(join(outDir, rel));

    const modules = moduleNames
      .map((name) => data.modules.find((m) => m.moduleName === name))
      .filter((m) => m !== undefined);

    if (modules.length === 0) continue;

    // Combine dtsContent from all modules in this file
    // Each module's dtsContent already contains the import and interface
    const content = modules.map((m) => m.dtsContent).join("\n");

    mkdirSync(dirname(sidecarPath), { recursive: true });
    writeFileSync(sidecarPath, content, "utf-8");
    written.push(sidecarPath);
  }

  return written;
}

/**
 * Remove all sidecar files that were previously generated.
 */
export function cleanSidecars(paths: string[]): void {
  for (const p of paths) {
    try {
      if (existsSync(p)) {
        unlinkSync(p);
      }
    } catch {
      // ignore cleanup errors
    }
  }
}

/**
 * Given `/path/to/.celox/src/Adder.veryl`, returns `/path/to/.celox/src/Adder.d.veryl.ts`.
 */
function sidecarPathFor(verylPath: string): string {
  const dir = dirname(verylPath);
  const base = verylPath.slice(dir.length + 1); // "Adder.veryl"
  const stem = base.replace(/\.veryl$/, "");
  return join(dir, `${stem}.d.veryl.ts`);
}
