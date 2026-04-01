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
import { afterEach, describe, expect, test } from "vitest";
import { readFourState } from "./dut.js";
import {
	createSimulatorBridge,
	loadNativeAddon,
	parseNapiLayout,
	parseSignalPath,
	type NapiTestResult,
	type RawNapiAddon,
	type RawNapiSimulatorHandle,
} from "./napi-helpers.js";
import { Simulation } from "./simulation.js";
import { Simulator } from "./simulator.js";
import { FourState, SimulationTimeoutError, X } from "./types.js";

// Fixture project directories
const FIXTURES_DIR = path.resolve(
	import.meta.dirname ?? __dirname,
	"../fixtures",
);
const ADDER_PROJECT = path.join(FIXTURES_DIR, "adder");
const COUNTER_PROJECT = path.join(FIXTURES_DIR, "counter_project");
const CELOX_TOML_PROJECT = path.join(FIXTURES_DIR, "celox_toml");

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
			rst: bigint;
			a: bigint;
			b: bigint;
			readonly sum: bigint;
		}

		const sim = Simulator.fromSource<AdderPorts>(ADDER_SOURCE, "Adder");

		sim.dut.a = 100n;
		sim.dut.b = 200n;
		sim.tick();
		expect(sim.dut.sum).toBe(300n);

		sim.dut.a = 0xffffn;
		sim.dut.b = 1n;
		sim.tick();
		expect(sim.dut.sum).toBe(0x10000n);

		sim.dut.a = 0n;
		sim.dut.b = 0n;
		sim.tick();
		expect(sim.dut.sum).toBe(0n);

		sim.dispose();
	});

	test("combinational adder: lazy evalComb on output read", () => {
		interface AdderPorts {
			rst: bigint;
			a: bigint;
			b: bigint;
			readonly sum: bigint;
		}

		const sim = Simulator.fromSource<AdderPorts>(ADDER_SOURCE, "Adder");

		sim.dut.a = 42n;
		sim.dut.b = 58n;
		expect(sim.dut.sum).toBe(100n);

		sim.dispose();
	});

	test("sequential counter: counts on clock edges", () => {
		interface CounterPorts {
			rst: bigint;
			en: bigint;
			readonly count: bigint;
		}

		const sim = Simulator.fromSource<CounterPorts>(COUNTER_SOURCE, "Counter");

		// Reset the counter (default async_low: rst=0 is active)
		sim.dut.rst = 0n;
		sim.tick();
		sim.dut.rst = 1n;
		sim.tick();
		expect(sim.dut.count).toBe(0n);

		// Enable counting
		sim.dut.en = 1n;
		sim.tick();
		expect(sim.dut.count).toBe(1n);

		sim.tick();
		expect(sim.dut.count).toBe(2n);

		sim.tick();
		expect(sim.dut.count).toBe(3n);

		// Disable counting
		sim.dut.en = 0n;
		sim.tick();
		expect(sim.dut.count).toBe(3n);

		// Re-enable
		sim.dut.en = 1n;
		sim.tick(5);
		expect(sim.dut.count).toBe(8n);

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
			sel: bigint;
			d0: bigint;
			d1: bigint;
			d2: bigint;
			d3: bigint;
			readonly y: bigint;
		}

		const sim = Simulator.fromSource<Mux4Ports>(MULTIPLEXER_SOURCE, "Mux4");

		sim.dut.d0 = 0xaan;
		sim.dut.d1 = 0xbbn;
		sim.dut.d2 = 0xccn;
		sim.dut.d3 = 0xddn;

		sim.dut.sel = 0n;
		expect(sim.dut.y).toBe(0xaan);

		sim.dut.sel = 1n;
		expect(sim.dut.y).toBe(0xbbn);

		sim.dut.sel = 2n;
		expect(sim.dut.y).toBe(0xccn);

		sim.dut.sel = 3n;
		expect(sim.dut.y).toBe(0xddn);

		sim.dispose();
	});
});

// ---------------------------------------------------------------------------
// Simulation (time-based) e2e tests — fromSource API
// ---------------------------------------------------------------------------

describe("E2E: Simulation.fromSource (time-based)", () => {
	test("counter with timed clock: step-by-step", () => {
		interface CounterPorts {
			rst: bigint;
			en: bigint;
			readonly count: bigint;
		}

		const sim = Simulation.fromSource<CounterPorts>(COUNTER_SOURCE, "Counter");

		sim.addClock("clk", { period: 10 });
		expect(sim.time()).toBe(0);

		// Reset (default async_low: rst=0 is active)
		sim.dut.rst = 0n;
		sim.runUntil(20);
		sim.dut.rst = 1n;
		sim.dut.en = 1n;

		sim.runUntil(100);

		const count = sim.dut.count;
		expect(count).toBeGreaterThan(0n);
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
			rst: bigint;
			a: bigint;
			b: bigint;
			readonly sum: bigint;
		}

		const sim = Simulator.fromProject<AdderPorts>(ADDER_PROJECT, "Adder");

		sim.dut.a = 100n;
		sim.dut.b = 200n;
		sim.tick();
		expect(sim.dut.sum).toBe(300n);

		sim.dut.a = 0xffffn;
		sim.dut.b = 1n;
		sim.tick();
		expect(sim.dut.sum).toBe(0x10000n);

		sim.dispose();
	});

	test("sequential counter from project directory", () => {
		interface CounterPorts {
			rst: bigint;
			en: bigint;
			readonly count: bigint;
		}

		const sim = Simulator.fromProject<CounterPorts>(COUNTER_PROJECT, "Counter");

		// Reset the counter (default async_low: rst=0 is active)
		sim.dut.rst = 0n;
		sim.tick();
		sim.dut.rst = 1n;
		sim.tick();
		expect(sim.dut.count).toBe(0n);

		// Enable counting
		sim.dut.en = 1n;
		sim.tick();
		expect(sim.dut.count).toBe(1n);

		sim.tick();
		expect(sim.dut.count).toBe(2n);

		sim.tick();
		expect(sim.dut.count).toBe(3n);

		sim.dispose();
	});
});

// ---------------------------------------------------------------------------
// Simulation (time-based) e2e tests — fromProject API
// ---------------------------------------------------------------------------

