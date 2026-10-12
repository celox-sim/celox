import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test } from "vitest";
import { GenTsCache } from "../../vite-plugin/src/cache.js";

test("configuration-only edits refresh the real generated catalogue", () => {
	const root = mkdtempSync(join(tmpdir(), "celox-cache-real-"));
	try {
		mkdirSync(join(root, "src"));
		mkdirSync(join(root, "extra"));
		writeFileSync(
			join(root, "Veryl.toml"),
			'[project]\nname = "cache_real"\nversion = "0.1.0"\n\n[build]\nsources = ["src"]\n',
		);
		writeFileSync(join(root, "src", "Main.veryl"), "module Main {}\n");
		writeFileSync(join(root, "extra", "Extra.veryl"), "module Extra {}\n");
		const cache = new GenTsCache(root);
		const generated = cache.get();
		// Creating Veryl.lock conservatively refreshes the next lookup because
		// cache fingerprints cannot distinguish generator and external writes.
		const initial = cache.get();
		expect(initial).toEqual(generated);
		expect(initial.modules.some((m) => m.moduleName === "Extra")).toBe(false);
		expect(cache.get()).toBe(initial);
		writeFileSync(join(root, "celox.toml"), '[test]\nsources = ["extra"]\n');
		const updated = cache.get();
		expect(updated.modules.some((m) => m.moduleName === "Extra")).toBe(true);
		expect(cache.get()).toBe(updated);
	} finally {
		rmSync(root, { recursive: true, force: true });
	}
});
