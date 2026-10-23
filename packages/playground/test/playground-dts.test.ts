import { readdirSync, readFileSync } from "node:fs";
import { describe, expect, test } from "vitest";

import {
	buildVirtualModuleDts,
	celoxDtsLibs,
	extractPortsFromSource,
} from "../src/playground-dts";

const CELOX_DIST = new URL("../../celox/dist/", import.meta.url);

function celoxDistDts(): Record<string, string> {
	const files: Record<string, string> = {};
	for (const name of readdirSync(CELOX_DIST)) {
		if (name.endsWith(".d.ts")) {
			files[`../../celox/dist/${name}`] = readFileSync(
				new URL(name, CELOX_DIST),
				"utf8",
			);
		}
	}
	return files;
}

describe("celox declarations for Monaco", () => {
	test("every import between them resolves and explicit edges are typed", () => {
		const libs = celoxDtsLibs(celoxDistDts());
		const paths = new Set(libs.map((lib) => lib.filePath));
		for (const lib of libs) {
			for (const [, name] of lib.content.matchAll(
				/from\s+["']@celox-sim\/celox\/([\w-]+)["']/g,
			)) {
				expect(paths).toContain(
					`file:///node_modules/@celox-sim/celox/${name}.d.ts`,
				);
			}
			// Imports of sibling modules all point into the package.
			expect(lib.content).not.toMatch(/from\s+["']\.\/[\w-]+\.js["']/);
		}
		const simulator = libs.find((lib) =>
			lib.filePath.endsWith("/celox/simulator.d.ts"),
		);
		for (const method of [
			"posedge(",
			"negedge(",
			"activeEdge(",
			"inactiveEdge(",
			"tick(",
			"edges(",
		]) {
			expect(simulator?.content).toContain(method);
		}
		expect(paths).toContain("file:///node_modules/@celox-sim/celox/index.d.ts");
	});
});

describe("playground-dts", () => {
	test("4-state inputs accept FourStateSignalValue in virtual module DTS", () => {
		const ports = extractPortsFromSource(`module FourStateDemo (
    a: input logic<8>,
    b: input logic<8>,
    snapshot: output logic<8>,
) {
    assign snapshot = a;
}`);

		const dts = buildVirtualModuleDts("FourStateDemo", ports);

		expect(dts).toContain("import type { FourStateSignalValue, ModuleDefinition }");
		expect(dts).toContain("get a(): bigint;");
		expect(dts).toContain("set a(value: FourStateSignalValue);");
		expect(dts).toContain("readonly snapshot: bigint;");
	});

	test("clock ports are excluded and bit inputs stay bigint-writable", () => {
		const ports = extractPortsFromSource(`module Counter (
    clk: input clock,
    en: input bit,
    count: output logic<8>,
) {
}`);

		const dts = buildVirtualModuleDts("Counter", ports);

		expect(dts).not.toContain("clk");
		expect(dts).toContain("set en(value: bigint);");
		expect(dts).toContain("readonly count: bigint;");
	});
});
