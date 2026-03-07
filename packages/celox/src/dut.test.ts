import { describe, expect, test, vi } from "vitest";
import { createDut, type DirtyState, readFourState } from "./dut.js";
import type { NativeSimulatorHandle, PortInfo, SignalLayout } from "./types.js";
import { FourState, X } from "./types.js";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function mockHandle(): NativeSimulatorHandle {
	return {
		tick: vi.fn(),
		tickN: vi.fn(),
		evalComb: vi.fn(),
		dump: vi.fn(),
		dispose: vi.fn(),
	};
}

function makeBuffer(size: number): SharedArrayBuffer {
	return new SharedArrayBuffer(size);
}

// ---------------------------------------------------------------------------
// Basic scalar read/write
// ---------------------------------------------------------------------------

describe("createDut — scalar ports", () => {
	test("write and read 8-bit input", () => {
		const buffer = makeBuffer(64);
		const layout: Record<string, SignalLayout> = {
			a: {
				offset: 0,
				width: 8,
				byteSize: 1,
				is4state: false,
				direction: "input",
			},
		};
		const ports: Record<string, PortInfo> = {
			a: { direction: "input", type: "logic", width: 8 },
		};
		const handle = mockHandle();
		const state: DirtyState = { dirty: false };

		const dut = createDut<{ a: bigint }>(buffer, layout, ports, handle, state);

		dut.a = 42n;
		expect(state.dirty).toBe(true);
		expect(dut.a).toBe(42n);
		// Reading an input doesn't trigger evalComb
		expect(handle.evalComb).not.toHaveBeenCalled();
	});

	test("write and read 16-bit input", () => {
		const buffer = makeBuffer(64);
		const layout: Record<string, SignalLayout> = {
			a: {
				offset: 0,
				width: 16,
				byteSize: 2,
				is4state: false,
				direction: "input",
			},
		};
		const ports: Record<string, PortInfo> = {
			a: { direction: "input", type: "logic", width: 16 },
		};
		const handle = mockHandle();
		const state: DirtyState = { dirty: false };

		const dut = createDut<{ a: bigint }>(buffer, layout, ports, handle, state);

		dut.a = 0xabcdn;
		expect(dut.a).toBe(0xabcdn);
	});

	test("write and read 32-bit input", () => {
		const buffer = makeBuffer(64);
		const layout: Record<string, SignalLayout> = {
			a: {
				offset: 0,
				width: 32,
				byteSize: 4,
				is4state: false,
				direction: "input",
			},
		};
		const ports: Record<string, PortInfo> = {
			a: { direction: "input", type: "logic", width: 32 },
		};
		const handle = mockHandle();
		const state: DirtyState = { dirty: false };

		const dut = createDut<{ a: bigint }>(buffer, layout, ports, handle, state);

		dut.a = 0xdead_beefn;
		expect(dut.a).toBe(0xdead_beefn);
	});

	test("write and read 48-bit value", () => {
		const buffer = makeBuffer(64);
		const layout: Record<string, SignalLayout> = {
			a: {
				offset: 0,
				width: 48,
				byteSize: 8,
				is4state: false,
				direction: "input",
			},
		};
		const ports: Record<string, PortInfo> = {
			a: { direction: "input", type: "logic", width: 48 },
		};
		const handle = mockHandle();
		const state: DirtyState = { dirty: false };

		const dut = createDut<{ a: bigint }>(buffer, layout, ports, handle, state);

		const val = 0x1234_5678_9abcn;
		dut.a = val;
		expect(dut.a).toBe(val);
	});

	test("write and read 64-bit BigInt value", () => {
		const buffer = makeBuffer(64);
		const layout: Record<string, SignalLayout> = {
			a: {
				offset: 0,
				width: 64,
				byteSize: 8,
				is4state: false,
				direction: "input",
			},
		};
		const ports: Record<string, PortInfo> = {
			a: { direction: "input", type: "logic", width: 64 },
		};
		const handle = mockHandle();
		const state: DirtyState = { dirty: false };

		const dut = createDut<{ a: bigint }>(buffer, layout, ports, handle, state);

		const val = 0xdead_beef_cafe_baben;
		(dut as any).a = val;
		expect(dut.a).toBe(val);
	});

	test("8-bit write masks to width", () => {
		const buffer = makeBuffer(64);
		const layout: Record<string, SignalLayout> = {
			a: {
				offset: 0,
				width: 4,
				byteSize: 1,
				is4state: false,
				direction: "input",
			},
		};
		const ports: Record<string, PortInfo> = {
			a: { direction: "input", type: "logic", width: 4 },
		};
		const handle = mockHandle();
		const state: DirtyState = { dirty: false };

		const dut = createDut<{ a: bigint }>(buffer, layout, ports, handle, state);

		dut.a = 0xffn; // Only lower 4 bits should be stored
		expect(dut.a).toBe(0x0fn);
	});
});

