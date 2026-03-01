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
  rst: number;
  d: number;
  readonly q: number;
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

function createMockNative(): {
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

  const create: NativeCreateSimulationFn = vi.fn().mockReturnValue({
    buffer,
    layout: {
      clk: { offset: 6, width: 1, byteSize: 1, is4state: false, direction: "input", typeKind: "clock" },
      rst: { offset: 0, width: 1, byteSize: 1, is4state: false, direction: "input", typeKind: "reset_async_high" },
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

    sim.dut.d = 42;
    sim.runUntil(100);

    expect(mock.handle.runUntil).toHaveBeenCalledWith(100);
    expect(sim.dut.q).toBe(42);
  });

  test("step", () => {
    const mock = createMockNative();
    const sim = Simulation.create(TopModule, {
      __nativeCreate: mock.create,
    });

    sim.dut.d = 0xAB;
    const t = sim.step();

    expect(t).toBe(5);
    expect(sim.dut.q).toBe(0xAB);
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

    sim.dut.d = 42;
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

  test("waitForCycles: advances correct number of steps", () => {
    const mock = createMockNative();
    const sim = Simulation.create(TopModule, {
      __nativeCreate: mock.create,
    });

    const t = sim.waitForCycles("clk", 3);
    // 3 cycles = 6 steps, each step increments by 5 → time = 30
    expect(t).toBe(30);
    expect(mock.handle.step).toHaveBeenCalledTimes(6);
  });

  test("waitForCycles: throws on timeout", () => {
    const mock = createMockNative();
    const sim = Simulation.create(TopModule, {
      __nativeCreate: mock.create,
    });

    expect(() =>
      sim.waitForCycles("clk", 1000, { maxSteps: 3 }),
    ).toThrow(SimulationTimeoutError);
  });

  test("reset: active-high (default) asserts 1 then releases to 0", () => {
    const mock = createMockNative();
    const sim = Simulation.create(TopModule, {
      __nativeCreate: mock.create,
    });

    sim.reset("rst");
    // Default: activeCycles=2 → 4 steps, then rst released to 0
    expect(mock.handle.step).toHaveBeenCalledTimes(4);
    const view = new DataView(mock.buffer);
    expect(view.getUint8(0)).toBe(0);
  });

  test("reset: custom activeCycles", () => {
    const mock = createMockNative();
    const sim = Simulation.create(TopModule, {
      __nativeCreate: mock.create,
    });

    sim.reset("rst", { activeCycles: 3 });
    // 3 cycles → 6 steps
    expect(mock.handle.step).toHaveBeenCalledTimes(6);
  });

  test("reset: active-low asserts 0 then releases to 1", () => {
    // Create mock with active-low reset
    const buffer = new SharedArrayBuffer(64);
    let currentTime = 0;

    const handle: NativeSimulationHandle = {
      addClock: vi.fn(),
      schedule: vi.fn(),
      runUntil: vi.fn(),
      step: vi.fn().mockImplementation(() => {
        currentTime += 5;
        return currentTime;
      }),
      time: vi.fn().mockImplementation(() => currentTime),
      nextEventTime: vi.fn().mockReturnValue(null),
      evalComb: vi.fn(),
      dump: vi.fn(),
      dispose: vi.fn(),
    };

    const create: NativeCreateSimulationFn = vi.fn().mockReturnValue({
      buffer,
      layout: {
        clk: { offset: 6, width: 1, byteSize: 1, is4state: false, direction: "input", typeKind: "clock" },
        rst: { offset: 0, width: 1, byteSize: 1, is4state: false, direction: "input", typeKind: "reset_async_low" },
        d:   { offset: 2, width: 8, byteSize: 1, is4state: false, direction: "input", typeKind: "logic" },
        q:   { offset: 4, width: 8, byteSize: 1, is4state: false, direction: "output", typeKind: "logic" },
      },
      events: { clk: 0 },
      handle,
    } satisfies CreateResult<NativeSimulationHandle>);

    const sim = Simulation.create(TopModule, { __nativeCreate: create });

    sim.reset("rst");
    // active-low: asserts 0, releases to 1
    expect(handle.step).toHaveBeenCalledTimes(4);
    const view = new DataView(buffer);
    expect(view.getUint8(0)).toBe(1); // released to inactive = 1
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
