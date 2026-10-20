import { EventEmitter } from "node:events";
import {
	mkdirSync,
	mkdtempSync,
	rmSync,
	utimesSync,
	writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { normalizePath } from "vite";
import { afterEach, expect, test, vi } from "vitest";

const platform = vi.hoisted(() => ({
	paths: "native" as "native" | "posix" | "win32",
}));
vi.mock("node:path", async (importOriginal) => {
	const path = await importOriginal<typeof import("node:path")>();
	return {
		...path,
		resolve: (...paths: string[]) =>
			(platform.paths === "native" ? path : path[platform.paths]).resolve(
				...paths,
			),
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
vi.mock("../../vite-plugin/src/sidecar-lock.js", () => ({
	withSidecarLock: async (_root: string, run: Function) => run(),
}));
vi.mock(
	"../../vite-plugin/src/testbench-components.js",
	async (importOriginal) => {
		const original =
			await importOriginal<
				typeof import("../../vite-plugin/src/testbench-components.js")
			>();
		return {
			...original,
			loadTestbenchComponentModule: vi.fn(async (entry: string) => ({
				manifests: {},
				// Match the real loader's dependency-key contract on every OS.
				dependencies: new Set(
					[entry, join(dirname(entry), "helper.ts")].map(
						original.normalizeTestbenchComponentPath,
					),
				),
			})),
			writeTestbenchComponentSidecar: vi.fn(),
		};
	},
);

import { runGenTs } from "../../vite-plugin/src/generator.js";
import celox from "../../vite-plugin/src/index.js";
import {
	loadTestbenchComponentModule,
	normalizeTestbenchComponentPath,
} from "../../vite-plugin/src/testbench-components.js";

const roots: string[] = [];
afterEach(() => {
	platform.paths = "native";
	vi.mocked(runGenTs).mockClear();
	vi.mocked(loadTestbenchComponentModule).mockReset();
	vi.restoreAllMocks();
	for (const root of roots.splice(0))
		rmSync(root, { recursive: true, force: true });
});

async function initialize(root: string, testbenchComponents?: string) {
	const plugin = celox({ projectRoot: root, testbenchComponents });
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

test.each(["/", "C:/"])(
	"build-watch output filters recognize the filesystem root %s",
	(root) => {
		platform.paths = root === "C:/" ? "win32" : "posix";
		const plugin = celox({ projectRoot: root });
		const config = {
			root,
			command: "build",
			build: { outDir: "dist", watch: {} as Record<string, any> },
		};
		(plugin.configResolved as Function)(config);
		const excluded = [".celox", "target", "node_modules", ".git", "dist"].map(
			(name) => `${root}${name}/**`,
		);
		expect(config.build.watch.exclude).toEqual(excluded);
		expect(config.build.watch.chokidar.ignored).toEqual(excluded);
	},
);

test.each(["/", "C:/"])(
	"catalogue events recognize the filesystem root %s",
	async (root) => {
		platform.paths = root === "C:/" ? "win32" : "posix";
		const plugin = celox({ projectRoot: root });
		await (plugin.configResolved as Function)({ root });
		const watcher = Object.assign(new EventEmitter(), { add: vi.fn() });
		const changes = vi.fn();
		watcher.on("change", changes);
		(plugin.configureServer as Function)({
			watcher,
			httpServer: new EventEmitter(),
			config: { server: { hmr: true } },
		});
		for (const event of ["add", "unlink"])
			watcher.emit(event, `${root}rtl/New.veryl`);
		expect(changes).toHaveBeenCalledTimes(2);
		expect(changes).toHaveBeenCalledWith(`${root}Veryl.toml`);
	},
);

test("the watcher includes a custom project directory outside the Vite root", async () => {
	const root = mkdtempSync(join(tmpdir(), "celox-watch-external-"));
	roots.push(root);
	const plugin = celox({ projectRoot: root });
	await (plugin.configResolved as Function)({ root: join(root, "tests") });
	const watcher = Object.assign(new EventEmitter(), { add: vi.fn() });
	(plugin.configureServer as Function)({
		watcher,
		config: { server: { hmr: true } },
		environments: {},
	});
	expect(watcher.add).toHaveBeenCalledWith(
		expect.arrayContaining([normalizePath(root)]),
	);
});

test("Windows project imports resolve to a loadable dependency module", async () => {
	platform.paths = "win32";
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
	expect(code).not.toContain("Veryl.lock?celox-watch");
	const moduleGraph = graph();
	const result = await (plugin.hotUpdate as Function).call(
		{ environment: { moduleGraph } },
		{ file: "C:\\project\\celox.toml", timestamp: 1 },
	);
	expect(result).toEqual([{ id: projectId }]);
	expect(runGenTs).toHaveBeenCalledTimes(2);
});

test.each(["Main.veryl", "Veryl.toml", "celox.toml", "Veryl.lock"])(
	"watch edits of %s invalidate unchanged mtime/size fingerprints once across environments",
	async (name) => {
		const root = mkdtempSync(join(tmpdir(), "celox-watch-"));
		roots.push(root);
		const file = join(root, name);
		writeFileSync(file, "before");
		utimesSync(file, 1_700_000_000, 1_700_000_000);
		const { plugin } = await initialize(root);
		const read = vi.fn(() => "edited");
		writeFileSync(file, "edited");
		utimesSync(file, 1_700_000_000, 1_700_000_000);
		for (const moduleGraph of [graph(), graph()]) {
			await (plugin.hotUpdate as Function).call(
				{ environment: { moduleGraph } },
				{ file, timestamp: 1, type: "update", read },
			);
			expect(moduleGraph.invalidateModule).toHaveBeenCalled();
		}
		expect(runGenTs).toHaveBeenCalledTimes(2);
	},
);

test.each(["hotUpdate", "handleHotUpdate"] as const)(
	"%s preserves direct consumers while suppressing unchanged lockfile project updates",
	async (hook) => {
		const root = mkdtempSync(join(tmpdir(), "celox-watch-lock-"));
		roots.push(root);
		const file = join(root, "Veryl.lock");
		writeFileSync(file, "lock");
		const { plugin } = await initialize(root);
		const moduleGraph = graph();
		const result = await (plugin[hook] as Function).call(
			{ environment: { moduleGraph } },
			{
				file,
				timestamp: 1,
				modules: [{ id: `${file}?raw` }],
				server: { moduleGraph },
			},
		);
		expect(result).toEqual([{ id: `${file}?raw` }]);
		expect(moduleGraph.invalidateModule).not.toHaveBeenCalled();
		expect(runGenTs).toHaveBeenCalledTimes(1);
	},
);

test("HMR-disabled lockfile changes notify once and unchanged writes remain silent", async () => {
	const root = mkdtempSync(join(tmpdir(), "celox-watch-lock-notify-"));
	roots.push(root);
	const file = join(root, "Veryl.lock");
	writeFileSync(file, "before");
	const { plugin } = await initialize(root);
	const watcher = Object.assign(new EventEmitter(), { add: vi.fn() });
	const changes = vi.fn();
	watcher.on("change", changes);
	const moduleGraph = graph();
	const logger = { error: vi.fn() };
	const httpServer = new EventEmitter();
	(plugin.configureServer as Function)({
		watcher,
		httpServer,
		config: { server: { hmr: false }, logger },
		environments: { ssr: { moduleGraph } },
	});
	writeFileSync(file, "edited");
	watcher.emit("change", file);
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(changes).toHaveBeenCalledWith(normalizePath(join(root, "Veryl.toml")));
	expect(runGenTs).toHaveBeenCalledTimes(2);
	changes.mockClear();
	moduleGraph.invalidateModule.mockClear();
	watcher.emit("change", file);
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(changes).toHaveBeenCalledTimes(1); // Only the original raw event.
	expect(moduleGraph.invalidateModule).not.toHaveBeenCalled();
	expect(runGenTs).toHaveBeenCalledTimes(2);
	expect(logger.error).not.toHaveBeenCalled();
	httpServer.emit("close");
});

test("watch modules omit absent optional configs from import-analysis dependencies", async () => {
	const root = mkdtempSync(join(tmpdir(), "celox-watch-optional-"));
	roots.push(root);
	const { plugin, context } = await initialize(root);
	const file = join(root, "celox.toml");
	const id = (plugin.resolveId as Function)(`${file}?celox-watch`);
	expect((plugin.load as Function).call(context, id)).toBe("export {};");
	expect(context.addWatchFile).not.toHaveBeenCalledWith(file);
	writeFileSync(file, "[test]\nsources=[]\n");
	expect((plugin.load as Function).call(context, id)).toBe("export {};");
	expect(context.addWatchFile).toHaveBeenCalledWith(normalizePath(file));
});

test.each(
	(["hotUpdate", "handleHotUpdate"] as const).flatMap((hook) =>
		[false, true].map((loaded) => ({ hook, loaded })),
	),
)(
	"$hook suppresses the first generated lockfile notification with modules loaded=$loaded",
	async ({ hook, loaded }) => {
		const root = mkdtempSync(join(tmpdir(), "celox-watch-generated-lock-"));
		roots.push(root);
		const file = join(root, "Veryl.lock");
		vi.mocked(runGenTs).mockImplementationOnce((projectPath) => {
			writeFileSync(file, "generated lock");
			return { projectPath, modules: [], fileModules: {} };
		});
		writeFileSync(join(root, "Main.veryl"), "module Main {}\n");
		const { plugin, context } = await initialize(root);
		if (loaded)
			(plugin.load as Function).call(
				context,
				(plugin.resolveId as Function)(join(root, "Main.veryl")),
			);
		const moduleGraph = graph();
		const update = (timestamp: number) =>
			(plugin[hook] as Function).call(
				{ environment: { moduleGraph } },
				{ file, timestamp, modules: [], server: { moduleGraph } },
			);
		expect(await update(1)).toEqual([]);
		expect(moduleGraph.invalidateModule).not.toHaveBeenCalled();
		// An external lockfile edit still matters when port metadata is equal.
		writeFileSync(file, "external lock");
		await update(2);
		expect(moduleGraph.invalidateModule).toHaveBeenCalled();
		expect(runGenTs).toHaveBeenCalledTimes(3);
	},
);

test.each([undefined, "1.2.13", "1.3.0", "2.0.0"])(
	"build watch registers the project catalogue and lockfile with Rolldown %s",
	async (rolldownVersion) => {
		const root = mkdtempSync(join(tmpdir(), "celox-build-watch-"));
		roots.push(root);
		writeFileSync(join(root, "Veryl.lock"), "");
		const plugin = celox({ projectRoot: root });
		await (plugin.configResolved as Function)({
			root: join(root, "tests"),
			command: "build",
			build: { watch: {}, outDir: "dist" },
		});
		const context = {
			addWatchFile: vi.fn(),
			meta: { watchMode: true, rolldownVersion },
		};
		await (plugin.buildStart as Function).call(context);
		expect(context.addWatchFile).toHaveBeenCalledWith(normalizePath(root));
		expect(context.addWatchFile).toHaveBeenCalledWith(
			normalizePath(join(root, "Veryl.lock")),
		);
	},
);

test.each(["0.15.0", "1.1.99", "1.2.12", "1.2.13-rc.1"])(
	"build watch rejects Rolldown %s before registering any directories",
	async (rolldownVersion) => {
		const plugin = celox({ projectRoot: "/project" });
		(plugin.configResolved as Function)({
			root: "/project",
			command: "build",
			build: { watch: {}, outDir: "dist" },
		});
		const context = {
			addWatchFile: vi.fn(),
			meta: { watchMode: true, rolldownVersion },
		};
		await expect((plugin.buildStart as Function).call(context)).rejects.toThrow(
			"Celox build watch requires Rolldown >= 1.2.13",
		);
		expect(context.addWatchFile).not.toHaveBeenCalled();
	},
);

test.each([
	{ command: "serve", watchMode: true },
	{ command: "build", watchMode: false },
])("old Rolldown supports $command with watchMode=$watchMode", async (mode) => {
	const root = mkdtempSync(join(tmpdir(), "celox-no-build-watch-"));
	roots.push(root);
	const plugin = celox({ projectRoot: root });
	(plugin.configResolved as Function)({
		root,
		command: mode.command,
		build: { watch: false, outDir: "dist" },
	});
	const context = {
		addWatchFile: vi.fn(),
		meta: { watchMode: mode.watchMode, rolldownVersion: "1.2.12" },
	};
	await (plugin.buildStart as Function).call(context);
	expect(context.addWatchFile).not.toHaveBeenCalledWith(normalizePath(root));
});

test("HMR-disabled watcher suppresses generation-owned lockfile creation", async () => {
	const root = mkdtempSync(join(tmpdir(), "celox-watch-own-lock-"));
	roots.push(root);
	const file = join(root, "Veryl.lock");
	vi.mocked(runGenTs).mockImplementationOnce((projectPath) => {
		writeFileSync(file, "generated lock");
		return { projectPath, modules: [], fileModules: {} };
	});
	const { plugin } = await initialize(root);
	const watcher = Object.assign(new EventEmitter(), { add: vi.fn() });
	const moduleGraph = graph();
	(plugin.configureServer as Function)({
		watcher,
		config: { server: { hmr: false }, logger: { error: vi.fn() } },
		environments: { ssr: { moduleGraph } },
	});
	const projectChanged = vi.fn();
	watcher.on("change", (changed) => {
		if (changed === normalizePath(join(root, "Veryl.toml"))) projectChanged();
	});
	watcher.emit("add", file);
	await new Promise<void>((resolve) => setImmediate(resolve));
	watcher.emit("change", file);
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(projectChanged).not.toHaveBeenCalled();
	expect(moduleGraph.invalidateModule).not.toHaveBeenCalled();
	expect(runGenTs).toHaveBeenCalledTimes(2);
});

test("a generation-time lockfile write still publishes changed generated metadata", async () => {
	const root = mkdtempSync(join(tmpdir(), "celox-watch-lock-metadata-"));
	roots.push(root);
	const file = join(root, "Veryl.lock");
	vi.mocked(runGenTs).mockImplementationOnce((projectPath) => {
		writeFileSync(file, "generated lock");
		return { projectPath, modules: [], fileModules: {} };
	});
	const { plugin } = await initialize(root);
	vi.mocked(runGenTs).mockReturnValueOnce({
		projectPath: root,
		modules: [],
		fileModules: { "Empty.veryl": [] },
	});
	const moduleGraph = graph();
	await (plugin.hotUpdate as Function).call(
		{ environment: { moduleGraph } },
		{ file, timestamp: 1 },
	);
	expect(moduleGraph.invalidateModule).toHaveBeenCalled();
	expect(runGenTs).toHaveBeenCalledTimes(2);
});

test("HMR-disabled component updates notify the project after asynchronous regeneration", async () => {
	const root = mkdtempSync(join(tmpdir(), "celox-watch-async-components-"));
	roots.push(root);
	const entry = join(root, "components.ts");
	const helper = join(root, "helper.ts");
	writeFileSync(entry, "export {};");
	const { plugin } = await initialize(root, entry);
	let finish!: (
		value: Awaited<ReturnType<typeof loadTestbenchComponentModule>>,
	) => void;
	vi.mocked(loadTestbenchComponentModule).mockImplementationOnce(
		() =>
			new Promise((resolve) => {
				finish = resolve;
			}),
	);
	const watcher = Object.assign(new EventEmitter(), { add: vi.fn() });
	const moduleGraph = graph();
	(plugin.configureServer as Function)({
		watcher,
		config: { server: { hmr: false }, logger: { error: vi.fn() } },
		environments: { ssr: { moduleGraph } },
	});
	const projectChanged = vi.fn(() => {
		expect(runGenTs).toHaveBeenCalledTimes(2);
		expect(moduleGraph.invalidateModule).toHaveBeenCalled();
	});
	watcher.on("change", (file) => {
		if (file === normalizePath(join(root, "Veryl.toml"))) projectChanged();
	});
	watcher.emit("change", helper);
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(projectChanged).not.toHaveBeenCalled();
	finish({
		manifests: {},
		dependencies: new Set([entry, helper].map(normalizeTestbenchComponentPath)),
	});
	await vi.waitFor(() => expect(projectChanged).toHaveBeenCalledTimes(1));
	expect(runGenTs).toHaveBeenCalledTimes(2);
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
			watcher.emit(event, join(root, "Veryl.toml"));
		}
		const expected = hasEnvironments && hmr ? 0 : 6;
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
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(runGenTs).toHaveBeenCalledTimes(2);
	expect(moduleGraph.invalidateModule).toHaveBeenCalled();
	httpServer.emit("close");
	watcher.emit("change", file);
	expect(runGenTs).toHaveBeenCalledTimes(2);
});

test("raw watch events in the same millisecond each refresh generated data", async () => {
	const root = mkdtempSync(join(tmpdir(), "celox-watch-rapid-"));
	roots.push(root);
	const file = join(root, "Main.veryl");
	writeFileSync(file, "before");
	const { plugin } = await initialize(root);
	const watcher = Object.assign(new EventEmitter(), { add: vi.fn() });
	const httpServer = new EventEmitter();
	(plugin.configureServer as Function)({
		watcher,
		httpServer,
		config: { server: { hmr: false }, logger: { error: vi.fn() } },
		environments: { ssr: { moduleGraph: graph() } },
	});
	vi.spyOn(Date, "now").mockReturnValue(42);
	for (const value of ["first edit", "second edit"]) {
		writeFileSync(file, value);
		watcher.emit("change", file);
		await new Promise<void>((resolve) => setImmediate(resolve));
	}
	expect(runGenTs).toHaveBeenCalledTimes(3);
	httpServer.emit("close");
});

test.each(["hotUpdate", "handleHotUpdate"] as const)(
	"%s keeps changed component dependencies alongside HDL import variants",
	async (hook) => {
		const root = mkdtempSync(join(tmpdir(), "celox-watch-component-"));
		roots.push(root);
		const entry = join(root, "components.ts");
		const helper = join(root, "helper.ts");
		writeFileSync(entry, "export {};");
		writeFileSync(helper, "export const step = 1;");
		writeFileSync(join(root, "Main.veryl"), "module Main {}\n");
		const { plugin, context } = await initialize(root, entry);
		const plain = normalizePath(join(root, "Main.veryl"));
		const query = `${plain}?dse=preserveAllPorts`;
		for (const id of [plain, query])
			(plugin.load as Function).call(
				context,
				(plugin.resolveId as Function)(id),
			);
		const changed = { id: helper };
		const moduleGraph = graph();
		const result = await (plugin[hook] as Function).call(
			{ environment: { moduleGraph } },
			{
				file: helper,
				timestamp: 1,
				modules: [changed],
				server: { moduleGraph },
			},
		);
		expect(result).toContain(changed);
		expect(result.map((mod: { id: string }) => mod.id)).toEqual([
			helper,
			`${normalizePath(root)}/Veryl.toml?celox-project`,
			plain,
			query,
		]);
		expect(moduleGraph.invalidateModule).toHaveBeenCalledWith(changed);
		expect(loadTestbenchComponentModule).toHaveBeenCalledTimes(2);
	},
);

test.each([false, true])(
	"raw component regenerations serialize and recover after failure=%s",
	async (failFirst) => {
		const root = mkdtempSync(join(tmpdir(), "celox-watch-queued-"));
		roots.push(root);
		const entry = join(root, "components.ts");
		const helper = join(root, "helper.ts");
		writeFileSync(entry, "export {};");
		const { plugin } = await initialize(root, entry);
		const watcher = Object.assign(new EventEmitter(), { add: vi.fn() });
		const logger = { error: vi.fn() };
		(plugin.configureServer as Function)({
			watcher,
			config: { server: { hmr: false }, logger },
			environments: { ssr: { moduleGraph: graph() } },
		});
		let finish!: () => void;
		vi.mocked(loadTestbenchComponentModule).mockImplementationOnce(
			() =>
				new Promise((resolve, reject) => {
					finish = () =>
						failFirst
							? reject(new Error("incomplete first edit"))
							: resolve({
									manifests: {},
									dependencies: new Set([
										normalizeTestbenchComponentPath(entry),
									]),
								});
				}),
		);
		const latestDependencies = new Set(
			[entry, join(root, "latest.ts")].map(normalizeTestbenchComponentPath),
		);
		vi.mocked(loadTestbenchComponentModule).mockResolvedValueOnce({
			manifests: {},
			dependencies: latestDependencies,
		});
		const notifications = vi.fn();
		watcher.on("change", (file) => {
			if (file === normalizePath(join(root, "Veryl.toml"))) notifications();
		});
		watcher.emit("change", helper);
		watcher.emit("change", helper);
		expect(loadTestbenchComponentModule).toHaveBeenCalledTimes(2);
		finish();
		await vi.waitFor(() => {
			expect(loadTestbenchComponentModule).toHaveBeenCalledTimes(3);
			// Failed component loads also notify so the test runner reports them.
			expect(notifications).toHaveBeenCalledTimes(2);
		});
		expect(logger.error).toHaveBeenCalledTimes(failFirst ? 1 : 0);
		// The final dependency set, rather than the older load's set, is active.
		watcher.emit("change", join(root, "latest.ts"));
		await vi.waitFor(() =>
			expect(loadTestbenchComponentModule).toHaveBeenCalledTimes(4),
		);
	},
);

test.each(
	(["hotUpdate", "handleHotUpdate"] as const).flatMap((hook) =>
		["Veryl.toml", "celox.toml", "Veryl.lock"].map((name) => ({ hook, name })),
	),
)("$hook waits for stabilized $name contents", async ({ hook, name }) => {
	const root = mkdtempSync(join(tmpdir(), "celox-watch-read-"));
	roots.push(root);
	const file = join(root, name);
	writeFileSync(file, "before");
	const { plugin } = await initialize(root);
	writeFileSync(file, "");
	let finish!: () => void;
	const read = vi.fn(
		() =>
			new Promise<string>((resolve) => {
				finish = () => {
					writeFileSync(file, "complete edit");
					resolve("complete edit");
				};
			}),
	);
	const moduleGraph = graph();
	const result = (plugin[hook] as Function).call(
		{ environment: { moduleGraph } },
		{ file, type: "update", timestamp: 1, read, server: { moduleGraph } },
	);
	expect(read).toHaveBeenCalledTimes(1);
	expect(runGenTs).toHaveBeenCalledTimes(1);
	expect(moduleGraph.invalidateModule).not.toHaveBeenCalled();
	finish();
	await result;
	expect(runGenTs).toHaveBeenCalledTimes(2);
	expect(moduleGraph.invalidateModule).toHaveBeenCalled();
});

test.each(["create", "delete"])(
	"hotUpdate does not read %s events",
	async (type) => {
		const root = mkdtempSync(join(tmpdir(), "celox-watch-no-read-"));
		roots.push(root);
		const { plugin } = await initialize(root);
		const read = vi.fn(() => Promise.reject(new Error("file unavailable")));
		await (plugin.hotUpdate as Function).call(
			{ environment: { moduleGraph: graph() } },
			{ file: join(root, "celox.toml"), timestamp: 1, type, read },
		);
		expect(read).not.toHaveBeenCalled();
		expect(runGenTs).toHaveBeenCalledTimes(2);
	},
);

// Distinct Vite events can share Date.now(), but share read only across environments.
test.each(["update", "create", "delete"])(
	"same-timestamp %s events regenerate separately across interleaved environments",
	async (type) => {
		const root = mkdtempSync(join(tmpdir(), "celox-watch-timestamp-"));
		roots.push(root);
		const { plugin } = await initialize(root);
		const file = join(root, "Main.veryl");
		const events = [vi.fn(() => "first"), vi.fn(() => "second")];
		const invoke = (read: () => string) =>
			(plugin.hotUpdate as Function).call(
				{ environment: { moduleGraph: graph() } },
				{ file, type, timestamp: 42, read },
			);
		await invoke(events[0]);
		expect(runGenTs).toHaveBeenCalledTimes(2);
		await invoke(events[1]);
		expect(runGenTs).toHaveBeenCalledTimes(3);
		// A later environment still shares the original event's result.
		await invoke(events[0]);
		await invoke(events[1]);
		expect(runGenTs).toHaveBeenCalledTimes(3);
		for (const read of events)
			expect(read).toHaveBeenCalledTimes(type === "update" ? 1 : 0);
	},
);

test.each([
	{ hasEnvironments: false, hmr: true },
	{ hasEnvironments: false, hmr: false },
	{ hasEnvironments: true, hmr: false },
])(
	"component removal and recreation recover with environments=$hasEnvironments, hmr=$hmr",
	async ({ hasEnvironments, hmr }) => {
		const root = mkdtempSync(
			join(tmpdir(), "celox-watch-component-catalogue-"),
		);
		roots.push(root);
		const entry = join(root, "components.ts");
		const helper = join(root, "helper.ts");
		writeFileSync(entry, "export {};");
		const { plugin } = await initialize(root, entry);
		const watcher = Object.assign(new EventEmitter(), { add: vi.fn() });
		const moduleGraph = graph();
		const logger = { error: vi.fn() };
		const httpServer = new EventEmitter();
		(plugin.configureServer as Function)({
			watcher,
			httpServer,
			moduleGraph,
			config: { server: { hmr }, logger },
			environments: hasEnvironments ? { ssr: { moduleGraph } } : undefined,
		});
		const notifications = vi.fn();
		watcher.on("change", (file) => {
			if (file === normalizePath(join(root, "Veryl.toml"))) notifications();
		});
		for (const file of [helper, entry]) {
			vi.mocked(loadTestbenchComponentModule).mockRejectedValueOnce(
				new Error("removed dependency"),
			);
			watcher.emit("unlink", file);
			await vi.waitFor(() => expect(logger.error).toHaveBeenCalledTimes(1));
			expect(notifications).toHaveBeenCalledTimes(1);
			watcher.emit("add", file);
			await vi.waitFor(() => expect(notifications).toHaveBeenCalledTimes(2));
			expect(moduleGraph.invalidateModule).toHaveBeenCalled();
			logger.error.mockClear();
			notifications.mockClear();
		}
		expect(loadTestbenchComponentModule).toHaveBeenCalledTimes(5);
		expect(runGenTs).toHaveBeenCalledTimes(3);
		httpServer.emit("close");
		watcher.emit("add", helper);
		expect(loadTestbenchComponentModule).toHaveBeenCalledTimes(5);
	},
);

test.each(["hotUpdate", "handleHotUpdate"] as const)(
	"%s preserves direct consumers of a generation-owned lockfile write",
	async (hook) => {
		const root = mkdtempSync(join(tmpdir(), "celox-watch-direct-lock-"));
		roots.push(root);
		const file = join(root, "Veryl.lock");
		vi.mocked(runGenTs).mockImplementationOnce((projectPath) => {
			writeFileSync(file, "generated lock");
			return { projectPath, modules: [], fileModules: {} };
		});
		writeFileSync(join(root, "Main.veryl"), "module Main {}\n");
		const { plugin, context } = await initialize(root);
		const source = normalizePath(join(root, "Main.veryl"));
		(plugin.load as Function).call(
			context,
			(plugin.resolveId as Function)(source),
		);
		const direct = { id: `${normalizePath(file)}?raw` };
		const moduleGraph = graph();
		const modules = [
			direct,
			{ id: source },
			{ id: `${normalizePath(root)}/Veryl.toml?celox-project` },
			{ id: `${normalizePath(file)}?celox-watch` },
		];
		const result = await (plugin[hook] as Function).call(
			{ environment: { moduleGraph } },
			{ file, modules, server: { moduleGraph } },
		);
		expect(result).toEqual([direct]);
		expect(moduleGraph.invalidateModule).not.toHaveBeenCalled();
	},
);

test.each([
	"hotUpdate",
	"handleHotUpdate",
	"watcher",
	"add",
	"unlink",
	"add-after-failure",
	"unlink-after-failure",
])(
	"%s rechecks dependencies discovered by a preceding queued load",
	async (hook) => {
		const failFirst = hook.endsWith("-after-failure");
		hook = hook.replace("-after-failure", "");
		const root = mkdtempSync(join(tmpdir(), "celox-watch-discovered-"));
		roots.push(root);
		const entry = join(root, "components.ts");
		const added = join(root, "added.ts");
		writeFileSync(entry, "export {};");
		const { plugin } = await initialize(root, entry);
		const watcher = Object.assign(new EventEmitter(), { add: vi.fn() });
		const moduleGraph = graph();
		const logger = { error: vi.fn() };
		(plugin.configureServer as Function)({
			watcher,
			moduleGraph,
			config: { server: { hmr: false }, logger },
		});
		const notifications = vi.fn();
		watcher.on("change", (file) => {
			if (file === normalizePath(join(root, "Veryl.toml"))) notifications();
		});
		let finish!: () => void;
		vi.mocked(loadTestbenchComponentModule).mockImplementationOnce(
			() =>
				new Promise((resolve, reject) => {
					finish = () =>
						failFirst
							? reject(new Error("dependency not yet created"))
							: resolve({
									manifests: {},
									dependencies: new Set(
										[entry, added].map(normalizeTestbenchComponentPath),
									),
								});
				}),
		);
		const invoke = (file: string) =>
			["watcher", "add", "unlink"].includes(hook)
				? watcher.emit(
						file === entry || hook === "watcher" ? "change" : hook,
						file,
					)
				: (plugin[hook as "hotUpdate" | "handleHotUpdate"] as Function).call(
						{ environment: { moduleGraph } },
						{
							file,
							type: "update",
							read: () => "edited",
							modules: [],
							server: { moduleGraph },
						},
					);
		const first = invoke(entry);
		await vi.waitFor(() => expect(finish).toBeTypeOf("function"));
		const second = invoke(added);
		finish();
		await Promise.all([first, second]);
		await vi.waitFor(() =>
			expect(loadTestbenchComponentModule).toHaveBeenCalledTimes(3),
		);
		expect(runGenTs).toHaveBeenCalledTimes(failFirst ? 2 : 3);
		// The mock's final load drops added.ts again: notification must use
		// membership at execution time, not the dependency set after that load.
		if (["watcher", "add", "unlink"].includes(hook))
			expect(notifications).toHaveBeenCalledTimes(2);
		expect(logger.error).toHaveBeenCalledTimes(failFirst ? 1 : 0);
	},
);

test.each([undefined, "output", "../artifacts"])(
	"build watch excludes output %s relative to the Vite root",
	(outDir) => {
		const root = mkdtempSync(join(tmpdir(), "celox-watch-output-"));
		roots.push(root);
		const viteRoot = join(root, "client");
		const plugin = celox({ projectRoot: root });
		const config = {
			root: viteRoot,
			command: "build",
			build: { outDir: outDir ?? "dist", watch: {} as Record<string, any> },
		};
		(plugin.configResolved as Function)(config);
		const expected = `${normalizePath(join(viteRoot, outDir ?? "dist"))}/**`;
		expect(config.build.watch.exclude).toContain(expected);
		expect(config.build.watch.chokidar.ignored).toContain(expected);
	},
);

test.each(
	["helper.ts", "Veryl.lock"].flatMap((name) =>
		[false, true].map((interveningEdit) => ({ name, interveningEdit })),
	),
)(
	"legacy asynchronous HMR consumes $name notification with interveningEdit=$interveningEdit",
	async ({ name, interveningEdit }) => {
		const root = mkdtempSync(join(tmpdir(), "celox-watch-notification-"));
		roots.push(root);
		const entry = join(root, "components.ts");
		const project = join(root, "Veryl.toml");
		writeFileSync(entry, "export {};");
		writeFileSync(project, "original config");
		const { plugin } = await initialize(root, entry);
		const watcher = Object.assign(new EventEmitter(), { add: vi.fn() });
		const moduleGraph = graph();
		const logger = { error: vi.fn() };
		const server = {
			watcher,
			moduleGraph,
			config: { server: { hmr: true }, logger },
		};
		(plugin.configureServer as Function)(server);
		const events: Promise<unknown>[] = [];
		watcher.on("change", (file) => {
			// Vite awaits its watch hooks before calling handleHotUpdate.
			events.push(
				Promise.resolve().then(() => {
					if (interveningEdit) writeFileSync(project, "intervening edit");
					return (plugin.handleHotUpdate as Function)({
						file,
						server,
						modules: [],
					});
				}),
			);
		});
		writeFileSync(join(root, name), "changed");
		watcher.emit("add", join(root, name));
		await vi.waitFor(() => expect(events).toHaveLength(1));
		const result = await events[0];
		expect(result).toEqual([
			{ id: `${normalizePath(root)}/Veryl.toml?celox-project` },
		]);
		expect(runGenTs).toHaveBeenCalledTimes(interveningEdit ? 3 : 2);
		writeFileSync(project, "real config edit");
		watcher.emit("change", project);
		await events[1];
		expect(runGenTs).toHaveBeenCalledTimes(interveningEdit ? 4 : 3);
		expect(logger.error).not.toHaveBeenCalled();
	},
);

test("resolved configuration supplies roots and preserves user watch filters", () => {
	const root = mkdtempSync(join(tmpdir(), "celox-final-root-"));
	roots.push(root);
	writeFileSync(join(root, "Veryl.toml"), "[project]");
	const plugin = celox();
	expect(plugin.config).toBeUndefined();
	const ignored = () => false;
	const config = {
		root,
		command: "build",
		build: {
			outDir: "dist",
			watch: {
				exclude: /existing/,
				chokidar: { ignored, usePolling: true },
			} as Record<string, any>,
		},
	};
	(plugin.configResolved as Function)(config);
	expect(config.build.watch.exclude).toContainEqual(/existing/);
	expect(config.build.watch.exclude).toContain(
		`${normalizePath(root)}/dist/**`,
	);
	expect(config.build.watch.chokidar.ignored).toContain(ignored);
	expect(config.build.watch.chokidar.usePolling).toBe(true);
});

test("load leaves another plugin's Veryl-like virtual and unresolved IDs alone", async () => {
	const root = mkdtempSync(join(tmpdir(), "celox-module-owner-"));
	roots.push(root);
	writeFileSync(join(root, "Main.veryl"), "module Main {}\n");
	const { plugin, context } = await initialize(root);
	for (const id of [
		"\0virtual:fixture.veryl",
		join(root, "Main.veryl"),
		"\0virtual:fixture.veryl?celox-watch",
	]) {
		expect((plugin.load as Function).call(context, id)).toBeUndefined();
	}
	expect(context.addWatchFile).not.toHaveBeenCalled();
	expect(
		(plugin.resolveId as Function)("\0virtual:fixture.veryl"),
	).toBeUndefined();
	const id = (plugin.resolveId as Function)(join(root, "Main.veryl"));
	expect((plugin.load as Function).call(context, id)).toBe("export {};");
});

test.each([true, false])(
	"register refreshed external component dependencies with HMR=%s",
	async (hmr) => {
		const root = mkdtempSync(join(tmpdir(), "celox-component-project-"));
		const external = mkdtempSync(join(tmpdir(), "celox-external-components-"));
		roots.push(root, external);
		const entry = join(external, "components.ts");
		const added = join(external, "new-helper.ts");
		writeFileSync(entry, "export {};");
		const { plugin } = await initialize(root, entry);
		const watcher = Object.assign(new EventEmitter(), { add: vi.fn() });
		const httpServer = new EventEmitter();
		const moduleGraph = graph();
		(plugin.configureServer as Function)({
			watcher,
			httpServer,
			config: { server: { hmr } },
			environments: { ssr: { moduleGraph } },
		});
		const dependencies = new Set(
			[entry, added].map(normalizeTestbenchComponentPath),
		);
		vi.mocked(loadTestbenchComponentModule).mockResolvedValueOnce({
			manifests: {},
			dependencies,
		});
		await (plugin.hotUpdate as Function).call(
			{ environment: { moduleGraph } },
			{ file: entry, type: "update", read: () => "edited" },
		);
		expect(watcher.add).toHaveBeenCalledWith([...dependencies]);
		httpServer.emit("close");
		watcher.add.mockClear();
		await (plugin.hotUpdate as Function).call(
			{ environment: { moduleGraph } },
			{ file: added, type: "update", read: () => "edited" },
		);
		expect(watcher.add).not.toHaveBeenCalled();
	},
);

// Vitest's persistent module cache replays imports as URLs rooted at Vite's
// root, rather than their original paths relative to the importer.
test("cached root-relative imports retain their generated module identity", async () => {
	const root = mkdtempSync(join(tmpdir(), "celox-cached-import-"));
	roots.push(root);
	mkdirSync(join(root, "src"));
	writeFileSync(join(root, "src", "Main.veryl"), "module Main {}\n");
	writeFileSync(join(root, "Veryl.toml"), "[project]\nname = 'test'\n");
	const { plugin, context } = await initialize(root);
	for (const [url, file] of [
		["/src/Main.veryl", join(root, "src", "Main.veryl")],
		[
			"/src/Main.veryl?dse=preserveAllPorts",
			`${join(root, "src", "Main.veryl")}?dse=preserveAllPorts`,
		],
		["/Veryl.toml?celox-project", `${join(root, "Veryl.toml")}?celox-project`],
		["/celox.toml?celox-watch", `${join(root, "celox.toml")}?celox-watch`],
		[
			"/src/Main.veryl?celox-watch",
			`${join(root, "src", "Main.veryl")}?celox-watch`,
		],
	]) {
		const id = (plugin.resolveId as Function)(url, join(root, "index.html"));
		expect(id).toBe(normalizePath(file!));
		expect((plugin.load as Function).call(context, id)).toBeTypeOf("string");
	}
	expect((plugin.resolveId as Function)("/missing.veryl")).toBeUndefined();
	// Optional configuration is absent, but its canonical watch identity must
	// survive both the initial absolute import and the cached root-relative URL.
	const optional = `${normalizePath(join(root, "celox.toml"))}?celox-watch`;
	expect((plugin.resolveId as Function)(optional)).toBe(optional);
	expect((plugin.load as Function).call(context, optional)).toBe("export {};");
	expect(context.addWatchFile).not.toHaveBeenCalledWith(
		normalizePath(join(root, "celox.toml")),
	);
});

// A project outside Vite's root is replayed through the /@fs/ URL namespace.
test("cached external project imports retain their generated module identity", async () => {
	const root = mkdtempSync(join(tmpdir(), "celox-external-cache-"));
	const client = mkdtempSync(join(tmpdir(), "celox-external-client-"));
	roots.push(root, client);
	writeFileSync(join(root, "Main.veryl"), "module Main {}\n");
	writeFileSync(join(root, "Veryl.toml"), "[project]\nname = 'test'\n");
	const plugin = celox({ projectRoot: root });
	const context = { addWatchFile: vi.fn(), warn: vi.fn() };
	await (plugin.configResolved as Function)({ root: client });
	await (plugin.buildStart as Function).call(context);
	for (const query of ["", "?dse=preserveAllPorts", "?celox-watch"]) {
		const file = normalizePath(join(root, "Main.veryl")) + query;
		const id = (plugin.resolveId as Function)(
			`/@fs/${file.replace(/^\//, "")}`,
			join(client, "index.html"),
		);
		expect(id).toBe(file);
		expect((plugin.load as Function).call(context, id)).toBeTypeOf("string");
	}
	const project = `${normalizePath(join(root, "Veryl.toml"))}?celox-project`;
	const id = (plugin.resolveId as Function)(
		`/@fs/${project.replace(/^\//, "")}`,
	);
	expect(id).toBe(project);
	expect((plugin.load as Function).call(context, id)).toBeTypeOf("string");
});

// Both the host-absolute and Vite-root-relative paths exist. Their existence
// alone must not let an unrelated host file take over a cached project URL.
test("cached URLs prefer the project over colliding host paths", async () => {
	const client = mkdtempSync(join(tmpdir(), "celox-collision-client-"));
	const host = mkdtempSync(join(tmpdir(), "celox-collision-host-"));
	roots.push(client, host);
	const urlRoot = normalizePath(host).replace(/^[A-Z]:/i, "");
	const project = join(client, urlRoot.replace(/^\//, ""));
	mkdirSync(project, { recursive: true });
	for (const dir of [host, project]) {
		writeFileSync(join(dir, "Main.veryl"), "module Main {}\n");
		writeFileSync(join(dir, "Veryl.toml"), "[project]\nname = 'test'\n");
	}
	const plugin = celox({ projectRoot: project });
	const context = { addWatchFile: vi.fn(), warn: vi.fn() };
	await (plugin.configResolved as Function)({ root: client });
	await (plugin.buildStart as Function).call(context);
	for (const [name, query] of [
		["Main.veryl", ""],
		["Main.veryl", "?dse=preserveAllPorts"],
		["Main.veryl", "?celox-watch"],
		["Veryl.toml", "?celox-project"],
	]) {
		const file = normalizePath(join(project, name!)) + query;
		const url = `${urlRoot}/${name}${query}`;
		const id = (plugin.resolveId as Function)(url, join(client, "index.html"));
		expect(id).toBe(file);
		expect((plugin.load as Function).call(context, id)).toBeTypeOf("string");
	}

	// Canonical absolute project IDs must stay stable even if their root-relative
	// lookalikes also exist. Explicit /@fs/ URLs retain their absolute meaning.
	const mirror = join(
		client,
		normalizePath(project)
			.replace(/^[A-Z]:/i, "")
			.replace(/^\//, ""),
	);
	mkdirSync(mirror, { recursive: true });
	writeFileSync(join(mirror, "Main.veryl"), "module Main {}\n");
	writeFileSync(join(mirror, "Veryl.toml"), "[project]\nname = 'other'\n");
	for (const name of ["Main.veryl", "Veryl.toml?celox-project"]) {
		const file = normalizePath(join(project, name));
		expect((plugin.resolveId as Function)(file)).toBe(file);
	}
	const hostFile = normalizePath(join(host, "Main.veryl"));
	expect(
		(plugin.resolveId as Function)(`/@fs/${hostFile.replace(/^\//, "")}`),
	).toBe(hostFile);
});

test.each([false, true])(
	"cached prefix-shaped URLs fall back to the Vite root (filesystem root=%s)",
	async (filesystemRoot) => {
		const project = mkdtempSync(join(tmpdir(), "celox-prefix-project-"));
		roots.push(project);
		const client = join(project, "client");
		const url = filesystemRoot
			? `/celox-prefix-${project.split(/[\\/]/).at(-1)}/Main.veryl`
			: normalizePath(join(project, "Main.veryl")).replace(/^[A-Z]:/i, "");
		const rooted = join(client, url.replace(/^[A-Z]:/i, "").replace(/^\//, ""));
		mkdirSync(dirname(rooted), { recursive: true });
		writeFileSync(rooted, "module Main {}\n");
		const plugin = celox({ projectRoot: filesystemRoot ? "/" : project });
		// Resolution does not need buildStart; do not generate sidecars at '/'.
		await (plugin.configResolved as Function)({ root: client });
		for (const query of ["", "?dse=preserveAllPorts", "?celox-watch"]) {
			expect((plugin.resolveId as Function)(url + query)).toBe(
				normalizePath(rooted) + query,
			);
		}
		expect(
			(plugin.resolveId as Function)(`/@fs/${url.replace(/^\//, "")}`),
		).toBeUndefined();
		if (!filesystemRoot) {
			// Once the canonical project file exists, keep that identity even though
			// the cached-root lookalike exists too.
			writeFileSync(join(project, "Main.veryl"), "module Main {}\n");
			const canonical = normalizePath(join(project, "Main.veryl"));
			expect((plugin.resolveId as Function)(canonical)).toBe(canonical);
		}
	},
);