// ---------------------------------------------------------------------------
// Dirty tracking and evalComb
// ---------------------------------------------------------------------------

describe("createDut — dirty tracking", () => {
	test("reading output when dirty triggers evalComb", () => {
		const buffer = makeBuffer(64);
		const layout: Record<string, SignalLayout> = {
			a: {
				offset: 0,
				width: 16,
				byteSize: 2,
				is4state: false,
				direction: "input",
			},
			sum: {
				offset: 4,
				width: 17,
				byteSize: 4,
				is4state: false,
				direction: "output",
			},
		};
		const ports: Record<string, PortInfo> = {
			a: { direction: "input", type: "logic", width: 16 },
			sum: { direction: "output", type: "logic", width: 17 },
		};
		const handle = mockHandle();
		const state: DirtyState = { dirty: false };

		const dut = createDut<{ a: bigint; readonly sum: bigint }>(
			buffer,
			layout,
			ports,
			handle,
			state,
		);

		// Write input → dirty
		dut.a = 100n;
		expect(state.dirty).toBe(true);

		// Read output → evalComb should be called
		void dut.sum;
		expect(handle.evalComb).toHaveBeenCalledTimes(1);
		expect(state.dirty).toBe(false);
	});

	test("reading output when clean does NOT trigger evalComb", () => {
		const buffer = makeBuffer(64);
		const layout: Record<string, SignalLayout> = {
			sum: {
				offset: 0,
				width: 17,
				byteSize: 4,
				is4state: false,
				direction: "output",
			},
		};
		const ports: Record<string, PortInfo> = {
			sum: { direction: "output", type: "logic", width: 17 },
		};
		const handle = mockHandle();
		const state: DirtyState = { dirty: false };

		const dut = createDut<{ readonly sum: bigint }>(
			buffer,
			layout,
			ports,
			handle,
			state,
		);

		void dut.sum;
		expect(handle.evalComb).not.toHaveBeenCalled();
	});

	test("reading input does NOT trigger evalComb even when dirty", () => {
		const buffer = makeBuffer(64);
		const layout: Record<string, SignalLayout> = {
			a: {
				offset: 0,
				width: 8,
				byteSize: 1,
				is4state: false,
				direction: "input",
			},
		};
		const ports: Record<string, PortInfo> = {
			a: { direction: "input", type: "logic", width: 8 },
		};
		const handle = mockHandle();
		const state: DirtyState = { dirty: true };

		const dut = createDut<{ a: bigint }>(buffer, layout, ports, handle, state);

		void dut.a;
		expect(handle.evalComb).not.toHaveBeenCalled();
	});

	test("writing to output throws", () => {
		const buffer = makeBuffer(64);
		const layout: Record<string, SignalLayout> = {
			sum: {
				offset: 0,
				width: 17,
				byteSize: 4,
				is4state: false,
				direction: "output",
			},
		};
		const ports: Record<string, PortInfo> = {
			sum: { direction: "output", type: "logic", width: 17 },
		};
		const handle = mockHandle();
		const state: DirtyState = { dirty: false };

		const dut = createDut<{ sum: bigint }>(
			buffer,
			layout,
			ports,
			handle,
			state,
		);

		expect(() => {
			dut.sum = 42n;
		}).toThrow("Cannot write to output port 'sum'");
	});
});

// ---------------------------------------------------------------------------
// Clock port is hidden
// ---------------------------------------------------------------------------

describe("createDut — clock ports", () => {
	test("clock ports are not exposed on the DUT", () => {
		const buffer = makeBuffer(64);
		const layout: Record<string, SignalLayout> = {
			clk: {
				offset: 0,
				width: 1,
				byteSize: 1,
				is4state: false,
				direction: "input",
			},
			a: {
				offset: 1,
				width: 8,
				byteSize: 1,
				is4state: false,
				direction: "input",
			},
		};
		const ports: Record<string, PortInfo> = {
			clk: { direction: "input", type: "clock", width: 1 },
			a: { direction: "input", type: "logic", width: 8 },
		};
		const handle = mockHandle();
		const state: DirtyState = { dirty: false };

		const dut = createDut<{ a: bigint }>(buffer, layout, ports, handle, state);

		expect(Object.keys(dut as object)).toEqual(["a"]);
		expect((dut as any).clk).toBeUndefined();
	});
});

// ---------------------------------------------------------------------------
// Multiple signals at different offsets
// ---------------------------------------------------------------------------

