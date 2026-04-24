import { describe, expect, test } from "vitest";

import { transpileTestbench } from "../src/testbench-transpile";

describe("transpileTestbench", () => {
	test("uses emitted JavaScript when Monaco returns it", () => {
		const jsCode = transpileTestbench(
			'const value = (X as any); throw new Error("should not parse TS");',
			"test/example.test.ts",
			{
				outputFiles: [
					{
						name: "file:///test/example.test.js",
						text: 'import { expect } from "vitest";\nconst value = 1;\n',
					},
				],
			},
		);

		expect(jsCode).toContain("const value = 1;");
		expect(jsCode).not.toContain("import { expect }");
		expect(jsCode).not.toContain("as any");
	});

	test("falls back to sucrase when Monaco emit output is empty", () => {
		const jsCode = transpileTestbench(
			'const value = (X as any);\nexport { value };\n',
			"test/example.test.ts",
			{ outputFiles: [] },
		);

		expect(jsCode).toContain("const value = (X );");
		expect(jsCode).not.toContain("as any");
		expect(jsCode).not.toContain("export { value }");
	});

	test("strips CommonJS require destructuring used by transpilers", () => {
		const jsCode = transpileTestbench(
			'const value = 1 as any;',
			"test/example.test.ts",
			{
				outputFiles: [
					{
						name: "file:///test/example.test.js",
						text: 'const { expect } = require("vitest");\nconst value = 1;\n',
					},
				],
			},
		);

		expect(jsCode).toBe("const value = 1;\n");
	});

	test("throws a clear error when transpilation fails", () => {
		expect(() =>
			transpileTestbench("const value = (X as any;\n", "test/bad.test.ts", {
				outputFiles: [],
			}),
		).toThrow(/Failed to transpile test\/bad\.test\.ts as TypeScript/);
	});
});
