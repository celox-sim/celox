import {
	mkdtempSync,
	rmSync,
	unlinkSync,
	utimesSync,
	writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, test, vi } from "vitest";

vi.mock("../../vite-plugin/src/generator.js", () => ({
	runGenTs: vi.fn((projectRoot: string) => ({
		projectPath: projectRoot,
		modules: [],
		fileModules: {},
	})),
}));

import { GenTsCache } from "../../vite-plugin/src/cache.js";
import { runGenTs } from "../../vite-plugin/src/generator.js";

describe("GenTsCache", () => {
	const temporaryRoots: string[] = [];

	afterEach(() => {
		vi.mocked(runGenTs).mockClear();
		for (const root of temporaryRoots.splice(0)) {
			rmSync(root, { recursive: true, force: true });
		}
	});

	test("invalidates when an older source changes below the newest mtime", () => {
		const root = mkdtempSync(join(tmpdir(), "celox-vite-cache-"));
		temporaryRoots.push(root);
		const older = join(root, "older.veryl");
		const newest = join(root, "newest.veryl");
		writeFileSync(older, "module Older {}\n");
		writeFileSync(newest, "module Newest {}\n");
		utimesSync(older, 1_700_000_000, 1_700_000_000);
		utimesSync(newest, 1_700_000_100, 1_700_000_100);

		const cache = new GenTsCache(root);
		cache.get();
		cache.get();
		expect(runGenTs).toHaveBeenCalledTimes(1);

		writeFileSync(older, "module Other {}\n");
		utimesSync(older, 1_700_000_050, 1_700_000_050);

		cache.get();
		expect(runGenTs).toHaveBeenCalledTimes(2);
	});
	test.each(["Veryl.toml", "celox.toml", "Veryl.lock"])(
		"invalidates on creation, edit and deletion of %s",
		(name) => {
			const root = mkdtempSync(join(tmpdir(), "celox-vite-config-"));
			temporaryRoots.push(root);
			const config = join(root, name);
			const cache = new GenTsCache(root);
			cache.get();
			cache.get();
			expect(runGenTs).toHaveBeenCalledTimes(1);
			writeFileSync(config, "first");
			cache.get();
			expect(runGenTs).toHaveBeenCalledTimes(2);
			writeFileSync(config, "second config");
			cache.get();
			expect(runGenTs).toHaveBeenCalledTimes(3);
			unlinkSync(config);
			cache.get();
			expect(runGenTs).toHaveBeenCalledTimes(4);
		},
	);

	test("refreshes once after the generator writes the lockfile", () => {
		const root = mkdtempSync(join(tmpdir(), "celox-vite-lock-"));
		temporaryRoots.push(root);
		vi.mocked(runGenTs).mockImplementationOnce((projectRoot) => {
			writeFileSync(join(projectRoot, "Veryl.lock"), "generated lock");
			return { projectPath: projectRoot, modules: [], fileModules: {} };
		});
		const cache = new GenTsCache(root);
		const initial = cache.get();
		const updated = cache.get();
		expect(updated).not.toBe(initial);
		expect(runGenTs).toHaveBeenCalledTimes(2);
		expect(cache.get()).toBe(updated);
		expect(runGenTs).toHaveBeenCalledTimes(2);
	});

	test.each(
		["Veryl.toml", "celox.toml", "Veryl.lock"].flatMap((name) =>
			["between lookups", "during generation"].map((phase) => ({
				name,
				phase,
			})),
		),
	)(
		"detects $name edits with unchanged mtime/size $phase",
		({ name, phase }) => {
			const root = mkdtempSync(join(tmpdir(), "celox-vite-lock-content-"));
			temporaryRoots.push(root);
			const file = join(root, name);
			writeFileSync(file, "before");
			utimesSync(file, 1_700_000_000, 1_700_000_000);
			const edit = () => {
				writeFileSync(file, "edited");
				utimesSync(file, 1_700_000_000, 1_700_000_000);
			};
			if (phase === "during generation") {
				vi.mocked(runGenTs).mockImplementationOnce((projectPath) => {
					edit();
					return { projectPath, modules: [], fileModules: {} };
				});
			}
			const cache = new GenTsCache(root);
			const initial = cache.get();
			if (phase === "between lookups") edit();
			const updated = cache.get();
			expect(updated).not.toBe(initial);
			expect(cache.get()).toBe(updated);
			expect(runGenTs).toHaveBeenCalledTimes(2);
		},
	);

	test.each(["Veryl.toml", "celox.toml", "Veryl.lock"])(
		"reuses data after a %s rewrite with unchanged contents",
		(name) => {
			const root = mkdtempSync(join(tmpdir(), "celox-vite-lock-rewrite-"));
			temporaryRoots.push(root);
			const file = join(root, name);
			writeFileSync(file, "resolution");
			utimesSync(file, 1_700_000_000, 1_700_000_000);
			const cache = new GenTsCache(root);
			const initial = cache.get();
			writeFileSync(file, "resolution");
			utimesSync(file, 1_700_000_100, 1_700_000_100);
			expect(cache.get()).toBe(initial);
			expect(runGenTs).toHaveBeenCalledTimes(1);
		},
	);

	test.each(["creation", "edit", "deletion"])(
		"refreshes again after external lockfile %s during generation",
		(change) => {
			const root = mkdtempSync(join(tmpdir(), "celox-vite-lock-race-"));
			temporaryRoots.push(root);
			const lockfile = join(root, "Veryl.lock");
			if (change !== "creation") writeFileSync(lockfile, "initial lock");
			vi.mocked(runGenTs).mockImplementationOnce((projectRoot) => {
				// Generation has already read the old dependency resolution when
				// another process changes the lockfile before generation returns.
				if (change === "deletion") unlinkSync(lockfile);
				else writeFileSync(lockfile, "externally updated lockfile");
				return { projectPath: projectRoot, modules: [], fileModules: {} };
			});

			const cache = new GenTsCache(root);
			const initial = cache.get();
			const updated = cache.get();
			expect(updated).not.toBe(initial);
			expect(runGenTs).toHaveBeenCalledTimes(2);
			expect(cache.get()).toBe(updated);
			expect(runGenTs).toHaveBeenCalledTimes(2);
		},
	);

	test.each(["Veryl.toml", "celox.toml"])(
		"refreshes again when %s changes during generation",
		(name) => {
			const root = mkdtempSync(join(tmpdir(), "celox-vite-config-race-"));
			temporaryRoots.push(root);
			const config = join(root, name);
			writeFileSync(config, "initial config");
			vi.mocked(runGenTs).mockImplementationOnce((projectRoot) => {
				writeFileSync(config, "updated configuration");
				writeFileSync(join(projectRoot, "Veryl.lock"), "generated lock");
				return { projectPath: projectRoot, modules: [], fileModules: {} };
			});

			const cache = new GenTsCache(root);
			const initial = cache.get();
			const updated = cache.get();
			expect(updated).not.toBe(initial);
			expect(runGenTs).toHaveBeenCalledTimes(2);
			expect(cache.get()).toBe(updated);
			expect(runGenTs).toHaveBeenCalledTimes(2);
		},
	);
});