describe("createDut — multiple signals", () => {
	test("Adder-like module with a, b, sum", () => {
		const buffer = makeBuffer(64);
		const layout: Record<string, SignalLayout> = {
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
		};
		const ports: Record<string, PortInfo> = {
			clk: { direction: "input", type: "clock", width: 1 },
			rst: { direction: "input", type: "reset", width: 1 },
			a: { direction: "input", type: "logic", width: 16 },
			b: { direction: "input", type: "logic", width: 16 },
			sum: { direction: "output", type: "logic", width: 17 },
		};
		const handle = mockHandle();
		// Simulate evalComb by writing result into buffer
		(handle.evalComb as ReturnType<typeof vi.fn>).mockImplementation(() => {
			const view = new DataView(buffer);
			const a = view.getUint16(2, true);
			const b = view.getUint16(4, true);
			view.setUint32(8, a + b, true);
		});

		const state: DirtyState = { dirty: false };
		const dut = createDut<{
			rst: bigint;
			a: bigint;
			b: bigint;
			readonly sum: bigint;
		}>(buffer, layout, ports, handle, state);

		dut.a = 100n;
		dut.b = 200n;
		// sum read triggers evalComb
		expect(dut.sum).toBe(300n);
		expect(handle.evalComb).toHaveBeenCalledTimes(1);

		// second read without changes → no evalComb
		expect(dut.sum).toBe(300n);
		expect(handle.evalComb).toHaveBeenCalledTimes(1);
	});
});

// ---------------------------------------------------------------------------
// 4-state support
// ---------------------------------------------------------------------------

describe("createDut — 4-state", () => {
	test("write X to a 4-state signal", () => {
		// 8-bit signal: 1 byte value + 1 byte mask = 2 bytes
		const buffer = makeBuffer(64);
		const layout: Record<string, SignalLayout> = {
			a: {
				offset: 0,
				width: 8,
				byteSize: 1,
				is4state: true,
				direction: "input",
			},
		};
		const ports: Record<string, PortInfo> = {
			a: { direction: "input", type: "logic", width: 8, is4state: true },
		};
		const handle = mockHandle();
		const state: DirtyState = { dirty: false };

		const dut = createDut<{ a: bigint }>(buffer, layout, ports, handle, state);

		(dut as any).a = X;
		// X = (v=1, m=1) per bit: value should be 0xFF, mask should be 0xFF
		const [value, mask] = readFourState(buffer, layout.a);
		expect(value).toBe(0xffn);
		expect(mask).toBe(0xffn);
	});

	test("write FourState to a 4-state signal", () => {
		const buffer = makeBuffer(64);
		const layout: Record<string, SignalLayout> = {
			a: {
				offset: 0,
				width: 8,
				byteSize: 1,
				is4state: true,
				direction: "input",
			},
		};
		const ports: Record<string, PortInfo> = {
			a: { direction: "input", type: "logic", width: 8, is4state: true },
		};
		const handle = mockHandle();
		const state: DirtyState = { dirty: false };

		const dut = createDut<{ a: bigint }>(buffer, layout, ports, handle, state);

		(dut as any).a = FourState(0b1010, 0b0100);
		const [value, mask] = readFourState(buffer, layout.a);
		expect(value).toBe(0b1010n);
		expect(mask).toBe(0b0100n);
	});

	test("writing X to non-4-state signal throws", () => {
		const buffer = makeBuffer(64);
		const layout: Record<string, SignalLayout> = {
			a: {
				offset: 0,
				width: 8,
				byteSize: 1,
				is4state: false,
				direction: "input",
			},
		};
		const ports: Record<string, PortInfo> = {
			a: { direction: "input", type: "logic", width: 8 },
		};
		const handle = mockHandle();
		const state: DirtyState = { dirty: false };

		const dut = createDut<{ a: bigint }>(buffer, layout, ports, handle, state);

		expect(() => {
			(dut as any).a = X;
		}).toThrow("not 4-state");
	});

	test("writing FourState to non-4-state signal throws", () => {
		const buffer = makeBuffer(64);
		const layout: Record<string, SignalLayout> = {
			a: {
				offset: 0,
				width: 8,
				byteSize: 1,
				is4state: false,
				direction: "input",
			},
		};
		const ports: Record<string, PortInfo> = {
			a: { direction: "input", type: "logic", width: 8 },
		};
		const handle = mockHandle();
		const state: DirtyState = { dirty: false };

		const dut = createDut<{ a: bigint }>(buffer, layout, ports, handle, state);

		expect(() => {
			(dut as any).a = FourState(0xa5, 0x0f);
		}).toThrow("not 4-state");
	});

	test("writing defined value to 4-state signal clears mask", () => {
		const buffer = makeBuffer(64);
		const layout: Record<string, SignalLayout> = {
			a: {
				offset: 0,
				width: 8,
				byteSize: 1,
				is4state: true,
				direction: "input",
			},
		};
		const ports: Record<string, PortInfo> = {
			a: { direction: "input", type: "logic", width: 8, is4state: true },
		};
		const handle = mockHandle();
		const state: DirtyState = { dirty: false };

		const dut = createDut<{ a: bigint }>(buffer, layout, ports, handle, state);

		// First write X
		(dut as any).a = X;
		const [, maskBefore] = readFourState(buffer, layout.a);
		expect(maskBefore).toBe(0xffn);

		// Then write a defined value — mask should clear
		dut.a = 42n;
		const [value, maskAfter] = readFourState(buffer, layout.a);
		expect(value).toBe(42n);
		expect(maskAfter).toBe(0n);
	});

	test("reading 4-state output returns value part only", () => {
		const buffer = makeBuffer(64);
		const view = new DataView(buffer);
		const layout: Record<string, SignalLayout> = {
			y: {
				offset: 0,
				width: 8,
				byteSize: 1,
				is4state: true,
				direction: "output",
			},
		};
		const ports: Record<string, PortInfo> = {
			y: { direction: "output", type: "logic", width: 8, is4state: true },
		};
		const handle = mockHandle();
		const state: DirtyState = { dirty: false };

		const dut = createDut<{ readonly y: bigint }>(
			buffer,
			layout,
			ports,
			handle,
			state,
		);

		// Set value=0xAB, mask=0x0F (lower 4 bits are X)
		view.setUint8(0, 0xab);
		view.setUint8(1, 0x0f);

		// DUT getter returns the value part
		expect(dut.y).toBe(0xabn);
	});

	test("write X sets dirty flag", () => {
		const buffer = makeBuffer(64);
		const layout: Record<string, SignalLayout> = {
			a: {
				offset: 0,
				width: 8,
				byteSize: 1,
				is4state: true,
				direction: "input",
			},
		};
		const ports: Record<string, PortInfo> = {
			a: { direction: "input", type: "logic", width: 8, is4state: true },
		};
		const handle = mockHandle();
		const state: DirtyState = { dirty: false };

		const dut = createDut<{ a: bigint }>(buffer, layout, ports, handle, state);

		(dut as any).a = X;
		expect(state.dirty).toBe(true);
	});
});

