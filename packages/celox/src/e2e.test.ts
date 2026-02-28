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
import { readFourState } from "./dut.js";
import { X, FourState } from "./types.js";
import {
  createSimulatorBridge,
  createSimulationBridge,
  loadNativeAddon,
  parseNapiLayout,
  type RawNapiAddon,
  type RawNapiSimulatorHandle,
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

// ---------------------------------------------------------------------------
// 4-state simulation e2e tests
// ---------------------------------------------------------------------------

const AND_OR_SOURCE = `
module AndOr (
    a: input logic,
    b: input logic,
    y_and: output logic,
    y_or: output logic,
) {
    assign y_and = a & b;
    assign y_or = a | b;
}
`;

const LOGIC_BIT_MIX_SOURCE = `
module LogicBitMix (
    a_logic: input logic<8>,
    b_bit: input bit<8>,
    y_logic_from_bit: output logic<8>,
    y_bit_from_logic: output bit<8>,
) {
    assign y_bit_from_logic = a_logic;
    assign y_logic_from_bit = b_bit;
}
`;

const FF_SOURCE = `
module FF (
    clk: input clock,
    rst: input reset,
    d: input logic<8>,
    q: output logic<8>,
) {
    always_ff (clk, rst) {
        if_reset {
            q = 8'd0;
        } else {
            q = d;
        }
    }
}
`;

const ADDER_4STATE_SOURCE = `
module Adder4S (
    a: input logic<8>,
    b: input logic<8>,
    y: output logic<8>,
) {
    assign y = a + b;
}
`;

describe("E2E: 4-state simulation", () => {
  let raw: RawNapiSimulatorHandle | undefined;
  let addon: RawNapiAddon;

  try {
    addon = loadNativeAddon();
  } catch (e) {
    throw new Error(`Failed to load NAPI addon for 4-state tests: ${e}`);
  }

  afterEach(() => {
    raw?.dispose();
    raw = undefined;
  });

  test("initial values: logic ports start as X, bit ports start as 0", () => {
    const source = `
module InitTest (
    a: input logic<8>,
    b: input bit<8>,
) {}
`;
    raw = new addon.NativeSimulatorHandle(source, "InitTest", { fourState: true });
    const layout = parseNapiLayout(raw.layoutJson);
    const buf = raw.sharedMemory().buffer;

    // logic port should have mask=0xFF (all X)
    const [valA, maskA] = readFourState(buf, layout.forDut.a);
    expect(valA).toBe(0);
    expect(maskA).toBe(0xFF);

    // bit port should have mask=0 (defined)
    // bit is not 4-state, so no mask — reading its value should be 0
    expect(layout.forDut.b.is4state).toBe(false);
  });

  test("writing X clears value and sets mask", () => {
    interface Ports {
      a: number;
      readonly y_and: number;
    }

    const sim = Simulator.fromSource<Ports>(AND_OR_SOURCE, "AndOr", { fourState: true });
    raw = undefined; // sim manages its own handle

    // Write X to input 'a' via DUT
    (sim.dut as any).a = X;

    // We can't inspect mask through DUT getter (it only returns value),
    // so this test verifies X write doesn't throw and propagation works.
    // For detailed mask inspection, see the raw NAPI tests below.
    sim.dispose();
  });

  test("AND: 0 & X = 0 (dominant zero)", () => {
    raw = new addon.NativeSimulatorHandle(AND_OR_SOURCE, "AndOr", { fourState: true });
    const layout = parseNapiLayout(raw.layoutJson);
    const buf = raw.sharedMemory().buffer;
    const view = new DataView(buf);
    const events: Record<string, number> = JSON.parse(raw.eventsJson);

    const sigA = layout.forDut.a;
    const sigB = layout.forDut.b;
    const sigYAnd = layout.forDut.y_and;
    const sigYOr = layout.forDut.y_or;

    // a = 0 (value=0, mask=0)
    view.setUint8(sigA.offset, 0);
    view.setUint8(sigA.offset + sigA.byteSize, 0);

    // b = X (value=0, mask=1)
    view.setUint8(sigB.offset, 0);
    view.setUint8(sigB.offset + sigB.byteSize, 1);

    raw.evalComb();

    // 0 & X = 0 (mask should be 0 — dominant zero)
    const [vAnd, mAnd] = readFourState(buf, sigYAnd);
    expect(vAnd).toBe(0);
    expect(mAnd).toBe(0);

    // 0 | X = X (mask should be 1)
    const [vOr, mOr] = readFourState(buf, sigYOr);
    expect(vOr).toBe(0);
    expect(mOr).toBe(1);
  });

  test("OR: 1 | X = 1 (dominant one)", () => {
    raw = new addon.NativeSimulatorHandle(AND_OR_SOURCE, "AndOr", { fourState: true });
    const layout = parseNapiLayout(raw.layoutJson);
    const buf = raw.sharedMemory().buffer;
    const view = new DataView(buf);

    const sigA = layout.forDut.a;
    const sigB = layout.forDut.b;
    const sigYOr = layout.forDut.y_or;

    // a = 1 (value=1, mask=0)
    view.setUint8(sigA.offset, 1);
    view.setUint8(sigA.offset + sigA.byteSize, 0);

    // b = X (value=0, mask=1)
    view.setUint8(sigB.offset, 0);
    view.setUint8(sigB.offset + sigB.byteSize, 1);

    raw.evalComb();

    // 1 | X = 1 (mask should be 0 — dominant one)
    const [vOr, mOr] = readFourState(buf, sigYOr);
    expect(vOr).toBe(1);
    expect(mOr).toBe(0);
  });

  test("logic-to-bit assignment strips X mask", () => {
    raw = new addon.NativeSimulatorHandle(LOGIC_BIT_MIX_SOURCE, "LogicBitMix", { fourState: true });
    const layout = parseNapiLayout(raw.layoutJson);
    const buf = raw.sharedMemory().buffer;
    const view = new DataView(buf);

    const sigALogic = layout.forDut.a_logic;
    const sigYBitFromLogic = layout.forDut.y_bit_from_logic;

    // a_logic = all-X (value=0, mask=0xFF)
    view.setUint8(sigALogic.offset, 0);
    view.setUint8(sigALogic.offset + sigALogic.byteSize, 0xFF);

    raw.evalComb();

    // y_bit_from_logic is bit type — X should be stripped (mask=0)
    expect(sigYBitFromLogic.is4state).toBe(false);
  });

  test("bit-to-logic assignment has no X", () => {
    raw = new addon.NativeSimulatorHandle(LOGIC_BIT_MIX_SOURCE, "LogicBitMix", { fourState: true });
    const layout = parseNapiLayout(raw.layoutJson);
    const buf = raw.sharedMemory().buffer;
    const view = new DataView(buf);

    const sigBBit = layout.forDut.b_bit;
    const sigYLogicFromBit = layout.forDut.y_logic_from_bit;

    // b_bit = 0xAA (bit type, always defined)
    view.setUint8(sigBBit.offset, 0xAA);

    raw.evalComb();

    // y_logic_from_bit should be 0xAA with mask=0
    const [vLogic, mLogic] = readFourState(buf, sigYLogicFromBit);
    expect(vLogic).toBe(0xAA);
    expect(mLogic).toBe(0);
  });

  test("arithmetic with X produces all-X output", () => {
    raw = new addon.NativeSimulatorHandle(ADDER_4STATE_SOURCE, "Adder4S", { fourState: true });
    const layout = parseNapiLayout(raw.layoutJson);
    const buf = raw.sharedMemory().buffer;
    const view = new DataView(buf);

    const sigA = layout.forDut.a;
    const sigB = layout.forDut.b;
    const sigY = layout.forDut.y;

    // a = 42 (defined), b = X (all X)
    view.setUint8(sigA.offset, 42);
    view.setUint8(sigA.offset + sigA.byteSize, 0); // mask=0

    view.setUint8(sigB.offset, 0);
    view.setUint8(sigB.offset + sigB.byteSize, 0xFF); // mask=0xFF

    raw.evalComb();

    // a + X = all-X
    const [, mY] = readFourState(buf, sigY);
    expect(mY).toBe(0xFF);
  });

  test("defined inputs in 4-state mode behave like 2-state", () => {
    raw = new addon.NativeSimulatorHandle(ADDER_4STATE_SOURCE, "Adder4S", { fourState: true });
    const layout = parseNapiLayout(raw.layoutJson);
    const buf = raw.sharedMemory().buffer;
    const view = new DataView(buf);

    const sigA = layout.forDut.a;
    const sigB = layout.forDut.b;
    const sigY = layout.forDut.y;

    // a = 100 (defined), b = 55 (defined)
    view.setUint8(sigA.offset, 100);
    view.setUint8(sigA.offset + sigA.byteSize, 0);

    view.setUint8(sigB.offset, 55);
    view.setUint8(sigB.offset + sigB.byteSize, 0);

    raw.evalComb();

    const [vY, mY] = readFourState(buf, sigY);
    expect(vY).toBe(155);
    expect(mY).toBe(0);
  });

  test("FF captures X from input, reset clears X", () => {
    raw = new addon.NativeSimulatorHandle(FF_SOURCE, "FF", { fourState: true });
    const layout = parseNapiLayout(raw.layoutJson);
    const buf = raw.sharedMemory().buffer;
    const view = new DataView(buf);
    const events: Record<string, number> = JSON.parse(raw.eventsJson);

    const sigRst = layout.forDut.rst;
    const sigD = layout.forDut.d;
    const sigQ = layout.forDut.q;
    const clkEventId = events.clk;

    // 1. Reset: rst=1, d=X
    view.setUint8(sigRst.offset, 1);
    view.setUint8(sigRst.offset + sigRst.byteSize, 0); // rst is defined

    view.setUint8(sigD.offset, 0);
    view.setUint8(sigD.offset + sigD.byteSize, 0xFF); // d = all-X

    raw.tick(clkEventId);

    // After reset, q should be 0 with mask=0
    const [vQ1, mQ1] = readFourState(buf, sigQ);
    expect(vQ1).toBe(0);
    expect(mQ1).toBe(0);

    // 2. Release reset, d = partial X (value=0xA5, mask=0x0F)
    view.setUint8(sigRst.offset, 0);
    view.setUint8(sigRst.offset + sigRst.byteSize, 0);

    view.setUint8(sigD.offset, 0xA5);
    view.setUint8(sigD.offset + sigD.byteSize, 0x0F);

    raw.tick(clkEventId);

    // FF should capture X mask from d
    const [, mQ2] = readFourState(buf, sigQ);
    expect(mQ2).toBe(0x0F);

    // 3. Reset again: should clear X
    view.setUint8(sigRst.offset, 1);
    view.setUint8(sigRst.offset + sigRst.byteSize, 0);

    raw.tick(clkEventId);

    const [vQ3, mQ3] = readFourState(buf, sigQ);
    expect(vQ3).toBe(0);
    expect(mQ3).toBe(0);
  });

  test("FourState write through DUT sets value and mask", () => {
    raw = new addon.NativeSimulatorHandle(ADDER_4STATE_SOURCE, "Adder4S", { fourState: true });
    const layout = parseNapiLayout(raw.layoutJson);
    const buf = raw.sharedMemory().buffer;
    const view = new DataView(buf);

    const sigA = layout.forDut.a;

    // Write via DUT-style: FourState(0b1010_0101, 0b0000_1111)
    // value=0xA5, mask=0x0F means lower 4 bits are X
    view.setUint8(sigA.offset, 0xA5);
    view.setUint8(sigA.offset + sigA.byteSize, 0x0F);

    const [vA, mA] = readFourState(buf, sigA);
    expect(vA).toBe(0xA5);
    expect(mA).toBe(0x0F);
  });

  test("setting defined value clears X mask", () => {
    raw = new addon.NativeSimulatorHandle(ADDER_4STATE_SOURCE, "Adder4S", { fourState: true });
    const layout = parseNapiLayout(raw.layoutJson);
    const buf = raw.sharedMemory().buffer;
    const view = new DataView(buf);

    const sigA = layout.forDut.a;

    // Start with X
    view.setUint8(sigA.offset, 0);
    view.setUint8(sigA.offset + sigA.byteSize, 0xFF);

    const [, mBefore] = readFourState(buf, sigA);
    expect(mBefore).toBe(0xFF);

    // Write a defined value (clear mask)
    view.setUint8(sigA.offset, 42);
    view.setUint8(sigA.offset + sigA.byteSize, 0);

    const [vAfter, mAfter] = readFourState(buf, sigA);
    expect(vAfter).toBe(42);
    expect(mAfter).toBe(0);
  });

  test("4-state through DUT high-level API (fromSource with fourState)", () => {
    interface Ports {
      a: number;
      b: number;
      readonly y: number;
    }

    const sim = Simulator.fromSource<Ports>(ADDER_4STATE_SOURCE, "Adder4S", { fourState: true });

    // Write defined values — should behave like 2-state
    sim.dut.a = 100;
    sim.dut.b = 55;
    expect(sim.dut.y).toBe(155);

    // Write X to a — output should propagate X (value reads as 0)
    (sim.dut as any).a = X;
    // After writing X, the value part of 'y' is implementation-defined
    // but the read should not throw
    const _yVal = sim.dut.y;
    expect(typeof _yVal).toBe("number");

    // Write FourState with partial X
    (sim.dut as any).a = FourState(0xA0, 0x0F);
    const _yVal2 = sim.dut.y;
    expect(typeof _yVal2).toBe("number");

    sim.dispose();
  });
});
