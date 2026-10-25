/** Runtime events and `failOn` through the native addon. */
import { afterEach, describe, expect, test } from "vitest";
import { createSimulationBridge, loadNativeAddon } from "./napi-helpers.js";
import { Simulation } from "./simulation.js";
import type { ModuleDefinition, Report } from "./types.js";

const VERYL = `
module Top (
    clk: input clock,
    cnt: output logic<8>,
) {
    var r: logic<8>;
    always_ff (clk) {
        $display("tick %0d", r);
        $assert_continue(r != 8'd2, "r=%0d", r);
        r = r + 1;
    }
    assign cnt = r;
}
`;

type Ports = { readonly cnt: bigint };

const Top: ModuleDefinition<Ports> = {
	__celox_module: true,
	name: "Top",
	sources: [{ path: "Top.veryl", content: VERYL }],
	ports: {
		clk: { direction: "input", type: "clock", width: 1 },
		cnt: { direction: "output", type: "logic", width: 8 },
	},
	events: ["clk"],
};

let live: Simulation<Ports>[] = [];

function simulation(): Simulation<Ports> {
	const sim = Simulation.create(Top, {
		__nativeCreate: createSimulationBridge(loadNativeAddon()),
	});
	sim.addClock("clk", { period: 10 });
	live.push(sim);
	return sim;
}

afterEach(() => {
	for (const sim of live) sim.dispose();
	live = [];
});

const text = (bytes: Uint8Array) => new TextDecoder().decode(bytes);

describe("runtime events through the addon", () => {
	test("drainRuntimeEvents returns displays and reports in order", () => {
		const sim = simulation();
		sim.runUntil(100);
		const events = sim.drainRuntimeEvents();
		const reports = events.filter((event) => event.kind === "report");
		expect(reports).toHaveLength(1);
		expect(reports[0]).toMatchObject({ severity: "error" });
		expect(text((reports[0] as Report).message)).toBe("r=2");
		const before = events.slice(0, events.indexOf(reports[0]!));
		expect(
			before.map((event) => event.kind === "display" && text(event.message)),
		).toContain("tick 2");
		expect(sim.drainRuntimeEvents()).toEqual([]);
	});

	test("failOn stops the run after the time step of the report", () => {
		const sim = simulation();
		sim.setFailOn("error");
		expect(() => sim.runUntil(1000)).toThrow(/failOn: Error/);
		const stopped = sim.time();
		expect(stopped).toBeLessThan(1000);
		const report = sim
			.drainRuntimeEvents()
			.find((event): event is Report => event.kind === "report");
		expect(report?.time).toBe(stopped);
		sim.runUntil(1000);
		expect(sim.time()).toBe(1000);
	});

	test("failOn rejects an unknown severity", () => {
		const sim = simulation();
		expect(() => sim.setFailOn("loud" as never)).toThrow(/unknown severity/);
	});
});
