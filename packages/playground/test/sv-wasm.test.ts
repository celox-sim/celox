/**
 * SystemVerilog sources compiled through the playground's WASM pipeline:
 * the compiler worker's handle is built from `.sv` sources and driven by
 * the playground runtime, exactly as the browser does for SV examples.
 */
import { loadNativeAddon } from "@celox-sim/celox";
import { describe, expect, test } from "vitest";
import {
	type CompilerHandle,
	compiledFromHandle,
} from "../src/compile-simulator";
import {
	createPlaygroundSimulation,
	createPlaygroundSimulator,
} from "../src/playground-simulator";
import { SV_EXAMPLES } from "../src/sv-examples";

const isWasm =
	!!process.env.NAPI_RS_WASI_FLAVOR ||
	process.env.NAPI_RS_FORCE_WASI === "true" ||
	process.env.NAPI_RS_FORCE_WASI === "error";

type WasmAddon = {
	NativeSimulatorHandle: new (
		sources: { content: string; path: string }[],
		top: string,
		options?: { fourState?: boolean },
	) => CompilerHandle & { dispose(): void };
	genTsFromSource(sources: { content: string; path: string }[]): string;
};

function compileExample(key: string, top: string, fourState = false) {
	const addon = loadNativeAddon() as unknown as WasmAddon;
	const example = SV_EXAMPLES[key];
	const handle = new addon.NativeSimulatorHandle(
		[{ content: example.source, path: example.sourcePath }],
		top,
		{ fourState },
	);
	try {
		return compiledFromHandle(handle, fourState);
	} finally {
		handle.dispose();
	}
}

describe.skipIf(!isWasm)("SystemVerilog (playground WASM)", () => {
	test("genTsFromSource reports SV module ports", () => {
		const addon = loadNativeAddon() as unknown as WasmAddon;
		const example = SV_EXAMPLES.sv_adder;
		const result = JSON.parse(
			addon.genTsFromSource([
				{ content: example.source, path: example.sourcePath },
			]),
		);
		expect(result.modules[0].moduleName).toBe("Adder");
		expect(result.modules[0].ports.a.width).toBe(16);
		expect(result.modules[0].ports.sum.width).toBe(17);
	});

	test("adds two numbers with an SV always_comb adder", () => {
		const sim = createPlaygroundSimulator(compileExample("sv_adder", "Adder"));
		sim.dut.a = 100n;
		sim.dut.b = 200n;
		expect(sim.dut.sum).toBe(300n);
		sim.dut.a = 0xffffn;
		sim.dut.b = 1n;
		expect(sim.dut.sum).toBe(0x10000n);
	});

	test("counts with an SV always_ff counter", () => {
		const sim = createPlaygroundSimulation(
			compileExample("sv_counter", "Counter"),
		);
		sim.addClock("clk", { period: 10 });
		sim.reset("rst_n");
		sim.dut.en = 1n;
		sim.waitForCycles("clk", 5);
		expect(sim.dut.count).toBe(5n);
		sim.dispose();
	});

	test("evaluates the parameterized SV ALU by opcode", () => {
		const sim = createPlaygroundSimulator(compileExample("sv_alu", "Alu"));
		sim.dut.a = 0x3cn;
		sim.dut.b = 0x0fn;
		sim.dut.op = 0n;
		expect(sim.dut.y).toBe(0x4bn);
		sim.dut.op = 1n;
		expect(sim.dut.y).toBe(0x2dn);
		sim.dut.a = 5n;
		sim.dut.b = 5n;
		expect(sim.dut.y).toBe(0n);
		expect(sim.dut.zero).toBe(1n);
	});
});