// ---------------------------------------------------------------------------
// Array ports
// ---------------------------------------------------------------------------

describe("createDut — array ports", () => {
	test("read/write array elements", () => {
		// 4 elements of 8 bits each = 4 bytes
		const buffer = makeBuffer(64);
		const layout: Record<string, SignalLayout> = {
			data: {
				offset: 0,
				width: 8,
				byteSize: 4,
				is4state: false,
				direction: "input",
			},
		};
		const ports: Record<string, PortInfo> = {
			data: { direction: "input", type: "logic", width: 8, arrayDims: [4] },
		};
		const handle = mockHandle();
		const state: DirtyState = { dirty: false };

		const dut = createDut<{
			data: {
				at(i: number): bigint;
				set(i: number, v: bigint): void;
				length: number;
			};
		}>(buffer, layout, ports, handle, state);

		dut.data.set(0, 0xaan);
		dut.data.set(1, 0xbbn);
		dut.data.set(2, 0xccn);
		dut.data.set(3, 0xddn);

		expect(dut.data.at(0)).toBe(0xaan);
		expect(dut.data.at(1)).toBe(0xbbn);
		expect(dut.data.at(2)).toBe(0xccn);
		expect(dut.data.at(3)).toBe(0xddn);
		expect(dut.data.length).toBe(4);
	});

	test("1-bit elements (logic [N]): bit-packed read/write", () => {
		// logic[4]: 4 elements of 1 bit each = 1 byte total in native memory
		const buffer = makeBuffer(64);
		const layout: Record<string, SignalLayout> = {
			push: {
				offset: 0,
				width: 1,
				byteSize: 1,
				is4state: false,
				direction: "input",
			},
		};
		const ports: Record<string, PortInfo> = {
			push: { direction: "input", type: "logic", width: 1, arrayDims: [4] },
		};
		const handle = mockHandle();
		const state: DirtyState = { dirty: false };

		const dut = createDut<{
			push: {
				at(i: number): bigint;
				set(i: number, v: bigint): void;
				length: number;
			};
		}>(buffer, layout, ports, handle, state);

		dut.push.set(0, 1n);
		dut.push.set(1, 1n);
		dut.push.set(2, 0n);
		dut.push.set(3, 1n);

		expect(dut.push.at(0)).toBe(1n);
		expect(dut.push.at(1)).toBe(1n);
		expect(dut.push.at(2)).toBe(0n);
		expect(dut.push.at(3)).toBe(1n);
		expect(dut.push.length).toBe(4);

		// Verify native layout: all 4 elements packed in byte 0
		const view = new DataView(buffer);
		expect(view.getUint8(0)).toBe(0b1011); // bits 0,1,3 set = 0x0B
		expect(view.getUint8(1)).toBe(0); // no overflow into byte 1
	});

	test("1-bit elements: writes don't corrupt adjacent elements", () => {
		const buffer = makeBuffer(64);
		const layout: Record<string, SignalLayout> = {
			bits: {
				offset: 0,
				width: 1,
				byteSize: 1,
				is4state: false,
				direction: "input",
			},
		};
		const ports: Record<string, PortInfo> = {
			bits: { direction: "input", type: "logic", width: 1, arrayDims: [8] },
		};
		const handle = mockHandle();
		const state: DirtyState = { dirty: false };

		const dut = createDut<{
			bits: { at(i: number): bigint; set(i: number, v: bigint): void };
		}>(buffer, layout, ports, handle, state);

		// Set element 0; verify element 1 unaffected
		dut.bits.set(0, 1n);
		expect(dut.bits.at(0)).toBe(1n);
		dut.bits.set(1, 1n);
		expect(dut.bits.at(0)).toBe(1n); // element 0 must not be corrupted
		expect(dut.bits.at(1)).toBe(1n);

		// Clear element 0; element 1 must remain set
		dut.bits.set(0, 0n);
		expect(dut.bits.at(0)).toBe(0n);
		expect(dut.bits.at(1)).toBe(1n);
	});

	test("1-bit elements: 8 elements span exactly one byte", () => {
		const buffer = makeBuffer(64);
		const layout: Record<string, SignalLayout> = {
			bits: {
				offset: 2,
				width: 1,
				byteSize: 1,
				is4state: false,
				direction: "input",
			},
		};
		const ports: Record<string, PortInfo> = {
			bits: { direction: "input", type: "logic", width: 1, arrayDims: [8] },
		};
		const handle = mockHandle();
		const state: DirtyState = { dirty: false };

		const dut = createDut<{
			bits: { at(i: number): bigint; set(i: number, v: bigint): void };
		}>(buffer, layout, ports, handle, state);

		// Set all 8 bits
		for (let i = 0; i < 8; i++) dut.bits.set(i, 1n);
		const view = new DataView(buffer);
		expect(view.getUint8(2)).toBe(0xff);

		// Clear alternating bits
		for (let i = 0; i < 8; i += 2) dut.bits.set(i, 0n);
		expect(view.getUint8(2)).toBe(0b10101010);
	});

	test("elementWidth=3: elements spanning byte boundaries read/write correctly", () => {
		// logic<3>[6]: 6 × 3 = 18 bits = 3 bytes
		// Element 2: bitStart=6, spans bytes 0→1  ← cross-byte write/read path
		// Element 5: bitStart=15, spans bytes 1→2 ← cross-byte write/read path
		const buffer = makeBuffer(64);
		const layout: Record<string, SignalLayout> = {
			data: {
				offset: 0,
				width: 3,
				byteSize: 1,
				is4state: false,
				direction: "input",
			},
		};
		const ports: Record<string, PortInfo> = {
			data: { direction: "input", type: "logic", width: 3, arrayDims: [6] },
		};
		const handle = mockHandle();
		const state: DirtyState = { dirty: false };

		const dut = createDut<{
			data: {
				at(i: number): bigint;
				set(i: number, v: bigint): void;
				length: number;
			};
		}>(buffer, layout, ports, handle, state);

		// Set all 6 elements to distinct values 1..6
		for (let i = 0; i < 6; i++) dut.data.set(i, BigInt(i + 1));
		for (let i = 0; i < 6; i++) expect(dut.data.at(i)).toBe(BigInt(i + 1));

		// Overwrite the two cross-byte elements and verify neighbours are unaffected
		dut.data.set(2, 0b101n); // bitStart=6, crosses byte 0→1
		expect(dut.data.at(2)).toBe(5n);
		expect(dut.data.at(1)).toBe(2n); // neighbour below
		expect(dut.data.at(3)).toBe(4n); // neighbour above

		dut.data.set(5, 0b110n); // bitStart=15, crosses byte 1→2
		expect(dut.data.at(5)).toBe(6n);
		expect(dut.data.at(4)).toBe(5n); // neighbour below
	});

	test("1-bit 4-state: X writes value=0 and mask=1 at correct byte offset", () => {
		// logic[8] with 4-state: value in byte 0 (bits 0-7 = elements 0-7),
		// mask in byte 1 (bits 0-7 = masks for elements 0-7).
		// totalValueBytes = ceil(8*1/8) = 1, so maskBase = offset + 1.
		const buffer = makeBuffer(64);
		const layout: Record<string, SignalLayout> = {
			bits: {
				offset: 0,
				width: 1,
				byteSize: 1,
				is4state: true,
				direction: "input",
			},
		};
		const ports: Record<string, PortInfo> = {
			bits: {
				direction: "input",
				type: "logic",
				width: 1,
				arrayDims: [8],
				is4state: true,
			},
		};
		const handle = mockHandle();
		const state: DirtyState = { dirty: false };

		const dut = createDut<{
			bits: { at(i: number): bigint; set(i: number, v: unknown): void };
		}>(buffer, layout, ports, handle, state);

		const view = new DataView(buffer);

		// Assign X to element 3: X = (v=1, m=1), so value bit 3 → 1, mask bit 3 → 1
		dut.bits.set(3, X);
		expect(view.getUint8(0) & 0b00001000).toBe(0b00001000); // value: bit 3 = 1
		expect(view.getUint8(1) & 0b00001000).toBe(0b00001000); // mask:  bit 3 = 1

		// Write a defined 1 to element 3: value bit 3 → 1, mask bit 3 → 0
		dut.bits.set(3, 1n);
		expect(view.getUint8(0) & 0b00001000).toBe(0b00001000); // value: bit 3 = 1
		expect(view.getUint8(1) & 0b00001000).toBe(0); // mask:  bit 3 cleared

		// Setting elements 0 and 7 to X should only affect those mask bits
		dut.bits.set(0, X);
		dut.bits.set(7, X);
		expect(view.getUint8(1)).toBe(0b10000001); // mask: bits 0 and 7 set
	});

	test("3-bit 4-state: mask region starts at ceil(totalBits/8) bytes after value", () => {
		// logic<3>[5]: 5 × 3 = 15 bits → totalValueBytes = ceil(15/8) = 2
		// value occupies bytes 0-1, mask occupies bytes 2-3; maskBase = offset + 2
		const buffer = makeBuffer(64);
		const layout: Record<string, SignalLayout> = {
			data: {
				offset: 0,
				width: 3,
				byteSize: 1,
				is4state: true,
				direction: "input",
			},
		};
		const ports: Record<string, PortInfo> = {
			data: {
				direction: "input",
				type: "logic",
				width: 3,
				arrayDims: [5],
				is4state: true,
			},
		};
		const handle = mockHandle();
		const state: DirtyState = { dirty: false };

		const dut = createDut<{ data: { set(i: number, v: unknown): void } }>(
			buffer,
			layout,
			ports,
			handle,
			state,
		);

		const view = new DataView(buffer);

		// Assign X to element 0 (bitStart=0, no cross-byte): X = (v=1, m=1), value bits 0-2 = 0b111, mask bits 0-2 = 0b111
		dut.data.set(0, X);
		expect(view.getUint8(0) & 0b111).toBe(0b111); // value byte 0: bits 0-2 set (X encoding)
		expect(view.getUint8(2) & 0b111).toBe(0b111); // mask byte 2: bits 0-2 set
		expect(view.getUint8(1)).toBe(0); // value byte 1: untouched
		expect(view.getUint8(3)).toBe(0); // mask byte 3: untouched

		// Write a defined value to element 0, verify mask cleared
		dut.data.set(0, 0b110n);
		expect(view.getUint8(0) & 0b111).toBe(0b110); // value bits 0-2 = 6
		expect(view.getUint8(2) & 0b111).toBe(0); // mask bits 0-2 cleared
	});

	test("8-bit 4-state: mask region after all values, not per-element", () => {
		// logic<8>[4] with 4-state:
		//   value bytes 0-3 (4 elements × 1 byte each)
		//   mask  bytes 4-7 (4 elements × 1 byte each)
		const buffer = makeBuffer(64);
		const layout: Record<string, SignalLayout> = {
			data: {
				offset: 0,
				width: 8,
				byteSize: 1,
				is4state: true,
				direction: "input",
			},
		};
		const ports: Record<string, PortInfo> = {
			data: {
				direction: "input",
				type: "logic",
				width: 8,
				arrayDims: [4],
				is4state: true,
			},
		};
		const handle = mockHandle();
		const state: DirtyState = { dirty: false };

		const dut = createDut<{
			data: {
				at(i: number): bigint;
				set(i: number, v: bigint | symbol): void;
			};
		}>(buffer, layout, ports, handle, state);

		const view = new DataView(buffer);

		// Write defined values to all 4 elements
		dut.data.set(0, 0xaan);
		dut.data.set(1, 0xbbn);
		dut.data.set(2, 0xccn);
		dut.data.set(3, 0xddn);

		// Verify values are correct (not corrupted by mask writes)
		expect(view.getUint8(0)).toBe(0xaa); // value[0]
		expect(view.getUint8(1)).toBe(0xbb); // value[1]
		expect(view.getUint8(2)).toBe(0xcc); // value[2]
		expect(view.getUint8(3)).toBe(0xdd); // value[3]

		// Verify masks are in the correct region (bytes 4-7), all cleared
		expect(view.getUint8(4)).toBe(0); // mask[0]
		expect(view.getUint8(5)).toBe(0); // mask[1]
		expect(view.getUint8(6)).toBe(0); // mask[2]
		expect(view.getUint8(7)).toBe(0); // mask[3]

		// Read back via at()
		expect(dut.data.at(0)).toBe(0xaan);
		expect(dut.data.at(1)).toBe(0xbbn);
		expect(dut.data.at(2)).toBe(0xccn);
		expect(dut.data.at(3)).toBe(0xddn);
	});

	test("8-bit 4-state: X writes mask to correct offset, not adjacent element", () => {
		// logic<8>[4] with 4-state: values at 0-3, masks at 4-7
		const buffer = makeBuffer(64);
		const layout: Record<string, SignalLayout> = {
			data: {
				offset: 0,
				width: 8,
				byteSize: 1,
				is4state: true,
				direction: "input",
			},
		};
		const ports: Record<string, PortInfo> = {
			data: {
				direction: "input",
				type: "logic",
				width: 8,
				arrayDims: [4],
				is4state: true,
			},
		};
		const handle = mockHandle();
		const state: DirtyState = { dirty: false };

		const dut = createDut<{
			data: {
				at(i: number): bigint;
				set(i: number, v: bigint | symbol): void;
			};
		}>(buffer, layout, ports, handle, state);

		const view = new DataView(buffer);

		// Set element 1 to a defined value first
		dut.data.set(1, 0x42n);
		expect(view.getUint8(1)).toBe(0x42); // value[1]
		expect(view.getUint8(5)).toBe(0);    // mask[1] cleared

		// Set element 0 to X — must NOT corrupt element 1's value
		dut.data.set(0, X);
		expect(view.getUint8(0)).toBe(0xff); // value[0] = all-ones (X)
		expect(view.getUint8(4)).toBe(0xff); // mask[0] = all-ones (X)
		expect(view.getUint8(1)).toBe(0x42); // value[1] must be preserved
	});

	test("32-bit 4-state: mask region after all values", () => {
		// logic<32>[2] with 4-state:
		//   value bytes 0-7 (2 elements × 4 bytes each)
		//   mask  bytes 8-15 (2 elements × 4 bytes each)
		const buffer = makeBuffer(64);
		const layout: Record<string, SignalLayout> = {
			data: {
				offset: 0,
				width: 32,
				byteSize: 4,
				is4state: true,
				direction: "input",
			},
		};
		const ports: Record<string, PortInfo> = {
			data: {
				direction: "input",
				type: "logic",
				width: 32,
				arrayDims: [2],
				is4state: true,
			},
		};
		const handle = mockHandle();
		const state: DirtyState = { dirty: false };

		const dut = createDut<{
			data: {
				at(i: number): bigint;
				set(i: number, v: bigint | symbol): void;
			};
		}>(buffer, layout, ports, handle, state);

		const view = new DataView(buffer);

		// Write defined values
		dut.data.set(0, 0xdeadbeefn);
		dut.data.set(1, 0xcafebaben);

		// Values at correct positions
		expect(view.getUint32(0, true)).toBe(0xdeadbeef); // value[0]
		expect(view.getUint32(4, true)).toBe(0xcafebabe); // value[1]

		// Masks at bytes 8-15
		expect(view.getUint32(8, true)).toBe(0);  // mask[0]
		expect(view.getUint32(12, true)).toBe(0); // mask[1]

		// Read back
		expect(dut.data.at(0)).toBe(0xdeadbeefn);
		expect(dut.data.at(1)).toBe(0xcafebaben);

		// Set element 0 to X, verify element 1 is not corrupted
		dut.data.set(0, X);
		expect(view.getUint32(0, true)).toBe(0xffffffff);  // value[0] = X
		expect(view.getUint32(8, true)).toBe(0xffffffff);  // mask[0] = X
		expect(view.getUint32(4, true)).toBe(0xcafebabe);  // value[1] preserved
		expect(view.getUint32(12, true)).toBe(0);           // mask[1] unchanged
	});
});

