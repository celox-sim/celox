import { describe, test, expect, vi } from "vitest";
import { createDut, readFourState, type DirtyState } from "./dut.js";
import type {
  NativeSimulatorHandle,
  PortInfo,
  SignalLayout,
} from "./types.js";
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
      a: { offset: 0, width: 8, byteSize: 1, is4state: false, direction: "input" },
    };
    const ports: Record<string, PortInfo> = {
      a: { direction: "input", type: "logic", width: 8 },
    };
    const handle = mockHandle();
    const state: DirtyState = { dirty: false };

    const dut = createDut<{ a: number }>(buffer, layout, ports, handle, state);

    dut.a = 42;
    expect(state.dirty).toBe(true);
    expect(dut.a).toBe(42);
    // Reading an input doesn't trigger evalComb
    expect(handle.evalComb).not.toHaveBeenCalled();
  });

  test("write and read 16-bit input", () => {
    const buffer = makeBuffer(64);
    const layout: Record<string, SignalLayout> = {
      a: { offset: 0, width: 16, byteSize: 2, is4state: false, direction: "input" },
    };
    const ports: Record<string, PortInfo> = {
      a: { direction: "input", type: "logic", width: 16 },
    };
    const handle = mockHandle();
    const state: DirtyState = { dirty: false };

    const dut = createDut<{ a: number }>(buffer, layout, ports, handle, state);

    dut.a = 0xABCD;
    expect(dut.a).toBe(0xABCD);
  });

  test("write and read 32-bit input", () => {
    const buffer = makeBuffer(64);
    const layout: Record<string, SignalLayout> = {
      a: { offset: 0, width: 32, byteSize: 4, is4state: false, direction: "input" },
    };
    const ports: Record<string, PortInfo> = {
      a: { direction: "input", type: "logic", width: 32 },
    };
    const handle = mockHandle();
    const state: DirtyState = { dirty: false };

    const dut = createDut<{ a: number }>(buffer, layout, ports, handle, state);

    dut.a = 0xDEAD_BEEF;
    expect(dut.a).toBe(0xDEAD_BEEF);
  });

  test("write and read 48-bit value (fits in number)", () => {
    const buffer = makeBuffer(64);
    const layout: Record<string, SignalLayout> = {
      a: { offset: 0, width: 48, byteSize: 8, is4state: false, direction: "input" },
    };
    const ports: Record<string, PortInfo> = {
      a: { direction: "input", type: "logic", width: 48 },
    };
    const handle = mockHandle();
    const state: DirtyState = { dirty: false };

    const dut = createDut<{ a: number }>(buffer, layout, ports, handle, state);

    const val = 0x1234_5678_9ABC;
    dut.a = val;
    expect(dut.a).toBe(val);
  });

  test("write and read 64-bit BigInt value", () => {
    const buffer = makeBuffer(64);
    const layout: Record<string, SignalLayout> = {
      a: { offset: 0, width: 64, byteSize: 8, is4state: false, direction: "input" },
    };
    const ports: Record<string, PortInfo> = {
      a: { direction: "input", type: "logic", width: 64 },
    };
    const handle = mockHandle();
    const state: DirtyState = { dirty: false };

    const dut = createDut<{ a: bigint }>(buffer, layout, ports, handle, state);

    const val = 0xDEAD_BEEF_CAFE_BABEn;
    (dut as any).a = val;
    expect(dut.a).toBe(val);
  });

  test("8-bit write masks to width", () => {
    const buffer = makeBuffer(64);
    const layout: Record<string, SignalLayout> = {
      a: { offset: 0, width: 4, byteSize: 1, is4state: false, direction: "input" },
    };
    const ports: Record<string, PortInfo> = {
      a: { direction: "input", type: "logic", width: 4 },
    };
    const handle = mockHandle();
    const state: DirtyState = { dirty: false };

    const dut = createDut<{ a: number }>(buffer, layout, ports, handle, state);

    dut.a = 0xFF; // Only lower 4 bits should be stored
    expect(dut.a).toBe(0x0F);
  });
});