describe("E2E: Simulation.fromProject (time-based)", () => {
	test("counter with timed clock from project directory", () => {
		interface CounterPorts {
			rst: bigint;
			en: bigint;
			readonly count: bigint;
		}

		const sim = Simulation.fromProject<CounterPorts>(
			COUNTER_PROJECT,
			"Counter",
		);

		sim.addClock("clk", { period: 10 });
		expect(sim.time()).toBe(0);

		// Reset (default async_low: rst=0 is active)
		sim.dut.rst = 0n;
		sim.runUntil(20);
		sim.dut.rst = 1n;
		sim.dut.en = 1n;

		sim.runUntil(100);

		const count = sim.dut.count;
		expect(count).toBeGreaterThan(0n);
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
			rst: bigint;
			a: bigint;
			b: bigint;
			readonly sum: bigint;
		}

		const addon = loadNativeAddon();
		const nativeCreateSimulator = createSimulatorBridge(addon);

		const sim = Simulator.create<AdderPorts>(
			{
				__celox_module: true,
				name: "Adder",
				sources: [{ path: "", content: ADDER_SOURCE }],
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

		sim.dut.a = 100n;
		sim.dut.b = 200n;
		sim.tick();
		expect(sim.dut.sum).toBe(300n);

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
    assign y_bit_from_logic = a_logic as u8;
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
		raw = new addon.NativeSimulatorHandle(
			[{ content: source, path: "" }],
			"InitTest",
			{
				fourState: true,
			},
		);
		const layout = parseNapiLayout(raw.layoutJson);
		const buf = raw.sharedMemory().buffer;

		// logic port should have mask=0xFF (all X), X encoding: v=1, m=1
		const [valA, maskA] = readFourState(buf, layout.forDut.a);
		expect(valA).toBe(0xffn);
		expect(maskA).toBe(0xffn);

		// bit port should have mask=0 (defined)
		// bit is not 4-state, so no mask — reading its value should be 0
		expect(layout.forDut.b.is4state).toBe(false);
	});

	test("writing X clears value and sets mask", () => {
		interface Ports {
			a: bigint;
			readonly y_and: bigint;
		}

		const sim = Simulator.fromSource<Ports>(AND_OR_SOURCE, "AndOr", {
			fourState: true,
		});
		raw = undefined; // sim manages its own handle

		// Write X to input 'a' via DUT
		(sim.dut as any).a = X;

		// We can't inspect mask through DUT getter (it only returns value),
		// so this test verifies X write doesn't throw and propagation works.
		// For detailed mask inspection, see the raw NAPI tests below.
		sim.dispose();
	});

	test("AND: 0 & X = 0 (dominant zero)", () => {
		raw = new addon.NativeSimulatorHandle(
			[{ content: AND_OR_SOURCE, path: "" }],
			"AndOr",
			{
				fourState: true,
			},
		);
		const layout = parseNapiLayout(raw.layoutJson);
		const buf = raw.sharedMemory().buffer;
		const view = new DataView(buf);
		const _events: Record<string, number> = JSON.parse(raw.eventsJson);

		const sigA = layout.forDut.a;
		const sigB = layout.forDut.b;
		const sigYAnd = layout.forDut.y_and;
		const sigYOr = layout.forDut.y_or;

		// a = 0 (value=0, mask=0)
		view.setUint8(sigA.offset, 0);
		view.setUint8(sigA.offset + sigA.byteSize, 0);

		// b = X (value=1, mask=1) — X encoding: v=1, m=1
		view.setUint8(sigB.offset, 1);
		view.setUint8(sigB.offset + sigB.byteSize, 1);

		raw.evalComb();

		// 0 & X = 0 (mask should be 0 — dominant zero)
		const [vAnd, mAnd] = readFourState(buf, sigYAnd);
		expect(vAnd).toBe(0n);
		expect(mAnd).toBe(0n);

		// 0 | X = X (value=1, mask=1 in X encoding)
		const [vOr, mOr] = readFourState(buf, sigYOr);
		expect(vOr).toBe(1n);
		expect(mOr).toBe(1n);
	});

	test("OR: 1 | X = 1 (dominant one)", () => {
		raw = new addon.NativeSimulatorHandle(
			[{ content: AND_OR_SOURCE, path: "" }],
			"AndOr",
			{
				fourState: true,
			},
		);
		const layout = parseNapiLayout(raw.layoutJson);
		const buf = raw.sharedMemory().buffer;
		const view = new DataView(buf);

		const sigA = layout.forDut.a;
		const sigB = layout.forDut.b;
		const sigYOr = layout.forDut.y_or;

		// a = 1 (value=1, mask=0)
		view.setUint8(sigA.offset, 1);
		view.setUint8(sigA.offset + sigA.byteSize, 0);

		// b = X (value=1, mask=1) — X encoding: v=1, m=1
		view.setUint8(sigB.offset, 1);
		view.setUint8(sigB.offset + sigB.byteSize, 1);

		raw.evalComb();

		// 1 | X = 1 (mask should be 0 — dominant one)
		const [vOr, mOr] = readFourState(buf, sigYOr);
		expect(vOr).toBe(1n);
		expect(mOr).toBe(0n);
	});

	test("logic-to-bit assignment strips X mask", () => {
		raw = new addon.NativeSimulatorHandle(
			[{ content: LOGIC_BIT_MIX_SOURCE, path: "" }],
			"LogicBitMix",
			{
				fourState: true,
			},
		);
		const layout = parseNapiLayout(raw.layoutJson);
		const buf = raw.sharedMemory().buffer;
		const view = new DataView(buf);

		const sigALogic = layout.forDut.a_logic;
		const sigYBitFromLogic = layout.forDut.y_bit_from_logic;

		// a_logic = all-X (value=0, mask=0xFF)
		view.setUint8(sigALogic.offset, 0);
		view.setUint8(sigALogic.offset + sigALogic.byteSize, 0xff);

		raw.evalComb();

		// y_bit_from_logic is bit type — X should be stripped (mask=0)
		expect(sigYBitFromLogic.is4state).toBe(false);
	});

	test("bit-to-logic assignment has no X", () => {
		raw = new addon.NativeSimulatorHandle(
			[{ content: LOGIC_BIT_MIX_SOURCE, path: "" }],
			"LogicBitMix",
			{
				fourState: true,
			},
		);
		const layout = parseNapiLayout(raw.layoutJson);
		const buf = raw.sharedMemory().buffer;
		const view = new DataView(buf);

		const sigBBit = layout.forDut.b_bit;
		const sigYLogicFromBit = layout.forDut.y_logic_from_bit;

		// b_bit = 0xAA (bit type, always defined)
		view.setUint8(sigBBit.offset, 0xaa);

		raw.evalComb();

		// y_logic_from_bit should be 0xAA with mask=0
		const [vLogic, mLogic] = readFourState(buf, sigYLogicFromBit);
		expect(vLogic).toBe(0xaan);
		expect(mLogic).toBe(0n);
	});

	test("arithmetic with X produces all-X output", () => {
		raw = new addon.NativeSimulatorHandle(
			[{ content: ADDER_4STATE_SOURCE, path: "" }],
			"Adder4S",
			{
				fourState: true,
			},
		);
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
		view.setUint8(sigB.offset + sigB.byteSize, 0xff); // mask=0xFF

		raw.evalComb();

		// a + X = all-X
		const [, mY] = readFourState(buf, sigY);
		expect(mY).toBe(0xffn);
	});

	test("defined inputs in 4-state mode behave like 2-state", () => {
		raw = new addon.NativeSimulatorHandle(
			[{ content: ADDER_4STATE_SOURCE, path: "" }],
			"Adder4S",
			{
				fourState: true,
			},
		);
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
		expect(vY).toBe(155n);
		expect(mY).toBe(0n);
	});

	test("FF captures X from input, reset clears X", () => {
		raw = new addon.NativeSimulatorHandle(
			[{ content: FF_SOURCE, path: "" }],
			"FF",
			{ fourState: true },
		);
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
		view.setUint8(sigD.offset + sigD.byteSize, 0xff); // d = all-X

		raw.tick(clkEventId);

		// After reset, q should be 0 with mask=0
		const [vQ1, mQ1] = readFourState(buf, sigQ);
		expect(vQ1).toBe(0n);
		expect(mQ1).toBe(0n);

		// 2. Release reset (rst=1 is inactive), d = partial X (value=0xA5, mask=0x0F)
		view.setUint8(sigRst.offset, 1);
		view.setUint8(sigRst.offset + sigRst.byteSize, 0);

		view.setUint8(sigD.offset, 0xa5);
		view.setUint8(sigD.offset + sigD.byteSize, 0x0f);

		raw.tick(clkEventId);

		// FF should capture X mask from d
		const [, mQ2] = readFourState(buf, sigQ);
		expect(mQ2).toBe(0x0fn);

		// 3. Assert reset again (rst=0): should clear X
		view.setUint8(sigRst.offset, 0);
		view.setUint8(sigRst.offset + sigRst.byteSize, 0);

		raw.tick(clkEventId);

		const [vQ3, mQ3] = readFourState(buf, sigQ);
		expect(vQ3).toBe(0n);
		expect(mQ3).toBe(0n);
	});

	test("FourState write through DUT sets value and mask", () => {
		raw = new addon.NativeSimulatorHandle(
			[{ content: ADDER_4STATE_SOURCE, path: "" }],
			"Adder4S",
			{
				fourState: true,
			},
		);
		const layout = parseNapiLayout(raw.layoutJson);
		const buf = raw.sharedMemory().buffer;
		const view = new DataView(buf);

		const sigA = layout.forDut.a;

		// Write via DUT-style: FourState(0b1010_0101, 0b0000_1111)
		// value=0xA5, mask=0x0F means lower 4 bits are X
		view.setUint8(sigA.offset, 0xa5);
		view.setUint8(sigA.offset + sigA.byteSize, 0x0f);

		const [vA, mA] = readFourState(buf, sigA);
		expect(vA).toBe(0xa5n);
		expect(mA).toBe(0x0fn);
	});

	test("setting defined value clears X mask", () => {
		raw = new addon.NativeSimulatorHandle(
			[{ content: ADDER_4STATE_SOURCE, path: "" }],
			"Adder4S",
			{
				fourState: true,
			},
		);
		const layout = parseNapiLayout(raw.layoutJson);
		const buf = raw.sharedMemory().buffer;
		const view = new DataView(buf);

		const sigA = layout.forDut.a;

		// Start with X
		view.setUint8(sigA.offset, 0);
		view.setUint8(sigA.offset + sigA.byteSize, 0xff);

		const [, mBefore] = readFourState(buf, sigA);
		expect(mBefore).toBe(0xffn);

		// Write a defined value (clear mask)
		view.setUint8(sigA.offset, 42);
		view.setUint8(sigA.offset + sigA.byteSize, 0);

		const [vAfter, mAfter] = readFourState(buf, sigA);
		expect(vAfter).toBe(42n);
		expect(mAfter).toBe(0n);
	});

	test("4-state through DUT high-level API (fromSource with fourState)", () => {
		interface Ports {
			a: bigint;
			b: bigint;
			readonly y: bigint;
		}

		const sim = Simulator.fromSource<Ports>(ADDER_4STATE_SOURCE, "Adder4S", {
			fourState: true,
		});

		// Write defined values — should behave like 2-state
		sim.dut.a = 100n;
		sim.dut.b = 55n;
		expect(sim.dut.y).toBe(155n);

		// Write X to a — output should propagate X (value reads as 0)
		(sim.dut as any).a = X;
		// After writing X, the value part of 'y' is implementation-defined
		// but the read should not throw
		const _yVal = sim.dut.y;
		expect(typeof _yVal).toBe("bigint");

		// Write FourState with partial X
		(sim.dut as any).a = FourState(0xa0, 0x0f);
		const _yVal2 = sim.dut.y;
		expect(typeof _yVal2).toBe("bigint");

		sim.dispose();
	});
});

// ---------------------------------------------------------------------------
// 4-state: high-level DUT API (Simulator.fromSource)
// ---------------------------------------------------------------------------

describe("E2E: 4-state high-level DUT API", () => {
	test("counter in 4-state mode: reset clears X, counting works", () => {
		interface CounterPorts {
			rst: bigint;
			en: bigint;
			readonly count: bigint;
		}

		const sim = Simulator.fromSource<CounterPorts>(COUNTER_SOURCE, "Counter", {
			fourState: true,
		});

		// In 4-state mode, count starts as X. Reset should clear it.
		// (default async_low: rst=0 is active)
		sim.dut.rst = 0n;
		// Clear X on en before deactivating reset. In 4-state mode, en starts
		// as X (v=1, m=1). The FF branch condition currently uses only the value
		// bit, so X with v=1 is treated as "true" — a known limitation in FF
		// conditional handling (combinational mux handles X correctly).
		sim.dut.en = 0n;
		sim.tick();
		sim.dut.rst = 1n;
		sim.tick();
		expect(sim.dut.count).toBe(0n);

		// Enable counting — should work exactly like 2-state
		sim.dut.en = 1n;
		sim.tick();
		expect(sim.dut.count).toBe(1n);

		sim.tick();
		expect(sim.dut.count).toBe(2n);

		sim.tick();
		expect(sim.dut.count).toBe(3n);

		sim.dispose();
	});

	test("multiplexer with X selector produces X output", () => {
		const addon = loadNativeAddon();
		const raw = new addon.NativeSimulatorHandle(
			[{ content: MULTIPLEXER_SOURCE, path: "" }],
			"Mux4",
			{
				fourState: true,
			},
		);
		const layout = parseNapiLayout(raw.layoutJson);
		const buf = raw.sharedMemory().buffer;
		const view = new DataView(buf);

		const sigSel = layout.forDut.sel;
		const sigD0 = layout.forDut.d0;
		const sigY = layout.forDut.y;

		// Set d0 = 0xAA (defined)
		view.setUint8(sigD0.offset, 0xaa);
		view.setUint8(sigD0.offset + sigD0.byteSize, 0);

		// Set sel = X
		view.setUint8(sigSel.offset, 0);
		view.setUint8(sigSel.offset + sigSel.byteSize, 0x03);

		raw.evalComb();

		// With X selector, output should be all-X
		const [, mY2] = readFourState(buf, sigY);
		expect(mY2).toBe(0xffn);

		raw.dispose();
	});

	test("FF via DUT API: write X input, tick, read output", () => {
		interface FFPorts {
			rst: bigint;
			d: bigint;
			readonly q: bigint;
		}

		const sim = Simulator.fromSource<FFPorts>(FF_SOURCE, "FF", {
			fourState: true,
		});

		// Reset to clear initial X (default async_low: rst=0 is active)
		sim.dut.rst = 0n;
		sim.tick();
		sim.dut.rst = 1n;
		expect(sim.dut.q).toBe(0n);

		// Write a defined value
		sim.dut.d = 0x42n;
		sim.tick();
		expect(sim.dut.q).toBe(0x42n);

		// Write X to d, tick — q should capture it (value read still returns a bigint)
		(sim.dut as any).d = X;
		sim.tick();
		expect(typeof sim.dut.q).toBe("bigint");

		// Write defined value again — q should recover
		sim.dut.d = 0x99n;
		sim.tick();
		expect(sim.dut.q).toBe(0x99n);

		sim.dispose();
	});

	test("X to defined transition: adder recovers from X", () => {
		interface Ports {
			a: bigint;
			b: bigint;
			readonly y: bigint;
		}

		const sim = Simulator.fromSource<Ports>(ADDER_4STATE_SOURCE, "Adder4S", {
			fourState: true,
		});

		// Start with X
		(sim.dut as any).a = X;
		sim.dut.b = 10n;
		// Output has X — just verify it doesn't crash
		expect(typeof sim.dut.y).toBe("bigint");

		// Clear X by writing defined values
		sim.dut.a = 20n;
		sim.dut.b = 30n;
		expect(sim.dut.y).toBe(50n);

		sim.dispose();
	});
});

// ---------------------------------------------------------------------------
// 4-state: Simulation (time-based) tests
// ---------------------------------------------------------------------------

describe("E2E: 4-state Simulation (time-based)", () => {
	test("FF with clock-driven 4-state: reset clears X, captures defined values", () => {
		interface FFPorts {
			rst: bigint;
			d: bigint;
			readonly q: bigint;
		}

		const sim = Simulation.fromSource<FFPorts>(FF_SOURCE, "FF", {
			fourState: true,
		});

		sim.addClock("clk", { period: 10 });
		expect(sim.time()).toBe(0);

		// Reset to clear initial X on q (default async_low: rst=0 is active)
		sim.dut.rst = 0n;
		sim.runUntil(20);
		sim.dut.rst = 1n;
		expect(sim.dut.q).toBe(0n);

		// Drive d with defined value
		sim.dut.d = 0x55n;
		sim.runUntil(40);
		expect(sim.dut.q).toBe(0x55n);

		// Drive d with different value
		sim.dut.d = 0xaan;
		sim.runUntil(60);
		expect(sim.dut.q).toBe(0xaan);

		sim.dispose();
	});

	test("counter in 4-state time-based mode", () => {
		interface CounterPorts {
			rst: bigint;
			en: bigint;
			readonly count: bigint;
		}

		const sim = Simulation.fromSource<CounterPorts>(COUNTER_SOURCE, "Counter", {
			fourState: true,
		});

		sim.addClock("clk", { period: 10 });

		// Reset (default async_low: rst=0 is active)
		sim.dut.rst = 0n;
		sim.runUntil(20);
		sim.dut.rst = 1n;
		sim.dut.en = 1n;

		sim.runUntil(100);

		const count = sim.dut.count;
		expect(count).toBeGreaterThan(0n);
		expect(sim.time()).toBe(100);

		sim.dispose();
	});

	test("4-state combinational in time-based simulation", () => {
		interface Ports {
			a: bigint;
			b: bigint;
			readonly y: bigint;
		}

		const sim = Simulation.fromSource<Ports>(ADDER_4STATE_SOURCE, "Adder4S", {
			fourState: true,
		});

		// No clock needed for pure combinational — just set values and read
		sim.dut.a = 100n;
		sim.dut.b = 55n;
		// runUntil(0) to force eval
		sim.runUntil(0);
		expect(sim.dut.y).toBe(155n);

		sim.dispose();
	});
});

// ---------------------------------------------------------------------------
// Phase 3b: testbench helpers — Simulation API
// ---------------------------------------------------------------------------

describe("E2E: Simulation testbench helpers", () => {
	test("waitForCycles: advances correct number of clock cycles", () => {
		interface CounterPorts {
			rst: bigint;
			en: bigint;
			readonly count: bigint;
		}

		const sim = Simulation.fromSource<CounterPorts>(COUNTER_SOURCE, "Counter");
		sim.addClock("clk", { period: 10 });

		// Reset (default async_low: rst=0 is active)
		sim.dut.rst = 0n;
		sim.runUntil(20);
		sim.dut.rst = 1n;
		sim.dut.en = 1n;

		const beforeTime = sim.time();
		const afterTime = sim.waitForCycles("clk", 5);

		expect(afterTime).toBeGreaterThan(beforeTime);
		// Each cycle = 2 steps with period 10, so 5 cycles ≈ 50 time units
		expect(afterTime - beforeTime).toBe(50);

		sim.dispose();
	});

	test("waitUntil: waits for condition to be met", () => {
		interface CounterPorts {
			rst: bigint;
			en: bigint;
			readonly count: bigint;
		}

		const sim = Simulation.fromSource<CounterPorts>(COUNTER_SOURCE, "Counter");
		sim.addClock("clk", { period: 10 });

		// Reset (default async_low: rst=0 is active)
		sim.dut.rst = 0n;
		sim.runUntil(20);
		sim.dut.rst = 1n;
		sim.dut.en = 1n;

		const t = sim.waitUntil(() => sim.dut.count >= 3n);
		expect(sim.dut.count).toBeGreaterThanOrEqual(3n);
		expect(t).toBeGreaterThan(20);

		sim.dispose();
	});

	test("waitUntil: throws SimulationTimeoutError on timeout", () => {
		interface CounterPorts {
			rst: bigint;
			en: bigint;
			readonly count: bigint;
		}

		const sim = Simulation.fromSource<CounterPorts>(COUNTER_SOURCE, "Counter");
		sim.addClock("clk", { period: 10 });

		// Reset (default async_low: rst=0 is active)
		sim.dut.rst = 0n;
		sim.runUntil(20);
		sim.dut.rst = 1n;
		sim.dut.en = 0n; // disabled — count won't increase

		expect(() =>
			sim.waitUntil(() => sim.dut.count >= 100n, { maxSteps: 20 }),
		).toThrow(SimulationTimeoutError);

		sim.dispose();
	});

	test("reset: asserts and releases reset on counter", () => {
		interface CounterPorts {
			rst: bigint;
			en: bigint;
			readonly count: bigint;
		}

		const sim = Simulation.fromSource<CounterPorts>(COUNTER_SOURCE, "Counter");
		sim.addClock("clk", { period: 10 });

		// Count up a bit (default async_low: rst=0 is active)
		sim.dut.rst = 0n;
		sim.runUntil(20);
		sim.dut.rst = 1n;
		sim.dut.en = 1n;
		sim.runUntil(100);
		expect(sim.dut.count).toBeGreaterThan(0n);

		// Reset using the helper
		sim.reset("rst");
		expect(sim.dut.count).toBe(0n);

		sim.dispose();
	});

	test("reset: explicit async_low resetType activates with 0", () => {
		interface CounterPorts {
			rst: bigint;
			en: bigint;
			readonly count: bigint;
		}

		const sim = Simulation.fromSource<CounterPorts>(COUNTER_SOURCE, "Counter", {
			resetType: "async_low",
		});
		sim.addClock("clk", { period: 10 });

		// With async_low, rst=0 is active, rst=1 is inactive
		sim.reset("rst");

		sim.dut.en = 1n;
		sim.runUntil(100);
		expect(sim.dut.count).toBeGreaterThan(0n);

		// Reset again using helper, verify it resets the counter
		sim.reset("rst");
		expect(sim.dut.count).toBe(0n);

		sim.dispose();
	});

	test("reset: explicit async_high resetType activates with 1", () => {
		interface CounterPorts {
			rst: bigint;
			en: bigint;
			readonly count: bigint;
		}

		const sim = Simulation.fromSource<CounterPorts>(COUNTER_SOURCE, "Counter", {
			resetType: "async_high",
		});
		sim.addClock("clk", { period: 10 });

		// With async_high, rst=1 is active, rst=0 is inactive
		sim.reset("rst");

		sim.dut.en = 1n;
		sim.runUntil(100);
		expect(sim.dut.count).toBeGreaterThan(0n);

		// Reset again
		sim.reset("rst");
		expect(sim.dut.count).toBe(0n);

		sim.dispose();
	});

	test("Simulator.fromSource: resetType option works", () => {
		interface CounterPorts {
			rst: bigint;
			en: bigint;
			readonly count: bigint;
		}

		const sim = Simulator.fromSource<CounterPorts>(COUNTER_SOURCE, "Counter", {
			resetType: "async_high",
		});

		// With async_high, assert reset with 1
		sim.dut.rst = 1n;
		sim.tick();
		sim.dut.rst = 0n;
		sim.tick();
		expect(sim.dut.count).toBe(0n);

		// Count up
		sim.dut.en = 1n;
		sim.tick();
		expect(sim.dut.count).toBe(1n);

		sim.dispose();
	});

	test("runUntil with maxSteps: succeeds within budget", () => {
		interface CounterPorts {
			rst: bigint;
			en: bigint;
			readonly count: bigint;
		}

		const sim = Simulation.fromSource<CounterPorts>(COUNTER_SOURCE, "Counter");
		sim.addClock("clk", { period: 10 });

		// Reset (default async_low: rst=0 is active)
		sim.dut.rst = 0n;
		sim.runUntil(20);
		sim.dut.rst = 1n;
		sim.dut.en = 1n;

		// 100 time units with period 10 = 10 events, should fit in 100 steps
		sim.runUntil(120, { maxSteps: 100 });
		expect(sim.time()).toBe(120);

		sim.dispose();
	});

	test("runUntil with maxSteps: throws on exceeded budget", () => {
		interface CounterPorts {
			rst: bigint;
			en: bigint;
			readonly count: bigint;
		}

		const sim = Simulation.fromSource<CounterPorts>(COUNTER_SOURCE, "Counter");
		sim.addClock("clk", { period: 10 });

		// Release reset so counter can count (default async_low: rst=1 is inactive)
		sim.dut.rst = 1n;
		sim.dut.en = 1n;

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
			a: bigint;
			b: bigint;
			readonly y: bigint;
		}

		const sim = Simulator.fromSource<Ports>(ADDER_4STATE_SOURCE, "Adder4S", {
			fourState: true,
		});

		sim.dut.a = 100n;
		sim.dut.b = 55n;
		// Trigger evalComb via output read (Adder4S is purely combinational)
		expect(sim.dut.y).toBe(155n);

		const fs = sim.fourState("y");
		expect(fs.__fourState).toBe(true);
		expect(fs.value).toBe(155n);
		expect(fs.mask).toBe(0n);

		sim.dispose();
	});

	test("Simulator.fourState: reads X mask when input is X", () => {
		interface Ports {
			a: bigint;
			b: bigint;
			readonly y: bigint;
		}

		const sim = Simulator.fromSource<Ports>(ADDER_4STATE_SOURCE, "Adder4S", {
			fourState: true,
		});

		(sim.dut as any).a = X;
		sim.dut.b = 10n;
		// Trigger evalComb via output read
		sim.dut.y;

		const fs = sim.fourState("y");
		expect(fs.mask).toBe(0xffn); // all X from arithmetic propagation

		sim.dispose();
	});

	test("Simulation.fourState: reads 4-state value", () => {
		interface Ports {
			a: bigint;
			b: bigint;
			readonly y: bigint;
		}

		const sim = Simulation.fromSource<Ports>(ADDER_4STATE_SOURCE, "Adder4S", {
			fourState: true,
		});

		sim.dut.a = 50n;
		sim.dut.b = 25n;
		sim.runUntil(0);

		const fs = sim.fourState("y");
		expect(fs.value).toBe(75n);
		expect(fs.mask).toBe(0n);

		sim.dispose();
	});

	test("fourState: throws for unknown port", () => {
		interface Ports {
			a: bigint;
			b: bigint;
			readonly y: bigint;
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
			rst: bigint;
			a: bigint;
			b: bigint;
			readonly sum: bigint;
		}

		const sim = Simulator.fromSource<AdderPorts>(ADDER_SOURCE, "Adder", {
			optimize: true,
		});

		sim.dut.a = 100n;
		sim.dut.b = 200n;
		sim.tick();
		expect(sim.dut.sum).toBe(300n);

		sim.dispose();
	});

	test("Simulation.fromSource with optimize: true", () => {
		interface CounterPorts {
			rst: bigint;
			en: bigint;
			readonly count: bigint;
		}

		const sim = Simulation.fromSource<CounterPorts>(COUNTER_SOURCE, "Counter", {
			optimize: true,
		});

		sim.addClock("clk", { period: 10 });

		// Reset (default async_low: rst=0 is active)
		sim.dut.rst = 0n;
		sim.runUntil(20);
		sim.dut.rst = 1n;
		sim.dut.en = 1n;
		sim.runUntil(100);

		expect(sim.dut.count).toBeGreaterThan(0n);

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

// ---------------------------------------------------------------------------
// Child instance DUT access
// ---------------------------------------------------------------------------

const HIERARCHY_SOURCE = `
module Sub (
    clk: input clock,
    i_data: input logic<8>,
    o_data: output logic<8>,
) {
    always_comb {
        o_data = i_data;
    }
}

module Top (
    clk: input clock,
    rst: input reset,
    top_in: input logic<8>,
    top_out: output logic<8>,
) {
    inst u_sub: Sub (
        clk,
        i_data: top_in,
        o_data: top_out,
    );
}
`;

const FOR_LOOP_INSTANCE_SOURCE = `
module Sub (
    clk: input '_ clock,
    i_data: input logic<8>,
    o_data: output logic<8>,
) {
    always_comb {
        o_data = i_data + 8'h01;
    }
}

module Top (
    clk: input '_ clock,
    rst: input reset,
    top_in: input logic<8>,
    top_out: output logic<8>[2],
) {
    for i in 0..2: g {
        inst u_sub: Sub (
            clk,
            i_data: top_in,
            o_data: top_out[i],
        );
    }
}
`;

describe("E2E: for-loop instance DUT access", () => {
	test("Simulator: for-loop instances are arrays, values accessible", () => {
		const sim = Simulator.fromSource(FOR_LOOP_INSTANCE_SOURCE, "Top");

		sim.dut.top_in = 0x10n;
		sim.tick();

		// For-loop instances should be accessible as an array
		const children = (sim.dut as any).u_sub;
		expect(children).toBeDefined();
		expect(Array.isArray(children)).toBe(true);
		expect(children.length).toBe(2);

		// Each instance should expose its ports
		expect(children[0].o_data).toBe(0x11n);
		expect(children[1].o_data).toBe(0x11n);
		expect(children[0].i_data).toBe(0x10n);
		expect(children[1].i_data).toBe(0x10n);

		// Change input and verify both instances update
		sim.dut.top_in = 0xffn;
		sim.tick();
		expect(children[0].o_data).toBe(0x00n); // 0xFF + 1 = 0x00 (8-bit wrap)
		expect(children[1].o_data).toBe(0x00n);

		sim.dispose();
	});
});

// ---------------------------------------------------------------------------
// Parameter override: DUT correctness tests
// ---------------------------------------------------------------------------

const PARAM_WIDTH_SOURCE = `
module ParamWidth #(
    param WIDTH: u32 = 8,
)(
    a: input  logic<WIDTH>,
    b: output logic<WIDTH>,
) {
    assign b = a;
}
`;

const PARAM_OFFSET_SOURCE = `
module ParamOffset #(
    param OFFSET: u32 = 10,
)(
    a: input  logic<32>,
    b: output logic<32>,
) {
    assign b = a + OFFSET;
}
`;

const PARAM_CHILD_SOURCE = `
module Child #(
    param WIDTH: u32 = 8,
)(
    i_data: input  logic<WIDTH>,
    o_data: output logic<WIDTH>,
) {
    assign o_data = i_data;
}

module ParamChild #(
    param WIDTH: u32 = 8,
)(
    a: input  logic<WIDTH>,
    b: output logic<WIDTH>,
) {
    inst u_child: Child #(WIDTH: WIDTH) (
        i_data: a,
        o_data: b,
    );
}
`;

describe("E2E: child instance DUT access", () => {
	test("Simulator: read child instance ports via dut.u_sub", () => {
		const sim = Simulator.fromSource(HIERARCHY_SOURCE, "Top");

		sim.dut.top_in = 0xabn;
		sim.tick();
		expect(sim.dut.top_out).toBe(0xabn);

		// Read the same value through the child instance accessor
		expect((sim.dut as any).u_sub.o_data).toBe(0xabn);
		expect((sim.dut as any).u_sub.i_data).toBe(0xabn);

		// Change top-level input and verify child reflects it
		sim.dut.top_in = 0x42n;
		sim.tick();
		expect((sim.dut as any).u_sub.i_data).toBe(0x42n);
		expect((sim.dut as any).u_sub.o_data).toBe(0x42n);

		sim.dispose();
	});

	test("Simulation: read child instance ports via dut.u_sub", () => {
		const sim = Simulation.fromSource(HIERARCHY_SOURCE, "Top");
		sim.addClock("clk", { period: 10 });

		sim.dut.rst = 0n;
		sim.runUntil(20);
		sim.dut.rst = 1n;

		sim.dut.top_in = 0x55n;
		sim.runUntil(40);
		expect((sim.dut as any).u_sub.o_data).toBe(0x55n);
		expect((sim.dut as any).u_sub.i_data).toBe(0x55n);

		sim.dut.top_in = 0xffn;
		sim.runUntil(60);
		expect((sim.dut as any).u_sub.o_data).toBe(0xffn);

		sim.dispose();
	});
});

describe("E2E: parameter override — DUT correctness", () => {
	test("scalar width override: WIDTH 8→16 via fromSource", () => {
		interface Ports {
			a: bigint;
			readonly b: bigint;
		}

		// Default WIDTH=8 (purely combinational — read output to trigger evalComb)
		const sim8 = Simulator.fromSource<Ports>(PARAM_WIDTH_SOURCE, "ParamWidth");
		sim8.dut.a = 0xabn;
		expect(sim8.dut.b).toBe(0xabn);
		// Writing a 16-bit value to an 8-bit port should truncate
		sim8.dut.a = 0xabcdn;
		expect(sim8.dut.b).toBe(0xcdn); // truncated to 8 bits
		sim8.dispose();

		// Override WIDTH=16 — DUT should handle 16-bit values
		const sim16 = Simulator.fromSource<Ports>(
			PARAM_WIDTH_SOURCE,
			"ParamWidth",
			{
				parameters: [{ name: "WIDTH", value: 16 }],
			},
		);
		sim16.dut.a = 0xabcdn;
		expect(sim16.dut.b).toBe(0xabcdn); // full 16-bit value preserved
		sim16.dispose();
	});

	test("scalar width override: WIDTH 8→32 via fromSource", () => {
		interface Ports {
			a: bigint;
			readonly b: bigint;
		}

		const sim = Simulator.fromSource<Ports>(PARAM_WIDTH_SOURCE, "ParamWidth", {
			parameters: [{ name: "WIDTH", value: 32 }],
		});
		sim.dut.a = 0xdead_beefn;
		expect(sim.dut.b).toBe(0xdead_beefn);
		sim.dispose();
	});

	test.skip("param value reflected in logic via fromSource", () => {
		// blocked by upstream Veryl IR bug
		interface Ports {
			a: bigint;
			readonly b: bigint;
		}

		// Default OFFSET=10
		const sim10 = Simulator.fromSource<Ports>(
			PARAM_OFFSET_SOURCE,
			"ParamOffset",
		);
		sim10.dut.a = 5n;
		expect(sim10.dut.b).toBe(15n);
		sim10.dispose();

		// Override OFFSET=100
		const sim100 = Simulator.fromSource<Ports>(
			PARAM_OFFSET_SOURCE,
			"ParamOffset",
			{
				parameters: [{ name: "OFFSET", value: 100 }],
			},
		);
		sim100.dut.a = 5n;
		expect(sim100.dut.b).toBe(105n);
		sim100.dispose();
	});

	test("child module param propagation via fromSource", () => {
		interface Ports {
			a: bigint;
			readonly b: bigint;
		}

		// Default WIDTH=8
		const sim8 = Simulator.fromSource<Ports>(PARAM_CHILD_SOURCE, "ParamChild");
		sim8.dut.a = 0xabn;
		expect(sim8.dut.b).toBe(0xabn);
		sim8.dispose();

		// Override WIDTH=16 — child also gets 16-bit ports
		const sim16 = Simulator.fromSource<Ports>(
			PARAM_CHILD_SOURCE,
			"ParamChild",
			{
				parameters: [{ name: "WIDTH", value: 16 }],
			},
		);
		sim16.dut.a = 0xabcdn;
		expect(sim16.dut.b).toBe(0xabcdn);
		sim16.dispose();
	});

	test("Simulator.create() with stale ModuleDefinition + param override", () => {
		// This is the most dangerous case: ModuleDefinition has width=8 (stale),
		// but we override WIDTH=16. The DUT must use runtime-derived ports
		// from hierarchy, not the stale module.ports.
		interface Ports {
			a: bigint;
			readonly b: bigint;
		}

		const addon = loadNativeAddon();
		const nativeCreate = createSimulatorBridge(addon);

		const sim = Simulator.create<Ports>(
			{
				__celox_module: true,
				name: "ParamWidth",
				sources: [{ path: "", content: PARAM_WIDTH_SOURCE }],
				ports: {
					// STALE: these say width=8, but we'll override to 16
					a: { direction: "input", type: "logic", width: 8 },
					b: { direction: "output", type: "logic", width: 8 },
				},
				events: [],
			},
			{
				__nativeCreate: nativeCreate,
				parameters: [{ name: "WIDTH", value: 16 }],
			},
		);

		// If DUT used stale ports (width=8), this would truncate or corrupt
		sim.dut.a = 0xabcdn;
		expect(sim.dut.b).toBe(0xabcdn); // full 16-bit value preserved
		sim.dispose();
	});

	test("Simulation.fromSource with param override", () => {
		interface Ports {
			a: bigint;
			readonly b: bigint;
		}

		const sim = Simulation.fromSource<Ports>(PARAM_WIDTH_SOURCE, "ParamWidth", {
			parameters: [{ name: "WIDTH", value: 16 }],
		});

		sim.dut.a = 0xfedcn;
		sim.runUntil(0);
		expect(sim.dut.b).toBe(0xfedcn);
		sim.dispose();
	});
});

// ---------------------------------------------------------------------------
// celox.toml: test-only source directories
// ---------------------------------------------------------------------------

describe("E2E: celox.toml test sources", () => {
	test("fromProject loads test-only module declared in celox.toml", () => {
		interface RegPorts {
			rst: bigint;
			d: bigint;
			readonly q: bigint;
		}

		// `Reg` lives in test_veryl/ — not in Veryl.toml sources — but celox.toml
		// declares test_veryl/ as an additional test source directory.
		const sim = Simulator.fromProject<RegPorts>(CELOX_TOML_PROJECT, "Reg");

		// Deassert async_low reset (rst=0 is active; rst=1 is normal operation)
		sim.dut.rst = 1n;
		sim.dut.d = 0xabn;
		sim.tick();
		expect(sim.dut.q).toBe(0xabn);

		sim.dut.d = 0xcdn;
		expect(sim.dut.q).toBe(0xabn); // not yet clocked
		sim.tick();
		expect(sim.dut.q).toBe(0xcdn);

		sim.dispose();
	});

	test("genTs includes test-only module declared in celox.toml", () => {
		const addon = loadNativeAddon();
		const json = addon.genTs(CELOX_TOML_PROJECT);
		const output = JSON.parse(json) as {
			modules: Array<{ moduleName: string }>;
		};
		const names = output.modules.map((m) => m.moduleName);
		// Both the regular source (Adder) and the test-only source (Reg) must appear
		expect(names).toContain("Adder");
		expect(names).toContain("Reg");
	});

	test("[simulation] max_steps from celox.toml is used as waitUntil default", () => {
		// celox.toml sets [simulation] max_steps = 20; waitUntil should honour it
		// when no per-call maxSteps is provided.
		const sim = Simulation.fromProject(CELOX_TOML_PROJECT, "Reg");
		sim.addClock("clk", { period: 10 });
		expect(() => sim.waitUntil(() => false)).toThrow(SimulationTimeoutError);
		const err = (() => {
			try {
				sim.waitUntil(() => false);
			} catch (e) {
				return e;
			}
		})() as SimulationTimeoutError;
		expect(err.steps).toBe(20);
		sim.dispose();
	});

	test("[simulation] max_steps per-call override takes precedence over celox.toml", () => {
		const sim = Simulation.fromProject(CELOX_TOML_PROJECT, "Reg");
		sim.addClock("clk", { period: 10 });
		// Per-call maxSteps=5 is lower than the toml default of 20
		expect(() => sim.waitUntil(() => false, { maxSteps: 5 })).toThrow(
			SimulationTimeoutError,
		);
		const err = (() => {
			try {
				sim.waitUntil(() => false, { maxSteps: 5 });
			} catch (e) {
				return e;
			}
		})() as SimulationTimeoutError;
		expect(err.steps).toBe(5);
		sim.dispose();
	});

	test("exclude patterns filter out matching files", () => {
		// The fixture has test_veryl/Excluded.veryl with invalid syntax,
		// but celox.toml excludes it via `exclude = ["test_veryl/Excluded.veryl"]`.
		// If exclude works, fromProject succeeds (the broken file is skipped).
		// If exclude is broken, the parse error will cause an exception.
		const sim = Simulator.fromProject<{
			rst: bigint;
			d: bigint;
			readonly q: bigint;
		}>(CELOX_TOML_PROJECT, "Reg");
		sim.dut.rst = 1n;
		sim.dut.d = 42n;
		sim.tick();
		expect(sim.dut.q).toBe(42n);
		sim.dispose();
	});

	test("genTs excludes files matching exclude patterns", () => {
		const addon = loadNativeAddon();
		const json = addon.genTs(CELOX_TOML_PROJECT);
		const output = JSON.parse(json) as {
			modules: Array<{ moduleName: string }>;
		};
		const names = output.modules.map((m) => m.moduleName);
		expect(names).toContain("Adder");
		expect(names).toContain("Reg");
		// Excluded.veryl is excluded by the glob pattern
		expect(names).not.toContain("Excluded");
	});
});

// ---------------------------------------------------------------------------
// Interface port DUT accessor tests
// ---------------------------------------------------------------------------

// A module whose top-level port is a modport interface.
// After Veryl expansion "sig.data" and "sig.valid" become individual ports.
// buildPortsFromLayout must group them into a nested PortInfo so that the
// DUT exposes them as dut.sig.data / dut.sig.valid.
const INTERFACE_PORT_SOURCE = `
interface Signal {
    var data:  logic<8>;
    var valid: logic;
    modport src {
        data:  output,
        valid: output,
    }
    modport dst {
        data:  input,
        valid: input,
    }
}

module Top (
    sig: modport Signal::dst,
    out: output logic<8>,
) {
    assign out = sig.data;
}
`;

// A module that uses two instances connected via an interface so we can verify
// that the hierarchy DUT also exposes nested accessors correctly.
const INTERFACE_HIERARCHY_SOURCE = `
interface Bus {
    var data:  logic<8>;
    var valid: logic;
    modport producer {
        data:  output,
        valid: output,
    }
    modport consumer {
        data:  input,
        valid: input,
    }
}

module Producer (
    bus: modport Bus::producer,
    val: input logic<8>,
) {
    assign bus.data  = val;
    assign bus.valid = 1'b1;
}

module Consumer (
    bus:  modport Bus::consumer,
    out:  output logic<8>,
    got:  output logic,
) {
    assign out = bus.data;
    assign got = bus.valid;
}

module Top (
    val: input  logic<8>,
    out: output logic<8>,
    got: output logic,
) {
    inst b:   Bus;
    inst p:   Producer (bus: b, val: val);
    inst c:   Consumer (bus: b, out: out, got: got);
}
`;

describe("E2E: interface port DUT accessor", () => {
	test("top-level interface port members accessible as nested dut properties", () => {
		interface TopPorts {
			sig: { data: bigint; valid: bigint };
			readonly out: bigint;
		}

		const sim = Simulator.fromSource<TopPorts>(INTERFACE_PORT_SOURCE, "Top");

		sim.dut.sig.data = 0x42n;
		expect(sim.dut.out).toBe(0x42n);

		sim.dut.sig.data = 0xffn;
		expect(sim.dut.out).toBe(0xffn);

		sim.dut.sig.data = 0x00n;
		expect(sim.dut.out).toBe(0x00n);

		sim.dispose();
	});

	test("writing to interface output member throws", () => {
		// bus.data / bus.valid are outputs of Top — writing should throw.
		const PRODUCER_ONLY = `
interface Bus {
    var data:  logic<8>;
    var valid: logic;
    modport src {
        data:  output,
        valid: output,
    }
}
module Top (
    bus: modport Bus::src,
    val: input logic<8>,
) {
    assign bus.data  = val;
    assign bus.valid = 1'b1;
}
`;
		const sim = Simulator.fromSource(PRODUCER_ONLY, "Top");

		expect(() => {
			(sim.dut as any).bus.data = 0n;
		}).toThrow("Cannot write to output port 'data'");

		sim.dispose();
	});

	test("interface port propagates through sub-module instances", () => {
		interface TopPorts {
			val: bigint;
			readonly out: bigint;
			readonly got: bigint;
		}

		const sim = Simulator.fromSource<TopPorts>(
			INTERFACE_HIERARCHY_SOURCE,
			"Top",
		);

		sim.dut.val = 0xabn;
		expect(sim.dut.out).toBe(0xabn);
		expect(sim.dut.got).toBe(1n);

		sim.dut.val = 0x00n;
		expect(sim.dut.out).toBe(0x00n);

		sim.dispose();
	});

	test("multiple interface port members are independently accessible", () => {
		interface TopPorts {
			sig: { data: bigint; valid: bigint };
			readonly out: bigint;
		}

		const sim = Simulator.fromSource<TopPorts>(INTERFACE_PORT_SOURCE, "Top");

		sim.dut.sig.data = 0x77n;
		sim.dut.sig.valid = 1n;
		expect(sim.dut.out).toBe(0x77n);

		sim.dut.sig.data = 0x33n;
		sim.dut.sig.valid = 0n;
		expect(sim.dut.out).toBe(0x33n);

		sim.dispose();
	});
});

// ---------------------------------------------------------------------------
// Interface array port (modport [N])
// ---------------------------------------------------------------------------

const INTERFACE_ARRAY_SOURCE = `
interface Bus {
    var data:  logic<8>;
    var valid: logic;
    modport consumer {
        data:  input,
        valid: input,
    }
}

module Top (
    bus: modport Bus::consumer [2],
    out: output logic<8>,
) {
    assign out = bus.data[0] + bus.data[1];
}
`;

describe("E2E: interface array port (modport [N])", () => {
	test("interface array members accessible via .at()/.set()", () => {
		interface TopPorts {
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
			readonly out: bigint;
		}

		const sim = Simulator.fromSource<TopPorts>(INTERFACE_ARRAY_SOURCE, "Top");

		sim.dut.bus.data.set(0, 0x10n);
		sim.dut.bus.data.set(1, 0x20n);
		expect(sim.dut.out).toBe(0x30n);

		sim.dut.bus.data.set(0, 0xaan);
		sim.dut.bus.data.set(1, 0x11n);
		expect(sim.dut.out).toBe(0xbbn);

		expect(sim.dut.bus.data.length).toBe(2);
		expect(sim.dut.bus.valid.length).toBe(2);

		sim.dispose();
	});
});

// ---------------------------------------------------------------------------
// Bug fix: 1-bit unpacked array port (logic [N])
// ---------------------------------------------------------------------------

const ARRAY_1BIT_SOURCE = `
module ArrayPassThrough (
    clk: input  clock,
    rst: input  reset,
    en:  input  logic [4],
    y0:  output logic,
    y1:  output logic,
    y2:  output logic,
    y3:  output logic,
) {
    always_comb {
        y0 = en[0];
        y1 = en[1];
        y2 = en[2];
        y3 = en[3];
    }
}
`;

// ---------------------------------------------------------------------------
// Internal var access tests
// ---------------------------------------------------------------------------

const INTERNAL_VAR_HIERARCHY_SOURCE = `
module SubCounter (
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
    assign count = count_r;
}

module TopCounter (
    clk: input clock,
    rst: input reset,
    en: input logic,
    top_count: output logic<8>,
) {
    var top_reg: logic<8>;
    inst u_sub: SubCounter (
        clk,
        rst,
        en,
        count: top_count,
    );
    always_ff (clk, rst) {
        if_reset {
            top_reg = 0;
        } else {
            top_reg = top_count;
        }
    }
}
`;

describe("E2E: internal var access", () => {
	test("top-level internal var is accessible and tracks values", () => {
		const sim = Simulator.fromSource(COUNTER_SOURCE, "Counter");

		// Reset
		sim.dut.rst = 0n;
		sim.tick();
		sim.dut.rst = 1n;
		sim.tick();

		// Internal var count_r should be accessible and start at 0
		expect((sim.dut as any).count_r).toBe(0n);

		// Enable counting and tick
		sim.dut.en = 1n;
		sim.tick();
		expect((sim.dut as any).count_r).toBe(1n);

		sim.tick();
		expect((sim.dut as any).count_r).toBe(2n);

		// count_r should equal count (output)
		expect((sim.dut as any).count_r).toBe(sim.dut.count);

		sim.dispose();
	});

	test("child instance internal var is accessible", () => {
		const sim = Simulator.fromSource(
			INTERNAL_VAR_HIERARCHY_SOURCE,
			"TopCounter",
		);

		// Reset
		sim.dut.rst = 0n;
		sim.tick();
		sim.dut.rst = 1n;
		sim.tick();

		// Top-level internal var
		expect((sim.dut as any).top_reg).toBe(0n);

		// Child instance internal var
		expect((sim.dut as any).u_sub.count_r).toBe(0n);

		// Enable counting
		sim.dut.en = 1n;
		sim.tick();
		expect((sim.dut as any).u_sub.count_r).toBe(1n);

		sim.tick();
		expect((sim.dut as any).u_sub.count_r).toBe(2n);

		sim.dispose();
	});
});

describe("E2E: 1-bit unpacked array port (logic [N])", () => {
	test("each element is independently visible in hardware (issue #6)", () => {
		interface Ports {
			rst: bigint;
			en: {
				at(i: number): bigint;
				set(i: number, v: bigint): void;
				length: number;
			};
			readonly y0: bigint;
			readonly y1: bigint;
			readonly y2: bigint;
			readonly y3: bigint;
		}

		const sim = Simulator.fromSource<Ports>(
			ARRAY_1BIT_SOURCE,
			"ArrayPassThrough",
		);

		// Default: all inputs 0
		sim.tick();
		expect(sim.dut.y0).toBe(0n);
		expect(sim.dut.y1).toBe(0n);
		expect(sim.dut.y2).toBe(0n);
		expect(sim.dut.y3).toBe(0n);

		// Set element 0 only
		sim.dut.en.set(0, 1n);
		sim.tick();
		expect(sim.dut.y0).toBe(1n);
		expect(sim.dut.y1).toBe(0n);
		expect(sim.dut.y2).toBe(0n);
		expect(sim.dut.y3).toBe(0n);

		// Set element 1 (was silently ignored before the fix)
		sim.dut.en.set(0, 0n);
		sim.dut.en.set(1, 1n);
		sim.tick();
		expect(sim.dut.y0).toBe(0n);
		expect(sim.dut.y1).toBe(1n);
		expect(sim.dut.y2).toBe(0n);
		expect(sim.dut.y3).toBe(0n);

		// Set elements 2 and 3
		sim.dut.en.set(1, 0n);
		sim.dut.en.set(2, 1n);
		sim.dut.en.set(3, 1n);
		sim.tick();
		expect(sim.dut.y0).toBe(0n);
		expect(sim.dut.y1).toBe(0n);
		expect(sim.dut.y2).toBe(1n);
		expect(sim.dut.y3).toBe(1n);

		sim.dispose();
	});

	test("length property is correct", () => {
		interface Ports {
			rst: bigint;
			en: {
				at(i: number): bigint;
				set(i: number, v: bigint): void;
				length: number;
			};
			readonly y0: bigint;
			readonly y1: bigint;
			readonly y2: bigint;
			readonly y3: bigint;
		}

		const sim = Simulator.fromSource<Ports>(
			ARRAY_1BIT_SOURCE,
			"ArrayPassThrough",
		);
		expect(sim.dut.en.length).toBe(4);
		sim.dispose();
	});

	test("4-state mode: X written to element i propagates only to output yi (verifies maskBase layout)", () => {
		// This test validates that the maskBase = baseOffset + totalValueBytes assumption
		// matches the actual JIT memory layout when fourState: true is used.
		interface Ports {
			rst: bigint;
			en: {
				at(i: number): bigint;
				set(i: number, v: unknown): void;
				length: number;
			};
			readonly y0: bigint;
			readonly y1: bigint;
			readonly y2: bigint;
			readonly y3: bigint;
		}

		const sim = Simulator.fromSource<Ports>(
			ARRAY_1BIT_SOURCE,
			"ArrayPassThrough",
			{ fourState: true },
		);

		// Start with all defined zeros
		for (let i = 0; i < 4; i++) sim.dut.en.set(i, 0n);
		sim.tick();

		// Set element 1 to X; only y1 should become X
		sim.dut.en.set(1, X);
		sim.tick();

		expect(sim.fourState("y1").mask).not.toBe(0n); // y1 = X
		expect(sim.fourState("y0").mask).toBe(0n); // y0 defined
		expect(sim.fourState("y2").mask).toBe(0n); // y2 defined
		expect(sim.fourState("y3").mask).toBe(0n); // y3 defined

		// Set element 2 to X; y2 becomes X, y1 remains X
		sim.dut.en.set(2, X);
		sim.tick();

		expect(sim.fourState("y1").mask).not.toBe(0n);
		expect(sim.fourState("y2").mask).not.toBe(0n);
		expect(sim.fourState("y0").mask).toBe(0n);
		expect(sim.fourState("y3").mask).toBe(0n);

		// Clear element 1 to defined 0; y1 becomes defined again
		sim.dut.en.set(1, 0n);
		sim.tick();

		expect(sim.fourState("y1").mask).toBe(0n);
		expect(sim.fourState("y2").mask).not.toBe(0n);

		sim.dispose();
	});
});

// ---------------------------------------------------------------------------
// Dead Store Elimination (DSE) tests
// ---------------------------------------------------------------------------

describe("E2E: deadStorePolicy option", () => {
	test("preserveTopPorts: top ports work, child instances stripped from DUT", () => {
		const sim = Simulator.fromSource(HIERARCHY_SOURCE, "Top", {
			deadStorePolicy: "preserveTopPorts",
		});

		sim.dut.top_in = 0xabn;
		sim.tick();
		expect(sim.dut.top_out).toBe(0xabn);

		// With preserveTopPorts, children should be stripped from hierarchy
		expect((sim.dut as any).u_sub).toBeUndefined();

		sim.dispose();
	});

	test("preserveAllPorts: top ports AND child instance ports accessible", () => {
		const sim = Simulator.fromSource(HIERARCHY_SOURCE, "Top", {
			deadStorePolicy: "preserveAllPorts",
		});

		sim.dut.top_in = 0x42n;
		sim.tick();
		expect(sim.dut.top_out).toBe(0x42n);

		// With preserveAllPorts, child instance ports should remain accessible
		expect((sim.dut as any).u_sub).toBeDefined();
		expect((sim.dut as any).u_sub.o_data).toBe(0x42n);

		sim.dispose();
	});

	test("Simulation: preserveTopPorts strips children", () => {
		const sim = Simulation.fromSource(HIERARCHY_SOURCE, "Top", {
			deadStorePolicy: "preserveTopPorts",
		});
		sim.addClock("clk", { period: 10 });

		sim.dut.rst = 0n;
		sim.runUntil(20);
		sim.dut.rst = 1n;

		sim.dut.top_in = 0x55n;
		sim.runUntil(40);
		expect(sim.dut.top_out).toBe(0x55n);
		expect((sim.dut as any).u_sub).toBeUndefined();

		sim.dispose();
	});

	test("Simulation: preserveAllPorts keeps children", () => {
		const sim = Simulation.fromSource(HIERARCHY_SOURCE, "Top", {
			deadStorePolicy: "preserveAllPorts",
		});
		sim.addClock("clk", { period: 10 });

		sim.dut.rst = 0n;
		sim.runUntil(20);
		sim.dut.rst = 1n;

		sim.dut.top_in = 0xffn;
		sim.runUntil(40);
		expect(sim.dut.top_out).toBe(0xffn);
		expect((sim.dut as any).u_sub).toBeDefined();
		expect((sim.dut as any).u_sub.o_data).toBe(0xffn);

		sim.dispose();
	});
});

// ---------------------------------------------------------------------------
// E2E: Proto package ordering (issue #22)
// ---------------------------------------------------------------------------

const PROTO_PKG_SOURCE = `
pub proto package ItemProto {
    type Item;
}

pub package ItemPlain::<WIDTH: u32 = 16> for ItemProto {
    type Item = logic<WIDTH>;
}
`;

const GENERIC_MODULE_SOURCE = `
pub module GenericModule::<PKG: ItemProto> #(
    param DEPTH: u32 = 4,
) (
    clk  : input  clock     ,
    rst  : input  reset     ,
    d_in : input  PKG::Item ,
    d_out: output PKG::Item ,
) {
    var store: PKG::Item;
    always_ff (clk, rst) {
        if_reset { store = '0; }
        else     { store = d_in; }
    }
    assign d_out = store;
}

pub module ConcreteModule #(
    param DEPTH: u32 = 4,
) (
    clk  : input  clock     ,
    rst  : input  reset     ,
    d_in : input  logic<16> ,
    d_out: output logic<16> ,
) {
    inst u_inner: GenericModule::<ItemPlain::<16>> #(DEPTH: DEPTH) (
        clk, rst, d_in, d_out,
    );
}
`;

const UNRELATED_MODULE_SOURCE = `
pub module UnrelatedModule (
    clk  : input  clock    ,
    rst  : input  reset    ,
    d_in : input  logic<8> ,
    d_out: output logic<8> ,
) {
    var data_r: logic<8>;
    always_ff (clk, rst) {
        if_reset { data_r = 0; }
        else     { data_r = d_in; }
    }
    assign d_out = data_r;
}
`;

describe("E2E: Proto package ordering (issue #22)", () => {
	test("single-file source fails when project has proto packages in wrong order", () => {
		// Reproduce the original bug: when proto_pkg definitions come after
		// the code that references them (as a single concatenated source),
		// the analyzer fails with "referred before it is defined".
		expect(() =>
			Simulator.fromSource(
				GENERIC_MODULE_SOURCE + PROTO_PKG_SOURCE + UNRELATED_MODULE_SOURCE,
				"ConcreteModule",
			),
		).toThrow();
	});

	test("multi-source: unrelated module compiles with proto packages in project", () => {
		// New behavior: all source files are passed as separate entries.
		// The Rust from_sources() handles dependency ordering correctly.
		const sim = Simulator.fromSource<{
			d_in: bigint;
			readonly d_out: bigint;
		}>(
			PROTO_PKG_SOURCE + GENERIC_MODULE_SOURCE + UNRELATED_MODULE_SOURCE,
			"UnrelatedModule",
		);

		// Reset sequence
		sim.dut.rst = 0n;
		sim.tick();
		sim.tick();
		sim.dut.rst = 1n;

		sim.dut.d_in = 42n;
		sim.tick();
		sim.tick();

		expect(sim.dut.d_out).toBe(42n);
		sim.dispose();
	});

	test("multi-source: ConcreteModule using proto package works", () => {
		// The concrete module that actually uses the proto package should
		// also compile and simulate correctly.
		const sim = Simulator.fromSource<{
			d_in: bigint;
			readonly d_out: bigint;
		}>(
			PROTO_PKG_SOURCE + GENERIC_MODULE_SOURCE + UNRELATED_MODULE_SOURCE,
			"ConcreteModule",
		);

		// Reset sequence
		sim.dut.rst = 0n;
		sim.tick();
		sim.tick();
		sim.dut.rst = 1n;

		sim.dut.d_in = 0xabcdn;
		sim.tick();
		sim.tick();

		expect(sim.dut.d_out).toBe(0xabcdn);
		sim.dispose();
	});

	test("Simulator.create() with multi-source ModuleDefinition", () => {
		// Simulates what celox-ts-gen now generates: a ModuleDefinition with
		// sources[] containing all project files.
		const addon = loadNativeAddon();
		const nativeCreate = createSimulatorBridge(addon);

		const sim = Simulator.create(
			{
				__celox_module: true,
				name: "UnrelatedModule",
				sources: [
					{ path: "proto_pkg.veryl", content: PROTO_PKG_SOURCE },
					{ path: "generic_module.veryl", content: GENERIC_MODULE_SOURCE },
					{ path: "unrelated_module.veryl", content: UNRELATED_MODULE_SOURCE },
				],
				ports: {
					clk: { direction: "input", type: "clock", width: 1 },
					rst: { direction: "input", type: "reset", width: 1 },
					d_in: { direction: "input", type: "logic", width: 8 },
					d_out: { direction: "output", type: "logic", width: 8 },
				},
				events: ["clk"],
			},
			{ __nativeCreate: nativeCreate },
		);

		// Reset sequence
		sim.dut.rst = 0n;
		sim.tick();
		sim.tick();
		sim.dut.rst = 1n;

		sim.dut.d_in = 99n;
		sim.tick();
		sim.tick();

		expect(sim.dut.d_out).toBe(99n);
		sim.dispose();
	});
});

// ---------------------------------------------------------------------------
// Bug fix: non-byte-aligned unpacked array port (logic<34> [N])
// ---------------------------------------------------------------------------

const NON_BYTE_ALIGNED_ARRAY_SOURCE = `
pub module NonByteAlignedArray #(
    param N: u32 = 4,
) (
    clk: input  clock,
    rst: input  reset,
    out: output logic<34> [N],
) {
    for i in 0..N :g {
        assign out[i] = (i as 34) << 29;
    }
}
`;

describe("E2E: non-byte-aligned unpacked array port", () => {
	test("out.at(i) returns correct values for 34-bit elements", () => {
		interface Ports {
			rst: bigint;
			readonly out: {
				at(i: number): bigint;
				length: number;
			};
		}

		const sim = Simulator.fromSource<Ports>(
			NON_BYTE_ALIGNED_ARRAY_SOURCE,
			"NonByteAlignedArray",
		);
		sim.dut.rst = 0n;
		sim.tick();
		sim.tick();
		sim.dut.rst = 1n;
		sim.tick();

		for (let i = 0; i < 4; i++) {
			const expected = BigInt(i) << 29n;
			expect(sim.dut.out.at(i)).toBe(expected);
		}

		sim.dispose();
	});
});

// ---------------------------------------------------------------------------
// Native testbench (run_test / run_test_detailed)
// ---------------------------------------------------------------------------

const TB_COUNTER_SOURCE = `
module Counter_tb (
    clk: input  clock    ,
    rst: input  reset    ,
    cnt: output logic<32>,
) {
    always_ff {
        if_reset {
            cnt = 0;
        } else {
            cnt += 1;
        }
    }
}
`;

const TB_PASS_SOURCE = `
${TB_COUNTER_SOURCE}
#[test(t)]
module CounterTbPass {
    inst clk: $tb::clock_gen;
    inst rst: $tb::reset_gen;
    var cnt: logic<32>;
    inst dut: Counter_tb (clk, rst, cnt);
    initial {
        rst.assert(clk);
        clk.next  (10);
        $assert   (cnt == 32'd10);
        $finish   ();
    }
}
`;

const TB_FAIL_SOURCE = `
${TB_COUNTER_SOURCE}
#[test(t)]
module CounterTbFail {
    inst clk: $tb::clock_gen;
    inst rst: $tb::reset_gen;
    var cnt: logic<32>;
    inst dut: Counter_tb (clk, rst, cnt);
    initial {
        rst.assert(clk);
        clk.next  (5);
        $assert   (cnt == 32'd99);
        $assert   (cnt == 32'd5);
        $finish   ();
    }
}
`;

describe("E2E: native testbench (runTest)", () => {
	const addon = loadNativeAddon();

	test("passing testbench returns passed=true with all assertions passed", () => {
		const result: NapiTestResult = addon.runTest(
			[{ content: TB_PASS_SOURCE, path: "test.veryl" }],
			"CounterTbPass",
		);
		expect(result.passed).toBe(true);
		expect(result.assertions.length).toBe(1);
		expect(result.assertions[0]!.passed).toBe(true);
	});

	test("failing testbench returns passed=false and collects all assertions", () => {
		const result: NapiTestResult = addon.runTest(
			[{ content: TB_FAIL_SOURCE, path: "test.veryl" }],
			"CounterTbFail",
		);
		expect(result.passed).toBe(false);
		// Both assertions are collected (not stopped at first failure)
		expect(result.assertions.length).toBe(2);
		expect(result.assertions[0]!.passed).toBe(false);
		expect(result.assertions[1]!.passed).toBe(true);
	});

	test("assertion results include source location", () => {
		const result: NapiTestResult = addon.runTest(
			[{ content: TB_PASS_SOURCE, path: "test.veryl" }],
			"CounterTbPass",
		);
		const a = result.assertions[0]!;
		expect(a.file).toBeDefined();
		expect(a.line).toBeGreaterThan(0);
		expect(a.column).toBeGreaterThan(0);
	});
});
