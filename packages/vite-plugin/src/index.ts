import { existsSync } from "node:fs";
import { dirname, isAbsolute, resolve } from "node:path";
import { normalizePath, type Plugin, type ResolvedConfig } from "vite";
import { GenTsCache } from "./cache.js";
import { cleanSidecars, generateSidecars } from "./sidecar.js";
import {
	loadTestbenchComponentModule,
	normalizeTestbenchComponentPath,
	writeTestbenchComponentSidecar,
} from "./testbench-components.js";
import type {
	CeloxPluginOptions,
	GenTsModule,
	TestbenchComponentManifests,
} from "./types.js";

export type { CeloxPluginOptions } from "./types.js";

const PROJECT_QUERY = "celox-project";
const WATCH_QUERY = "celox-watch";
const CONFIG_FILES = ["Veryl.toml", "celox.toml", "Veryl.lock"];

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
	let generatedData: ReturnType<GenTsCache["get"]> | undefined;
	let testbenchComponents: string | undefined;
	let testbenchComponentDependencies = new Set<string>();
	let viteConfig: ResolvedConfig;
	let projectId: string;
	const loadedIds = new Set<string>();
	let lastUpdate:
		| { file: string; timestamp: number; result: Promise<boolean> }
		| undefined;

	function projectFile(name: string): string {
		return normalizePath(resolve(projectRoot, name));
	}

	function regenerate(file: string, timestamp?: number): Promise<boolean> {
		file = normalizePath(file);
		if (
			timestamp !== undefined &&
			lastUpdate?.file === file &&
			lastUpdate.timestamp === timestamp
		)
			return lastUpdate.result;
		const result = (async () => {
			const componentChanged = testbenchComponentDependencies.has(
				normalizeTestbenchComponentPath(file),
			);
			if (
				!file.endsWith(".veryl") &&
				!componentChanged &&
				!CONFIG_FILES.some((name) => projectFile(name) === file)
			)
				return false;
			if (componentChanged && testbenchComponents) {
				const loaded = await loadTestbenchComponentModule(
					testbenchComponents,
					projectRoot,
					viteConfig,
				);
				testbenchComponentDependencies = loaded.dependencies;
				writeTestbenchComponentSidecar(
					projectRoot,
					testbenchComponents,
					loaded.manifests,
				);
				cache.setTestbenchComponents(loaded.manifests);
			}
			// Watch events can detect edits that preserve both mtime and size.
			// Keep fingerprint checks for generator-owned lockfile notifications.
			if (file !== projectFile("Veryl.lock")) cache.invalidate();
			const data = cache.get();
			if (data !== generatedData) {
				const nextPaths = generateSidecars(data, projectRoot);
				const retained = new Set(nextPaths);
				cleanSidecars(sidecarPaths.filter((path) => !retained.has(path)));
				sidecarPaths = nextPaths;
				generatedData = data;
			}
			return true;
		})();
		if (timestamp !== undefined) lastUpdate = { file, timestamp, result };
		return result;
	}

	function affectedModules<T>(
		graph: {
			getModuleById(id: string): T | undefined;
			invalidateModule(mod: T): void;
		},
		changed: T[] = [],
	) {
		const modules = new Set(changed);
		for (const id of [projectId, ...loadedIds]) {
			const mod = graph.getModuleById(id);
			if (mod) modules.add(mod);
		}
		for (const mod of modules) graph.invalidateModule(mod);
		return [...modules];
	}

	return {
		name: "vite-plugin-celox",
		enforce: "pre",

		configResolved(config) {
			viteConfig = config;
			// Determine project root
			if (options?.projectRoot) {
				projectRoot = resolve(options.projectRoot);
			} else {
				projectRoot = findVerylProjectRoot(config.root);
			}

			projectRoot = normalizePath(projectRoot);
			projectId = `${projectFile("Veryl.toml")}?${PROJECT_QUERY}`;

			if (options?.testbenchComponents) {
				const componentsModule = options.testbenchComponents;
				testbenchComponents = isAbsolute(componentsModule)
					? componentsModule
					: resolve(projectRoot, componentsModule);
				if (!existsSync(testbenchComponents)) {
					throw new Error(
						`Could not find testbench components module: ${testbenchComponents}`,
					);
				}
			}
		},

		async buildStart() {
			let componentManifests: TestbenchComponentManifests | undefined;
			if (testbenchComponents) {
				const loaded = await loadTestbenchComponentModule(
					testbenchComponents,
					projectRoot,
					viteConfig,
				);
				componentManifests = loaded.manifests;
				testbenchComponentDependencies = loaded.dependencies;
				for (const dependency of testbenchComponentDependencies) {
					this.addWatchFile(dependency);
				}
				writeTestbenchComponentSidecar(
					projectRoot,
					testbenchComponents,
					componentManifests,
				);
			}
			cache = new GenTsCache(projectRoot, componentManifests);
			// Run generator and create type sidecars
			const data = cache.get();
			cleanSidecars(sidecarPaths);
			sidecarPaths = generateSidecars(data, projectRoot);
			generatedData = data;
		},

		configureServer(server) {
			server.watcher.add(CONFIG_FILES.map(projectFile));
			const hmrDisabled = server.config.server.hmr === false;
			// Vitest disables HMR, so its watcher also needs cache/sidecar updates.
			const sourceChanged = (file: string) => {
				// Raw events have no shared per-environment timestamp to deduplicate.
				void regenerate(file)
					.then((changed) => {
						if (!changed) return;
						if (server.environments) {
							for (const environment of Object.values(server.environments))
								affectedModules(environment.moduleGraph);
						} else {
							affectedModules(server.moduleGraph);
						}
					})
					.catch((error) => server.config.logger.error(String(error)));
			};
			if (hmrDisabled) server.watcher.on("change", sourceChanged);
			// Vite 6+ with HMR invokes hotUpdate for create/delete itself.
			if (server.environments && !hmrDisabled) return;
			// New/deleted sources have no existing module node. Notify through
			// the shared project dependency on Vite 5 or with HMR disabled.
			const catalogueChanged = (file: string) => {
				file = normalizePath(file);
				if (
					(file.startsWith(`${projectRoot}/`) && file.endsWith(".veryl")) ||
					CONFIG_FILES.some((name) => projectFile(name) === file)
				) {
					server.watcher.emit("change", projectFile("Veryl.toml"));
				}
			};
			server.watcher.on("add", catalogueChanged);
			server.watcher.on("unlink", catalogueChanged);
			server.httpServer?.once("close", () => {
				server.watcher.off("change", sourceChanged);
				server.watcher.off("add", catalogueChanged);
				server.watcher.off("unlink", catalogueChanged);
			});
		},

		resolveId(source, importer) {
			// Split off query string (e.g. "?dse=preserveAllPorts")
			const [rawPath, queryStr] = source.split("?", 2);
			if (queryStr === PROJECT_QUERY || queryStr === WATCH_QUERY)
				return normalizePath(source);
			if (!rawPath!.endsWith(".veryl")) return;

			// Resolve to absolute path
			let absPath: string;
			if (isAbsolute(rawPath!)) {
				absPath = rawPath!;
			} else if (importer) {
				absPath = resolve(dirname(importer.split("?", 1)[0]!), rawPath!);
			} else {
				absPath = resolve(rawPath!);
			}

			absPath = normalizePath(absPath);

			// Only handle .veryl files that exist
			if (!existsSync(absPath)) return;

			// Preserve the real file identity so Vite/Vitest can trace changes.
			return absPath + (queryStr ? `?${queryStr}` : "");
		},

		load(id) {
			const [absPath, queryStr] = id.split("?", 2);
			if (queryStr === WATCH_QUERY) {
				// Vite 5 also adds watch files to import analysis, which requires
				// them to exist. configureServer watches optional configs separately.
				if (existsSync(absPath!)) this.addWatchFile(absPath!);
				return "export {};";
			}
			if (id === projectId) {
				cache.get();
				const files = new Set([
					...cache.sourceFiles.map(normalizePath),
					...CONFIG_FILES.map(projectFile),
					...testbenchComponentDependencies,
				]);
				return [...files]
					.map((file) => `import ${JSON.stringify(`${file}?${WATCH_QUERY}`)};`)
					.join("\n");
			}
			if (!absPath!.endsWith(".veryl")) return;
			loadedIds.add(id);
			this.addWatchFile(absPath!);
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

			// Partition into regular and test modules
			const regularModules: string[] = [];
			const testModules: string[] = [];
			for (const name of moduleNames) {
				const mod = data.modules.find((m) => m.moduleName === name);
				if (!mod) continue;
				if (mod.isTest) {
					testModules.push(name);
				} else {
					regularModules.push(name);
				}
			}

			const parts: string[] = [`import ${JSON.stringify(projectId)};`];

			// Build ESM exports for regular (non-test) modules
			for (const name of regularModules) {
				const mod = data.modules.find((m) => m.moduleName === name);
				if (!mod) continue;
				const code = generateEsmExport(mod, data.projectPath, defaultOptions);
				if (code.length > 0) parts.push(code);
			}

			// Generate vitest test code for test modules (only in test mode)
			if (
				testModules.length > 0 &&
				(process.env.VITEST === "true" || process.env.NODE_ENV === "test")
			) {
				parts.push(
					generateTestCode(testModules, data.projectPath, testbenchComponents),
				);
			}

			return parts.join("\n\n") || "export {};";
		},

		async hotUpdate({ file, timestamp, modules }) {
			if ((await regenerate(file, timestamp)) === false) return;
			return affectedModules(this.environment.moduleGraph, modules);
		},

		// Vite 5 uses the server graph; newer Vite calls hotUpdate above.
		async handleHotUpdate({ file, timestamp, server, modules }) {
			if ((await regenerate(file, timestamp)) === false) return;
			return affectedModules(server.moduleGraph, modules);
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

/**
 * Generate vitest test code for native testbench modules.
 *
 * Each `#[test]` module becomes a single `test()` case that runs the
 * testbench via NAPI and reports the assertion results observed before that
 * test finishes or stops on a fatal failure.
 */
function generateTestCode(
	testModuleNames: string[],
	projectPath: string,
	testbenchComponents?: string,
): string {
	const lines: string[] = [`import { test, expect } from "vitest";`];
	if (testbenchComponents) {
		lines.push(
			`import { runTestFromProject } from "@celox-sim/celox";`,
			`import __components from ${JSON.stringify(testbenchComponents.replace(/\\/g, "/"))};`,
			``,
		);
	} else {
		lines.push(
			`import { loadNativeAddon } from "@celox-sim/celox";`,
			``,
			`const __addon = loadNativeAddon();`,
			``,
		);
	}

	for (const name of testModuleNames) {
		lines.push(`test(${JSON.stringify(name)}, () => {`);
		lines.push(
			testbenchComponents
				? `  const __result = runTestFromProject(${JSON.stringify(projectPath)}, ${JSON.stringify(name)}, { components: __components });`
				: `  const __result = __addon.runTestFromProject(${JSON.stringify(projectPath)}, ${JSON.stringify(name)});`,
		);
		lines.push(`  for (const __a of __result.assertions) {`);
		lines.push(`    if (!__a.passed) {`);
		lines.push(
			`      const __loc = __a.file ? \` (\${__a.file}:\${__a.line}:\${__a.column})\` : "";`,
		);
		lines.push(`      let __message = "assertion failed";`);
		lines.push(`      if (__a.message) {`);
		lines.push(
			`        try { __message = new TextDecoder("utf-8", { fatal: true }).decode(__a.message); }`,
		);
		lines.push(
			`        catch { __message = Array.from(__a.message, b => "\\\\x" + b.toString(16).padStart(2, "0")).join(""); }`,
		);
		lines.push(`      }`);
		lines.push(`      expect.soft(__a.passed, __message + __loc).toBe(true);`);
		lines.push(`    }`);
		lines.push(`  }`);
		lines.push(
			`  expect(__result.passed, __result.error ?? "testbench failed").toBe(true);`,
		);
		lines.push(`});`);
		lines.push(``);
	}

	return lines.join("\n");
}
