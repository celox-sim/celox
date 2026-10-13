/** Integration coverage for periodic clocks through the native addon. */
import { afterEach, describe, expect, test } from "vitest";
import { createSimulationBridge, loadNativeAddon } from "./napi-helpers.js";
import { Simulation } from "./simulation.js";
import type { ModuleDefinition } from "./types.js";

const VERYL = `
module Top (
    clk: input clock,
    cnt: output logic<32>,
    clock_level: output logic,
) {
    var r: logic<32>;
    always_ff (clk) {
        r = r + 1;
    }
    assign cnt = r;
    assign clock_level = clk;
}
`;

const Counter: ModuleDefinition<{
	readonly cnt: bigint;
	readonly clock_level: bigint;
}> = {
	__celox_module: true,
	name: "Top",
	sources: [{ path: "Top.veryl", content: VERYL }],
	ports: {
		clk: { direction: "input", type: "clock", width: 1 },
		cnt: { direction: "output", type: "logic", width: 32 },
		clock_level: { direction: "output", type: "logic", width: 1 },
	},
	events: ["clk"],
};

let live: Simulation<{ readonly cnt: bigint; readonly clock_level: bigint }>[] =
	[];

function counter(): Simulation<{
	readonly cnt: bigint;
	readonly clock_level: bigint;
}> {
	const addon = loadNativeAddon();
	const sim = Simulation.create(Counter, {
		__nativeCreate: createSimulationBridge(addon),
	});
	live.push(sim);
	return sim;
}

afterEach(() => {
	for (const sim of live) sim.dispose();
	live = [];
});

describe("timed Simulation through the addon", () => {
	test("an odd period keeps the extra tick in the high phase", () => {
		const sim = counter();
		sim.addClock("clk", { period: 3 });

		sim.runUntil(0);
		expect(sim.dut.clock_level).toBe(1n);
		sim.runUntil(1);
		expect(sim.dut.clock_level).toBe(1n);
		sim.runUntil(2); // falling edge; rising edges at 0 and 3
		expect(sim.dut.clock_level).toBe(0n);
		expect(sim.dut.cnt).toBe(1n);
		sim.runUntil(3);
		expect(sim.dut.cnt).toBe(2n);
		sim.runUntil(9); // rising edges at 6 and 9
		expect(sim.dut.cnt).toBe(4n);
	});

	test.each([undefined, { maxSteps: 0 }])(
		"an earlier run preserves pending combinational inputs (%j)",
		(opts) => {
			const Wire: ModuleDefinition<{ d: bigint; readonly q: bigint }> = {
				__celox_module: true,
				name: "Wire",
				sources: [
					{
						path: "Wire.veryl",
						content:
							"module Wire (d: input logic<8>, q: output logic<8>) { assign q = d + 1; }",
					},
				],
				ports: {
					d: { direction: "input", type: "logic", width: 8 },
					q: { direction: "output", type: "logic", width: 8 },
				},
				events: [],
			};
			const sim = Simulation.create(Wire, {
				__nativeCreate: createSimulationBridge(loadNativeAddon()),
			});
			try {
				sim.runUntil(10);
				sim.dut.d = 42n;
				sim.runUntil(5, opts);
				expect(sim.time()).toBe(10);
				expect(sim.dut.q).toBe(43n);
			} finally {
				sim.dispose();
			}
		},
	);

	test("a degenerate period still advances time", () => {
		const sim = counter();
		sim.addClock("clk", { period: 0 });

		sim.runUntil(4); // effective period 2: rising edges at 0, 2, 4
		expect(sim.time()).toBe(4);
		expect(sim.dut.cnt).toBe(3n);
	});

	test("runUntil an earlier time changes neither time nor state", () => {
		const sim = counter();
		sim.addClock("clk", { period: 10, initialDelay: 5 });
		sim.runUntil(40); // rising edges at 5, 15, 25, 35
		expect(sim.dut.cnt).toBe(4n);

		sim.runUntil(20);
		expect(sim.time()).toBe(40);
		expect(sim.dut.cnt).toBe(4n);

		sim.runUntil(50); // rising edge at 45
		expect(sim.time()).toBe(50);
		expect(sim.dut.cnt).toBe(5n);
	});
});
