import { describe, expect, test } from "vitest";
import { FourState, Simulator, X } from "@celox-sim/celox";

const FOUR_STATE_SOURCE = `\
module FourStateDemo (
    a: input logic<8>,
    b: input logic<8>,
    snapshot: output logic<8>,
) {
    assign snapshot = a;
}
`;

const isWasm = !!process.env.NAPI_RS_FORCE_WASI;

describe.skipIf(!isWasm)("FourState (WASM bridge)", () => {
	test("writes and reads all-X", () => {
		const sim = Simulator.fromSource(FOUR_STATE_SOURCE, "FourStateDemo", {
			fourState: true,
		});

		(sim.dut as any).a = X;

		const a = sim.fourState("a");

		expect(a.value).toBe(0xffn);
		expect(a.mask).toBe(0xffn);

		sim.dispose();
	});

	test("round-trips partial X and clears mask on defined write", () => {
		const sim = Simulator.fromSource(FOUR_STATE_SOURCE, "FourStateDemo", {
			fourState: true,
		});

		(sim.dut as any).b = FourState(0x05, 0xf0);

		const partial = sim.fourState("b");
		expect(partial.value).toBe(0x05n);
		expect(partial.mask).toBe(0xf0n);

		sim.dut.b = 0x33n;

		const defined = sim.fourState("b");
		expect(defined.value).toBe(0x33n);
		expect(defined.mask).toBe(0n);

		sim.dispose();
	});
});
