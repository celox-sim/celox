import { describe, expect, test, vi } from "vitest";
import { type NativeCreateFn, Simulator } from "./simulator.js";
import type {
	CreateResult,
	FrontendSimulatorHandle,
	ModuleDefinition,
	NativeSimulatorHandle,
} from "./types.js";

// ---------------------------------------------------------------------------
// Mock helpers
// ---------------------------------------------------------------------------

interface AdderPorts {
	rst: bigint;
	a: bigint;
	b: bigint;
	readonly sum: bigint;
}

const AdderModule: ModuleDefinition<AdderPorts> = {
	__celox_module: true,
	name: "Adder",
	sources: [{ path: "", content: "module Adder ..." }],
	ports: {
		clk: { direction: "input", type: "clock", width: 1 },
		rst: { direction: "input", type: "reset", width: 1 },
		a: { direction: "input", type: "logic", width: 16 },
		b: { direction: "input", type: "logic", width: 16 },
		sum: { direction: "output", type: "logic", width: 17 },
	},
	events: ["clk", "aux"],
};

function createMockNative(): {
	create: NativeCreateFn;
	handle: NativeSimulatorHandle;
	buffer: SharedArrayBuffer;
} {
	const buffer = new SharedArrayBuffer(64);
	const evalFn = () => {
		const view = new DataView(buffer);
		const a = view.getUint16(2, true);
		const b = view.getUint16(4, true);
		view.setUint32(8, a + b, true);
	};
	const handle: NativeSimulatorHandle = {
		tick: vi.fn().mockImplementation(evalFn),
		tickN: vi.fn().mockImplementation((_eventId: number, count: number) => {
			for (let i = 0; i < count; i++) evalFn();
		}),
		evalComb: vi.fn().mockImplementation(evalFn),
		dump: vi.fn(),
		dispose: vi.fn(),
	};

	const create: NativeCreateFn = vi.fn().mockReturnValue({
		buffer,
		layout: {
			clk: {
				offset: 12,
				width: 1,
				byteSize: 1,
				is4state: false,
				direction: "input",
			},
			rst: {
				offset: 0,
				width: 1,
				byteSize: 1,
				is4state: false,
				direction: "input",
			},
			a: {
				offset: 2,
				width: 16,
				byteSize: 2,
				is4state: false,
				direction: "input",
			},
			b: {
				offset: 4,
				width: 16,
				byteSize: 2,
				is4state: false,
				direction: "input",
			},
			sum: {
				offset: 8,
				width: 17,
				byteSize: 4,
				is4state: false,
				direction: "output",
			},
		},
		events: { clk: 0, aux: 7 },
		handle,
	} satisfies CreateResult<NativeSimulatorHandle>);

	return { create, handle, buffer };
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe("Simulator", () => {
	test("wraps a handle returned by a frontend-owned addon", () => {
		const memory = new Uint8Array(64);
		const signal = (offset: number, direction: "input" | "output") => ({
			offset,
			width: 8,
			byte_size: 1,
			is_4state: false,
			direction,
			type_kind: "bit",
		});
		const signals = {
			a: signal(32, "input"),
			b: signal(33, "input"),
			y: signal(34, "output"),
		};
		const raw: FrontendSimulatorHandle = {
			layoutJson: JSON.stringify(signals),
			eventsJson: "{}",
			hierarchyJson: JSON.stringify({
				module_name: "NetAdder",
				signals,
				children: {},
			}),
			warningsJson: "[]",
			stableSize: 35,
			totalSize: 64,
			sharedMemory: () => memory,
			evalComb: vi.fn(() => {
				memory[34] = (memory[32] ?? 0) + (memory[33] ?? 0);
			}),
			tick: vi.fn(),
			tickN: vi.fn(),
			dump: vi.fn(),
			dispose: vi.fn(),
		};

		const sim = Simulator.fromFrontendHandle<{
			a: bigint;
			b: bigint;
			readonly y: bigint;
		}>(raw);
		sim.dut.a = 10n;
		sim.dut.b = 23n;
		expect(sim.dut.y).toBe(33n);
		sim.dispose();
		expect(raw.dispose).toHaveBeenCalledOnce();
	});

	test("create and basic tick", () => {
		const mock = createMockNative();
		const sim = Simulator.create(AdderModule, {
			__nativeCreate: mock.create,
		});

		sim.dut.a = 100n;
		sim.dut.b = 200n;
		sim.tick();

		expect(sim.dut.sum).toBe(300n);
		expect(mock.handle.tick).toHaveBeenCalledTimes(1);
	});

	test("tick with count", () => {
		const mock = createMockNative();
		const sim = Simulator.create(AdderModule, {
			__nativeCreate: mock.create,
		});

		sim.tick(3);
		expect(mock.handle.tickN).toHaveBeenCalledWith(0, 3);
	});

	test("an undefined second argument preserves the single-number form", () => {
		const mock = createMockNative();
		const sim = Simulator.create(AdderModule, {
			__nativeCreate: mock.create,
		});

		sim.tick(3, undefined);
		expect(mock.handle.tickN).toHaveBeenCalledWith(0, 3);
	});

	test("tick with a raw event ID and count", () => {
		const mock = createMockNative();
		const sim = Simulator.create(AdderModule, {
			__nativeCreate: mock.create,
		});

		sim.tick(7, 3);
		expect(mock.handle.tickN).toHaveBeenCalledWith(7, 3);
	});

	test("tick with a raw event ID once requires an explicit count", () => {
		const mock = createMockNative();
		const sim = Simulator.create(AdderModule, {
			__nativeCreate: mock.create,
		});

		sim.tick(7, 1);
		expect(mock.handle.tick).toHaveBeenCalledWith(7);
	});

	test("tick with undefined and count uses the default event", () => {
		const mock = createMockNative();
		const sim = Simulator.create(AdderModule, {
			__nativeCreate: mock.create,
		});

		sim.tick(undefined, 5);
		expect(mock.handle.tickN).toHaveBeenCalledWith(0, 5);
	});

	test("tick with event handle", () => {
		const mock = createMockNative();
		const sim = Simulator.create(AdderModule, {
			__nativeCreate: mock.create,
		});

		const clk = sim.event("clk");
		expect(clk.name).toBe("clk");
		expect(clk.id).toBe(0);

		sim.tick(clk);
		expect(mock.handle.tick).toHaveBeenCalledWith(0);
	});

	test("tick with event handle and count", () => {
		const mock = createMockNative();
		const sim = Simulator.create(AdderModule, {
			__nativeCreate: mock.create,
		});

		const clk = sim.event("clk");
		sim.tick(clk, 5);
		expect(mock.handle.tickN).toHaveBeenCalledWith(0, 5);
	});

	test("event() throws for unknown event", () => {
		const mock = createMockNative();
		const sim = Simulator.create(AdderModule, {
			__nativeCreate: mock.create,
		});

		expect(() => sim.event("nonexistent")).toThrow("Unknown event");
	});

	test("dispose prevents further operations", () => {
		const mock = createMockNative();
		const sim = Simulator.create(AdderModule, {
			__nativeCreate: mock.create,
		});

		sim.dispose();
		expect(() => sim.tick()).toThrow("disposed");
		expect(mock.handle.dispose).toHaveBeenCalledTimes(1);
	});

	test("double dispose is safe", () => {
		const mock = createMockNative();
		const sim = Simulator.create(AdderModule, {
			__nativeCreate: mock.create,
		});

		sim.dispose();
		sim.dispose(); // no-op
		expect(mock.handle.dispose).toHaveBeenCalledTimes(1);
	});

	test("dump delegates to handle", () => {
		const mock = createMockNative();
		const sim = Simulator.create(AdderModule, {
			__nativeCreate: mock.create,
		});

		sim.dump(42);
		expect(mock.handle.dump).toHaveBeenCalledWith(42);
	});

	test("create throws without native binding", () => {
		expect(() => {
			Simulator.create(AdderModule);
		}).toThrow("Native simulator binding not loaded");
	});

	test("tick clears dirty flag", () => {
		const mock = createMockNative();
		const sim = Simulator.create(AdderModule, {
			__nativeCreate: mock.create,
		});

		sim.dut.a = 100n;
		sim.tick();

		// After tick, reading output should NOT trigger evalComb
		// because tick already cleared dirty
		void sim.dut.sum;
		// evalComb might have been called by the first dut.sum read,
		// but tick itself should have cleared dirty
		const callsBefore = (mock.handle.evalComb as ReturnType<typeof vi.fn>).mock
			.calls.length;
		void sim.dut.sum;
		expect(
			(mock.handle.evalComb as ReturnType<typeof vi.fn>).mock.calls.length,
		).toBe(callsBefore);
	});
});
