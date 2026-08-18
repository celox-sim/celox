import { describe, expect, test } from "vitest";

import {
	FourState,
	initializeFourStateMemory,
	writeSignalValue,
	X,
	Z,
} from "../src/playground-runtime-helpers";

describe("playground-runtime-helpers", () => {
	test("provides 4-state sentinels compatible with celox runtime encoding", () => {
		expect(X).toBe(Symbol.for("veryl:X"));
		expect(Z).toBe(Symbol.for("veryl:Z"));

		const value = FourState(0x05, 0xf0);

		expect(value).toEqual({
			__fourState: true,
			value: 0x05n,
			mask: 0xf0n,
		});
	});

	test("initializes 4-state value and mask planes to X", () => {
		const memory = new WebAssembly.Memory({ initial: 1 });
		initializeFourStateMemory(memory, [
			[4, 1],
			[8, 2],
		]);

		const bytes = new Uint8Array(memory.buffer);
		expect([...bytes.slice(4, 6)]).toEqual([0xff, 0xff]);
		expect([...bytes.slice(8, 12)]).toEqual([0xff, 0xff, 0xff, 0xff]);
		expect([...bytes.slice(12, 14)]).toEqual([0, 0]);
	});

	test("clears a stale mask when writing a defined scheduled value", () => {
		const memory = new WebAssembly.Memory({ initial: 1 });
		const sig = {
			offset: 0,
			width: 8,
			byte_size: 1,
			is_4state: true,
		};
		initializeFourStateMemory(memory, [[0, 1]]);
		const view = new DataView(memory.buffer);

		writeSignalValue(view, sig, 0x35, true);

		expect(view.getUint8(0)).toBe(0x35);
		expect(view.getUint8(1)).toBe(0);
	});
});
