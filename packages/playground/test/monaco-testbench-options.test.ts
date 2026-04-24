import { describe, expect, test } from "vitest";

import {
	buildMonacoTestbenchCompilerOptions,
	MONACO_TESTBENCH_LIBS,
} from "../src/monaco-testbench-options";

describe("buildMonacoTestbenchCompilerOptions", () => {
	test("pins libs needed for bigint-aware diagnostics", () => {
		const options = buildMonacoTestbenchCompilerOptions({
			ScriptTarget: { ES2022: "target-es2022" },
			ModuleKind: { ES2022: "module-es2022" },
			ModuleResolutionKind: { Bundler: "resolution-bundler" },
		} as never);

		expect(options.target).toBe("target-es2022");
		expect(options.module).toBe("module-es2022");
		expect(options.moduleResolution).toBe("resolution-bundler");
		expect(options.lib).toEqual([...MONACO_TESTBENCH_LIBS]);
		expect(options.lib).toContain("es2020.bigint");
		expect(options.rootDirs).toEqual([
			"file:///src",
			"file:///test",
			"file:///.celox/src",
		]);
	});

	test("keeps celox declaration paths resolvable for Monaco", () => {
		const options = buildMonacoTestbenchCompilerOptions({
			ScriptTarget: { ES2022: 1 },
			ModuleKind: { ES2022: 2 },
			ModuleResolutionKind: { Bundler: 3 },
		} as never);

		expect(options.paths?.["@celox-sim/celox"]).toEqual([
			"file:///node_modules/@celox-sim/celox/index.d.ts",
		]);
		expect(options.paths?.["@celox-sim/celox/*"]).toEqual([
			"file:///node_modules/@celox-sim/celox/*.d.ts",
			"file:///node_modules/@celox-sim/celox/dist/*.d.ts",
		]);
	});
});
