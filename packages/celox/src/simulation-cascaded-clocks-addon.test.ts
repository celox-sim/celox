import { expect, test } from "vitest";
import { Simulation } from "./simulation.js";

test("independent generated clocks keep their event IDs and edge phases", () => {
	const source = `
module Generator(clk: input '_ clock, g: output logic) {
    var r: logic;
    always_ff (clk) { r = ~r; }
    assign g = r;
}
module Top(cp: input '_ clock, cn: input '_ clock,
           p: output logic<8>, n: output logic<8>) {
    var gp: '_ clock_posedge;
    var gn: '_ clock_negedge;
    inst up: Generator(clk: cp, g: gp);
    inst un: Generator(clk: cn, g: gn);
    always_ff (gp) { p += 1; }
    always_ff (gn) { n += 1; }
}
`;
	const sim = Simulation.fromSource<{ p: bigint; n: bigint }>(source, "Top");
	try {
		sim.addClock("cp", { period: 10, initialDelay: 2 });
		sim.addClock("cn", { period: 10, initialDelay: 7 });
		sim.runUntil(27);
		expect(sim.dut.p).toBe(2n);
		expect(sim.dut.n).toBe(1n);
	} finally {
		sim.dispose();
	}
});
