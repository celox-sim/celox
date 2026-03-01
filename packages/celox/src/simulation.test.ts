import { describe, test, expect, vi } from "vitest";
import { Simulation, type NativeCreateSimulationFn } from "./simulation.js";
import type {
  CreateResult,
  ModuleDefinition,
  NativeSimulationHandle,
} from "./types.js";
import { SimulationTimeoutError } from "./types.js";

// ---------------------------------------------------------------------------
// Mock helpers
// ---------------------------------------------------------------------------

interface TopPorts {
  rst: bigint;
  d: bigint;
  readonly q: bigint;
}

const TopModule: ModuleDefinition<TopPorts> = {
  __celox_module: true,
  name: "Top",
  source: "module Top ...",
  ports: {
    clk: { direction: "input", type: "clock", width: 1 },
    rst: { direction: "input", type: "reset", width: 1 },
    d:   { direction: "input", type: "logic", width: 8 },
    q:   { direction: "output", type: "logic", width: 8 },
  },
  events: ["clk"],
};

function createMockNative(opts?: {
  resetTypeKind?: string;
  associatedClock?: string;
}): {
  create: NativeCreateSimulationFn;
  handle: NativeSimulationHandle;
  buffer: SharedArrayBuffer;
} {
  const buffer = new SharedArrayBuffer(64);
  let currentTime = 0;

  const handle: NativeSimulationHandle = {
    addClock: vi.fn(),
    schedule: vi.fn(),
    runUntil: vi.fn().mockImplementation((endTime: number) => {
      // Simulate: q = d after running
      const view = new DataView(buffer);
      view.setUint8(4, view.getUint8(2));
      currentTime = endTime;
    }),
    step: vi.fn().mockImplementation(() => {
      currentTime += 5;
      const view = new DataView(buffer);
      view.setUint8(4, view.getUint8(2));
      // Toggle clock (offset 6) on each step to simulate half-period=5
      view.setUint8(6, view.getUint8(6) === 0 ? 1 : 0);
      return currentTime;
    }),
    time: vi.fn().mockImplementation(() => currentTime),
    nextEventTime: vi.fn().mockReturnValue(null),
    evalComb: vi.fn().mockImplementation(() => {
      const view = new DataView(buffer);
      view.setUint8(4, view.getUint8(2));
    }),
    dump: vi.fn(),
    dispose: vi.fn(),
  };

  const resetTypeKind = opts?.resetTypeKind ?? "reset_async_high";
  const associatedClock = opts?.associatedClock;

  const create: NativeCreateSimulationFn = vi.fn().mockReturnValue({
    buffer,
    layout: {
      clk: { offset: 6, width: 1, byteSize: 1, is4state: false, direction: "input", typeKind: "clock" },
      rst: { offset: 0, width: 1, byteSize: 1, is4state: false, direction: "input", typeKind: resetTypeKind, ...(associatedClock ? { associatedClock } : {}) },
      d:   { offset: 2, width: 8, byteSize: 1, is4state: false, direction: "input", typeKind: "logic" },
      q:   { offset: 4, width: 8, byteSize: 1, is4state: false, direction: "output", typeKind: "logic" },
    },
    events: { clk: 0 },
    handle,
  } satisfies CreateResult<NativeSimulationHandle>);

  return { create, handle, buffer };
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe("Simulation", () => {
  test("create and addClock", () => {
    const mock = createMockNative();
    const sim = Simulation.create(TopModule, {
      __nativeCreate: mock.create,
    });

    sim.addClock("clk", { period: 10 });
    expect(mock.handle.addClock).toHaveBeenCalledWith(0, 10, 0);
  });

  test("addClock with initialDelay", () => {
    const mock = createMockNative();
    const sim = Simulation.create(TopModule, {
      __nativeCreate: mock.create,
    });

    sim.addClock("clk", { period: 10, initialDelay: 5 });
    expect(mock.handle.addClock).toHaveBeenCalledWith(0, 10, 5);
  });

  test("schedule", () => {
    const mock = createMockNative();
    const sim = Simulation.create(TopModule, {
      __nativeCreate: mock.create,
    });

    sim.schedule("clk", { time: 50, value: 1 });
    expect(mock.handle.schedule).toHaveBeenCalledWith(0, 50, 1);
  });

  test("runUntil", () => {
    const mock = createMockNative();
    const sim = Simulation.create(TopModule, {
      __nativeCreate: mock.create,
    });

    sim.dut.d = 42n;
    sim.runUntil(100);

    expect(mock.handle.runUntil).toHaveBeenCalledWith(100);
    expect(sim.dut.q).toBe(42n);
  });

  test("step", () => {
    const mock = createMockNative();
    const sim = Simulation.create(TopModule, {
      __nativeCreate: mock.create,
    });

    sim.dut.d = 0xABn;
    const t = sim.step();

    expect(t).toBe(5);
    expect(sim.dut.q).toBe(0xABn);
  });

  test("time", () => {
    const mock = createMockNative();
    const sim = Simulation.create(TopModule, {
      __nativeCreate: mock.create,
    });

    expect(sim.time()).toBe(0);
    sim.runUntil(100);
    expect(sim.time()).toBe(100);
  });

  test("unknown event throws", () => {
    const mock = createMockNative();
    const sim = Simulation.create(TopModule, {
      __nativeCreate: mock.create,
    });

    expect(() => sim.addClock("bad", { period: 10 })).toThrow("Unknown event");
  });

  test("dispose prevents further operations", () => {
    const mock = createMockNative();
    const sim = Simulation.create(TopModule, {
      __nativeCreate: mock.create,
    });

    sim.dispose();
    expect(() => sim.runUntil(100)).toThrow("disposed");
  });

  test("dump delegates to handle", () => {
    const mock = createMockNative();
    const sim = Simulation.create(TopModule, {
      __nativeCreate: mock.create,
    });

    sim.dump(99);
    expect(mock.handle.dump).toHaveBeenCalledWith(99);
  });

  test("create throws without native binding", () => {
    expect(() => {
      Simulation.create(TopModule);
    }).toThrow("Native simulator binding not loaded");
  });

  test("runUntil with maxSteps: fast path when omitted", () => {
    const mock = createMockNative();
    const sim = Simulation.create(TopModule, {
      __nativeCreate: mock.create,
    });

    sim.runUntil(100);
    // Without maxSteps, the Rust fast-path should be used
    expect(mock.handle.runUntil).toHaveBeenCalledWith(100);
  });

  test("runUntil with maxSteps: throws SimulationTimeoutError", () => {
    const mock = createMockNative();
    const sim = Simulation.create(TopModule, {
      __nativeCreate: mock.create,
    });

    // step mock increments time by 5 each call, so reaching 10000 requires 2000 steps
    // With maxSteps=10 we'll exhaust before getting there
    expect(() => sim.runUntil(10000, { maxSteps: 10 })).toThrow(
      SimulationTimeoutError,
    );
  });

  test("runUntil with maxSteps: timeout error has correct properties", () => {
    const mock = createMockNative();
    const sim = Simulation.create(TopModule, {
      __nativeCreate: mock.create,
    });

    try {
      sim.runUntil(10000, { maxSteps: 5 });
      expect.unreachable();
    } catch (e) {
      expect(e).toBeInstanceOf(SimulationTimeoutError);
      const err = e as SimulationTimeoutError;
      expect(err.steps).toBe(5);
      expect(err.time).toBeGreaterThan(0);
    }
  });

  test("waitUntil: returns time when condition is met", () => {
    const mock = createMockNative();
    const sim = Simulation.create(TopModule, {
      __nativeCreate: mock.create,
    });

    sim.dut.d = 42n;
    let callCount = 0;
    const t = sim.waitUntil(() => {
      callCount++;
      return callCount >= 3;
    });

    expect(t).toBeGreaterThan(0);
    expect(callCount).toBe(3);
  });

  test("waitUntil: throws on timeout", () => {
    const mock = createMockNative();
    const sim = Simulation.create(TopModule, {
      __nativeCreate: mock.create,
    });

    expect(() =>
      sim.waitUntil(() => false, { maxSteps: 5 }),
    ).toThrow(SimulationTimeoutError);
  });

  test("waitForCycles: counts rising edges", () => {
    const mock = createMockNative();
    const sim = Simulation.create(TopModule, {
      __nativeCreate: mock.create,
    });

    sim.addClock("clk", { period: 10 });
    const t = sim.waitForCycles("clk", 3);
    // clk toggles each step: 0→1→0→1→0→1 (5 steps for 3 rising edges)
    expect(mock.handle.step).toHaveBeenCalledTimes(5);
    expect(t).toBe(25);
  });

  test("waitForCycles: throws without addClock", () => {
    const mock = createMockNative();
    const sim = Simulation.create(TopModule, {
      __nativeCreate: mock.create,
    });

    expect(() => sim.waitForCycles("clk", 3)).toThrow("No clock registered");
  });

  test("reset: active-high with associatedClock steps until target time", () => {
    const mock = createMockNative({ associatedClock: "clk" });
    const sim = Simulation.create(TopModule, {
      __nativeCreate: mock.create,
    });

    sim.addClock("clk", { period: 10 });
    sim.reset("rst");
    // Default activeCycles=2: 3 steps for 2 rising edges (0→1→0→1)
    expect(mock.handle.step).toHaveBeenCalledTimes(3);
    // Released to inactive value (0 for active-high)
    const view = new DataView(mock.buffer);
    expect(view.getUint8(0)).toBe(0);
  });

  test("reset: custom activeCycles with associatedClock", () => {
    const mock = createMockNative({ associatedClock: "clk" });
    const sim = Simulation.create(TopModule, {
      __nativeCreate: mock.create,
    });

    sim.addClock("clk", { period: 10 });
    sim.reset("rst", { activeCycles: 3 });
    // 3 cycles: 5 steps for 3 rising edges (0→1→0→1→0→1)
    expect(mock.handle.step).toHaveBeenCalledTimes(5);
  });

  test("reset: explicit duration overrides cycle calculation", () => {
    const mock = createMockNative({ associatedClock: "clk" });
    const sim = Simulation.create(TopModule, {
      __nativeCreate: mock.create,
    });

    sim.addClock("clk", { period: 10 });
    sim.reset("rst", { duration: 50 });
    // Explicit duration → runUntil(0 + 50 = 50)
    expect(mock.handle.runUntil).toHaveBeenCalledWith(50);
  });

  test("reset: active-low with associatedClock asserts 0 then releases to 1", () => {
    const mock = createMockNative({
      resetTypeKind: "reset_async_low",
      associatedClock: "clk",
    });
    const sim = Simulation.create(TopModule, {
      __nativeCreate: mock.create,
    });

    sim.addClock("clk", { period: 10 });
    sim.reset("rst");
    // active-low: releases to 1
    const view = new DataView(mock.buffer);
    expect(view.getUint8(0)).toBe(1);
    // activeCycles=2: 3 steps for 2 rising edges
    expect(mock.handle.step).toHaveBeenCalledTimes(3);
  });

  test("reset: throws when no associatedClock and no duration", () => {
    // No associatedClock in layout
    const mock = createMockNative();
    const sim = Simulation.create(TopModule, {
      __nativeCreate: mock.create,
    });

    expect(() => sim.reset("rst")).toThrow("has no associated clock");
  });

  test("reset: no associatedClock but duration specified works", () => {
    // No associatedClock in layout, but duration is given
    const mock = createMockNative();
    const sim = Simulation.create(TopModule, {
      __nativeCreate: mock.create,
    });

    sim.reset("rst", { duration: 100 });
    expect(mock.handle.runUntil).toHaveBeenCalledWith(100);
  });

  test("reset: throws when associatedClock not registered via addClock", () => {
    const mock = createMockNative({ associatedClock: "clk" });
    const sim = Simulation.create(TopModule, {
      __nativeCreate: mock.create,
    });
    // addClock not called
    expect(() => sim.reset("rst")).toThrow(
      "No clock registered for 'clk'",
    );
  });

  test("reset: throws on non-reset port", () => {
    const mock = createMockNative();
    const sim = Simulation.create(TopModule, {
      __nativeCreate: mock.create,
    });

    expect(() => sim.reset("d")).toThrow("not a reset signal");
  });

  test("reset: throws on unknown port", () => {
    const mock = createMockNative();
    const sim = Simulation.create(TopModule, {
      __nativeCreate: mock.create,
    });

    expect(() => sim.reset("nonexistent")).toThrow("Unknown port");
  });
});
