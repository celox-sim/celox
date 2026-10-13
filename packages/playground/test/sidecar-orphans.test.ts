import {
	existsSync,
	mkdirSync,
	mkdtempSync,
	readFileSync,
	renameSync,
	rmSync,
	writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, expect, test, vi } from "vitest";

const aliases = vi.hoisted(() => new Map<string, string>());
const unlinkFailures = vi.hoisted(() => new Map<string, string>());
const vanishedDirectories = vi.hoisted(() => new Set<string>());
vi.mock("node:fs", async (importOriginal) => {
	const fs = await importOriginal<typeof import("node:fs")>();
	// Emulate filesystem aliases left by a case-only rename. readdirSync keeps
	// returning the old entry while reads, writes and stats through either name work.
	function alias(path: any) {
		return aliases.get(String(path)) ?? path;
	}
	return {
		...fs,
		existsSync: (path: any) => fs.existsSync(alias(path)),
		mkdirSync: (path: any, options: any) => fs.mkdirSync(alias(path), options),
		writeFileSync: (path: any, data: any, options: any) =>
			fs.writeFileSync(alias(path), data, options),
		readFileSync: (path: any, options: any) => fs.readFileSync(alias(path), options),
		statSync: (path: any, options: any) => fs.statSync(alias(path), options),
		unlinkSync: (path: any) => {
			const code = unlinkFailures.get(String(path));
			if (code) {
				if (code === "ENOENT") fs.unlinkSync(path);
				throw Object.assign(new Error(`cleanup ${code}`), { code });
			}
			return fs.unlinkSync(path);
		},
		readdirSync: (path: any, options: any) => {
			if (vanishedDirectories.delete(String(path)))
				fs.rmSync(path, { recursive: true });
			return fs.readdirSync(path, options);
		},
	};
});

import {
	cleanSidecars,
	generateSidecars,
} from "../../vite-plugin/src/sidecar.js";
import type { GenTsJsonOutput } from "../../vite-plugin/src/types.js";

const roots: string[] = [];
afterEach(() => {
	aliases.clear();
	unlinkFailures.clear();
	vanishedDirectories.clear();
	for (const root of roots.splice(0))
		rmSync(root, { recursive: true, force: true });
});
function project(): string {
	const root = mkdtempSync(join(tmpdir(), "celox-sidecars-"));
	roots.push(root);
	mkdirSync(join(root, "src"));
	mkdirSync(join(root, ".celox/src"), { recursive: true });
	writeFileSync(join(root, "src/Main.veryl"), "module Main {}\n");
	return root;
}
function data(root: string): GenTsJsonOutput {
	return {
		projectPath: root,
		fileModules: { "src/Main.veryl": ["Main"] },
		modules: [
			{
				moduleName: "Main",
				sourceFile: "src/Main.veryl",
				dtsContent: "export declare const Main: {};\n",
				mdContent: "",
				ports: {},
				events: [],
				isTest: false,
			},
		],
	};
}
test("startup removes old declaration sidecars and keeps other generated files", () => {
	const root = project();
	const ghost = join(root, ".celox/src/Removed.d.veryl.ts");
	const components = join(root, ".celox/components.d.ts");
	writeFileSync(ghost, "old declaration");
	writeFileSync(components, "component declaration");
	generateSidecars(data(root), root);
	expect(existsSync(ghost)).toBe(false);
	expect(existsSync(components)).toBe(true);
	expect(existsSync(join(root, ".celox/src/Main.d.veryl.ts"))).toBe(true);
});
test("a file that no longer exports a module loses its previous declaration", () => {
	const root = project();
	generateSidecars(data(root), root);
	generateSidecars(
		{ projectPath: root, modules: [], fileModules: { "src/Main.veryl": [] } },
		root,
	);
	expect(existsSync(join(root, ".celox/src/Main.d.veryl.ts"))).toBe(false);
});
test("failed replacement leaves old declaration sidecars available", () => {
	const root = project();
	const ghost = join(root, ".celox/Removed.d.veryl.ts");
	writeFileSync(ghost, "old declaration");
	rmSync(join(root, ".celox/src"), { recursive: true });
	writeFileSync(join(root, ".celox/src"), "blocks directory creation");
	expect(() => generateSidecars(data(root), root)).toThrow();
	expect(existsSync(ghost)).toBe(true);
});

test.each(["src", "Src"])(
	"startup retains declarations after a case-only rename in %s",
	(directory) => {
		const root = project();
		const oldPath = join(root, ".celox/src/main.d.veryl.ts");
		const nextPath = join(root, ".celox", directory, "Main.d.veryl.ts");
		const ghost = join(root, ".celox/src/Removed.d.veryl.ts");
		writeFileSync(oldPath, "old declaration");
		writeFileSync(ghost, "orphan declaration");
		aliases.set(nextPath, oldPath);
		aliases.set(join(root, ".celox", directory), join(root, ".celox/src"));
		aliases.set(
			join(root, directory, "Main.veryl"),
			join(root, "src/Main.veryl"),
		);
		const output = {
			...data(root),
			fileModules: { [`${directory}/Main.veryl`]: ["Main"] },
		};
		expect(generateSidecars(output, root)).toEqual([nextPath]);
		expect(existsSync(oldPath)).toBe(true);
		expect(readFileSync(oldPath, "utf8")).toBe(
			"export declare const Main: {};\n",
		);
		expect(existsSync(ghost)).toBe(false);
		cleanSidecars([oldPath], [nextPath]);
		expect(existsSync(oldPath)).toBe(true);
	},
);

