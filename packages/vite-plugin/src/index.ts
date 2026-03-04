import { existsSync } from "node:fs";
import { dirname, isAbsolute, resolve } from "node:path";
import type { Plugin } from "vite";
import { GenTsCache } from "./cache.js";
import { cleanSidecars, generateSidecars } from "./sidecar.js";
import type { CeloxPluginOptions, GenTsModule } from "./types.js";

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
			// Split off query string (e.g. "?dse=preserveAllPorts")
			const [rawPath, queryStr] = source.split("?", 2);
			if (!rawPath!.endsWith(".veryl")) return;

			// Resolve to absolute path
			let absPath: string;
			if (isAbsolute(rawPath!)) {
				absPath = rawPath!;
			} else if (importer) {
				absPath = resolve(dirname(importer), rawPath!);
			} else {
				absPath = resolve(rawPath!);
			}

			// Only handle .veryl files that exist
			if (!existsSync(absPath)) return;

			// Preserve query string in virtual ID
			return VERYL_PREFIX + absPath + (queryStr ? `?${queryStr}` : "");
		},

		load(id) {
			if (!id.startsWith(VERYL_PREFIX)) return;

			const rest = id.slice(VERYL_PREFIX.length);
			const [absPath, queryStr] = rest.split("?", 2);
			const params = new URLSearchParams(queryStr ?? "");

			// Parse ?dse= query parameter
			let dsePolicy: string | undefined;
			if (params.has("dse")) {
				const raw = params.get("dse");
				// ?dse (no value) defaults to "preserveAllPorts" (safe side)
				dsePolicy = raw || "preserveAllPorts";
			}

			const data = cache.get();

			// Find the relative source file path
			const relPath = makeRelative(absPath!, projectRoot);
			const moduleNames = data.fileModules[relPath];

			if (!moduleNames || moduleNames.length === 0) {
				this.warn(`No modules found in ${relPath}`);
				return "export {};";
			}

			const defaultOptions = dsePolicy
				? { deadStorePolicy: dsePolicy }
				: undefined;

			// Build ESM exports for each module in this file
			const exports = moduleNames
				.map((name) => {
					const mod = data.modules.find((m) => m.moduleName === name);
					if (!mod) return "";
					return generateEsmExport(mod, data.projectPath, defaultOptions);
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
	// Normalise to forward slashes so the comparison works on Windows
	const normAbs = absPath.replace(/\\/g, "/");
	const normBase = base.replace(/\\/g, "/").replace(/\/$/, "");
	let rel = normAbs;
	if (normAbs.startsWith(`${normBase}/`)) {
		rel = normAbs.slice(normBase.length + 1);
	} else if (normAbs.startsWith(normBase)) {
		rel = normAbs.slice(normBase.length);
	}
	if (rel.startsWith("/")) {
		rel = rel.slice(1);
	}
	return rel;
}

/**
 * Generate an ESM export for a single module.
 */
function generateEsmExport(
	mod: GenTsModule,
	projectPath: string,
	defaultOptions?: Record<string, unknown>,
): string {
	const portsJson = JSON.stringify(mod.ports, null, 2)
		.split("\n")
		.map((line, i) => (i === 0 ? line : `  ${line}`))
		.join("\n");

	const eventsJson = JSON.stringify(mod.events);

	const defaultOptsLine = defaultOptions
		? `\n  defaultOptions: ${JSON.stringify(defaultOptions)},`
		: "";

	return `export const ${mod.moduleName} = {
  __celox_module: true,
  name: ${JSON.stringify(mod.moduleName)},
  sources: [],
  projectPath: ${JSON.stringify(projectPath)},
  ports: ${portsJson},
  events: ${eventsJson},${defaultOptsLine}
};`;
}
