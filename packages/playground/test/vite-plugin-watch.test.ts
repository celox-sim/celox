import { EventEmitter } from "node:events";
import { mkdtempSync, rmSync, utimesSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, expect, test, vi } from "vitest";

const platform = vi.hoisted(() => ({ windows: false }));
vi.mock("node:path", async (importOriginal) => {
	const path = await importOriginal<typeof import("node:path")>();
	return {
		...path,
		resolve: (...paths: string[]) =>
			platform.windows ? path.win32.resolve(...paths) : path.resolve(...paths),
	};
});
vi.mock("vite", async (importOriginal) => ({
	...(await importOriginal<typeof import("vite")>()),
	// Simulate Windows normalization together with win32.resolve above.
	normalizePath: (path: string) => path.replace(/\\/g, "/"),
}));
vi.mock("../../vite-plugin/src/generator.js", () => ({
	runGenTs: vi.fn((projectRoot: string) => ({
		projectPath: projectRoot,
		modules: [],
		fileModules: {},
	})),
}));

import { runGenTs } from "../../vite-plugin/src/generator.js";
import celox from "../../vite-plugin/src/index.js";

const roots: string[] = [];
afterEach(() => {
	platform.windows = false;
	vi.mocked(runGenTs).mockClear();
	for (const root of roots.splice(0))
		rmSync(root, { recursive: true, force: true });
});

async function initialize(root: string) {
	const plugin = celox({ projectRoot: root });
	const context = { addWatchFile: vi.fn(), warn: vi.fn() };
	await (plugin.configResolved as Function)({ root });
	await (plugin.buildStart as Function).call(context);
	return { plugin, context };
}

function graph() {
	const modules = new Map<string, { id: string }>();
	return {
		getModuleById(id: string) {
			if (!modules.has(id)) modules.set(id, { id });
			return modules.get(id);
		},
		invalidateModule: vi.fn(),
	};
}

test("Windows project imports resolve to a loadable dependency module", async () => {
	platform.windows = true;
	const { plugin, context } = await initialize("C:/project");
	const sourceId = "C:/project/Main.veryl";
	// A source with no exported modules still needs the project module loaded
	// directly here to check its dependency graph identity.
	const projectId = "C:/project/Veryl.toml?celox-project";
	const resolved = (plugin.resolveId as Function)(projectId, sourceId);
	expect(resolved).toBe(projectId);
	const code = (plugin.load as Function).call(context, resolved);
	expect(code).toContain('import "C:/project/Veryl.toml?celox-watch"');
	expect(code).toContain('import "C:/project/celox.toml?celox-watch"');
	const moduleGraph = graph();
	const result = await (plugin.hotUpdate as Function).call(
		{ environment: { moduleGraph } },
		{ file: "C:\\project\\celox.toml", timestamp: 1 },
	);
	expect(result).toEqual([{ id: projectId }]);
	expect(runGenTs).toHaveBeenCalledTimes(2);
});

test.each(["Main.veryl", "Veryl.toml", "celox.toml"])(
	"watch edits of %s invalidate unchanged mtime/size fingerprints once across environments",
	async (name) => {
		const root = mkdtempSync(join(tmpdir(), "celox-watch-"));
		roots.push(root);
		const file = join(root, name);
		writeFileSync(file, "before");
		utimesSync(file, 1_700_000_000, 1_700_000_000);
		const { plugin } = await initialize(root);
		writeFileSync(file, "edited");
		utimesSync(file, 1_700_000_000, 1_700_000_000);
		for (const moduleGraph of [graph(), graph()]) {
			await (plugin.hotUpdate as Function).call(
				{ environment: { moduleGraph } },
				{ file, timestamp: 1 },
			);
			expect(moduleGraph.invalidateModule).toHaveBeenCalled();
		}
		expect(runGenTs).toHaveBeenCalledTimes(2);
	},
);

test("unchanged lockfile notifications reuse generation", async () => {
	const root = mkdtempSync(join(tmpdir(), "celox-watch-lock-"));
	roots.push(root);
	const file = join(root, "Veryl.lock");
	writeFileSync(file, "lock");
	const { plugin } = await initialize(root);
	await (plugin.hotUpdate as Function).call(
		{ environment: { moduleGraph: graph() } },
		{ file, timestamp: 1 },
	);
	expect(runGenTs).toHaveBeenCalledTimes(1);
});

test("watch modules omit absent optional configs from import-analysis dependencies", async () => {
	const root = mkdtempSync(join(tmpdir(), "celox-watch-optional-"));
	roots.push(root);
	const { plugin, context } = await initialize(root);
	const file = join(root, "celox.toml");
	const id = `${file}?celox-watch`;
	expect((plugin.load as Function).call(context, id)).toBe("export {};");
	expect(context.addWatchFile).not.toHaveBeenCalledWith(file);
	writeFileSync(file, "[test]\nsources=[]\n");
	expect((plugin.load as Function).call(context, id)).toBe("export {};");
	expect(context.addWatchFile).toHaveBeenCalledWith(file);
});

test.each([
	{ hasEnvironments: false, hmr: true },
	{ hasEnvironments: true, hmr: true },
	{ hasEnvironments: true, hmr: false },
])(
	"catalogue events avoid duplicate HMR with environments=$hasEnvironments, hmr=$hmr",
	async ({ hasEnvironments, hmr }) => {
		const root = mkdtempSync(join(tmpdir(), "celox-watch-events-"));
		roots.push(root);
		const { plugin } = await initialize(root);
		const watcher = Object.assign(new EventEmitter(), { add: vi.fn() });
		const changes = vi.fn();
		watcher.on("change", changes);
		const httpServer = new EventEmitter();
		(plugin.configureServer as Function)({
			watcher,
			httpServer,
			config: { server: { hmr }, logger: { error: vi.fn() } },
			environments: hasEnvironments ? {} : undefined,
			moduleGraph: graph(),
		});
		for (const event of ["add", "unlink"]) {
			watcher.emit(event, join(root, "Extra.veryl"));
			watcher.emit(event, join(root, "celox.toml"));
		}
		const expected = hasEnvironments && hmr ? 0 : 4;
		expect(changes).toHaveBeenCalledTimes(expected);
		httpServer.emit("close");
		watcher.emit("add", join(root, "Extra.veryl"));
		expect(changes).toHaveBeenCalledTimes(expected);
	},
);

test("HMR-disabled watch edits refresh data and stop updating after server close", async () => {
	const root = mkdtempSync(join(tmpdir(), "celox-watch-disabled-"));
	roots.push(root);
	const file = join(root, "Main.veryl");
	writeFileSync(file, "before");
	utimesSync(file, 1_700_000_000, 1_700_000_000);
	const { plugin } = await initialize(root);
	const watcher = Object.assign(new EventEmitter(), { add: vi.fn() });
	const httpServer = new EventEmitter();
	const moduleGraph = graph();
	(plugin.configureServer as Function)({
		watcher,
		httpServer,
		config: { server: { hmr: false }, logger: { error: vi.fn() } },
		environments: { ssr: { moduleGraph } },
	});
	writeFileSync(file, "edited");
	utimesSync(file, 1_700_000_000, 1_700_000_000);
	watcher.emit("change", file);
	await Promise.resolve();
	expect(runGenTs).toHaveBeenCalledTimes(2);
	expect(moduleGraph.invalidateModule).toHaveBeenCalled();
	httpServer.emit("close");
	watcher.emit("change", file);
	expect(runGenTs).toHaveBeenCalledTimes(2);
});
