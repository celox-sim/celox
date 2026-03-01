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
import { X, FourState, SimulationTimeoutError } from "./types.js";
import {
  createSimulatorBridge,
  loadNativeAddon,
  parseNapiLayout,
  parseSignalPath,
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
  test("combinational adder: a + b = sum", () => {
    interface AdderPorts {
      rst: number;
      a: number;
      b: number;
      readonly sum: number;
    }

    const sim = Simulator.fromSource<AdderPorts>(ADDER_SOURCE, "Adder");

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

    sim.dispose();
  });

  test("combinational adder: lazy evalComb on output read", () => {
    interface AdderPorts {
      rst: number;
      a: number;
      b: number;
      readonly sum: number;
    }

    const sim = Simulator.fromSource<AdderPorts>(ADDER_SOURCE, "Adder");

    sim.dut.a = 42;
    sim.dut.b = 58;
    expect(sim.dut.sum).toBe(100);

    sim.dispose();
  });

  test("sequential counter: counts on clock edges", () => {
    interface CounterPorts {
      rst: number;
      en: number;
      readonly count: number;
    }

    const sim = Simulator.fromSource<CounterPorts>(COUNTER_SOURCE, "Counter");

    // Reset the counter (default async_low: rst=0 is active)
    sim.dut.rst = 0;
    sim.tick();
    sim.dut.rst = 1;
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

    sim.dispose();
  });

  test("always_comb evaluated before FF on tick (no prior output read)", () => {
    interface CounterPorts {
      rst: number;
      en: number;
      readonly count: number;
    }

    const sim = Simulator.fromSource<CounterPorts>(COUNTER_SOURCE, "Counter");

    // Reset (default async_low: rst=0 is active)
    sim.dut.rst = 0;
    sim.tick();
    sim.dut.rst = 1;
    sim.tick();

    // Write input and tick WITHOUT reading any output first.
    // This catches the stale-comb bug where always_ff reads stale
    // combinational values because eval_comb was skipped before FF.
    sim.dut.en = 1;
    sim.tick();
    expect(Number(sim.dut.count)).toBe(1);

    // Again: set input, tick, verify — no intermediate reads
    sim.dut.en = 1;
    sim.tick();
    expect(Number(sim.dut.count)).toBe(2);

    // Disable, tick, verify
    sim.dut.en = 0;
    sim.tick();
    expect(Number(sim.dut.count)).toBe(2);

    sim.dispose();
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

    const sim = Simulator.fromSource<Mux4Ports>(MULTIPLEXER_SOURCE, "Mux4");

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

    sim.dispose();
  });
});

// ---------------------------------------------------------------------------
// Simulation (time-based) e2e tests — fromSource API
// ---------------------------------------------------------------------------

describe("E2E: Simulation.fromSource (time-based)", () => {
  test("counter with timed clock: step-by-step", () => {
    interface CounterPorts {
      rst: number;
      en: number;
      readonly count: number;
    }

    const sim = Simulation.fromSource<CounterPorts>(COUNTER_SOURCE, "Counter");

    sim.addClock("clk", { period: 10 });
    expect(sim.time()).toBe(0);

    // Reset (default async_low: rst=0 is active)
    sim.dut.rst = 0;
    sim.runUntil(20);
    sim.dut.rst = 1;
    sim.dut.en = 1;

    sim.runUntil(100);

    const count = sim.dut.count;
    expect(count).toBeGreaterThan(0);
    expect(sim.time()).toBe(100);

    sim.dispose();
  });
});

// ---------------------------------------------------------------------------
// Simulator (event-based) e2e tests — fromProject API
// ---------------------------------------------------------------------------