// ---------------------------------------------------------------------------
// Interface (nested) ports
// ---------------------------------------------------------------------------

describe("createDut — interface ports with array members", () => {
	test("interface member with arrayDims uses array accessor", () => {
		// Interface array pattern: bus: modport Bus::consumer [2]
		// Veryl expands to bus.data with arrayDims:[2], bus.valid with arrayDims:[2]
		const buffer = makeBuffer(64);
		const layout: Record<string, SignalLayout> = {
			"bus.data": {
				offset: 0,
				width: 8,
				byteSize: 2,
				is4state: false,
				direction: "input",
			},
			"bus.valid": {
				offset: 2,
				width: 1,
				byteSize: 1,
				is4state: false,
				direction: "input",
			},
		};
		const ports: Record<string, PortInfo> = {
			bus: {
				direction: "inout",
				type: "logic",
				width: 0,
				interface: {
					data: { direction: "input", type: "logic", width: 8, arrayDims: [2] },
					valid: {
						direction: "input",
						type: "logic",
						width: 1,
						arrayDims: [2],
					},
				},
			},
		};
		const handle = mockHandle();
		const state: DirtyState = { dirty: false };

		const dut = createDut<{
			bus: {
				data: {
					at(i: number): bigint;
					set(i: number, v: bigint): void;
					length: number;
				};
				valid: {
					at(i: number): bigint;
					set(i: number, v: bigint): void;
					length: number;
				};
			};
		}>(buffer, layout, ports, handle, state);

		// data: 2 elements of 8 bits each
		dut.bus.data.set(0, 0xaan);
		dut.bus.data.set(1, 0xbbn);
		expect(dut.bus.data.at(0)).toBe(0xaan);
		expect(dut.bus.data.at(1)).toBe(0xbbn);
		expect(dut.bus.data.length).toBe(2);

		// valid: 2 elements of 1 bit each (bit-packed)
		dut.bus.valid.set(0, 1n);
		dut.bus.valid.set(1, 0n);
		expect(dut.bus.valid.at(0)).toBe(1n);
		expect(dut.bus.valid.at(1)).toBe(0n);
		expect(dut.bus.valid.length).toBe(2);
	});

	test("interface with mixed scalar and array members", () => {
		const buffer = makeBuffer(64);
		const layout: Record<string, SignalLayout> = {
			"bus.data": {
				offset: 0,
				width: 8,
				byteSize: 2,
				is4state: false,
				direction: "input",
			},
			"bus.valid": {
				offset: 4,
				width: 1,
				byteSize: 1,
				is4state: false,
				direction: "input",
			},
		};
		const ports: Record<string, PortInfo> = {
			bus: {
				direction: "inout",
				type: "logic",
				width: 0,
				interface: {
					data: { direction: "input", type: "logic", width: 8, arrayDims: [2] },
					valid: { direction: "input", type: "logic", width: 1 },
				},
			},
		};
		const handle = mockHandle();
		const state: DirtyState = { dirty: false };

		const dut = createDut<{
			bus: {
				data: {
					at(i: number): bigint;
					set(i: number, v: bigint): void;
					length: number;
				};
				valid: bigint;
			};
		}>(buffer, layout, ports, handle, state);

		// Array member
		dut.bus.data.set(0, 0x11n);
		dut.bus.data.set(1, 0x22n);
		expect(dut.bus.data.at(0)).toBe(0x11n);
		expect(dut.bus.data.at(1)).toBe(0x22n);

		// Scalar member
		dut.bus.valid = 1n;
		expect(dut.bus.valid).toBe(1n);
	});
});

