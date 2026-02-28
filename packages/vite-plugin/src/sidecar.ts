import { writeFileSync, unlinkSync, existsSync } from "node:fs";
import { join, resolve, dirname } from "node:path";
import type { GenTsJsonOutput } from "./types.js";

/**
 * Generate `.d.veryl.ts` sidecar files next to each `.veryl` source file.
 *
 * TypeScript picks these up via `allowArbitraryExtensions: true` in tsconfig.
 * The file `Adder.d.veryl.ts` provides types for `import { Adder } from './Adder.veryl'`.
 */
export function generateSidecars(
  data: GenTsJsonOutput,
  projectRoot: string,
): string[] {
  const written: string[] = [];

  for (const [sourceFile, moduleNames] of Object.entries(data.fileModules)) {
    const verylPath = resolve(projectRoot, sourceFile);
    // Skip files that don't exist on disk (e.g. standard library paths
    // reported with a stripped leading "/" by celox-gen-ts)
    if (!existsSync(verylPath)) continue;
    const sidecarPath = sidecarPathFor(verylPath);

    const modules = moduleNames
      .map((name) => data.modules.find((m) => m.moduleName === name))
      .filter((m) => m !== undefined);

    if (modules.length === 0) continue;

    // Combine dtsContent from all modules in this file
    // Each module's dtsContent already contains the import and interface
    const content = modules.map((m) => m.dtsContent).join("\n");

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
 * Given `/path/to/Adder.veryl`, returns `/path/to/Adder.d.veryl.ts`.
 */
function sidecarPathFor(verylPath: string): string {
  const dir = dirname(verylPath);
  // "Adder.veryl" → "Adder.d.veryl.ts"
  const base = verylPath.slice(dir.length + 1); // "Adder.veryl"
  const stem = base.replace(/\.veryl$/, "");
  return join(dir, `${stem}.d.veryl.ts`);
}
