/**
 * End-to-end tests for the TypeScript testbench.
 *
 * These tests exercise the full pipeline:
 *   Veryl source → Rust JIT (via NAPI) → SharedArrayBuffer bridge → TS DUT → verify
 *
 * Unlike the unit tests which use mock handles, these tests use the real
 * `celox-napi` native addon compiled from the Rust simulator.
 */

import path from "node:path";
import { describe, test, expect, afterEach } from "vitest";
import { Simulator } from "./simulator.js";
import { Simulation } from "./simulation.js";
import {
  createSimulatorBridge,
  createSimulationBridge,
  loadNativeAddon,
  type RawNapiAddon,
} from "./napi-helpers.js";

// Fixture project directories
const FIXTURES_DIR = path.resolve(import.meta.dirname ?? __dirname, "../fixtures");
const ADDER_PROJECT = path.join(FIXTURES_DIR, "adder");
const COUNTER_PROJECT = path.join(FIXTURES_DIR, "counter_project");

// ---------------------------------------------------------------------------
// Test Veryl sources
// ---------------------------------------------------------------------------

const ADDER_SOURCE = `
module Adder (
    clk: input clock,
    rst: input reset,
    a: input logic<16>,
    b: input logic<16>,
    sum: output logic<17>,
) {
    always_comb {
        sum = a + b;
    }
}
`;

const COUNTER_SOURCE = `
module Counter (
    clk: input clock,
    rst: input reset,
    en: input logic,
    count: output logic<8>,
) {
    var count_r: logic<8>;

    always_ff (clk, rst) {
        if_reset {
            count_r = 0;
        } else if en {
            count_r = count_r + 1;
        }
    }

    always_comb {
        count = count_r;
    }
}
`;

const MULTIPLEXER_SOURCE = `
module Mux4 (
    sel: input logic<2>,
    d0: input logic<8>,
    d1: input logic<8>,
    d2: input logic<8>,
    d3: input logic<8>,
    y: output logic<8>,
) {
    always_comb {
        case sel {
            2'd0: y = d0;
            2'd1: y = d1;
            2'd2: y = d2;
            2'd3: y = d3;
            default: y = 0;
        }
    }
}
`;

// ---------------------------------------------------------------------------
// Simulator (event-based) e2e tests — fromSource API
// ---------------------------------------------------------------------------

describe("E2E: Simulator.fromSource (event-based)", () => {
  let sim: Simulator | undefined;

  afterEach(() => {
    sim?.dispose();
    sim = undefined;
  });

  test("combinational adder: a + b = sum", () => {
    interface AdderPorts {
      rst: number;
      a: number;
      b: number;
      readonly sum: number;
    }

    sim = Simulator.fromSource<AdderPorts>(ADDER_SOURCE, "Adder");

    sim.dut.a = 100;
    sim.dut.b = 200;
    sim.tick();
    expect(sim.dut.sum).toBe(300);

    sim.dut.a = 0xFFFF;
    sim.dut.b = 1;
    sim.tick();
    expect(sim.dut.sum).toBe(0x10000);

    sim.dut.a = 0;
    sim.dut.b = 0;
    sim.tick();
    expect(sim.dut.sum).toBe(0);
  });

  test("combinational adder: lazy evalComb on output read", () => {
    interface AdderPorts {
      rst: number;
      a: number;
      b: number;
      readonly sum: number;
    }

    sim = Simulator.fromSource<AdderPorts>(ADDER_SOURCE, "Adder");

    sim.dut.a = 42;
    sim.dut.b = 58;
    expect(sim.dut.sum).toBe(100);
  });

  test("sequential counter: counts on clock edges", () => {
    interface CounterPorts {
      rst: number;
      en: number;
      readonly count: number;
    }

    sim = Simulator.fromSource<CounterPorts>(COUNTER_SOURCE, "Counter");

    // Reset the counter
    sim.dut.rst = 1;
    sim.tick();
    sim.dut.rst = 0;
    sim.tick();
    expect(sim.dut.count).toBe(0);

    // Enable counting
    sim.dut.en = 1;
    sim.tick();
    expect(sim.dut.count).toBe(1);

    sim.tick();
    expect(sim.dut.count).toBe(2);

    sim.tick();
    expect(sim.dut.count).toBe(3);

    // Disable counting
    sim.dut.en = 0;
    sim.tick();
    expect(sim.dut.count).toBe(3);

    // Re-enable
    sim.dut.en = 1;
    sim.tick(5);
    expect(sim.dut.count).toBe(8);
  });

  test("combinational multiplexer", () => {
    interface Mux4Ports {
      sel: number;
      d0: number;
      d1: number;
      d2: number;
      d3: number;
      readonly y: number;
    }

    sim = Simulator.fromSource<Mux4Ports>(MULTIPLEXER_SOURCE, "Mux4");

    sim.dut.d0 = 0xAA;
    sim.dut.d1 = 0xBB;
    sim.dut.d2 = 0xCC;
    sim.dut.d3 = 0xDD;

    sim.dut.sel = 0;
    expect(sim.dut.y).toBe(0xAA);

    sim.dut.sel = 1;
    expect(sim.dut.y).toBe(0xBB);

    sim.dut.sel = 2;
    expect(sim.dut.y).toBe(0xCC);

    sim.dut.sel = 3;
    expect(sim.dut.y).toBe(0xDD);
  });
});

