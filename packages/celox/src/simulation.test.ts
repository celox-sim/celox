import { describe, test, expect, vi } from "vitest";
import { Simulation, type NativeCreateSimulationFn } from "./simulation.js";
import type {
  CreateResult,
  ModuleDefinition,
  NativeSimulationHandle,
} from "./types.js";

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
      clk: { offset: 6, width: 1, byteSize: 1, is4state: false, direction: "input" },
      rst: { offset: 0, width: 1, byteSize: 1, is4state: false, direction: "input" },
      d:   { offset: 2, width: 8, byteSize: 1, is4state: false, direction: "input" },
      q:   { offset: 4, width: 8, byteSize: 1, is4state: false, direction: "output" },
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
});
