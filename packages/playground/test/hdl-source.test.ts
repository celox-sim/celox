import { describe, expect, test } from "vitest";

import {
	extractModuleNames,
	extractSvPortsFromSource,
	hdlDeclarationPaths,
	isHdlPath,
	isSvPath,
} from "../src/hdl-source";
import { SV_EXAMPLES } from "../src/sv-examples";
import { buildVirtualModuleDts } from "../src/virtual-module-dts";

const SV_ADDER = `module Adder (
    input  logic [15:0] a,
    input  logic [15:0] b,
    output logic [16:0] sum
);
    always_comb begin
        sum = a + b;
    end
endmodule
`;

describe("hdl-source", () => {
	test("classifies SystemVerilog and Veryl paths", () => {
		expect(isSvPath("src/Adder.sv")).toBe(true);
		expect(isSvPath("src/Adder.svh")).toBe(true);
		expect(isSvPath("src/Adder.veryl")).toBe(false);
		expect(isHdlPath("src/Adder.sv")).toBe(true);
		expect(isHdlPath("src/Adder.veryl")).toBe(true);
		expect(isHdlPath("test/adder.test.ts")).toBe(false);
	});

	test("extracts ANSI SystemVerilog ports with widths", () => {
		const ports = extractSvPortsFromSource(SV_ADDER);
		expect(ports.a).toMatchObject({ direction: "input", kind: "logic", width: 16, is4state: true });
		expect(ports.b).toMatchObject({ direction: "input", width: 16 });
		expect(ports.sum).toMatchObject({ direction: "output", width: 17 });
	});

	test("inherits direction and type for grouped ANSI ports", () => {
		const ports = extractSvPortsFromSource(
			"module M(input logic [7:0] a, b, output bit [3:0] y); endmodule",
		);
		expect(ports.b).toMatchObject({ direction: "input", kind: "logic", width: 8 });
		expect(ports.y).toMatchObject({ direction: "output", kind: "bit", width: 4, is4state: false });
	});

	test("extracts non-ANSI ports declared in the body", () => {
		const ports = extractSvPortsFromSource(`module M(a, y);
    input logic [7:0] a;
    output logic [7:0] y;
    assign y = a;
endmodule`);
		expect(ports.a).toMatchObject({ direction: "input", width: 8 });
		expect(ports.y).toMatchObject({ direction: "output", width: 8 });
	});

	test("extracts module names", () => {
		expect(extractModuleNames(SV_ADDER)).toEqual(["Adder"]);
		expect(extractModuleNames("module A; endmodule module B; endmodule")).toEqual(["A", "B"]);
	});

	test("ignores module keywords inside comments", () => {
		expect(extractModuleNames("// module for adding\n/* module Fake */\nmodule Real; endmodule")).toEqual(["Real"]);
		expect(extractModuleNames("/* multi\nline module Hidden */ module Shown; endmodule")).toEqual(["Shown"]);
	});

	test("registers declarations at the actual source path", () => {
		const paths = hdlDeclarationPaths("Adder", [
			{ path: "rtl/Adder.sv", content: SV_ADDER },
		]);
		expect(paths).toContain("rtl/Adder.sv");
		expect(paths).toContain("rtl/Adder.d.sv.ts");
		expect(paths).toContain("src/Adder.sv");
	});

	test("builds a virtual DTS from SystemVerilog ports", () => {
		const dts = buildVirtualModuleDts("Adder", extractSvPortsFromSource(SV_ADDER));
		expect(dts).toContain("set a(value: FourStateSignalValue);");
		expect(dts).toContain("readonly sum: bigint;");
	});
});

describe("sv-examples", () => {
	test("every SV example declares its module and a matching testbench import", () => {
		expect(Object.keys(SV_EXAMPLES).length).toBeGreaterThanOrEqual(3);
		for (const [key, example] of Object.entries(SV_EXAMPLES)) {
			expect(example.sourcePath.endsWith(".sv"), key).toBe(true);
			const modules = extractModuleNames(example.source);
			expect(modules.length, key).toBeGreaterThanOrEqual(1);
			expect(example.testbench, key).toContain(modules[0]);
			expect(example.testbench, key).toContain(example.sourcePath.split("/").pop());
			expect(Object.keys(extractSvPortsFromSource(example.source)).length, key).toBeGreaterThanOrEqual(2);
		}
	});
});
