import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { createServer, type Plugin, type ResolvedConfig } from "vite";
import type { TestbenchComponentManifests } from "./types.js";

export const TESTBENCH_COMPONENT_SIDECAR = join(
	".celox",
	"testbench-components",
	"veryl.manifest.json",
);

function componentLoaderPlugins(config: ResolvedConfig): Plugin[] {
	return config.plugins
		.filter(
			(plugin) =>
				plugin.name !== "vite-plugin-celox" && !plugin.name.startsWith("vite:"),
		)
		.map((plugin) => ({
			name: `celox-component-loader:${plugin.name}`,
			enforce: plugin.enforce,
			resolveId: plugin.resolveId,
			load: plugin.load,
			transform: plugin.transform,
		}));
}

/** Load a TypeScript component module through Vite's own transform pipeline. */
export async function loadTestbenchComponentModule(
	modulePath: string,
	projectRoot: string,
	config: ResolvedConfig,
): Promise<TestbenchComponentManifests> {
	const server = await createServer({
		root: projectRoot,
		configFile: false,
		mode: config.mode,
		define: config.define,
		resolve: {
			alias: config.resolve.alias,
			conditions: config.resolve.conditions,
			dedupe: config.resolve.dedupe,
			extensions: config.resolve.extensions,
			mainFields: config.resolve.mainFields,
			preserveSymlinks: config.resolve.preserveSymlinks,
		},
		plugins: componentLoaderPlugins(config),
		logLevel: "silent",
		appType: "custom",
		server: { middlewareMode: true },
	});
	try {
		const loaded = (await server.ssrLoadModule(modulePath)) as {
			default?: unknown;
		};
		if (
			typeof loaded.default !== "object" ||
			loaded.default === null ||
			Array.isArray(loaded.default)
		) {
			throw new TypeError(
				`Testbench component module must default-export an object: ${modulePath}`,
			);
		}
		const components = loaded.default as Record<
			string,
			{ readonly manifest?: unknown }
		>;
		const manifests: TestbenchComponentManifests = {};
		for (const [name, component] of Object.entries(components)) {
			if (typeof component?.manifest !== "string") {
				throw new TypeError(
					`Testbench component ${name} has no generated manifest`,
				);
			}
			JSON.parse(component.manifest);
			manifests[name] = { manifest: component.manifest };
		}
		return manifests;
	} finally {
		await server.close();
	}
}

/**
 * Write an aggregated, deterministic manifest where Veryl's existing
 * `[[components]]` discovery can consume it as a committed manifest.
 */
export function writeTestbenchComponentSidecar(
	projectRoot: string,
	modulePath: string,
	components: TestbenchComponentManifests,
): string {
	const path = join(projectRoot, TESTBENCH_COMPONENT_SIDECAR);
	const types = Object.fromEntries(
		Object.entries(components)
			.sort(([left], [right]) => left.localeCompare(right))
			.map(([name, component]) => [name, JSON.parse(component.manifest)]),
	);
	const source = relative(projectRoot, modulePath).replace(/\\/g, "/");
	mkdirSync(dirname(path), { recursive: true });
	writeFileSync(
		path,
		`${JSON.stringify({ source, types }, null, 2)}\n`,
		"utf8",
	);
	return path;
}
