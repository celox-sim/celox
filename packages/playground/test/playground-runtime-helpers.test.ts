import { describe, expect, test, vi } from "vitest";

import {
	FourState,
	initializeFourStateMemory,
	runPlaygroundTicks,
	writeSignalValue,
	X,
	Z,
} from "../src/playground-runtime-helpers";

function createTickRuntime(options?: {
	defaultEventId?: number;
	knownEventIds?: ReadonlySet<number>;
	flushDirty?: () => void;
	tickOne?: (eventId: number) => void;
}) {
	return {
		defaultEventId: options?.defaultEventId ?? 0,
		knownEventIds: options?.knownEventIds ?? new Set([0, 7]),
		flushDirty: options?.flushDirty ?? vi.fn(),
		tickOne: options?.tickOne ?? vi.fn(),
	};
}

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

	test("dispatches every supported tick overload consistently", () => {
		const flushDirty = vi.fn();
		const tickOne = vi.fn();
		const runtime = createTickRuntime({ flushDirty, tickOne });

		runPlaygroundTicks(runtime, 3);
		runPlaygroundTicks(runtime, 7, 2);
		runPlaygroundTicks(runtime, undefined, 2);
		runPlaygroundTicks(runtime, { name: "aux", id: 7 }, 2);

		expect(flushDirty).toHaveBeenCalledTimes(4);
		expect(tickOne.mock.calls).toEqual([
			[0],
			[0],
			[0],
			[7],
			[7],
			[0],
			[0],
			[7],
			[7],
		]);
	});

	test.each([NaN, Infinity, -Infinity, -1, 0.5, 1.5, 0x1_0000_0000])(
		"rejects invalid tick count %s before flushing or entering the loop",
		(invalidCount) => {
			const flushDirty = vi.fn();
			const tickOne = vi.fn(() => {
				throw new Error("tick loop entered");
			});
			const runtime = createTickRuntime({ flushDirty, tickOne });

			expect(() => runPlaygroundTicks(runtime, invalidCount)).toThrowError(
				new RangeError(
					`Tick count ${invalidCount} must be an integer between 0 and 4294967295`,
				),
			);
			expect(flushDirty).not.toHaveBeenCalled();
			expect(tickOne).not.toHaveBeenCalled();
		},
	);

	test.each([NaN, Infinity, -Infinity, -1, 0.5, 1.5, 0x1_0000_0000])(
		"rejects invalid event ID %s before flushing or entering the loop",
		(invalidEventId) => {
			const flushDirty = vi.fn();
			const tickOne = vi.fn(() => {
				throw new Error("tick loop entered");
			});
			const runtime = createTickRuntime({ flushDirty, tickOne });

			expect(() =>
				runPlaygroundTicks(runtime, invalidEventId, 1),
			).toThrowError(
				new RangeError(
					`Event ID ${invalidEventId} must be an integer between 0 and 4294967295`,
				),
			);
			expect(flushDirty).not.toHaveBeenCalled();
			expect(tickOne).not.toHaveBeenCalled();
		},
	);

	test("validates the second argument as the tick count", () => {
		const flushDirty = vi.fn();
		const tickOne = vi.fn(() => {
			throw new Error("tick loop entered");
		});
		const runtime = createTickRuntime({ flushDirty, tickOne });

		expect(() => runPlaygroundTicks(runtime, 7, Infinity)).toThrowError(
			new RangeError(
				"Tick count Infinity must be an integer between 0 and 4294967295",
			),
		);
		expect(flushDirty).not.toHaveBeenCalled();
		expect(tickOne).not.toHaveBeenCalled();
	});

	test("accepts known raw event IDs at both u32 boundaries", () => {
		const tickOne = vi.fn();
		const runtime = createTickRuntime({
			knownEventIds: new Set([0, 0xffff_ffff]),
			tickOne,
		});

		runPlaygroundTicks(runtime, 0, 1);
		runPlaygroundTicks(runtime, 0xffff_ffff, 1);

		expect(tickOne).toHaveBeenNthCalledWith(1, 0);
		expect(tickOne).toHaveBeenNthCalledWith(2, 0xffff_ffff);
	});

	test("rejects unknown raw and handle event IDs even when count is zero", () => {
		const flushDirty = vi.fn();
		const tickOne = vi.fn();
		const runtime = createTickRuntime({
			knownEventIds: new Set([7, 0]),
			flushDirty,
			tickOne,
		});
		const expected = new RangeError(
			"Unknown event ID 9. Available IDs: 0, 7",
		);

		expect(() => runPlaygroundTicks(runtime, 9, 0)).toThrowError(expected);
		expect(() =>
			runPlaygroundTicks(runtime, { name: "unknown", id: 9 }, 0),
		).toThrowError(expected);
		expect(flushDirty).not.toHaveBeenCalled();
		expect(tickOne).not.toHaveBeenCalled();
	});

	test("rejects a positive default count when the simulator has no events", () => {
		const flushDirty = vi.fn();
		const tickOne = vi.fn();
		const runtime = createTickRuntime({
			defaultEventId: -1,
			knownEventIds: new Set(),
			flushDirty,
			tickOne,
		});

		expect(() => runPlaygroundTicks(runtime, 1)).toThrowError(
			new Error("Simulator has no events to tick"),
		);
		expect(flushDirty).not.toHaveBeenCalled();
		expect(tickOne).not.toHaveBeenCalled();
	});

	test("validates default event metadata before flushing", () => {
		const flushDirty = vi.fn();
		const tickOne = vi.fn();
		const runtime = createTickRuntime({
			defaultEventId: Number.NaN,
			knownEventIds: new Set([Number.NaN]),
			flushDirty,
			tickOne,
		});

		expect(() => runPlaygroundTicks(runtime)).toThrowError(
			new RangeError(
				"Event ID NaN must be an integer between 0 and 4294967295",
			),
		);
		expect(flushDirty).not.toHaveBeenCalled();
		expect(tickOne).not.toHaveBeenCalled();
	});

	test("default count-zero forms flush dirty state without requiring an event", () => {
		const flushDirty = vi.fn();
		const tickOne = vi.fn();
		const runtime = createTickRuntime({
			defaultEventId: -1,
			knownEventIds: new Set(),
			flushDirty,
			tickOne,
		});

		runPlaygroundTicks(runtime, 0);
		runPlaygroundTicks(runtime, undefined, 0);

		expect(flushDirty).toHaveBeenCalledTimes(2);
		expect(tickOne).not.toHaveBeenCalled();
	});

	test("an explicit event is still validated at count zero with no events", () => {
		const flushDirty = vi.fn();
		const tickOne = vi.fn();
		const runtime = createTickRuntime({
			defaultEventId: -1,
			knownEventIds: new Set(),
			flushDirty,
			tickOne,
		});

		expect(() => runPlaygroundTicks(runtime, 0, 0)).toThrowError(
			new RangeError("Unknown event ID 0. Available IDs: (none)"),
		);
		expect(flushDirty).not.toHaveBeenCalled();
		expect(tickOne).not.toHaveBeenCalled();
	});

	test("accepts the maximum u32 tick count without pre-iterating it", () => {
		const stopAfterFirstTick = new Error("stop after first tick");
		const flushDirty = vi.fn();
		const tickOne = vi.fn(() => {
			throw stopAfterFirstTick;
		});
		const runtime = createTickRuntime({ flushDirty, tickOne });

		expect(() => runPlaygroundTicks(runtime, 0xffff_ffff)).toThrowError(
			stopAfterFirstTick,
		);
		expect(flushDirty).toHaveBeenCalledOnce();
		expect(tickOne).toHaveBeenCalledOnce();
		expect(tickOne).toHaveBeenCalledWith(0);
	});
});