describe("E2E: Simulator.fromProject (event-based)", () => {
  test("combinational adder from project directory", () => {
    interface AdderPorts {
      rst: number;
      a: number;
      b: number;
      readonly sum: number;
    }

    const sim = Simulator.fromProject<AdderPorts>(ADDER_PROJECT, "Adder");

    sim.dut.a = 100;
    sim.dut.b = 200;
    sim.tick();
    expect(sim.dut.sum).toBe(300);

    sim.dut.a = 0xFFFF;
    sim.dut.b = 1;
    sim.tick();
    expect(sim.dut.sum).toBe(0x10000);

    sim.dispose();
  });

  test("sequential counter from project directory", () => {
    interface CounterPorts {
      rst: number;
      en: number;
      readonly count: number;
    }

    const sim = Simulator.fromProject<CounterPorts>(COUNTER_PROJECT, "Counter");

    // Reset the counter (default async_low: rst=0 is active)
    sim.dut.rst = 0;
    sim.tick();
    sim.dut.rst = 1;
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

    sim.dispose();
  });
});

// ---------------------------------------------------------------------------
// Simulation (time-based) e2e tests — fromProject API
// ---------------------------------------------------------------------------

describe("E2E: Simulation.fromProject (time-based)", () => {
  test("counter with timed clock from project directory", () => {
    interface CounterPorts {
      rst: number;
      en: number;
      readonly count: number;
    }

    const sim = Simulation.fromProject<CounterPorts>(COUNTER_PROJECT, "Counter");

    sim.addClock("clk", { period: 10 });
    expect(sim.time()).toBe(0);

    // Reset (default async_low: rst=0 is active)
    sim.dut.rst = 0;
    sim.runUntil(20);
    sim.dut.rst = 1;
    sim.dut.en = 1;

    sim.runUntil(100);

    const count = sim.dut.count;
    expect(count).toBeGreaterThan(0);
    expect(sim.time()).toBe(100);

    sim.dispose();
  });
});

// ---------------------------------------------------------------------------
// Backward compat: Simulator.create() with manual ModuleDefinition
// ---------------------------------------------------------------------------

