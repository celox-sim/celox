import { expect, test } from "vitest";
import { evaluateWasmFloat } from "../src/wasm-float";
import { createPlaygroundSimulator } from "../src/playground-simulator";

for (const wide of [false, true]) {
	const bits = (value: number) => {
		const view = new DataView(new ArrayBuffer(8));
		if (wide) {
			view.setFloat64(0, value, true);
			return view.getBigInt64(0, true);
		}
		view.setFloat32(0, value, true);
		return BigInt(view.getUint32(0, true));
	};
	const run = (a: number, b: number, opcode: bigint) =>
		evaluateWasmFloat(bits(a), bits(b), opcode | (wide ? 256n : 0n));
	test(`f${wide ? 64 : 32} power and remainder preserve bit payloads`, () => {
		expect(run(2, 3, 5n)).toBe(bits(8));
		expect(run(-2, 3, 5n)).toBe(bits(-8));
		expect(run(1, Number.NaN, 5n)).toBe(bits(1));
		expect(run(-1, Number.POSITIVE_INFINITY, 5n)).toBe(bits(1));
		expect(run(-7.5, 2, 4n)).toBe(bits(-1.5));
		expect(run(-4, 2, 4n)).toBe(bits(-0));
	});
}

test("rejects helpers that should have been emitted as native WASM", () => {
	expect(() => evaluateWasmFloat(0n, 0n, 34n)).toThrow(/Unsupported/);
});

test("transferred modules link the helper without a compiler handle", () => {
	const text = (s: string) => [s.length, ...new TextEncoder().encode(s)];
	const section = (id: number, bytes: number[]) => [id, bytes.length, ...bytes];
	// run() calls f32 pow(0, 0) and drops its result.
	const body = [0, 0x42, 0, 0x42, 0, 0x42, 5, 0x10, 0, 0x1a, 0x0b];
	const module = new WebAssembly.Module(new Uint8Array([
		0, 0x61, 0x73, 0x6d, 1, 0, 0, 0,
		...section(1, [2, 0x60, 3, 0x7e, 0x7e, 0x7e, 1, 0x7e, 0x60, 0, 0]),
		...section(2, [1, ...text("env"), ...text("celox_extern_4294967295"), 0, 0]),
		...section(3, [1, 1]),
		...section(7, [1, ...text("run"), 0, 1]),
		...section(10, [1, body.length, ...body]),
	]));
	const sim = createPlaygroundSimulator({
		fourState: false, fourStateInitRegions: [], layout: {}, events: { clk: 0 },
		totalSize: 8, combModule: module, eventModules: { clk: module },
		phaseModules: {}, edgePlans: { fourState: false, events: {} },
	});
	expect(() => sim.tick()).not.toThrow();
});