// ---------------------------------------------------------------------------
// Simulation (time-based) e2e tests — fromSource API
// ---------------------------------------------------------------------------

describe("E2E: Simulation.fromSource (time-based)", () => {
  let sim: Simulation | undefined;

  afterEach(() => {
    sim?.dispose();
    sim = undefined;
  });

  test("counter with timed clock: step-by-step", () => {
    interface CounterPorts {
      rst: number;
      en: number;
      readonly count: number;
    }

    sim = Simulation.fromSource<CounterPorts>(COUNTER_SOURCE, "Counter");

    sim.addClock("clk", { period: 10 });
    expect(sim.time()).toBe(0);

    // Reset
    sim.dut.rst = 1;
    sim.runUntil(20);
    sim.dut.rst = 0;
    sim.dut.en = 1;

    sim.runUntil(100);

    const count = sim.dut.count;
    expect(count).toBeGreaterThan(0);
    expect(sim.time()).toBe(100);
  });
});

// ---------------------------------------------------------------------------
// Simulator (event-based) e2e tests — fromProject API
// ---------------------------------------------------------------------------

describe("E2E: Simulator.fromProject (event-based)", () => {
  let sim: Simulator | undefined;

  afterEach(() => {
    sim?.dispose();
    sim = undefined;
  });

  test("combinational adder from project directory", () => {
    interface AdderPorts {
      rst: number;
      a: number;
      b: number;
      readonly sum: number;
    }

    sim = Simulator.fromProject<AdderPorts>(ADDER_PROJECT, "Adder");

    sim.dut.a = 100;
    sim.dut.b = 200;
    sim.tick();
    expect(sim.dut.sum).toBe(300);

    sim.dut.a = 0xFFFF;
    sim.dut.b = 1;
    sim.tick();
    expect(sim.dut.sum).toBe(0x10000);
  });

  test("sequential counter from project directory", () => {
    interface CounterPorts {
      rst: number;
      en: number;
      readonly count: number;
    }

    sim = Simulator.fromProject<CounterPorts>(COUNTER_PROJECT, "Counter");

    // Reset the counter
    sim.dut.rst = 1;
    sim.tick();
    sim.dut.rst = 0;
    sim.tick();
    expect(sim.dut.count).toBe(0);

    // Enable counting
    sim.dut.en = 1;
    sim.tick();
    expect(sim.dut.count).toBe(1);

    sim.tick();
    expect(sim.dut.count).toBe(2);

    sim.tick();
    expect(sim.dut.count).toBe(3);
  });
});

// ---------------------------------------------------------------------------
// Simulation (time-based) e2e tests — fromProject API
// ---------------------------------------------------------------------------

describe("E2E: Simulation.fromProject (time-based)", () => {
  let sim: Simulation | undefined;

  afterEach(() => {
    sim?.dispose();
    sim = undefined;
  });

  test("counter with timed clock from project directory", () => {
    interface CounterPorts {
      rst: number;
      en: number;
      readonly count: number;
    }

    sim = Simulation.fromProject<CounterPorts>(COUNTER_PROJECT, "Counter");

    sim.addClock("clk", { period: 10 });
    expect(sim.time()).toBe(0);

    // Reset
    sim.dut.rst = 1;
    sim.runUntil(20);
    sim.dut.rst = 0;
    sim.dut.en = 1;

    sim.runUntil(100);

    const count = sim.dut.count;
    expect(count).toBeGreaterThan(0);
    expect(sim.time()).toBe(100);
  });
});

// ---------------------------------------------------------------------------
// Backward compat: Simulator.create() with manual ModuleDefinition
// ---------------------------------------------------------------------------

describe("E2E: Simulator.create (backward compat)", () => {
  let sim: Simulator | undefined;
  let addon: RawNapiAddon;
  let nativeCreateSimulator: ReturnType<typeof createSimulatorBridge>;

  // Load addon once
  try {
    addon = loadNativeAddon();
    nativeCreateSimulator = createSimulatorBridge(addon);
  } catch (e) {
    throw new Error(`Failed to load NAPI addon for backward compat tests: ${e}`);
  }

  afterEach(() => {
    sim?.dispose();
    sim = undefined;
  });

  test("combinational adder via Simulator.create()", () => {
    interface AdderPorts {
      rst: number;
      a: number;
      b: number;
      readonly sum: number;
    }

    sim = Simulator.create<AdderPorts>(
      {
        __celox_module: true,
        name: "Adder",
        source: ADDER_SOURCE,
        ports: {
          clk: { direction: "input", type: "clock", width: 1 },
          rst: { direction: "input", type: "reset", width: 1 },
          a: { direction: "input", type: "logic", width: 16 },
          b: { direction: "input", type: "logic", width: 16 },
          sum: { direction: "output", type: "logic", width: 17 },
        },
        events: ["clk"],
      },
      { __nativeCreate: nativeCreateSimulator },
    );

    sim.dut.a = 100;
    sim.dut.b = 200;
    sim.tick();
    expect(sim.dut.sum).toBe(300);
  });
});
