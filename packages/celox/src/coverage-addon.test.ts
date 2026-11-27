/** Covergroup coverage reports through the native addon. */
import { afterEach, describe, expect, expectTypeOf, test } from "vitest";
import { createSimulationBridge, loadNativeAddon } from "./napi-helpers.js";
import { Simulation } from "./simulation.js";
import type { Simulator } from "./simulator.js";
import type { CoverItemReport, ModuleDefinition, Report } from "./types.js";

/**
 * Two instances of a covergroup with an illegal bin, a `default` bin array
 * (which takes no part in coverage) and a cross.
 */
const SV = `\`timescale 1ns/1ns
module Top;
    bit [1:0] v;
    bit w;
    covergroup cg;
        cp: coverpoint v {
            bins lo = {0};
            bins hi = {1};
            illegal_bins bad = {3};
        }
        cw: coverpoint w;
        dv: coverpoint v { bins d[] = default; }
        x: cross cp, cw;
    endgroup
    cg a = new;
    cg b = new;
    initial begin
        v = 0; w = 1; a.sample();
        #1 v = 1; w = 0; a.sample();
        #1 v = 3; b.sample();
    end
endmodule
`;

const Covergroups: ModuleDefinition<Record<string, never>> = {
	__celox_module: true,
	name: "Top",
	sources: [{ path: "Top.sv", content: SV }],
	ports: {},
	events: [],
};

const Counter: ModuleDefinition<{ readonly cnt: bigint }> = {
	__celox_module: true,
	name: "Top",
	sources: [
		{
			path: "Top.veryl",
			content: `
module Top (
    clk: input clock,
    cnt: output logic<8>,
) {
    var r: logic<8>;
    always_ff (clk) {
        r = r + 1;
    }
    assign cnt = r;
}
`,
		},
	],
	ports: {
		clk: { direction: "input", type: "clock", width: 1 },
		cnt: { direction: "output", type: "logic", width: 8 },
	},
	events: ["clk"],
};

let live: Simulation<unknown>[] = [];

function simulation<P>(module: ModuleDefinition<P>): Simulation<P> {
	const sim = Simulation.create(module, {
		__nativeCreate: createSimulationBridge(loadNativeAddon()),
	});
	live.push(sim as Simulation<unknown>);
	return sim;
}

afterEach(() => {
	for (const sim of live) sim.dispose();
	live = [];
});

const bins = (item: CoverItemReport | undefined) =>
	item?.bins.map((bin) => [bin.name, bin.kind, bin.hits, bin.covered]);

describe("covergroup coverage through the addon", () => {
	test("coverage() reports types, instances, crosses and illegal bins", () => {
		const sim = simulation(Covergroups);
		const before = sim.coverage();
		expect(before.covergroups.map((group) => group.name)).toEqual(["Top.cg"]);
		expect(before.covergroups[0]!.instances.map((i) => i.samples)).toEqual([
			0, 0,
		]);

		sim.runUntil(10);
		const report = sim.coverage();
		expect(report.coverage).toBe(50);
		const [group] = report.covergroups;
		expect(group).toMatchObject({
			name: "Top.cg",
			mergeInstances: false,
			covered: 7,
			total: 16,
		});
		expect(group!.items.map((item) => [item.name, item.kind])).toEqual([
			["cp", "coverpoint"],
			["cw", "coverpoint"],
			["dv", "coverpoint"],
			["x", "cross"],
		]);
		// A bin of each sampled value, by the instances that sampled them.
		expect(bins(group!.items[2])).toEqual([
			["d[0]", "default", 1, true],
			["d[1]", "default", 1, true],
			["d[3]", "default", 1, true],
		]);
		expect(group!.items[2]).toMatchObject({ covered: 0, total: 0 });
		const cross = group!.items[3];
		expect(cross!.coverpoints).toEqual(["cp", "cw"]);
		expect(bins(cross)).toEqual([
			["<lo,auto[1]>", "bins", 1, true],
			["<hi,auto[0]>", "bins", 1, true],
		]);
		expect(cross!.omittedBins).toBe(2);
		expect(bins(group!.items[0])).toEqual([
			["lo", "bins", 1, true],
			["hi", "bins", 1, true],
			["bad", "illegal", 1, true],
		]);
		expect(
			group!.instances.map((instance) => [
				instance.name,
				instance.samples,
				instance.live,
				instance.collecting,
			]),
		).toEqual([
			["Top.cg#0", 2, true, true],
			["Top.cg#1", 1, true, true],
		]);
		expect(bins(group!.instances[1]!.items[0])).toEqual([
			["lo", "bins", 0, false],
			["hi", "bins", 0, false],
			["bad", "illegal", 1, true],
		]);
		// The illegal bin hit is an error report, which the run drains.
		const errors = sim
			.drainRuntimeEvents()
			.filter((event): event is Report => event.kind === "report");
		expect(errors.map((event) => event.severity)).toEqual(["error"]);
		expect(new TextDecoder().decode(errors[0]!.message)).toContain(
			"illegal_bins `bad` of coverpoint `cp`",
		);
		// Reading the report changes nothing.
		expect(sim.coverage()).toEqual(report);
	});

	test("the event-based Simulator has no coverage()", () => {
		expectTypeOf<Simulator<unknown>>().not.toHaveProperty("coverage");
	});

	test("a Veryl design has an empty report", () => {
		const sim = simulation(Counter);
		sim.addClock("clk", { period: 10 });
		sim.runUntil(100);
		expect(sim.coverage()).toEqual({ coverage: 100, covergroups: [] });
	});

	test("a disposed handle refuses the report", () => {
		const addon = loadNativeAddon();
		const raw = new addon.NativeSimulationHandle!(
			[{ path: "Top.sv", content: SV }],
			"Top",
		);
		expect(JSON.parse(raw.coverageJson!()).covergroups).toHaveLength(1);
		raw.dispose();
		expect(() => raw.coverageJson!()).toThrow("Simulation has been disposed");
		const sim = simulation(Covergroups);
		sim.dispose();
		expect(() => sim.coverage()).toThrow("Simulation has been disposed");
	});
});
