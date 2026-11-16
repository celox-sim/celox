/**
 * HDL testbench examples through the WASM timed-testbench runner: a
 * SystemVerilog testbench checks a Veryl design, with no TypeScript
 * stimulus involved.
 */
import { loadNativeAddon } from "@celox-sim/celox";
import { describe, expect, test } from "vitest";
import { TB_EXAMPLES } from "../src/tb-examples";

const isWasm =
	!!process.env.NAPI_RS_WASI_FLAVOR ||
	process.env.NAPI_RS_FORCE_WASI === "true" ||
	process.env.NAPI_RS_FORCE_WASI === "error";

type WasmAddon = {
	runTimedTestbench(
		sources: { content: string; path: string }[],
		top: string,
	): string;
};

describe.skipIf(!isWasm)("HDL testbench (playground WASM)", () => {
	test("SystemVerilog testbench tests the Veryl Counter", () => {
		const addon = loadNativeAddon() as unknown as WasmAddon;
		const example = TB_EXAMPLES.sv_tb_veryl;
		const result = JSON.parse(
			addon.runTimedTestbench(
				example.files.map((f) => ({ content: f.content, path: f.path })),
				example.top,
			),
		);
		expect(result.error).toBeNull();
		expect(result.finished).toBe(true);
		expect(result.passed).toBe(true);
		expect(result.signals.errors).toBe("0");
		expect(result.signals.done).toBe("1");
		expect(result.signals.count).toBe("5");
	});
});