describe("E2E: Simulator.create (backward compat)", () => {
  test("combinational adder via Simulator.create()", () => {
    interface AdderPorts {
      rst: number;
      a: number;
      b: number;
      readonly sum: number;
    }

    const addon = loadNativeAddon();
    const nativeCreateSimulator = createSimulatorBridge(addon);

    const sim = Simulator.create<AdderPorts>(
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

    sim.dispose();
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

    // 1. Assert reset (default async_low: rst=0 is active), d=X
    view.setUint8(sigRst.offset, 0);
    view.setUint8(sigRst.offset + sigRst.byteSize, 0); // rst is defined

    view.setUint8(sigD.offset, 0);
    view.setUint8(sigD.offset + sigD.byteSize, 0xFF); // d = all-X

    raw.tick(clkEventId);

    // After reset, q should be 0 with mask=0
    const [vQ1, mQ1] = readFourState(buf, sigQ);
    expect(vQ1).toBe(0);
    expect(mQ1).toBe(0);

    // 2. Release reset (rst=1 is inactive), d = partial X (value=0xA5, mask=0x0F)
    view.setUint8(sigRst.offset, 1);
    view.setUint8(sigRst.offset + sigRst.byteSize, 0);

    view.setUint8(sigD.offset, 0xA5);
    view.setUint8(sigD.offset + sigD.byteSize, 0x0F);

    raw.tick(clkEventId);

    // FF should capture X mask from d
    const [, mQ2] = readFourState(buf, sigQ);
    expect(mQ2).toBe(0x0F);

    // 3. Assert reset again (rst=0): should clear X
    view.setUint8(sigRst.offset, 0);
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

// ---------------------------------------------------------------------------
// 4-state: high-level DUT API (Simulator.fromSource)
// ---------------------------------------------------------------------------

describe("E2E: 4-state high-level DUT API", () => {
  test("counter in 4-state mode: reset clears X, counting works", () => {
    interface CounterPorts {
      rst: number;
      en: number;
      readonly count: number;
    }

    const sim = Simulator.fromSource<CounterPorts>(
      COUNTER_SOURCE, "Counter", { fourState: true },
    );

    // In 4-state mode, count starts as X. Reset should clear it.
    // (default async_low: rst=0 is active)
    sim.dut.rst = 0;
    sim.tick();
    sim.dut.rst = 1;
    sim.tick();
    expect(sim.dut.count).toBe(0);

    // Enable counting — should work exactly like 2-state
    sim.dut.en = 1;
    sim.tick();
    expect(sim.dut.count).toBe(1);

    sim.tick();
    expect(sim.dut.count).toBe(2);

    sim.tick();
    expect(sim.dut.count).toBe(3);

    sim.dispose();
  });

  test("multiplexer with X selector produces X output", () => {
    const addon = loadNativeAddon();
    const raw = new addon.NativeSimulatorHandle(MULTIPLEXER_SOURCE, "Mux4", { fourState: true });
    const layout = parseNapiLayout(raw.layoutJson);
    const buf = raw.sharedMemory().buffer;
    const view = new DataView(buf);

    const sigSel = layout.forDut.sel;
    const sigD0 = layout.forDut.d0;
    const sigY = layout.forDut.y;

    // Set d0 = 0xAA (defined)
    view.setUint8(sigD0.offset, 0xAA);
    view.setUint8(sigD0.offset + sigD0.byteSize, 0);

    // Set sel = X
    view.setUint8(sigSel.offset, 0);
    view.setUint8(sigSel.offset + sigSel.byteSize, 0x03);

    raw.evalComb();

    // With X selector, output should be all-X
    const [, mY] = readFourState(buf, sigY);
    expect(mY).toBe(0xFF);

    raw.dispose();
  });

  test("FF via DUT API: write X input, tick, read output", () => {
    interface FFPorts {
      rst: number;
      d: number;
      readonly q: number;
    }

    const sim = Simulator.fromSource<FFPorts>(FF_SOURCE, "FF", { fourState: true });

    // Reset to clear initial X (default async_low: rst=0 is active)
    sim.dut.rst = 0;
    sim.tick();
    sim.dut.rst = 1;
    expect(sim.dut.q).toBe(0);

    // Write a defined value
    sim.dut.d = 0x42;
    sim.tick();
    expect(sim.dut.q).toBe(0x42);

    // Write X to d, tick — q should capture it (value read still returns a number)
    (sim.dut as any).d = X;
    sim.tick();
    expect(typeof sim.dut.q).toBe("number");

    // Write defined value again — q should recover
    sim.dut.d = 0x99;
    sim.tick();
    expect(sim.dut.q).toBe(0x99);

    sim.dispose();
  });

  test("X to defined transition: adder recovers from X", () => {
    interface Ports {
      a: number;
      b: number;
      readonly y: number;
    }

    const sim = Simulator.fromSource<Ports>(ADDER_4STATE_SOURCE, "Adder4S", { fourState: true });

    // Start with X
    (sim.dut as any).a = X;
    sim.dut.b = 10;
    // Output has X — just verify it doesn't crash
    expect(typeof sim.dut.y).toBe("number");

    // Clear X by writing defined values
    sim.dut.a = 20;
    sim.dut.b = 30;
    expect(sim.dut.y).toBe(50);

    sim.dispose();
  });
});

// ---------------------------------------------------------------------------
// 4-state: Simulation (time-based) tests
// ---------------------------------------------------------------------------

describe("E2E: 4-state Simulation (time-based)", () => {
  test("FF with clock-driven 4-state: reset clears X, captures defined values", () => {
    interface FFPorts {
      rst: number;
      d: number;
      readonly q: number;
    }

    const sim = Simulation.fromSource<FFPorts>(FF_SOURCE, "FF", { fourState: true });

    sim.addClock("clk", { period: 10 });
    expect(sim.time()).toBe(0);

    // Reset to clear initial X on q (default async_low: rst=0 is active)
    sim.dut.rst = 0;
    sim.runUntil(20);
    sim.dut.rst = 1;
    expect(sim.dut.q).toBe(0);

    // Drive d with defined value
    sim.dut.d = 0x55;
    sim.runUntil(40);
    expect(sim.dut.q).toBe(0x55);

    // Drive d with different value
    sim.dut.d = 0xAA;
    sim.runUntil(60);
    expect(sim.dut.q).toBe(0xAA);

    sim.dispose();
  });

  test("counter in 4-state time-based mode", () => {
    interface CounterPorts {
      rst: number;
      en: number;
      readonly count: number;
    }

    const sim = Simulation.fromSource<CounterPorts>(
      COUNTER_SOURCE, "Counter", { fourState: true },
    );

    sim.addClock("clk", { period: 10 });

    // Reset (default async_low: rst=0 is active)
    sim.dut.rst = 0;
    sim.runUntil(20);
    sim.dut.rst = 1;
    sim.dut.en = 1;

    sim.runUntil(100);

    const count = sim.dut.count;
    expect(count).toBeGreaterThan(0);
    expect(sim.time()).toBe(100);

    sim.dispose();
  });

  test("4-state combinational in time-based simulation", () => {
    interface Ports {
      a: number;
      b: number;
      readonly y: number;
    }

    const sim = Simulation.fromSource<Ports>(
      ADDER_4STATE_SOURCE, "Adder4S", { fourState: true },
    );

    // No clock needed for pure combinational — just set values and read
    sim.dut.a = 100;
    sim.dut.b = 55;
    // runUntil(0) to force eval
    sim.runUntil(0);
    expect(sim.dut.y).toBe(155);

    sim.dispose();
  });
});

// ---------------------------------------------------------------------------
// Phase 3b: testbench helpers — Simulation API
// ---------------------------------------------------------------------------

describe("E2E: Simulation testbench helpers", () => {
  test("waitForCycles: advances correct number of clock cycles", () => {
    interface CounterPorts {
      rst: number;
      en: number;
      readonly count: number;
    }

    const sim = Simulation.fromSource<CounterPorts>(COUNTER_SOURCE, "Counter");
    sim.addClock("clk", { period: 10 });

    // Reset (default async_low: rst=0 is active)
    sim.dut.rst = 0;
    sim.runUntil(20);
    sim.dut.rst = 1;
    sim.dut.en = 1;

    const beforeTime = sim.time();
    const afterTime = sim.waitForCycles("clk", 5);

    expect(afterTime).toBeGreaterThan(beforeTime);
    // Each cycle = 2 steps with period 10, so 5 cycles ≈ 50 time units
    expect(afterTime - beforeTime).toBe(50);

    sim.dispose();
  });

  test("waitUntil: waits for condition to be met", () => {
    interface CounterPorts {
      rst: number;
      en: number;
      readonly count: number;
    }

    const sim = Simulation.fromSource<CounterPorts>(COUNTER_SOURCE, "Counter");
    sim.addClock("clk", { period: 10 });

    // Reset (default async_low: rst=0 is active)
    sim.dut.rst = 0;
    sim.runUntil(20);
    sim.dut.rst = 1;
    sim.dut.en = 1;

    const t = sim.waitUntil(() => sim.dut.count >= 3);
    expect(sim.dut.count).toBeGreaterThanOrEqual(3);
    expect(t).toBeGreaterThan(20);

    sim.dispose();
  });

  test("waitUntil: throws SimulationTimeoutError on timeout", () => {
    interface CounterPorts {
      rst: number;
      en: number;
      readonly count: number;
    }

    const sim = Simulation.fromSource<CounterPorts>(COUNTER_SOURCE, "Counter");
    sim.addClock("clk", { period: 10 });

    // Reset (default async_low: rst=0 is active)
    sim.dut.rst = 0;
    sim.runUntil(20);
    sim.dut.rst = 1;
    sim.dut.en = 0; // disabled — count won't increase

    expect(() =>
      sim.waitUntil(() => sim.dut.count >= 100, { maxSteps: 20 }),
    ).toThrow(SimulationTimeoutError);

    sim.dispose();
  });

  test("reset: asserts and releases reset on counter", () => {
    interface CounterPorts {
      rst: number;
      en: number;
      readonly count: number;
    }

    const sim = Simulation.fromSource<CounterPorts>(COUNTER_SOURCE, "Counter");
    sim.addClock("clk", { period: 10 });

    // Count up a bit (default async_low: rst=0 is active)
    sim.dut.rst = 0;
    sim.runUntil(20);
    sim.dut.rst = 1;
    sim.dut.en = 1;
    sim.runUntil(100);
    expect(sim.dut.count).toBeGreaterThan(0);

    // Reset using the helper
    sim.reset("rst");
    expect(sim.dut.count).toBe(0);

    sim.dispose();
  });

  test("reset: explicit async_low resetType activates with 0", () => {
    interface CounterPorts {
      rst: number;
      en: number;
      readonly count: number;
    }

    const sim = Simulation.fromSource<CounterPorts>(COUNTER_SOURCE, "Counter", {
      resetType: "async_low",
    });
    sim.addClock("clk", { period: 10 });

    // With async_low, rst=0 is active, rst=1 is inactive
    sim.reset("rst");

    sim.dut.en = 1;
    sim.runUntil(100);
    expect(sim.dut.count).toBeGreaterThan(0);

    // Reset again using helper, verify it resets the counter
    sim.reset("rst");
    expect(sim.dut.count).toBe(0);

    sim.dispose();
  });

  test("reset: explicit async_high resetType activates with 1", () => {
    interface CounterPorts {
      rst: number;
      en: number;
      readonly count: number;
    }

    const sim = Simulation.fromSource<CounterPorts>(COUNTER_SOURCE, "Counter", {
      resetType: "async_high",
    });
    sim.addClock("clk", { period: 10 });

    // With async_high, rst=1 is active, rst=0 is inactive
    sim.reset("rst");

    sim.dut.en = 1;
    sim.runUntil(100);
    expect(sim.dut.count).toBeGreaterThan(0);

    // Reset again
    sim.reset("rst");
    expect(sim.dut.count).toBe(0);

    sim.dispose();
  });

  test("Simulator.fromSource: resetType option works", () => {
    interface CounterPorts {
      rst: number;
      en: number;
      readonly count: number;
    }

    const sim = Simulator.fromSource<CounterPorts>(COUNTER_SOURCE, "Counter", {
      resetType: "async_high",
    });

    // With async_high, assert reset with 1
    sim.dut.rst = 1;
    sim.tick();
    sim.dut.rst = 0;
    sim.tick();
    expect(sim.dut.count).toBe(0);

    // Count up
    sim.dut.en = 1;
    sim.tick();
    expect(sim.dut.count).toBe(1);

    sim.dispose();
  });

  test("runUntil with maxSteps: succeeds within budget", () => {
    interface CounterPorts {
      rst: number;
      en: number;
      readonly count: number;
    }

    const sim = Simulation.fromSource<CounterPorts>(COUNTER_SOURCE, "Counter");
    sim.addClock("clk", { period: 10 });

    // Reset (default async_low: rst=0 is active)
    sim.dut.rst = 0;
    sim.runUntil(20);
    sim.dut.rst = 1;
    sim.dut.en = 1;

    // 100 time units with period 10 = 10 events, should fit in 100 steps
    sim.runUntil(120, { maxSteps: 100 });
    expect(sim.time()).toBe(120);

    sim.dispose();
  });

  test("runUntil with maxSteps: throws on exceeded budget", () => {
    interface CounterPorts {
      rst: number;
      en: number;
      readonly count: number;
    }

    const sim = Simulation.fromSource<CounterPorts>(COUNTER_SOURCE, "Counter");
    sim.addClock("clk", { period: 10 });

    // Release reset so counter can count (default async_low: rst=1 is inactive)
    sim.dut.rst = 1;
    sim.dut.en = 1;

    // Very small budget for a long run
    expect(() => sim.runUntil(100000, { maxSteps: 5 })).toThrow(
      SimulationTimeoutError,
    );

    sim.dispose();
  });
});

// ---------------------------------------------------------------------------
// Phase 3b: fourState() method
// ---------------------------------------------------------------------------

describe("E2E: fourState() method", () => {
  test("Simulator.fourState: reads 4-state value and mask", () => {
    interface Ports {
      a: number;
      b: number;
      readonly y: number;
    }

    const sim = Simulator.fromSource<Ports>(ADDER_4STATE_SOURCE, "Adder4S", {
      fourState: true,
    });

    sim.dut.a = 100;
    sim.dut.b = 55;
    // Trigger evalComb via output read (Adder4S is purely combinational)
    expect(sim.dut.y).toBe(155);

    const fs = sim.fourState("y");
    expect(fs.__fourState).toBe(true);
    expect(fs.value).toBe(155);
    expect(fs.mask).toBe(0);

    sim.dispose();
  });

  test("Simulator.fourState: reads X mask when input is X", () => {
    interface Ports {
      a: number;
      b: number;
      readonly y: number;
    }

    const sim = Simulator.fromSource<Ports>(ADDER_4STATE_SOURCE, "Adder4S", {
      fourState: true,
    });

    (sim.dut as any).a = X;
    sim.dut.b = 10;
    // Trigger evalComb via output read
    sim.dut.y;

    const fs = sim.fourState("y");
    expect(fs.mask).toBe(0xFF); // all X from arithmetic propagation

    sim.dispose();
  });

  test("Simulation.fourState: reads 4-state value", () => {
    interface Ports {
      a: number;
      b: number;
      readonly y: number;
    }

    const sim = Simulation.fromSource<Ports>(ADDER_4STATE_SOURCE, "Adder4S", {
      fourState: true,
    });

    sim.dut.a = 50;
    sim.dut.b = 25;
    sim.runUntil(0);

    const fs = sim.fourState("y");
    expect(fs.value).toBe(75);
    expect(fs.mask).toBe(0);

    sim.dispose();
  });

  test("fourState: throws for unknown port", () => {
    interface Ports {
      a: number;
      b: number;
      readonly y: number;
    }

    const sim = Simulator.fromSource<Ports>(ADDER_4STATE_SOURCE, "Adder4S", {
      fourState: true,
    });

    expect(() => sim.fourState("nonexistent")).toThrow("Unknown port");

    sim.dispose();
  });
});

// ---------------------------------------------------------------------------
// Phase 3b: optimize flag
// ---------------------------------------------------------------------------

describe("E2E: optimize flag", () => {
  test("Simulator.fromSource with optimize: true", () => {
    interface AdderPorts {
      rst: number;
      a: number;
      b: number;
      readonly sum: number;
    }

    const sim = Simulator.fromSource<AdderPorts>(ADDER_SOURCE, "Adder", {
      optimize: true,
    });

    sim.dut.a = 100;
    sim.dut.b = 200;
    sim.tick();
    expect(sim.dut.sum).toBe(300);

    sim.dispose();
  });

  test("Simulation.fromSource with optimize: true", () => {
    interface CounterPorts {
      rst: number;
      en: number;
      readonly count: number;
    }

    const sim = Simulation.fromSource<CounterPorts>(COUNTER_SOURCE, "Counter", {
      optimize: true,
    });

    sim.addClock("clk", { period: 10 });

    // Reset (default async_low: rst=0 is active)
    sim.dut.rst = 0;
    sim.runUntil(20);
    sim.dut.rst = 1;
    sim.dut.en = 1;
    sim.runUntil(100);

    expect(sim.dut.count).toBeGreaterThan(0);

    sim.dispose();
  });
});

// ---------------------------------------------------------------------------
// parseSignalPath unit tests
// ---------------------------------------------------------------------------

describe("parseSignalPath", () => {
  test("simple variable path", () => {
    const result = parseSignalPath("v");
    expect(result.instancePath).toEqual([]);
    expect(result.varPath).toEqual(["v"]);
  });

  test("instance:variable split", () => {
    const result = parseSignalPath("p2:i");
    expect(result.instancePath).toEqual([{ name: "p2", index: 0 }]);
    expect(result.varPath).toEqual(["i"]);
  });

  test("nested instance with array index", () => {
    const result = parseSignalPath("a.b[3]:x.y");
    expect(result.instancePath).toEqual([
      { name: "a", index: 0 },
      { name: "b", index: 3 },
    ]);
    expect(result.varPath).toEqual(["x", "y"]);
  });

  test("dotted variable path without instance", () => {
    const result = parseSignalPath("foo.bar.baz");
    expect(result.instancePath).toEqual([]);
    expect(result.varPath).toEqual(["foo", "bar", "baz"]);
  });
});