test("startup removes a distinct case-sensitive declaration with the same folded path", (context) => {
	const root = project();
	const ghost = join(root, ".celox/src/main.d.veryl.ts");
	const retained = join(root, ".celox/src/Main.d.veryl.ts");
	writeFileSync(retained, "current declaration");
	if (existsSync(ghost))
		context.skip("filesystem does not support distinct spellings");
	writeFileSync(ghost, "orphan declaration");
	generateSidecars(data(root), root);
	expect(existsSync(ghost)).toBe(false);
	expect(existsSync(retained)).toBe(true);
});

test("watch cleanup retains case aliases and removes distinct obsolete files", () => {
	const root = project();
	const oldPath = join(root, ".celox/src/main.d.veryl.ts");
	const nextPath = join(root, ".celox/src/Main.d.veryl.ts");
	const ghost = join(root, ".celox/src/Removed.d.veryl.ts");
	writeFileSync(oldPath, "old declaration");
	writeFileSync(ghost, "orphan declaration");
	aliases.set(nextPath, oldPath);
	const written = generateSidecars(data(root), root, { removeOrphans: false });
	cleanSidecars([oldPath, ghost], written);
	expect(existsSync(oldPath)).toBe(true);
	expect(readFileSync(oldPath, "utf8")).toBe(
		"export declare const Main: {};\n",
	);
	expect(existsSync(ghost)).toBe(false);
});

test("watch cleanup removes a distinct case-sensitive file with the same folded path", (context) => {
	const root = project();
	const ghost = join(root, ".celox/src/main.d.veryl.ts");
	const retained = join(root, ".celox/src/Main.d.veryl.ts");
	writeFileSync(retained, "current declaration");
	if (existsSync(ghost))
		context.skip("filesystem does not support distinct spellings");
	writeFileSync(ghost, "orphan declaration");
	cleanSidecars([ghost], [retained]);
	expect(existsSync(ghost)).toBe(false);
	expect(existsSync(retained)).toBe(true);
});

test("actual filesystem preserves a declaration after a case-only source rename", () => {
	const root = project();
	renameSync(join(root, "src/Main.veryl"), join(root, "src/main.veryl"));
	const initial = {
		...data(root),
		fileModules: { "src/main.veryl": ["Main"] },
	};
	generateSidecars(initial, root);
	renameSync(join(root, "src/main.veryl"), join(root, "src/Main.veryl"));
	const [written] = generateSidecars(data(root), root);
	expect(existsSync(written)).toBe(true);
	expect(readFileSync(written, "utf8")).toBe(
		"export declare const Main: {};\n",
	);
});

test.each([
	{ previous: "Caf\u00e9", next: "Cafe\u0301" },
	{ previous: "\u03a3", next: "\u03c2" },
	{ previous: "\u1e9e", next: "\u00df" },
])(
	"startup and watch cleanup preserve Unicode aliases ($previous to $next)",
	({ previous, next }) => {
		const root = project();
		const oldPath = join(root, ".celox/src", `${previous}.d.veryl.ts`);
		const nextPath = join(root, ".celox/src", `${next}.d.veryl.ts`);
		writeFileSync(oldPath, "old declaration");
		aliases.set(nextPath, oldPath);
		aliases.set(
			join(root, "src", `${next}.veryl`),
			join(root, "src/Main.veryl"),
		);
		const output = {
			...data(root),
			fileModules: { [`src/${next}.veryl`]: ["Main"] },
		};
		expect(generateSidecars(output, root)).toEqual([nextPath]);
		cleanSidecars([oldPath], [nextPath]);
		expect(existsSync(oldPath)).toBe(true);
		expect(readFileSync(oldPath, "utf8")).toBe(
			"export declare const Main: {};\n",
		);
	},
);

test("startup removes distinct normalization-sensitive obsolete declarations", (context) => {
	const root = project();
	const oldPath = join(root, ".celox/src/Caf\u00e9.d.veryl.ts");
	const nextPath = join(root, ".celox/src/Cafe\u0301.d.veryl.ts");
	writeFileSync(oldPath, "orphan declaration");
	if (existsSync(nextPath))
		context.skip("filesystem aliases normalized spellings");
	writeFileSync(join(root, "src/Cafe\u0301.veryl"), "module Main {}\n");
	const output = {
		...data(root),
		fileModules: { "src/Cafe\u0301.veryl": ["Main"] },
	};
	generateSidecars(output, root);
	expect(existsSync(oldPath)).toBe(false);
	expect(existsSync(nextPath)).toBe(true);
});

test("startup tolerates another process deleting an observed orphan", () => {
	const root = project();
	const ghost = join(root, ".celox/src/Removed.d.veryl.ts");
	writeFileSync(ghost, "orphan declaration");
	unlinkFailures.set(ghost, "ENOENT");
	expect(() => generateSidecars(data(root), root)).not.toThrow();
	expect(existsSync(ghost)).toBe(false);
	expect(existsSync(join(root, ".celox/src/Main.d.veryl.ts"))).toBe(true);
});

test("startup tolerates an orphan directory disappearing before traversal", () => {
	const root = project();
	const dir = join(root, ".celox/removed");
	mkdirSync(dir);
	writeFileSync(join(dir, "Removed.d.veryl.ts"), "orphan declaration");
	vanishedDirectories.add(dir);
	expect(() => generateSidecars(data(root), root)).not.toThrow();
	expect(existsSync(dir)).toBe(false);
	expect(existsSync(join(root, ".celox/src/Main.d.veryl.ts"))).toBe(true);
});

test("startup cleanup reports genuine I/O failures", () => {
	const root = project();
	const ghost = join(root, ".celox/src/Removed.d.veryl.ts");
	writeFileSync(ghost, "orphan declaration");
	unlinkFailures.set(ghost, "EACCES");
	expect(() => generateSidecars(data(root), root)).toThrow("cleanup EACCES");
	expect(existsSync(ghost)).toBe(true);
});
