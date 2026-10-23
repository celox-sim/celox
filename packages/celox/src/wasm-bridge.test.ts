import { expect, expectTypeOf, test, vi } from "vitest";
import type { RawWasmSimulatorHandle } from "./wasm-bridge.js";
import {
	createWasmSimulatorBridge,
	createWasmSimulatorBridgeAsync,
	isWasmHandle,
} from "./wasm-bridge.js";

test("narrows an unknown frontend handle to the WASM contract", () => {
	const handle: unknown = {
		layoutJson: "{}",
		eventsJson: "{}",
		hierarchyJson: "{}",
		warningsJson: "[]",
		stableSize: 0,
		totalSize: 0,
		dispose: vi.fn(),
		initialMemoryBytes: () => new Uint8Array(),
		combWasmBytes: () => new Uint8Array(),
		eventWasmBytes: () => new Uint8Array(),
	};

	expect(isWasmHandle(handle)).toBe(true);
	if (!isWasmHandle(handle)) {
		throw new Error("expected a WASM frontend handle");
	}
	expectTypeOf(handle).toEqualTypeOf<RawWasmSimulatorHandle>();
	expect(handle.initialMemoryBytes()).toEqual(new Uint8Array());
});

// A real WASM module calling the generated FP helper ABI, including a signed
// i64 payload. This exercises linking and bigint preservation in both bridges.
function floatHelperModule(): Uint8Array {
	const text = (s: string) => [s.length, ...new TextEncoder().encode(s)];
	const section = (id: number, bytes: number[]) => [id, bytes.length, ...bytes];
	const body = [0, 0x42, 0x7f, 0x42, 2, 0x42, 3, 0x10, 0, 0x0b];
	return new Uint8Array([
		0,
		0x61,
		0x73,
		0x6d,
		1,
		0,
		0,
		0,
		...section(1, [2, 0x60, 3, 0x7e, 0x7e, 0x7e, 1, 0x7e, 0x60, 0, 1, 0x7e]),
		...section(2, [
			1,
			...text("env"),
			...text("celox_extern_4294967295"),
			0,
			0,
		]),
		...section(3, [1, 1]),
		...section(7, [1, ...text("run"), 0, 1]),
		...section(10, [1, body.length, ...body]),
	]);
}

for (const [name, create] of [
	["sync", createWasmSimulatorBridge],
	["async", createWasmSimulatorBridgeAsync],
] as const) {
	test(`${name} bridge binds the compiler FP helper for comb and event modules`, async () => {
		const evaluateFloat = vi.fn(() => 0n);
		const raw: RawWasmSimulatorHandle = {
			layoutJson: "{}",
			eventsJson: '{"clk":0}',
			hierarchyJson: "{}",
			warningsJson: "[]",
			stableSize: 8,
			totalSize: 8,
			dispose: vi.fn(),
			initialMemoryBytes: () => new Uint8Array(8),
			combWasmBytes: floatHelperModule,
			eventWasmBytes: floatHelperModule,
			evaluateFloat,
		};
		const { handle } = await create(raw);
		handle.evalComb();
		handle.tick(0);
		expect(evaluateFloat).toHaveBeenCalledWith(-1n, 2n, 3n);
		expect(evaluateFloat.mock.calls.length).toBeGreaterThanOrEqual(2);
	});
}
