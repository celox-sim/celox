import { EventEmitter } from "node:events";
import { mkdtempSync, rmSync, utimesSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
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
vi.mock("../../vite-plugin/src/sidecar-lock.js", () => ({
	withSidecarLock: async (_root: string, run: Function) => run(),
}));
vi.mock(
	"../../vite-plugin/src/testbench-components.js",
	async (importOriginal) => ({
		...(await importOriginal<
			typeof import("../../vite-plugin/src/testbench-components.js")
		>()),
		loadTestbenchComponentModule: vi.fn(async (entry: string) => ({
			manifests: {},
			dependencies: new Set([entry, join(dirname(entry), "helper.ts")]),
		})),
		writeTestbenchComponentSidecar: vi.fn(),
	}),
);

import { runGenTs } from "../../vite-plugin/src/generator.js";
import celox from "../../vite-plugin/src/index.js";
import { loadTestbenchComponentModule } from "../../vite-plugin/src/testbench-components.js";

const roots: string[] = [];
afterEach(() => {
	platform.windows = false;
	vi.mocked(runGenTs).mockClear();
	vi.mocked(loadTestbenchComponentModule).mockClear();
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
		platform.windows = root === "C:/";
		const plugin = celox({ projectRoot: root });
		const config = (plugin.config as Function)(
			{ root, build: { watch: {} } },
			{ command: "build" },
		);
		const excluded = [`${root}.celox/**`, `${root}target/**`];
		expect(config.build.watch.exclude).toEqual(excluded);
		expect(config.build.watch.chokidar.ignored).toEqual(excluded);
	},
);

test.each(["/", "C:/"])(
	"catalogue events recognize the filesystem root %s",
	async (root) => {
		platform.windows = root === "C:/";
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
	expect(watcher.add).toHaveBeenCalledWith(expect.arrayContaining([root]));
});

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

test.each(["hotUpdate", "handleHotUpdate"] as const)(
	"%s suppresses unchanged lockfile notifications",
	async (hook) => {
		const root = mkdtempSync(join(tmpdir(), "celox-watch-lock-"));
		roots.push(root);
		const file = join(root, "Veryl.lock");
		writeFileSync(file, "lock");
		const { plugin } = await initialize(root);
		const moduleGraph = graph();
		const result = await (plugin[hook] as Function).call(
			{ environment: { moduleGraph } },
			{ file, timestamp: 1, modules: [{ id: file }], server: { moduleGraph } },
		);
		expect(result).toEqual([]);
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
	expect(changes).toHaveBeenCalledWith(join(root, "Veryl.toml"));
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
	const id = `${file}?celox-watch`;
	expect((plugin.load as Function).call(context, id)).toBe("export {};");
	expect(context.addWatchFile).not.toHaveBeenCalledWith(file);
	writeFileSync(file, "[test]\nsources=[]\n");
	expect((plugin.load as Function).call(context, id)).toBe("export {};");
	expect(context.addWatchFile).toHaveBeenCalledWith(file);
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
		const { plugin, context } = await initialize(root);
		if (loaded)
			(plugin.load as Function).call(context, join(root, "Main.veryl"));
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

test("build watch registers the project catalogue and lockfile outside the Vite root", async () => {
	const root = mkdtempSync(join(tmpdir(), "celox-build-watch-"));
	roots.push(root);
	const plugin = celox({ projectRoot: root });
	await (plugin.configResolved as Function)({
		root: join(root, "tests"),
		command: "build",
	});
	const context = { addWatchFile: vi.fn(), meta: { watchMode: true } };
	await (plugin.buildStart as Function).call(context);
	for (const path of [root, join(root, "Veryl.lock")])
		expect(context.addWatchFile).toHaveBeenCalledWith(path);
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
		if (changed === join(root, "Veryl.toml")) projectChanged();
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
		if (file === join(root, "Veryl.toml")) projectChanged();
	});
	watcher.emit("change", helper);
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(projectChanged).not.toHaveBeenCalled();
	finish({ manifests: {}, dependencies: new Set([entry, helper]) });
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
		const { plugin, context } = await initialize(root, entry);
		const plain = join(root, "Main.veryl");
		const query = `${plain}?dse=preserveAllPorts`;
		for (const id of [plain, query])
			(plugin.load as Function).call(context, id);
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
			`${root}/Veryl.toml?celox-project`,
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
							: resolve({ manifests: {}, dependencies: new Set([entry]) });
				}),
		);
		const latestDependencies = new Set([entry, join(root, "latest.ts")]);
		vi.mocked(loadTestbenchComponentModule).mockResolvedValueOnce({
			manifests: {},
			dependencies: latestDependencies,
		});
		const notifications = vi.fn();
		watcher.on("change", (file) => {
			if (file === join(root, "Veryl.toml")) notifications();
		});
		watcher.emit("change", helper);
		watcher.emit("change", helper);
		expect(loadTestbenchComponentModule).toHaveBeenCalledTimes(2);
		finish();
		await vi.waitFor(() => {
			expect(loadTestbenchComponentModule).toHaveBeenCalledTimes(3);
			expect(notifications).toHaveBeenCalledTimes(failFirst ? 1 : 2);
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

test("build watch ignores generated-only notifications and preserves real edits", async () => {
	const root = mkdtempSync(join(tmpdir(), "celox-build-output-"));
	roots.push(root);
	const entry = join(root, "components.ts");
	const helper = join(root, "helper.ts");
	writeFileSync(entry, "export {};");
	const plugin = celox({ projectRoot: root, testbenchComponents: entry });
	await (plugin.configResolved as Function)({ root, command: "build" });
	const context = { addWatchFile: vi.fn(), meta: { watchMode: true } };
	const build = () => (plugin.buildStart as Function).call(context);
	const changed = (name: string) =>
		(plugin.watchChange as Function | undefined)?.(join(root, name));
	await build();
	const calls = vi.mocked(runGenTs).mock.calls.length;
	for (const name of [
		".celox/src/Main.d.veryl.ts",
		".celox/.generation-locks/claim",
	]) {
		context.addWatchFile.mockClear();
		changed(name);
		await build();
		expect(runGenTs).toHaveBeenCalledTimes(calls);
		for (const dependency of [root, join(root, "Veryl.lock"), entry, helper])
			expect(context.addWatchFile).toHaveBeenCalledWith(dependency);
	}
	// A real edit arriving alongside a delayed output event must still run.
	for (const name of ["src/Main.veryl", "celox.toml", "helper.ts"]) {
		const before = vi.mocked(runGenTs).mock.calls.length;
		changed(".celox/.generation-locks/claim");
		changed(name);
		await build();
		expect(runGenTs).toHaveBeenCalledTimes(before + 1);
	}
	// A build without watch notifications (including a retry) still generates.
	const before = vi.mocked(runGenTs).mock.calls.length;
	await build();
	expect(runGenTs).toHaveBeenCalledTimes(before + 1);
});
