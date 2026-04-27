import { describe, expect, test } from "vitest";

import {
	buildVirtualModuleDts,
	extractPortsFromSource,
} from "../src/virtual-module-dts";

describe("virtual-module-dts", () => {
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
