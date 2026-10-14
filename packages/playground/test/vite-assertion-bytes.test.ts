import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { runInNewContext } from "node:vm";
import { expect, test, vi } from "vitest";

vi.mock("../../vite-plugin/src/generator.js", () => ({
	runGenTs: (projectRoot: string) => ({
		projectPath: projectRoot,
		modules: [
			{
				moduleName: "T",
				sourceFile: "t.veryl",
				dtsContent: "",
				mdContent: "",
				ports: {},
				events: [],
				isTest: true,
			},
		],
		fileModules: { "t.veryl": ["T"] },
	}),
}));

import celox from "../../vite-plugin/src/index.js";

test("generated assertion diagnostics explicitly decode or escape message bytes", async () => {
	const root = mkdtempSync(join(tmpdir(), "celox-output-bytes-"));
	try {
		writeFileSync(join(root, "t.veryl"), "module T {}\n");
		const plugin = celox({ projectRoot: root }) as unknown as {
			configResolved(config: { root: string }): void;
			buildStart(): Promise<void>;
			resolveId(path: string): string;
			load(this: { addWatchFile(file: string): void }, id: string): string;
		};
		plugin.configResolved({ root });
		await plugin.buildStart();
		const code = plugin.load
			.call({ addWatchFile: vi.fn() }, plugin.resolveId(join(root, "t.veryl")))
			.replace(/^import .*\n/gm, "");
		for (const [bytes, expected] of [
			[new TextEncoder().encode("failure é"), "failure é"],
			[new Uint8Array([255, 0, 128]), "\\xff\\x00\\x80"],
		] as const) {
			const soft = vi.fn(() => ({ toBe: vi.fn() }));
			const assertion = Object.assign(
				vi.fn(() => ({ toBe: vi.fn() })),
				{ soft },
			);
			runInNewContext(code, {
				TextDecoder,
				test: (_name: string, callback: () => void) => callback(),
				expect: assertion,
				loadNativeAddon: () => ({
					runTestFromProject: () => ({
						passed: false,
						assertions: [{ passed: false, message: bytes }],
					}),
				}),
			});
			expect(soft).toHaveBeenCalledWith(false, expected);
		}
	} finally {
		rmSync(root, { recursive: true, force: true });
	}
});