describe("createDut — interface ports", () => {
	test("nested interface members", () => {
		const buffer = makeBuffer(64);
		const layout: Record<string, SignalLayout> = {
			"bus.addr": {
				offset: 0,
				width: 32,
				byteSize: 4,
				is4state: false,
				direction: "input",
			},
			"bus.data": {
				offset: 4,
				width: 32,
				byteSize: 4,
				is4state: false,
				direction: "input",
			},
			"bus.valid": {
				offset: 8,
				width: 1,
				byteSize: 1,
				is4state: false,
				direction: "input",
			},
			"bus.ready": {
				offset: 9,
				width: 1,
				byteSize: 1,
				is4state: false,
				direction: "output",
			},
		};
		const ports: Record<string, PortInfo> = {
			bus: {
				direction: "input",
				type: "logic",
				width: 0,
				interface: {
					addr: { direction: "input", type: "logic", width: 32 },
					data: { direction: "input", type: "logic", width: 32 },
					valid: { direction: "input", type: "logic", width: 1 },
					ready: { direction: "output", type: "logic", width: 1 },
				},
			},
		};
		const handle = mockHandle();
		(handle.evalComb as ReturnType<typeof vi.fn>).mockImplementation(() => {
			const view = new DataView(buffer);
			// mock: ready = valid
			view.setUint8(9, view.getUint8(8));
		});

		const state: DirtyState = { dirty: false };
		const dut = createDut<{
			bus: {
				addr: bigint;
				data: bigint;
				valid: bigint;
				readonly ready: bigint;
			};
		}>(buffer, layout, ports, handle, state);

		dut.bus.addr = 0x1000n;
		dut.bus.data = 0xffn;
		dut.bus.valid = 1n;

		expect(dut.bus.addr).toBe(0x1000n);
		expect(dut.bus.data).toBe(0xffn);
		expect(dut.bus.ready).toBe(1n);
		expect(handle.evalComb).toHaveBeenCalledTimes(1);
	});
});
