import { resolve, isAbsolute, dirname } from "node:path";
import { existsSync } from "node:fs";
import type { Plugin } from "vite";
import type { CeloxPluginOptions, GenTsModule } from "./types.js";
import { GenTsCache } from "./cache.js";
import { generateSidecars, cleanSidecars } from "./sidecar.js";

export type { CeloxPluginOptions } from "./types.js";

const VERYL_PREFIX = "\0veryl:";

/**
 * Vite plugin for importing `.veryl` files as typed `ModuleDefinition` objects.
 *
 * ```ts
 * // vitest.config.ts
 * import celox from "@celox-sim/vite-plugin";
 * export default defineConfig({ plugins: [celox()] });
 *
 * // test file
 * import { Adder } from './src/Adder.veryl';
 * const sim = Simulator.create(Adder); // fully typed
 * ```
 */
export default function celoxPlugin(options?: CeloxPluginOptions): Plugin {
  let projectRoot: string;
  let cache: GenTsCache;
  let sidecarPaths: string[] = [];

  return {
    name: "vite-plugin-celox",
    enforce: "pre",

    configResolved(config) {
      // Determine project root
      if (options?.projectRoot) {
        projectRoot = resolve(options.projectRoot);
      } else {
        projectRoot = findVerylProjectRoot(config.root);
      }

      cache = new GenTsCache(projectRoot);
    },

    buildStart() {
      // Run generator and create type sidecars
      const data = cache.get();
      cleanSidecars(sidecarPaths);
      sidecarPaths = generateSidecars(data, projectRoot);
    },

    resolveId(source, importer) {
      if (!source.endsWith(".veryl")) return;

      // Resolve to absolute path
      let absPath: string;
      if (isAbsolute(source)) {
        absPath = source;
      } else if (importer) {
        absPath = resolve(dirname(importer), source);
      } else {
        absPath = resolve(source);
      }

      // Only handle .veryl files that exist
      if (!existsSync(absPath)) return;

      return VERYL_PREFIX + absPath;
    },

    load(id) {
      if (!id.startsWith(VERYL_PREFIX)) return;

      const absPath = id.slice(VERYL_PREFIX.length);
      const data = cache.get();

      // Find the relative source file path
      const relPath = makeRelative(absPath, projectRoot);
      const moduleNames = data.fileModules[relPath];

      if (!moduleNames || moduleNames.length === 0) {
        this.warn(`No modules found in ${relPath}`);
        return "export {};";
      }

      // Build ESM exports for each module in this file
      const exports = moduleNames
        .map((name) => {
          const mod = data.modules.find((m) => m.moduleName === name);
          if (!mod) return "";
          return generateEsmExport(mod, data.projectPath);
        })
        .filter((s) => s.length > 0)
        .join("\n\n");

      return exports;
    },

    handleHotUpdate({ file }) {
      if (!file.endsWith(".veryl")) return;

      // Invalidate cache so next load re-runs the generator
      cache.invalidate();

      // Re-generate sidecars
      const data = cache.get();
      cleanSidecars(sidecarPaths);
      sidecarPaths = generateSidecars(data, projectRoot);
    },
  };
}

/**
 * Search upward from `startDir` for a directory containing `Veryl.toml`.
 */
function findVerylProjectRoot(startDir: string): string {
  let dir = resolve(startDir);
  // eslint-disable-next-line no-constant-condition
  while (true) {
    if (existsSync(resolve(dir, "Veryl.toml"))) {
      return dir;
    }
    const parent = dirname(dir);
    if (parent === dir) {
      throw new Error(
        "Could not find Veryl.toml in any parent directory. " +
          "Set the projectRoot plugin option explicitly.",
      );
    }
    dir = parent;
  }
}

/**
 * Make `absPath` relative to `base`, using forward slashes.
 */
function makeRelative(absPath: string, base: string): string {
  let rel = absPath;
  if (absPath.startsWith(base)) {
    rel = absPath.slice(base.length);
  }
  // Trim leading slash
  if (rel.startsWith("/")) {
    rel = rel.slice(1);
  }
  return rel;
}

/**
 * Generate an ESM export for a single module.
 */
function generateEsmExport(mod: GenTsModule, projectPath: string): string {
  const portsJson = JSON.stringify(mod.ports, null, 2)
    .split("\n")
    .map((line, i) => (i === 0 ? line : "  " + line))
    .join("\n");

  const eventsJson = JSON.stringify(mod.events);

  return `export const ${mod.moduleName} = {
  __celox_module: true,
  name: ${JSON.stringify(mod.moduleName)},
  source: "",
  projectPath: ${JSON.stringify(projectPath)},
  ports: ${portsJson},
  events: ${eventsJson},
};`;
}
