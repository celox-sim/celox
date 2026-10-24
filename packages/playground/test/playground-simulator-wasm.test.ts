/**
 * The playground's own runtime over modules from the WASM addon: ticks,
 * explicit edges and timed steps fire derived and gated clocks as the
 * native simulator does. The expected counts are the native simulator's.
 */
import { loadNativeAddon, Simulator, X } from "@celox-sim/celox";
import { describe, expect, test } from "vitest";
import {
	type CompilerHandle,
	compiledFromHandle,
} from "../src/compile-simulator";
import {
	createPlaygroundSimulation,
	createPlaygroundSimulator,
} from "../src/playground-simulator";

const isWasm =
	!!process.env.NAPI_RS_WASI_FLAVOR ||
	process.env.NAPI_RS_FORCE_WASI === "true" ||
	process.env.NAPI_RS_FORCE_WASI === "error";

const CLOCKS = `module Top (
    clk: input clock,
    en: input logic,
    rising: output logic<8>,
    falling: output logic<8>,
    gated_count: output logic<8>,
    divided_count: output logic<8>,
) {
    always_ff (clk) { rising += 1; }
    let inverted: '_ clock = ~clk;
    always_ff (inverted) { falling += 1; }
    let gated: '_ clock = clk & en;
    always_ff (gated) { gated_count += 1; }
    var half: logic;
    always_ff (clk) { half = ~half; }
    let divided: '_ clock = half;
    always_ff (divided) { divided_count += 1; }
}`;

// `clk` has no domain of its own: it only drives the gated clock.
const LEVER = `module Lever (clk: input clock, en: input logic, count: output logic<8>) {
    let gated: '_ clock = clk & en;
    always_ff (gated) { count += 1; }
}`;

type WasmAddon = {
	NativeSimulatorHandle: new (
		sources: { content: string; path: string }[],
		top: string,
		options?: { fourState?: boolean },
	) => CompilerHandle & { dispose(): void };
};

// A gated clock with a reset, so that its counter is known in four-state
// simulation.
const GATED_RESET = `module GatedReset (
    clk: input clock,
    rst: input reset_async_low,
    en: input logic,
    count: output logic<8>,
) {
    let gated: '_ clock = clk & en;
    always_ff (gated, rst) { if_reset { count = 0; } else { count += 1; } }
}`;

function compile(source: string, top: string, fourState = false) {
	const addon = loadNativeAddon() as unknown as WasmAddon;
	const handle = new addon.NativeSimulatorHandle(
		[{ content: source, path: "top.veryl" }],
		top,
		{ fourState },
	);
	try {
		return compiledFromHandle(handle, fourState);
	} finally {
		handle.dispose();
	}
}

describe.skipIf(!isWasm)("playground runtime (WASM)", () => {
	test("explicit edges and ticks run derived and gated domains", () => {
		const sim = createPlaygroundSimulator(compile(CLOCKS, "Top"));
		const counts = () =>
			[
				sim.dut.rising,
				sim.dut.falling,
				sim.dut.gated_count,
				sim.dut.divided_count,
			].map(Number);
		const clk = sim.event("clk");
		sim.dut.en = 1n;

		sim.posedge(clk);
		expect(counts()).toEqual([1, 0, 1, 1]);
		sim.negedge(clk);
		expect(counts()).toEqual([1, 1, 1, 1]);
		// A tick is a clock period: both edges of the clock.
		sim.tick(clk);
		expect(counts()).toEqual([2, 2, 2, 1]);
		sim.tick(clk);
		expect(counts()).toEqual([3, 3, 3, 2]);
		sim.dut.en = 0n;
		sim.tick(clk);
		expect(counts()).toEqual([4, 4, 3, 2]);
		// The clock is already at its inactive level: no edge.
		sim.inactiveEdge(clk);
		expect(counts()).toEqual([4, 4, 3, 2]);
		sim.activeEdge();
		sim.edges([[clk, "negedge"]]);
		expect(counts()).toEqual([5, 5, 3, 3]);
		expect(() => sim.edges([[clk, "sideways" as "posedge"]])).toThrow(
			RangeError,
		);
	});

	test("a clock that only drives a gated clock is an event", () => {
		const sim = createPlaygroundSimulator(compile(LEVER, "Lever"));
		const clk = sim.event("clk");
		sim.dut.en = 1n;
		sim.tick(clk);
		sim.tick(clk);
		expect(sim.dut.count).toBe(2n);
		sim.tick(clk);
		expect(sim.dut.count).toBe(3n);
	});

	test("the package's WASM bridge also drives such a clock", () => {
		const sim = Simulator.fromSource(LEVER, "Lever");
		const clk = sim.event("clk");
		sim.dut.en = 1n;
		sim.tick(clk);
		sim.tick(clk);
		expect(sim.dut.count).toBe(2n);
		sim.dispose();
	});

	test("timed steps fire derived and gated clocks", () => {
		const sim = createPlaygroundSimulation(compile(CLOCKS, "Top"));
		sim.dut.en = 1n;
		sim.addClock("clk", { period: 10 });
		sim.runUntil(100);
		expect(
			[
				sim.dut.rising,
				sim.dut.falling,
				sim.dut.gated_count,
				sim.dut.divided_count,
			].map(Number),
		).toEqual([11, 10, 11, 6]);
	});

	test("dump reports every signal to the sink", () => {
		const dumps: { timestamp: number; names: string[] }[] = [];
		const sim = createPlaygroundSimulator(compile(LEVER, "Lever"), (timestamp, signals) =>
			dumps.push({ timestamp, names: signals.map((s) => s.name) }),
		);
		sim.dump(7);
		expect(dumps).toHaveLength(1);
		expect(dumps[0]?.timestamp).toBe(7);
		expect(dumps[0]?.names).toEqual(expect.arrayContaining(["en", "count"]));
	});

	test("four-state edges judge a gated clock from x (IEEE 1800-2023 Table 9-2)", () => {
		const sim = createPlaygroundSimulator(
			compile(GATED_RESET, "GatedReset", true),
		);
		const clk = sim.event("clk");
		const rst = sim.event("rst");
		sim.negedge(rst);
		sim.posedge(rst);
		expect(sim.dut.count).toBe(0n);
		// The clock rises from x with `en` at x: `x & x` stays x, no edge.
		// The DUT is typed for two-state values; its setter takes X too.
		(sim.dut as Record<string, unknown>).en = X;
		sim.posedge(clk);
		expect(sim.dut.count).toBe(0n);
		// From a known 0, `0 & 1` -> `1 & 1` rises.
		sim.negedge(clk);
		sim.dut.en = 1n;
		sim.posedge(clk);
		expect(sim.dut.count).toBe(1n);
	});
});
