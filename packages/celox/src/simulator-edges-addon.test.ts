import { describe, expect, test } from "vitest";
import { Simulation } from "./simulation.js";
import { Simulator } from "./simulator.js";

const COUNTERS = `
module Top (
    clk: input '_ clock,
    rst: input '_ reset_async_low,
    en: input logic,
    rising: output logic<8>,
    falling: output logic<8>,
    gated_falling: output logic<8>,
) {
    var r: logic<8>;
    var f: logic<8>;
    var g: logic<8>;
    always_ff (clk, rst) { if_reset { r = 0; } else { r = r + 8'd1; } }
    let inverted: '_ clock = ~clk;
    always_ff (inverted, rst) { if_reset { f = 0; } else { f = f + 8'd1; } }
    let gated: '_ clock = ~(clk & en);
    always_ff (gated, rst) { if_reset { g = 0; } else { g = g + 8'd1; } }
    assign rising = r;
    assign falling = f;
    assign gated_falling = g;
}
`;

type Counters = {
	en: bigint;
	rising: bigint;
	falling: bigint;
	gated_falling: bigint;
};

function reset(sim: Simulator<unknown>): void {
	const rst = sim.event("rst");
	sim.posedge(rst);
	sim.negedge(rst);
	sim.posedge(rst);
}

describe("explicit clock edges", () => {
	for (const tier of [undefined, "tiered"] as const) {
		test(`posedge and negedge run the domains of their direction (${tier ?? "default"})`, () => {
			const sim = Simulator.fromSource<Counters>(
				COUNTERS,
				"Top",
				tier ? { tier: true } : undefined,
			);
			try {
				const clk = sim.event("clk");
				const counters = () => [
					sim.dut.rising,
					sim.dut.falling,
					sim.dut.gated_falling,
				];
				sim.dut.en = 1n;
				sim.negedge(clk);
				reset(sim);
				expect(counters()).toEqual([0n, 0n, 0n]);
				sim.posedge(clk);
				expect(counters()).toEqual([1n, 0n, 0n]);
				sim.posedge(clk);
				expect(counters()).toEqual([1n, 0n, 0n]);
				sim.negedge(clk);
				expect(counters()).toEqual([1n, 1n, 1n]);
				sim.dut.en = 0n;
				sim.cycle(clk);
				expect(counters()).toEqual([2n, 2n, 1n]);
				sim.tick(clk);
				expect(counters()).toEqual([3n, 2n, 1n]);
			} finally {
				sim.dispose();
			}
		});
	}

	for (const clockType of ["posedge", "negedge"] as const) {
		test(`active edges follow the ${clockType} clock type`, () => {
			const sim = Simulator.fromSource<{ q: bigint }>(
				`module Top (clk: input clock, rst: input reset_async_low, q: output logic<8>) {
    var r: logic<8>;
    always_ff (clk, rst) { if_reset { r = 0; } else { r = r + 8'd1; } }
    assign q = r;
}`,
				"Top",
				{ clockType },
			);
			try {
				const clk = sim.event("clk");
				sim.inactiveEdge(clk);
				reset(sim);
				sim.activeEdge(clk);
				expect(sim.dut.q).toBe(1n);
				sim.inactiveEdge(clk);
				expect(sim.dut.q).toBe(1n);
				if (clockType === "posedge") {
					sim.posedge(clk);
				} else {
					sim.negedge(clk);
				}
				expect(sim.dut.q).toBe(2n);
			} finally {
				sim.dispose();
			}
		});
	}

	test("a cycle matches two timed steps", () => {
		const source = `
module Top (
    clk: input '_ clock,
    rst: input '_ reset_async_low,
    en_in: input logic,
    a_out: output logic<8>,
    b_out: output logic<8>,
    d_out: output logic<8>,
) {
    var a: logic<8>;
    var en: logic;
    var b: logic<8>;
    var d: logic<8>;
    always_ff (clk, rst) { if_reset { a = 0; en = 1; } else { a = a + 8'd1; en = en_in; } }
    let gated: '_ clock = clk & en;
    always_ff (gated, rst) { if_reset { b = 0; } else { b = b + a; } }
    let inverted: '_ clock = ~clk;
    always_ff (inverted, rst) { if_reset { d = 0; } else { d = a ^ b; } }
    assign a_out = a;
    assign b_out = b;
    assign d_out = d;
}
`;
		type Ports = { en_in: bigint; a_out: bigint; b_out: bigint; d_out: bigint };
		const enables = [1n, 0n, 1n, 1n, 0n, 0n, 1n, 0n];
		const timed = Simulation.fromSource<Ports>(source, "Top");
		const expected: bigint[][] = [];
		try {
			timed.schedule("clk", { time: 0, value: 0 });
			timed.schedule("rst", { time: 0, value: 1 });
			timed.schedule("rst", { time: 1, value: 0 });
			timed.schedule("rst", { time: 2, value: 1 });
			timed.runUntil(2);
			enables.forEach((enable, cycle) => {
				timed.dut.en_in = enable;
				timed.schedule("clk", { time: 10 + cycle * 20, value: 1 });
				timed.schedule("clk", { time: 20 + cycle * 20, value: 0 });
				timed.runUntil(20 + cycle * 20);
				expected.push([timed.dut.a_out, timed.dut.b_out, timed.dut.d_out]);
			});
		} finally {
			timed.dispose();
		}

		const sim = Simulator.fromSource<Ports>(source, "Top");
		try {
			const clk = sim.event("clk");
			sim.negedge(clk);
			reset(sim);
			const observed = enables.map((enable) => {
				sim.dut.en_in = enable;
				sim.cycle(clk);
				return [sim.dut.a_out, sim.dut.b_out, sim.dut.d_out];
			});
			expect(observed).toEqual(expected);
		} finally {
			sim.dispose();
		}
	});

	test("coincident edges sample the state before them", () => {
		const sim = Simulator.fromSource<{ a: bigint; b: bigint }>(
			`module Top (a_clk: input '_ clock, b_clk: input '_ clock, rst: input '_ reset_async_low,
            a: output logic<8>, b: output logic<8>) {
    var ra: logic<8>;
    var rb: logic<8>;
    always_ff (a_clk, rst) { if_reset { ra = 3; } else { ra = rb + 8'd1; } }
    always_ff (b_clk, rst) { if_reset { rb = 5; } else { rb = ra + 8'd2; } }
    assign a = ra;
    assign b = rb;
}`,
			"Top",
		);
		try {
			const [aClk, bClk] = [sim.event("a_clk"), sim.event("b_clk")];
			sim.edges([
				[aClk, "negedge"],
				[bClk, "negedge"],
			]);
			reset(sim);
			sim.edges([
				[aClk, "posedge"],
				[bClk, "posedge"],
			]);
			expect([sim.dut.a, sim.dut.b]).toEqual([6n, 5n]);
			expect(() => sim.edges([[aClk, "sideways" as "posedge"]])).toThrow(
				RangeError,
			);
		} finally {
			sim.dispose();
		}
	});
});