// ---------------------------------------------------------------------------
// Dirty tracking and evalComb
// ---------------------------------------------------------------------------

describe("createDut — dirty tracking", () => {
  test("reading output when dirty triggers evalComb", () => {
    const buffer = makeBuffer(64);
    const layout: Record<string, SignalLayout> = {
      a: { offset: 0, width: 16, byteSize: 2, is4state: false, direction: "input" },
      sum: { offset: 4, width: 17, byteSize: 4, is4state: false, direction: "output" },
    };
    const ports: Record<string, PortInfo> = {
      a: { direction: "input", type: "logic", width: 16 },
      sum: { direction: "output", type: "logic", width: 17 },
    };
    const handle = mockHandle();
    const state: DirtyState = { dirty: false };

    const dut = createDut<{ a: number; readonly sum: number }>(
      buffer, layout, ports, handle, state,
    );

    // Write input → dirty
    dut.a = 100;
    expect(state.dirty).toBe(true);

    // Read output → evalComb should be called
    void dut.sum;
    expect(handle.evalComb).toHaveBeenCalledTimes(1);
    expect(state.dirty).toBe(false);
  });

  test("reading output when clean does NOT trigger evalComb", () => {
    const buffer = makeBuffer(64);
    const layout: Record<string, SignalLayout> = {
      sum: { offset: 0, width: 17, byteSize: 4, is4state: false, direction: "output" },
    };
    const ports: Record<string, PortInfo> = {
      sum: { direction: "output", type: "logic", width: 17 },
    };
    const handle = mockHandle();
    const state: DirtyState = { dirty: false };

    const dut = createDut<{ readonly sum: number }>(
      buffer, layout, ports, handle, state,
    );

    void dut.sum;
    expect(handle.evalComb).not.toHaveBeenCalled();
  });

  test("reading input does NOT trigger evalComb even when dirty", () => {
    const buffer = makeBuffer(64);
    const layout: Record<string, SignalLayout> = {
      a: { offset: 0, width: 8, byteSize: 1, is4state: false, direction: "input" },
    };
    const ports: Record<string, PortInfo> = {
      a: { direction: "input", type: "logic", width: 8 },
    };
    const handle = mockHandle();
    const state: DirtyState = { dirty: true };

    const dut = createDut<{ a: number }>(buffer, layout, ports, handle, state);

    void dut.a;
    expect(handle.evalComb).not.toHaveBeenCalled();
  });

  test("writing to output throws", () => {
    const buffer = makeBuffer(64);
    const layout: Record<string, SignalLayout> = {
      sum: { offset: 0, width: 17, byteSize: 4, is4state: false, direction: "output" },
    };
    const ports: Record<string, PortInfo> = {
      sum: { direction: "output", type: "logic", width: 17 },
    };
    const handle = mockHandle();
    const state: DirtyState = { dirty: false };

    const dut = createDut<{ sum: number }>(buffer, layout, ports, handle, state);

    expect(() => {
      dut.sum = 42;
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
      clk: { offset: 0, width: 1, byteSize: 1, is4state: false, direction: "input" },
      a: { offset: 1, width: 8, byteSize: 1, is4state: false, direction: "input" },
    };
    const ports: Record<string, PortInfo> = {
      clk: { direction: "input", type: "clock", width: 1 },
      a: { direction: "input", type: "logic", width: 8 },
    };
    const handle = mockHandle();
    const state: DirtyState = { dirty: false };

    const dut = createDut<{ a: number }>(buffer, layout, ports, handle, state);

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
      rst: { offset: 0, width: 1, byteSize: 1, is4state: false, direction: "input" },
      a:   { offset: 2, width: 16, byteSize: 2, is4state: false, direction: "input" },
      b:   { offset: 4, width: 16, byteSize: 2, is4state: false, direction: "input" },
      sum: { offset: 8, width: 17, byteSize: 4, is4state: false, direction: "output" },
    };
    const ports: Record<string, PortInfo> = {
      clk: { direction: "input", type: "clock", width: 1 },
      rst: { direction: "input", type: "reset", width: 1 },
      a:   { direction: "input", type: "logic", width: 16 },
      b:   { direction: "input", type: "logic", width: 16 },
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
      rst: number;
      a: number;
      b: number;
      readonly sum: number;
    }>(buffer, layout, ports, handle, state);

    dut.a = 100;
    dut.b = 200;
    // sum read triggers evalComb
    expect(dut.sum).toBe(300);
    expect(handle.evalComb).toHaveBeenCalledTimes(1);

    // second read without changes → no evalComb
    expect(dut.sum).toBe(300);
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
      a: { offset: 0, width: 8, byteSize: 1, is4state: true, direction: "input" },
    };
    const ports: Record<string, PortInfo> = {
      a: { direction: "input", type: "logic", width: 8, is4state: true },
    };
    const handle = mockHandle();
    const state: DirtyState = { dirty: false };

    const dut = createDut<{ a: number }>(buffer, layout, ports, handle, state);

    (dut as any).a = X;
    // Value should be 0, mask should be 0xFF
    const [value, mask] = readFourState(buffer, layout.a);
    expect(value).toBe(0);
    expect(mask).toBe(0xFF);
  });

  test("write FourState to a 4-state signal", () => {
    const buffer = makeBuffer(64);
    const layout: Record<string, SignalLayout> = {
      a: { offset: 0, width: 8, byteSize: 1, is4state: true, direction: "input" },
    };
    const ports: Record<string, PortInfo> = {
      a: { direction: "input", type: "logic", width: 8, is4state: true },
    };
    const handle = mockHandle();
    const state: DirtyState = { dirty: false };

    const dut = createDut<{ a: number }>(buffer, layout, ports, handle, state);

    (dut as any).a = FourState(0b1010, 0b0100);
    const [value, mask] = readFourState(buffer, layout.a);
    expect(value).toBe(0b1010);
    expect(mask).toBe(0b0100);
  });

  test("writing X to non-4-state signal throws", () => {
    const buffer = makeBuffer(64);
    const layout: Record<string, SignalLayout> = {
      a: { offset: 0, width: 8, byteSize: 1, is4state: false, direction: "input" },
    };
    const ports: Record<string, PortInfo> = {
      a: { direction: "input", type: "logic", width: 8 },
    };
    const handle = mockHandle();
    const state: DirtyState = { dirty: false };

    const dut = createDut<{ a: number }>(buffer, layout, ports, handle, state);

    expect(() => {
      (dut as any).a = X;
    }).toThrow("not 4-state");
  });

  test("writing FourState to non-4-state signal throws", () => {
    const buffer = makeBuffer(64);
    const layout: Record<string, SignalLayout> = {
      a: { offset: 0, width: 8, byteSize: 1, is4state: false, direction: "input" },
    };
    const ports: Record<string, PortInfo> = {
      a: { direction: "input", type: "logic", width: 8 },
    };
    const handle = mockHandle();
    const state: DirtyState = { dirty: false };

    const dut = createDut<{ a: number }>(buffer, layout, ports, handle, state);

    expect(() => {
      (dut as any).a = FourState(0xA5, 0x0F);
    }).toThrow("not 4-state");
  });

  test("writing defined value to 4-state signal clears mask", () => {
    const buffer = makeBuffer(64);
    const layout: Record<string, SignalLayout> = {
      a: { offset: 0, width: 8, byteSize: 1, is4state: true, direction: "input" },
    };
    const ports: Record<string, PortInfo> = {
      a: { direction: "input", type: "logic", width: 8, is4state: true },
    };
    const handle = mockHandle();
    const state: DirtyState = { dirty: false };

    const dut = createDut<{ a: number }>(buffer, layout, ports, handle, state);

    // First write X
    (dut as any).a = X;
    const [, maskBefore] = readFourState(buffer, layout.a);
    expect(maskBefore).toBe(0xFF);

    // Then write a defined value — mask should clear
    dut.a = 42;
    const [value, maskAfter] = readFourState(buffer, layout.a);
    expect(value).toBe(42);
    expect(maskAfter).toBe(0);
  });

  test("reading 4-state output returns value part only", () => {
    const buffer = makeBuffer(64);
    const view = new DataView(buffer);
    const layout: Record<string, SignalLayout> = {
      y: { offset: 0, width: 8, byteSize: 1, is4state: true, direction: "output" },
    };
    const ports: Record<string, PortInfo> = {
      y: { direction: "output", type: "logic", width: 8, is4state: true },
    };
    const handle = mockHandle();
    const state: DirtyState = { dirty: false };

    const dut = createDut<{ readonly y: number }>(buffer, layout, ports, handle, state);

    // Set value=0xAB, mask=0x0F (lower 4 bits are X)
    view.setUint8(0, 0xAB);
    view.setUint8(1, 0x0F);

    // DUT getter returns the value part
    expect(dut.y).toBe(0xAB);
  });

  test("write X sets dirty flag", () => {
    const buffer = makeBuffer(64);
    const layout: Record<string, SignalLayout> = {
      a: { offset: 0, width: 8, byteSize: 1, is4state: true, direction: "input" },
    };
    const ports: Record<string, PortInfo> = {
      a: { direction: "input", type: "logic", width: 8, is4state: true },
    };
    const handle = mockHandle();
    const state: DirtyState = { dirty: false };

    const dut = createDut<{ a: number }>(buffer, layout, ports, handle, state);

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
      data: { offset: 0, width: 8, byteSize: 4, is4state: false, direction: "input" },
    };
    const ports: Record<string, PortInfo> = {
      data: { direction: "input", type: "logic", width: 8, arrayDims: [4] },
    };
    const handle = mockHandle();
    const state: DirtyState = { dirty: false };

    const dut = createDut<{ data: number[] }>(
      buffer, layout, ports, handle, state,
    );

    (dut.data as any)[0] = 0xAA;
    (dut.data as any)[1] = 0xBB;
    (dut.data as any)[2] = 0xCC;
    (dut.data as any)[3] = 0xDD;

    expect((dut.data as any)[0]).toBe(0xAA);
    expect((dut.data as any)[1]).toBe(0xBB);
    expect((dut.data as any)[2]).toBe(0xCC);
    expect((dut.data as any)[3]).toBe(0xDD);
    expect((dut.data as any).length).toBe(4);
  });
});

// ---------------------------------------------------------------------------
// Interface (nested) ports
// ---------------------------------------------------------------------------

describe("createDut — interface ports", () => {
  test("nested interface members", () => {
    const buffer = makeBuffer(64);
    const layout: Record<string, SignalLayout> = {
      "bus.addr": { offset: 0, width: 32, byteSize: 4, is4state: false, direction: "input" },
      "bus.data": { offset: 4, width: 32, byteSize: 4, is4state: false, direction: "input" },
      "bus.valid": { offset: 8, width: 1, byteSize: 1, is4state: false, direction: "input" },
      "bus.ready": { offset: 9, width: 1, byteSize: 1, is4state: false, direction: "output" },
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
        addr: number;
        data: number;
        valid: number;
        readonly ready: number;
      };
    }>(buffer, layout, ports, handle, state);

    dut.bus.addr = 0x1000;
    dut.bus.data = 0xFF;
    dut.bus.valid = 1;

    expect(dut.bus.addr).toBe(0x1000);
    expect(dut.bus.data).toBe(0xFF);
    expect(dut.bus.ready).toBe(1);
    expect(handle.evalComb).toHaveBeenCalledTimes(1);
  });
});
