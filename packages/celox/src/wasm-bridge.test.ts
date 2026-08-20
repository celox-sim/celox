import { expect, expectTypeOf, test, vi } from "vitest";
import type { RawWasmSimulatorHandle } from "./wasm-bridge.js";
import { isWasmHandle } from "./wasm-bridge.js";

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
